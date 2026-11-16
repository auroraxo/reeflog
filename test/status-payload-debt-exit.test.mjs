// The client contract for the `status` payload-debt exit (PULS AM, v0.7.31).
//
// Since v0.7.31 the binary holds the exit code for the payload debt its own
// report prints: a journal that still owes a row's content (`payload_pending
// > 0`) makes `status` exit 3 — the same code the default payload-bearing
// `tail` holds at, the same code `webhook push` holds its cursor at, and the
// same code `hydrate` exits on while rows are pending.
//
// So `status()` must THROW on that verdict, like `verify`/`repair`/
// `hydrate` do, and the thrown error must carry the PARSED REPORT: a caller
// that catches it has to be able to see WHAT is owed, not only that
// something is. A health report that resolves as "fine" over an owed
// journal is the lie the CLI's exit code exists to prevent.
//
// What is pinned here:
//   F1 a journal that owes a payload makes `status()` reject with exit 3
//      and the report attached;
//   F2 a journal without the debt still resolves, and the report is the
//      same shape the CLI printed;
//   F3 the recorded outcomes are not a debt — they resolve;
//   F4 the opt-in envelope queues are not a debt — they resolve;
//   F5 `raw()` is still the unclassified hatch: the report and the code come
//      back under a debt, without an exception.
import test from 'node:test';
import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { DatabaseSync } from 'node:sqlite';
import { reeflog, ReeflogError } from '../index.js';

const BIN = process.env.REEFLOG_BIN || path.resolve('target/release/reeflog');
const PROGRAM = 'whirLbMiicVdio4qvUfM5KAg6Ct8VwpYzGff3uctyCc';

const binaryMissing = !fs.existsSync(BIN);

function tmpJournal(name) {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), `reeflog-status-debt-${name}-`));
  const db = path.join(dir, 'reeflog.db');
  const out = spawnSync(BIN, ['init', '--db', db, '--program', PROGRAM], { encoding: 'utf8' });
  assert.equal(out.status, 0, `init failed: ${out.stderr}`);
  return { dir, db };
}

/** One row per entry: logs / payload_state / accounts / account_state. */
function plant(db, rows) {
  const conn = new DatabaseSync(db);
  for (const r of rows) {
    conn.prepare(
      `INSERT INTO events(signature, slot, err, logs, source, first_seen, seal_status,
                          payload_state, accounts, account_state)
       VALUES(?, ?, NULL, ?, 'ws', 1700000000000, 'finalized', ?, ?, ?)`,
    ).run(r.sig, r.slot, r.logs ?? null, r.payloadState ?? null, r.accounts ?? null, r.accountState ?? null);
  }
  conn.close();
}

test('F1: an owed payload makes status() REJECT with exit 3 and the report attached', { skip: binaryMissing }, async () => {
  const { dir, db } = tmpJournal('owed');
  plant(db, [{ sig: `sig${'0'.repeat(40)}1`, slot: 440000001 }]);

  const err = await reeflog({ db, bin: BIN }).status().then(
    () => { throw new Error('F1: status RESOLVED over an owed payload'); },
    (e) => e,
  );

  assert.ok(err instanceof ReeflogError, `F1: raw ${err.constructor.name} escaped status()`);
  assert.equal(err.exitCode, 3, 'the verdict is the binary\'s own code');
  assert.match(err.message, /hydrate/, 'the message must name the command that pays the debt');
  // The facts are not lost with the verdict: the report is attached.
  assert.ok(err.report, 'the error must carry the parsed report');
  assert.equal(err.report.payload_pending, 1, 'the report names the debt');
  assert.equal(err.report.events, 1);

  fs.rmSync(dir, { recursive: true, force: true });
});

test('F2: a journal without the debt still RESOLVES with the CLI\'s report', { skip: binaryMissing }, async () => {
  const { dir, db } = tmpJournal('clean');
  const s = await reeflog({ db, bin: BIN }).status();
  assert.equal(s.payload_pending, 0);
  assert.equal(s.events, 0);
  assert.equal(s.program, PROGRAM);

  // The client\'s report is the CLI\'s report, byte for byte.
  const out = spawnSync(BIN, ['status', '--db', db], { encoding: 'utf8' });
  assert.equal(out.status, 0, 'a clean journal exits 0');
  assert.deepEqual(s, JSON.parse(out.stdout));

  fs.rmSync(dir, { recursive: true, force: true });
});

test('F3: a recorded outcome is NOT a debt — status() resolves', { skip: binaryMissing }, async () => {
  const { dir, db } = tmpJournal('recorded');
  plant(db, [
    { sig: `sig${'0'.repeat(40)}2`, slot: 440000002, payloadState: 'unavailable' },
    { sig: `sig${'0'.repeat(40)}3`, slot: 440000003, payloadState: 'invalid' },
  ]);

  const s = await reeflog({ db, bin: BIN }).status();
  assert.equal(s.payload_pending, 0);
  assert.equal(s.payload_unavailable, 1);
  assert.equal(s.payload_invalid, 1);

  fs.rmSync(dir, { recursive: true, force: true });
});

test('F4: the opt-in envelope queues are NOT a debt — status() resolves', { skip: binaryMissing }, async () => {
  const { dir, db } = tmpJournal('optin');
  plant(db, [1, 2, 3].map((i) => ({
    sig: `sig${'0'.repeat(40)}${i + 10}`,
    slot: 440000010 + i,
    logs: '["log"]',
    payloadState: 'held',
  })));

  const s = await reeflog({ db, bin: BIN }).status();
  assert.equal(s.payload_pending, 0);
  assert.equal(s.accounts_missing, 3, 'the opt-in queue is still counted');

  fs.rmSync(dir, { recursive: true, force: true });
});

test('F5: raw() is still the unclassified hatch under a debt', { skip: binaryMissing }, async () => {
  const { dir, db } = tmpJournal('raw');
  plant(db, [{ sig: `sig${'0'.repeat(40)}4`, slot: 440000004 }]);

  const r = await reeflog({ db, bin: BIN }).raw(['status', '--db', db]);
  assert.equal(r.exitCode, 3, 'raw hands the code back without throwing');
  const report = JSON.parse(r.stdout);
  assert.equal(report.payload_pending, 1, 'and the report comes back with it');

  fs.rmSync(dir, { recursive: true, force: true });
});
