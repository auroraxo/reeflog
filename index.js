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
          `reeflog binary not found at "${bin}" — install it (\`cargo install --git https://github.com/auroraxo/reeflog --tag v0.6.0\`) or pass { bin: "/path/to/reeflog" }`,
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

/** Convenience constructor: `const box = reeflog({ db: 'reeflog.db' })`. */
export function reeflog(opts) {
  return new Reeflog(opts);
}

export default reeflog;
