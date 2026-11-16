#!/usr/bin/env node
// MEASUREMENT: `status` must hold the EXIT CODE for the payload debt its own
// report already prints — the same journal fact `tail`, `hydrate`, the
// daemon's shutdown and `webhook push` all turn into exit 3.
//
// `status` is the report a monitoring caller polls: it asks "is this journal
// whole?" and reads the exit code. The report prints `payload_pending` — the
// count of rows the journal holds a SIGNATURE for but not what happened
// (`logs IS NULL AND payload_state IS NULL`). The default payload-bearing
// `tail` HOLDS at exactly that predicate (cursor stays, exit 3, `hydrate`
// named); `webhook push` holds the target's cursor at it since v0.7.23;
// `hydrate` exits 3 while its own `pending_left` is non-zero; the daemon's
// shutdown exits 3 while the payload hydrate is incomplete. Only `status`
// reads the same number as health and exits 0.
//
// Faces (each a predicate run against ONE tree):
//   D1 status exits 3 while a row still owes its payload.
//   D2 status and the default payload-bearing `tail` agree on the exit code
//      for the SAME journal — the "one fact, one verdict per surface" rule
//      the repo already holds for the webhook report.
//   D3 the verdict TRACKS the debt: paying the row flips status to exit 0
//      (an exit code that never comes back down is an alarm, not a verdict).
//
// Guards (must be green on BOTH trees):
//   G1 a journal with no debt exits 0 and reports `payload_pending: 0`.
//   G2 rows in a RECORDED outcome (`payload_state='unavailable'`) are not a
//      debt — the chain forgot them, which `hydrate` counts as a completed
//      pass. A status that alarmed on them would alarm forever.
//   G3 the OPT-IN queues (`accounts_missing`, `owners_missing`,
//      `account_data_missing`) do not set the exit code. They are filled only
//      by `hydrate --accounts|--owners|--account-data`, and `tail` only holds
//      on them behind the matching flag — for a default consumer they never
//      drain, so folding them into the verdict would be a permanent false
//      alarm (the failure class v0.7.27 removed for `gaps_open`).
//
//   node tools/measure-status-payload-debt-exit.mjs
//   BIN=/path/to/reeflog OUT=/tmp/x.json LABEL=published-v0.7.30 node tools/measure-status-payload-debt-exit.mjs
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { execFileSync, spawnSync } from 'node:child_process';
import { createHash } from 'node:crypto';
import { DatabaseSync } from 'node:sqlite';

const BIN = path.resolve(process.env.BIN || path.resolve(import.meta.dirname, '..', 'target/release/reeflog'));
const OUT = process.env.OUT || null;
const LABEL = process.env.LABEL || 'this-tree';
const PROGRAM = 'whirLbMiicVdio4qvUfM5KAg6Ct8VwpYzGff3uctyCc';

if (!fs.existsSync(BIN)) {
  console.error(`no reeflog binary at ${BIN} — refusing to measure without one`);
  process.exit(2);
}

function run(bin, args, cwd) {
  const r = spawnSync(bin, args, { cwd, encoding: 'utf8' });
  let json = null;
  try { json = JSON.parse((r.stdout || '').trim()); } catch { json = null; }
  return { exitCode: r.status, stdout: r.stdout || '', stderr: r.stderr || '', json };
}

/** A fresh journal in a fresh dir. */
function mkJournal(tag) {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), `reeflog-status-payload-${tag}-`));
  const db = path.join(dir, 'reeflog.db');
  execFileSync(BIN, ['init', '--db', db, '--program', PROGRAM], { cwd: dir, stdio: 'ignore' });
  return { dir, db };
}

/** One row per entry: logs/payload_state/accounts/account_state as given. */
function plant(db, rows) {
  const c = new DatabaseSync(db);
  for (const r of rows) {
    c.prepare(
      `INSERT INTO events(signature, slot, err, logs, source, first_seen, seal_status,
                          payload_state, accounts, account_state)
       VALUES(?, ?, NULL, ?, 'ws', 1700000000000, 'finalized', ?, ?, ?)`,
    ).run(
      r.sig,
      r.slot,
      r.logs ?? null,
      r.payloadState ?? null,
      r.accounts ?? null,
      r.accountState ?? null,
    );
  }
  c.close();
}

const OWED = [{ sig: `SIG${'0'.repeat(40)}1`, slot: 440000001 }];

/** Pay an owed row the way `hydrate` does: content arrives, state becomes
 *  `held`. An UPDATE, because it is the SAME row — paying a debt never
 *  mints a second event. */
function pay(db, sig) {
  const c = new DatabaseSync(db);
  c.prepare(
    `UPDATE events SET logs = '["log"]', payload_state = 'held'
     WHERE signature = ?1 AND logs IS NULL AND payload_state IS NULL`,
  ).run(sig);
  c.close();
}

// --- D1: status exits 3 while a row still owes its payload ------------------
{
  var d1 = (() => {
    const { dir, db } = mkJournal('owed');
    plant(db, OWED);
    const status = run(BIN, ['status', '--db', db], dir);
    return {
      status,
      pending: status.json?.payload_pending ?? null,
      dir,
    };
  })();
}

// --- D2: status and the default payload-bearing tail agree -----------------
{
  var d2 = (() => {
    const { dir, db } = mkJournal('parity');
    plant(db, OWED);
    const cursor = path.join(dir, 'c.txt');
    const status = run(BIN, ['status', '--db', db], dir);
    const tail = run(BIN, ['tail', '--db', db, '--cursor', cursor], dir);
    return { status, tail, dir };
  })();
}

// --- D3: paying the debt flips the verdict back to 0 -----------------------
{
  var d3 = (() => {
    const { dir, db } = mkJournal('paid');
    plant(db, OWED);
    const before = run(BIN, ['status', '--db', db], dir);
    pay(db, OWED[0].sig);
    const after = run(BIN, ['status', '--db', db], dir);
    return {
      before: { exitCode: before.exitCode, pending: before.json?.payload_pending ?? null },
      after: { exitCode: after.exitCode, pending: after.json?.payload_pending ?? null },
      dir,
    };
  })();
}

// --- G1: a clean journal stays exit 0 --------------------------------------
{
  var g1 = (() => {
    const { dir, db } = mkJournal('clean');
    const status = run(BIN, ['status', '--db', db], dir);
    return { status, pending: status.json?.payload_pending ?? null, dir };
  })();
}

// --- G2: a RECORDED outcome is not a debt ----------------------------------
{
  var g2 = (() => {
    const { dir, db } = mkJournal('unavailable');
    plant(db, [{ sig: `SIG${'0'.repeat(40)}2`, slot: 440000002, payloadState: 'unavailable' }]);
    plant(db, [{ sig: `SIG${'0'.repeat(40)}3`, slot: 440000003, payloadState: 'invalid' }]);
    const status = run(BIN, ['status', '--db', db], dir);
    return {
      status,
      unavailable: status.json?.payload_unavailable ?? null,
      invalid: status.json?.payload_invalid ?? null,
      pending: status.json?.payload_pending ?? null,
      dir,
    };
  })();
}

// --- G3: the opt-in queues do not set the exit code ------------------------
{
  var g3 = (() => {
    const { dir, db } = mkJournal('optin');
    plant(db, [1, 2, 3, 4, 5].map((i) => ({
      sig: `SIG${'0'.repeat(40)}${i + 10}`,
      slot: 440000010 + i,
      logs: `["log ${i}"]`,
      payloadState: 'held',
    })));
    const status = run(BIN, ['status', '--db', db], dir);
    return {
      status,
      accountsMissing: status.json?.accounts_missing ?? null,
      ownersMissing: status.json?.owners_missing ?? null,
      accountDataMissing: status.json?.account_data_missing ?? null,
      pending: status.json?.payload_pending ?? null,
      dir,
    };
  })();
}

const faces = [
  {
    face: 'D1',
    claim: '`status` exits 3 while a row still owes its payload (it prints the count and calls it health)',
    expected: { exitCode: 3, printed: 1 },
    observed: { exitCode: d1.status.exitCode, printed: d1.pending },
    ok: d1.status.exitCode === 3 && d1.pending === 1,
    note: d1.status.stderr.trim().split('\n').filter(Boolean).slice(-1)[0] || '(no stderr line)',
  },
  {
    face: 'D2',
    claim: 'status and the default payload-bearing tail return the SAME exit code for the same journal',
    expected: 'equal',
    observed: { status: d2.status.exitCode, tail: d2.tail.exitCode },
    ok: d2.status.exitCode === d2.tail.exitCode && d2.tail.exitCode === 3,
    note: `tail: ${d2.tail.stderr.trim().split('\n').filter(Boolean).slice(-1)[0] || '(no stderr line)'}`,
  },
  {
    face: 'D3',
    claim: 'paying the payload debt flips status back to exit 0 — the verdict tracks the debt',
    expected: { before: 3, after: 0 },
    observed: { before: d3.before.exitCode, after: d3.after.exitCode },
    ok: d3.before.exitCode === 3 && d3.after.exitCode === 0,
    note: `pending ${d3.before.pending} -> ${d3.after.pending}`,
  },
  {
    face: 'G1',
    claim: 'a journal with no debt reports 0 pending and exits 0',
    expected: { exitCode: 0, pending: 0 },
    observed: { exitCode: g1.status.exitCode, pending: g1.pending },
    ok: g1.status.exitCode === 0 && g1.pending === 0,
    note: 'an exit code that never comes back down is an alarm, not a verdict',
  },
  {
    face: 'G2',
    claim: 'rows in a recorded outcome (unavailable/invalid) are NOT a debt: status exits 0',
    expected: { exitCode: 0, pending: 0, unavailable: 1, invalid: 1 },
    observed: {
      exitCode: g2.status.exitCode,
      pending: g2.pending,
      unavailable: g2.unavailable,
      invalid: g2.invalid,
    },
    ok: g2.status.exitCode === 0 && g2.pending === 0 && g2.unavailable === 1 && g2.invalid === 1,
    note: 'the chain forgot these — hydrate counts them as a completed pass, not a pending row',
  },
  {
    face: 'G3',
    claim: 'the opt-in queues (accounts/owners/account-data missing) do NOT set the exit code',
    expected: { exitCode: 0, accountsMissing: 5 },
    observed: {
      exitCode: g3.status.exitCode,
      accountsMissing: g3.accountsMissing,
      ownersMissing: g3.ownersMissing,
      accountDataMissing: g3.accountDataMissing,
    },
    ok: g3.status.exitCode === 0 && g3.accountsMissing === 5,
    note: 'filled only by `hydrate --accounts` and held only behind `tail --with-accounts` — they never drain for a default consumer',
  },
];

const defectFaces = faces.filter((f) => !f.face.startsWith('G'));
const guardFaces = faces.filter((f) => f.face.startsWith('G'));
const verdict = defectFaces.every((f) => f.ok) && guardFaces.every((f) => f.ok)
  ? 'status-payload-debt-exit-held'
  : 'status-payload-debt-exit-blind';

const artifact = {
  measuredAt: new Date().toISOString(),
  label: LABEL,
  // The binary is NAMED BY ITS BYTES, never by its host path.
  binary: {
    sha256: createHash('sha256').update(fs.readFileSync(BIN)).digest('hex'),
    version: spawnSync(BIN, ['--version'], { encoding: 'utf8' }).stdout?.trim() ?? null,
  },
  program: PROGRAM,
  rule: [
    'payload_pending counts rows the journal holds a signature for but not what happened (logs IS NULL AND payload_state IS NULL)',
    'the default payload-bearing tail HOLDS at that predicate (cursor stays, exit 3, hydrate named) — since v0.7.23 webhook push holds its cursor at it too, hydrate exits 3 while pending_left is non-zero, and the daemon exits 3 at shutdown while the payload hydrate is incomplete',
    'status is the surface a monitoring caller polls; a debt it prints and then calls health is the same lie as a debt it never printed',
    'recorded outcomes (unavailable/invalid) are NOT a debt — hydrate counts them as a completed pass',
    'the opt-in account/owner/account-data queues are not part of the verdict: they drain only behind an opt-in flag and would be a permanent alarm for a default consumer',
  ],
  faces,
  verdict,
};

if (OUT) {
  fs.writeFileSync(OUT, JSON.stringify(artifact, null, 2) + '\n');
  console.error(`wrote ${OUT}`);
}

// No host paths in the artifact.
const dirs = [d1, d2, d3, g1, g2, g3].map((s) => s.dir);
const serialized = JSON.stringify(artifact);
if (serialized.includes(BIN) || dirs.some((d) => serialized.includes(d)) || /"\/(tmp|home)\//.test(serialized)) {
  console.error('ARTIFACT LEAK: a host path is in the artifact');
  process.exit(1);
}

console.log(JSON.stringify({
  verdict,
  faces: faces.map((f) => ({ face: f.face, ok: f.ok, observed: f.observed })),
}, null, 2));
process.exit(verdict === 'status-payload-debt-exit-held' ? 0 : 1);
