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
 *  decides what "held" or "incomplete" means for its own channel. */
function run(bin, args, { allowExit3 = false } = {}) {
  return new Promise((resolve, reject) => {
    const child = spawn(bin, args, { stdio: ['ignore', 'pipe', 'pipe'] });
    let stdout = '';
    let stderr = '';
    child.stdout.on('data', (d) => { stdout += d; });
    child.stderr.on('data', (d) => { stderr += d; });
    child.on('error', (e) => {
      if (e.code === 'ENOENT') {
        reject(new ReeflogError(
          `reeflog binary not found at "${bin}" — get one without a Rust toolchain: \`import { bootstrap } from 'reeflog-client'; await bootstrap()\` (verifies the release's sha256), or from source: \`cargo install --git https://github.com/auroraxo/reeflog --tag v0.7.0\`; or pass { bin: "/path/to/reeflog" }`,
          { args },
        ));
        return;
      }
      reject(new ReeflogError(`reeflog failed to start: ${e.message}`, { args }));
    });
    child.on('close', (code) => {
      const exitCode = code ?? 1;
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
      cursorValue: fs.existsSync(cursorFile)
        ? Number(fs.readFileSync(cursorFile, 'utf8').trim())
        : 0,
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
      cursorValue: fs.existsSync(cursorFile)
        ? Number(fs.readFileSync(cursorFile, 'utf8').trim())
        : 0,
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
   * file does not know yet — without re-implementing the spawn.
   */
  async raw(args) {
    return run(this.bin, args, { allowExit3: true });
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

function httpGet(url) {
  const mod = url.startsWith('https:') ? https : http;
  return new Promise((resolve, reject) => {
    const req = mod.get(url, { headers: { 'user-agent': 'reeflog-client' } }, (res) => {
      if (res.statusCode >= 300 && res.statusCode < 400 && res.headers.location) {
        res.resume();
        httpGet(res.headers.location).then(resolve, reject);
        return;
      }
      if (res.statusCode !== 200) {
        res.resume();
        reject(new ReeflogError(`download of ${url} failed: HTTP ${res.statusCode}`));
        return;
      }
      const chunks = [];
      res.on('data', (d) => chunks.push(d));
      res.on('end', () => resolve(Buffer.concat(chunks)));
    });
    req.on('error', (e) => reject(new ReeflogError(`download of ${url} failed: ${e.message}`)));
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
 * before the digest matches.
 *
 * @returns {Promise<{bin: string, asset: string, digest: string, cached: boolean}>}
 */
export async function bootstrap({
  version = packageVersion(),
  platform = process.platform,
  arch = process.arch,
  dest = path.join(os.homedir(), '.reeflog'),
  apiBase = 'https://api.github.com',
  downloadBase = null,
} = {}) {
  const asset = RELEASE_ASSETS[`${platform}-${arch}`];
  if (!asset) {
    throw new ReeflogError(
      `no prebuilt reeflog binary for ${platform}-${arch} (published: ${Object.keys(RELEASE_ASSETS).join(', ')}) — install from source: \`cargo install --git https://github.com/auroraxo/reeflog --tag ${version}\``,
    );
  }
  if (!/^v\d+\.\d+\.\d+$/.test(String(version))) {
    throw new ReeflogError(`bootstrap needs an explicit release tag like v0.7.0, got ${JSON.stringify(version)}`);
  }

  const bin = path.join(dest, 'reeflog');
  const metaFile = path.join(dest, 'install.json');
  if (fs.existsSync(bin) && fs.existsSync(metaFile)) {
    try {
      const meta = JSON.parse(fs.readFileSync(metaFile, 'utf8'));
      if (meta.version === version && meta.asset === asset) {
        return { bin, asset, digest: meta.sha256, cached: true };
      }
    } catch { /* fall through to a fresh install */ }
  }

  const releaseUrl = `${apiBase}/repos/auroraxo/reeflog/releases/tags/${version}`;
  const release = JSON.parse((await httpGet(releaseUrl)).toString('utf8'));
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
  const gz = await httpGet(url);
  const actual = createHash('sha256').update(gz).digest('hex');
  if (actual !== expected) {
    throw new ReeflogError(
      `sha256 mismatch for ${asset}: release says ${expected}, download is ${actual} — refusing to install`,
    );
  }

  const executable = extractReeflogFromTarGz(gz);
  fs.mkdirSync(dest, { recursive: true });
  const tmp = path.join(dest, `.reeflog-${process.pid}.tmp`);
  fs.writeFileSync(tmp, executable, { mode: 0o755 });
  fs.renameSync(tmp, bin); // atomic: a crash leaves the old binary or none
  fs.writeFileSync(metaFile, `${JSON.stringify({ version, asset, sha256: actual, installedAt: new Date().toISOString() }, null, 2)}\n`);

  return { bin, asset, digest: actual, cached: false };
}

/** Convenience constructor: `const box = reeflog({ db: 'reeflog.db' })`. */
export function reeflog(opts) {
  return new Reeflog(opts);
}

export default reeflog;
