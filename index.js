// The Node client for reeflog — the "SDKs" the README's free tier promises,
// shipped as one file with no dependencies.
//
// Why this exists (2026-11-07): reeflog's free tier claims "daemon, journal,
// CLI, SDKs, webhook fan-out …" and a third party who wanted to consume the
// mailbox from Node had to hand-roll `spawn('reeflog tail')` themselves and,
// worse, get the EXIT-CODE contract right by hand. That contract is the whole
// product:
//
//   exit 0 — the batch is fully delivered, the cursor advanced;
//   exit 3 — the delivery HELD at a row whose account envelope is still owed
//            (tail), or the scan proves nothing (verify/repair/hydrate).
//            Rows before the hold WERE delivered; nothing at or after it was
//            consumed. Treating exit 3 as "failure, retry the batch" replays
//            rows; treating it as success silently drops the owed row;
//   exit 1 — the run refused (no journal, unknown flag, missing value).
//
// The client maps that contract into types instead of letting each consumer
// re-derive it from a blog post. It never opens the journal: every call goes
// through the `reeflog` binary, so there is exactly one implementation of the
// reader contract and no drift between a library's idea of the schema and the
// daemon's.
//
// Cursor files are the binary's (it reads and writes them); the client only
// picks a default path per channel so two consumers of the same journal do
// not share a cursor by accident. Pass your own `cursor` to identify a
// consumer.

import { spawn } from 'node:child_process';
import path from 'node:path';
import fs from 'node:fs';
import os from 'node:os';
import http from 'node:http';
import https from 'node:https';
import { createHash } from 'node:crypto';
import { gunzipSync } from 'node:zlib';

/** Exit codes the reeflog CLI uses, mirrored as constants (not guessed:
 *  documented in the README and pinned by tests/arg_parsing.rs). */
export const EXIT = {
  OK: 0,
  REFUSED: 1,
  INCOMPLETE: 3,
};

/** A refusal or incomplete pass, carrying the binary's own words. */
export class ReeflogError extends Error {
  constructor(message, { exitCode, stderr, args } = {}) {
    super(message);
    this.name = 'ReeflogError';
    this.exitCode = exitCode;
    this.stderr = stderr;
    this.args = args;
  }
}

/** The single call everything else is built on: run the binary, collect
 *  stdout/stderr, and hand back { code, stdout, stderr, lines } with the
 *  refusal already classified. Nothing is thrown on exit 3 — the caller
 *  decides what "held" or "incomplete" means for its own channel.
 *
 *  CLASSIFIED runs (the typed API above) accept only the codes the product's
 *  contract defines: 0 delivered, 1 refused, 3 held/incomplete. Every other
 *  close — the journal-write failure's exit 2, an old binary's usage exit 64
 *  (version skew), a panic's 101, a kill by signal (close code null) — is a
 *  REFUSAL carrying that code and the binary's last words. Before this rule
 *  `run()` treated "not 1 and not 3" as success, so a run that died AFTER
 *  printing rows resolved as a delivered batch (held:null = "fully
 *  delivered"), and a SIGKILL'd child was dressed up as a forged "exit 1"
 *  refusal that named no signal. Both faces measured on the published
 *  v0.7.19 tree — research/client-exit-voice-2026-11-12.json.
 *
 *  `unclassified` (raw() only) keeps the documented escape hatch: the code
 *  comes back as { exitCode } so a caller can implement semantics this file
 *  does not know yet. */
function run(bin, args, { allowExit3 = false, unclassified = false } = {}) {
  return new Promise((resolve, reject) => {
    const child = spawn(bin, args, { stdio: ['ignore', 'pipe', 'pipe'] });
    let stdout = '';
    let stderr = '';
    child.stdout.on('data', (d) => { stdout += d; });
    child.stderr.on('data', (d) => { stderr += d; });
    child.on('error', (e) => {
      if (e.code === 'ENOENT') {
        reject(new ReeflogError(
          `reeflog binary not found at "${bin}" — get one without a Rust toolchain: \`import { bootstrap } from 'reeflog-client'; await bootstrap()\` (verifies the release's sha256), or from source: \`cargo install --git https://github.com/auroraxo/reeflog --tag ${packageVersion() || 'v0.7.0'}\`; or pass { bin: "/path/to/reeflog" }`,
          { args },
        ));
        return;
      }
      reject(new ReeflogError(`reeflog failed to start: ${e.message}`, { args }));
    });
    child.on('close', (code, signal) => {
      const exitCode = unclassified ? (code ?? 1) : code;
      // A kill (SIGKILL, OOM, Ctrl-C) leaves code=null. The binary never
      // refused anything — say who ended the run, with the stderr it did
      // manage to write, instead of minting "exit 1".
      if (!unclassified && code == null) {
        reject(new ReeflogError(
          `reeflog was killed by ${signal ?? 'an unknown signal'} — no verdict was produced${stderr.trim() ? `: ${stderr.trim()}` : ''}`,
          { exitCode: null, stderr, args },
        ));
        return;
      }
      if (exitCode === 1) {
        // The binary's own refusal, verbatim: "no journal at … — run
        // `reeflog init …`" and friends are the actionable part.
        const first = stderr.split('\n').find((l) => l.trim()) || `exit ${exitCode}`;
        reject(new ReeflogError(first.trim(), { exitCode, stderr, args }));
        return;
      }
      if (exitCode === 3 && !allowExit3) {
        reject(new ReeflogError(
          `reeflog pass INCOMPLETE: ${stderr.trim()}`,
          { exitCode, stderr, args },
        ));
        return;
      }
      if (!unclassified && exitCode !== 0 && exitCode !== 3) {
        // The contract knows 0/1/3. Anything else is a crashed or mismatched
        // run: name the REAL code and the binary's last words, so a version
        // skew (usage text) or a journal-write failure reaches the caller
        // instead of JSON.parse eating it or a zero-row batch passing as
        // "delivered".
        const first = stderr.split('\n').find((l) => l.trim()) || stderr.trim();
        const hint = exitCode === 64
          ? ' (64 = usage error — this binary does not know the invocation; check for client/binary version skew)'
          : '';
        reject(new ReeflogError(
          `reeflog exited with code ${exitCode}${hint} — refusing to call it a verdict${first ? `: ${first}` : ''}`,
          { exitCode, stderr, args },
        ));
        return;
      }
      resolve({
        exitCode,
        stdout,
        stderr,
        lines: stdout.split('\n').filter((l) => l.trim()),
      });
    });
  });
}

const parseJsonLines = (lines) => lines.map((l) => JSON.parse(l));

function cursorFor(channel, db) {
  return path.resolve(`${db}.${channel}.cursor`);
}

/**
 * The numeric POSITION a cursor file holds, in either form it may carry:
 * a bare whole number (the operator's deliberate rewind) or the structured
 * cursor reeflog writes since v0.7.13 (`{"v":1,"channel":…,"journal":…,
 * "value":N}`). The client reports the position, never the encoding.
 * @param {string} file cursor file path
 * @returns {number} the position, 0 when there is no file
 */
function cursorPosition(file) {
  if (!fs.existsSync(file)) return 0;
  const raw = fs.readFileSync(file, 'utf8').trim();
  if (!raw) return 0;
  if (raw.startsWith('{')) {
    try {
      const v = JSON.parse(raw);
      if (Number.isInteger(v?.value)) return v.value;
    } catch {
      // fall through to 0 — a cursor we cannot read is no position
    }
    return 0;
  }
  const n = Number(raw);
  return Number.isInteger(n) ? n : 0;
}

export class Reeflog {
  /**
   * @param {object} opts
   * @param {string} opts.db      path to the journal (created by `reeflog init`)
   * @param {string} [opts.bin]   path to the reeflog binary (default: `reeflog` on PATH)
   */
  constructor({ db, bin = 'reeflog' } = {}) {
    if (!db) throw new ReeflogError('Reeflog({ db }) requires the journal path');
    this.db = db;
    this.bin = bin;
  }

  /** `reeflog status` as parsed JSON: event counts, payload/envelope debt,
   *  gap counters, and the exact window the journal claims. */
  async status() {
    const r = await run(this.bin, ['status', '--db', this.db]);
    return JSON.parse(r.stdout);
  }

  /**
   * One batch of events after the cursor — `reeflog tail`.
   *
   * Cursor semantics are the binary's: the file advances to the newest row
   * CONSUMED (delivered, or skipped because a recorded outcome made it
   * permanently undeliverable). On exit 3 the hold row and everything after
   * it are NOT consumed — the next call returns them once the debt is paid
   * (`reeflog hydrate --accounts`), so nothing is ever lost to a consumer
   * that acted on a partial batch.
   *
   * @param {object} [opts]
   * @param {number} [opts.limit=1000]
   * @param {string} [opts.cursor]     cursor file; defaults per db+consumer
   * @param {string} [opts.consumer]   label for the default cursor file
   * @param {boolean} [opts.withAccounts] hold at a row whose account envelope is owed
   * @param {boolean} [opts.noLogs]    identity-only rows (no logs/accounts payload)
   * @returns {Promise<{events: object[], held: object|null, cursor: string, exitCode: number}>}
   *   `held` is the row the delivery stopped at (exit 3) or null.
   */
  async tail({ limit = 1000, cursor, consumer = 'default', withAccounts = false, noLogs = false } = {}) {
    const cursorFile = cursor ?? cursorFor(`tail.${consumer}`, this.db);
    const args = ['tail', '--db', this.db, '--cursor', cursorFile, '--limit', String(limit)];
    if (withAccounts) args.push('--with-accounts');
    if (noLogs) args.push('--no-logs');

    const r = await run(this.bin, args, { allowExit3: true });
    const events = parseJsonLines(r.lines);

    // exit 3 on tail means exactly one thing: HELD at an owed row, named in
    // stderr ("HELD at seq N <sig>: …"). The rows printed above it are
    // delivered and the cursor already advanced past them.
    let held = null;
    if (r.exitCode === EXIT.INCOMPLETE) {
      // The binary names the hold as "HELD at seq <n> <signature>: <why>".
      // The signature is base58, so it never carries a colon — stop the match
      // before the colon instead of swallowing it.
      const m = r.stderr.match(/HELD at seq (\d+) ([^\s:]+)/);
      held = m
        ? { seq: Number(m[1]), signature: m[2], reason: r.stderr.trim() }
        : { seq: null, signature: null, reason: r.stderr.trim() };
    }

    return {
      events,
      held,
      cursor: cursorFile,
      cursorValue: cursorPosition(cursorFile),
      exitCode: r.exitCode,
    };
  }

  /**
   * The retraction channel — `reeflog reorg tail`: events the chain no
   * longer knows, each with the two independent sources that refused it.
   * Separate cursor from `tail` (the channels are separate).
   */
  async reorgs({ limit = 1000, cursor, consumer = 'default' } = {}) {
    const cursorFile = cursor ?? cursorFor(`reorg.${consumer}`, this.db);
    const r = await run(this.bin, ['reorg', 'tail', '--db', this.db, '--cursor', cursorFile, '--limit', String(limit)]);
    return {
      retractions: parseJsonLines(r.lines),
      cursor: cursorFile,
      cursorValue: cursorPosition(cursorFile),
      exitCode: r.exitCode,
    };
  }

  /**
   * `reeflog verify` — held against the chain. Exit 3 (incomplete scan)
   * throws: an incomplete scan proves NOTHING and must never be mistaken
   * for a clean one. `complete: false` therefore never returns normally.
   */
  async verify({ pages, sinceSlot, rpc, program, anchorSlot } = {}) {
    const args = ['verify', '--db', this.db];
    if (pages != null) args.push('--pages', String(pages));
    if (sinceSlot != null) args.push('--since-slot', String(sinceSlot));
    if (rpc) args.push('--rpc', rpc);
    if (program) args.push('--program', program);
    if (anchorSlot != null) args.push('--anchor-slot', String(anchorSlot));
    const r = await run(this.bin, args); // exit 3 -> ReeflogError
    return JSON.parse(r.stdout);
  }

  /** `reeflog repair` — fill holes; throws on an incomplete pass. */
  async repair({ pages, sinceSlot, rpc, program, anchorSlot } = {}) {
    const args = ['repair', '--db', this.db];
    if (pages != null) args.push('--pages', String(pages));
    if (sinceSlot != null) args.push('--since-slot', String(sinceSlot));
    if (rpc) args.push('--rpc', rpc);
    if (program) args.push('--program', program);
    if (anchorSlot != null) args.push('--anchor-slot', String(anchorSlot));
    const r = await run(this.bin, args);
    return JSON.parse(r.stdout);
  }

  /**
   * `reeflog hydrate` — pay the payload/envelope debt for owed rows.
   * Throws on an incomplete pass (exit 3), because the debt it reports is
   * real; the counts in the returned summary say what was paid.
   */
  async hydrate({ limit, rpc, accounts = false, owners = false, accountData = false } = {}) {
    const args = ['hydrate', '--db', this.db];
    if (limit != null) args.push('--limit', String(limit));
    if (rpc) args.push('--rpc', rpc);
    if (accounts) args.push('--accounts');
    if (owners) args.push('--owners');
    if (accountData) args.push('--account-data');
    const r = await run(this.bin, args);
    return JSON.parse(r.stdout);
  }

  /**
   * Escape hatch: any documented command, raw. The exit code comes back
   * instead of being classified, so a caller can implement semantics this
   * file does not know yet — without re-implementing the spawn. Codes the
   * typed API refuses (2/64/101/signal) are still handed back here; that
   * is the hatch's whole point.
   */
  async raw(args) {
    return run(this.bin, args, { allowExit3: true, unclassified: true });
  }
}

/**
 * Minimal tar reader for ONE regular file named `reeflog` — enough for the
 * release archives (`tar -czf … reeflog`, ustar/gnu, one entry). It refuses
 * anything it does not understand rather than guessing: a bootstrap that
 * writes an unexpected layout is worse than one that stops.
 */
function extractReeflogFromTarGz(gz) {
  const tar = gunzipSync(gz);
  const wanted = [];
  for (let off = 0; off + 512 <= tar.length; off += 512) {
    const header = tar.subarray(off, off + 512);
    if (header.every((b) => b === 0)) break; // end-of-archive marker
    const name = header.subarray(0, 100).toString('utf8').replace(/\0.*$/s, '');
    const sizeField = header.subarray(124, 136).toString('utf8').replace(/\0.*$/s, '').trim();
    const size = parseInt(sizeField || '0', 8);
    const type = String.fromCharCode(header[156]);
    const data = tar.subarray(off + 512, off + 512 + size);
    if ((type === '0' || type === '\0' || type === '') && path.posix.basename(name) === 'reeflog') {
      wanted.push(Buffer.from(data));
    }
    off += 512 * Math.ceil(size / 512);
  }
  if (wanted.length !== 1) {
    throw new ReeflogError(
      `release archive holds ${wanted.length} 'reeflog' binaries, expected exactly 1 — refusing to install an archive I do not understand`,
    );
  }
  return wanted[0];
}

// The prebuilt archives the release workflow actually publishes. This list
// must name EXACTLY the assets in .github/workflows/release.yml — a client
// that maps a platform to an asset the release does not carry sends a
// stranger to a 404 instead of a binary. tests/bootstrap.test.mjs pins the
// two lists against each other.
export const RELEASE_ASSETS = {
  'linux-x64': 'reeflog-linux-amd64.tar.gz',
  'darwin-arm64': 'reeflog-macos-arm64.tar.gz',
};

/** The bound on one HTTP leg of bootstrap: the SAME no-answer class the
 *  daemon-side webhook refuses with `timeout_ms` (src/webhook.rs,
 *  MAX_TIMEOUT_MS — "a webhook whose endpoint hangs must not hang the
 *  delivery pass") was re-opened on the client. Measured 2026-11-11 against
 *  the PUBLISHED v0.7.17 tree: an endpoint that accepts the connection and
 *  never answers left `await bootstrap()` hanging with NO verdict (45 s
 *  watchdog, research/bootstrap-hang-published-v0717-2026-11-11.json), and a
 *  self-referential 302 was re-requested 14,937 times in 12 s with none.
 *
 *  The rule: every leg gets a verdict inside the budget. The timer is an
 *  INACTIVITY bound, not a total one — it arms at headers and resets on every
 *  received byte — so a slow link that makes progress keeps working (guard
 *  G3), while silence cannot park a stranger's install forever. Redirects
 *  follow at most MAX_REDIRECTS hops and a loop is REFUSED BY NAME, not
 *  retried into the ground. */
export const DEFAULT_GET_TIMEOUT_MS = 30_000;
export const MAX_REDIRECTS = 5;

function httpGet(url, { timeoutMs = DEFAULT_GET_TIMEOUT_MS, redirectsLeft = MAX_REDIRECTS } = {}) {
  const mod = url.startsWith('https:') ? https : http;
  return new Promise((resolve, reject) => {
    let settled = false;
    const fail = (message) => {
      if (settled) return;
      settled = true;
      reject(new ReeflogError(message));
    };
    const req = mod.get(url, { headers: { 'user-agent': 'reeflog-client' } }, (res) => {
      // Headers arrived: the headers-phase timer has done its job. Hand the
      // budget to the between-bytes timer below. (Without this, the socket
      // idle timeout re-arms on the last byte and races the quiet timer at
      // the same deadline, then reports a headers stall that never was.)
      req.setTimeout(0);
      if (res.statusCode >= 300 && res.statusCode < 400 && res.headers.location) {
        res.resume();
        if (redirectsLeft <= 0) {
          // The loop face, refused. A bounded hop count turning into this
          // refusal is the whole difference between "mirror hiccup" and
          // "14,937 re-requests with no verdict".
          fail(`download of ${url} failed: redirect chain exceeded ${MAX_REDIRECTS} hops — refusing to follow a redirect loop (this hop: ${url})`);
          return;
        }
        let next;
        try {
          next = new URL(res.headers.location, url).href;
        } catch {
          fail(`download of ${url} failed: HTTP ${res.statusCode} with unparseable redirect target ${JSON.stringify(res.headers.location)}`);
          return;
        }
        httpGet(next, { timeoutMs, redirectsLeft: redirectsLeft - 1 }).then(
          (b) => { if (!settled) resolve(b); },
          (e) => fail(e.message),
        );
        return;
      }
      if (res.statusCode !== 200) {
        res.resume();
        fail(`download of ${url} failed: HTTP ${res.statusCode}`);
        return;
      }
      // Headers arrived: the budget now runs BETWEEN bytes. Any single gap
      // of timeoutMs with no progress is the stall; any byte defers the
      // verdict, which keeps real slow links on the working side.
      let quiet = null;
      const armQuiet = () => {
        if (quiet) clearTimeout(quiet);
        quiet = setTimeout(() => {
          res.destroy();
          fail(`download of ${url} stalled: no bytes for ${timeoutMs} ms after headers — refusing to wait without a verdict (pass { timeoutMs } to bootstrap() to allow slower links)`);
        }, timeoutMs);
        quiet.unref?.();
      };
      armQuiet();
      const chunks = [];
      res.on('data', (d) => { chunks.push(d); armQuiet(); });
      res.on('end', () => {
        if (quiet) clearTimeout(quiet);
        if (!settled) { settled = true; resolve(Buffer.concat(chunks)); }
      });
      res.on('error', () => fail(`download of ${url} failed: connection reset mid-body`));
    });
    // No verdict at all inside the budget is the defect; destroy the socket.
    // (An un-destroyed stalled request also pins the process open — the
    // pre-fix ladder could not even exit its own runner.)
    req.setTimeout(timeoutMs, () => {
      req.destroy();
      fail(`download of ${url} stalled: no response headers within ${timeoutMs} ms — refusing to wait without a verdict (pass { timeoutMs } to bootstrap() to allow slower links)`);
    });
    req.on('error', (e) => fail(`download of ${url} failed: ${e.message}`));
  });
}

/** The version this client ships with — read from the package manifest, never
 *  hard-coded. A hard-coded version here is the pin-lag defect this repo has
 *  already measured twice (signerd v0.2.4→v0.2.6): the client would fetch
 *  an OLD release while the package says a newer one. test/bootstrap.test.mjs
 *  pins the two together. */
function packageVersion() {
  try {
    const pkg = JSON.parse(fs.readFileSync(path.join(import.meta.dirname, 'package.json'), 'utf8'));
    if (pkg && typeof pkg.version === 'string' && /^\d+\.\d+\.\d+$/.test(pkg.version)) {
      return `v${pkg.version}`;
    }
  } catch { /* fall through to the explicit version */ }
  return null;
}

/**
 * Install the `reeflog` binary for this platform WITHOUT a Rust toolchain:
 * take the prebuilt archive from the GitHub release, verify its bytes
 * against the sha256 the release API stores for that asset, then extract.
 *
 * Trust: the same anchor as `cargo install --git` (this repository over
 * TLS) — it does not defend against whoever can push tags here, and says
 * so. Unsupported platforms and missing digests REFUSE; nothing is written
 * before the digest matches. A cache hit re-hashes the installed binary
 * against the sidecar's `binarySha256` — the return value's `digest` is the
 * ARCHIVE hash, `binarySha256` is the hash of `bin` itself.
 *
 * @returns {Promise<{bin: string, asset: string, digest: string, binarySha256: string, cached: boolean}>}
 */
export async function bootstrap({
  version = packageVersion(),
  platform = process.platform,
  arch = process.arch,
  dest = path.join(os.homedir(), '.reeflog'),
  apiBase = 'https://api.github.com',
  downloadBase = null,
  timeoutMs = DEFAULT_GET_TIMEOUT_MS,
} = {}) {
  const asset = RELEASE_ASSETS[`${platform}-${arch}`];
  if (!asset) {
    throw new ReeflogError(
      `no prebuilt reeflog binary for ${platform}-${arch} (published: ${Object.keys(RELEASE_ASSETS).join(', ')}) — install from source: \`cargo install --git https://github.com/auroraxo/reeflog --tag ${version}\``,
    );
  }
  if (!/^v\d+\.\d+\.\d+$/.test(String(version))) {
    throw new ReeflogError(`bootstrap needs an explicit release tag like ${packageVersion() || 'v0.7.0'}, got ${JSON.stringify(version)}`);
  }

  const bin = path.join(dest, 'reeflog');
  const metaFile = path.join(dest, 'install.json');
  if (fs.existsSync(bin) && fs.existsSync(metaFile)) {
    try {
      const meta = JSON.parse(fs.readFileSync(metaFile, 'utf8'));
      if (meta.version === version && meta.asset === asset) {
        // A cache hit is a VERIFIED claim, so it has to be verified here:
        // re-hash the bytes on disk and compare them to the sidecar. Before
        // this check the sidecar's version+asset alone produced
        // `{ cached: true, digest: <archive sha256> }` for ANY file sitting
        // at `bin` — replaced, truncated, or corrupted — and `digest` names
        // the ARCHIVE, never the binary (measured 2026-11-07 against the
        // published v0.7.0 client: tampered binary reported as verified,
        // claimed f6b36c53… / actual 6b9fd8b7…).
        // A sidecar without `binarySha256` (every install made before this
        // fix) proves nothing about the bytes: fall through, re-fetch once,
        // upgrade the sidecar, cache again.
        if (typeof meta.binarySha256 === 'string' && /^[0-9a-f]{64}$/.test(meta.binarySha256)) {
          const onDisk = createHash('sha256').update(fs.readFileSync(bin)).digest('hex');
          if (onDisk === meta.binarySha256) {
            return { bin, asset, digest: meta.sha256, binarySha256: onDisk, cached: true };
          }
        }
      }
    } catch { /* fall through to a fresh install */ }
  }

  const releaseUrl = `${apiBase}/repos/auroraxo/reeflog/releases/tags/${version}`;
  const rawRelease = (await httpGet(releaseUrl, { timeoutMs })).toString('utf8');
  // The API leg can answer 200 with a body that is not a release: a captive
  // portal's login HTML (measured 2026-11-11 against the published v0.7.18
  // tree: JSON.parse threw SyntaxError straight out of bootstrap(), a crash
  // no ReeflogError catch ever saw), or a rate-limit/proxy JSON whose own
  // `message` was then discarded in favour of "release has no asset", a
  // verdict that sends a stranger to a release page that is fine
  // (research/captive-portal-2026-11-11.json). A body that is not a release
  // object must refuse WITH the body's own words — never with an engine
  // crash, never with a cause the endpoint did not give.
  let release;
  try {
    release = JSON.parse(rawRelease);
  } catch (e) {
    const looksHtml = /^\s*</.test(rawRelease);
    throw new ReeflogError(
      `the release API at ${releaseUrl} did not answer JSON — `
      + (looksHtml
        ? 'it answered with HTML (a captive portal or an intercepting proxy fits this shape)'
        : `JSON.parse failed: ${e.message}`)
      + ' — refusing to install from a body I do not understand. Behind a captive portal, authenticate in the browser first; for a mirror or a different host pass { apiBase }.',
    );
  }
  if (release === null || typeof release !== 'object' || Array.isArray(release) || !Array.isArray(release.assets)) {
    const words = release && typeof release.message === 'string'
      ? `its own message: ${release.message}`
      : `body: ${JSON.stringify(release)?.slice(0, 300) ?? String(release)}`;
    throw new ReeflogError(
      `the release API at ${releaseUrl} answered without a release object (${words}) — refusing to install; nothing was written. `
      + 'If this is a rate limit or a proxy, wait or bypass it; for a different host pass { apiBase }.',
    );
  }
  const entry = (release.assets || []).find((a) => a.name === asset);
  if (!entry) {
    throw new ReeflogError(`release ${version} has no asset named ${asset} — nothing to install`);
  }
  const expected = (entry.digest || '').replace(/^sha256:/i, '');
  if (!/^[0-9a-f]{64}$/.test(expected)) {
    throw new ReeflogError(
      `the release API reports no sha256 digest for ${asset} — refusing to install bytes I cannot verify`,
    );
  }

  const url = downloadBase ? `${downloadBase}/${asset}` : entry.browser_download_url;
  const gz = await httpGet(url, { timeoutMs });
  const actual = createHash('sha256').update(gz).digest('hex');
  if (actual !== expected) {
    throw new ReeflogError(
      `sha256 mismatch for ${asset}: release says ${expected}, download is ${actual} — refusing to install`,
    );
  }

  const executable = extractReeflogFromTarGz(gz);
  const binarySha256 = createHash('sha256').update(executable).digest('hex');
  fs.mkdirSync(dest, { recursive: true });
  const tmp = path.join(dest, `.reeflog-${process.pid}.tmp`);
  fs.writeFileSync(tmp, executable, { mode: 0o755 });
  fs.renameSync(tmp, bin); // atomic: a crash leaves the old binary or none
  // `sha256` is the ARCHIVE digest (what the release API stores);
  // `binarySha256` is the EXECUTABLE's own hash — the only value the cache
  // may be checked against, and the one a caller can check `bin` with.
  fs.writeFileSync(metaFile, `${JSON.stringify({ version, asset, sha256: actual, binarySha256, installedAt: new Date().toISOString() }, null, 2)}\n`);

  return { bin, asset, digest: actual, binarySha256, cached: false };
}

/** Convenience constructor: `const box = reeflog({ db: 'reeflog.db' })`. */
export function reeflog(opts) {
  return new Reeflog(opts);
}

export default reeflog;
