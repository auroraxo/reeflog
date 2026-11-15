#!/usr/bin/env node
// MEASUREMENT: `webhook status` must hold the EXIT CODE for the debts its own
// report already prints — the same journal facts `webhook push` turns into
// exit 3.
//
// `webhook status` is the report a monitoring caller polls. v0.7.29 taught it
// to exit 3 for an ORPHANED dead letter, on the stated ground that this is
// "the one debt that never drains by itself". Two debts the same report
// already prints are still read as health:
//
//   * a CONFIGURED target's pending dead letters — the report prints
//     `dead_letters: 1` and exits 0, while `webhook push` on the same
//     journal prints `dead_letters_pending: 1` and exits 3. The premise that
//     separates the two does not hold: `webhook_dead_letter` moves the
//     target's cursor PAST the quarantined row, and `deliver_events` reads
//     `events_after(next_seq)` — so `push` never re-attempts a quarantined
//     row. It does not drain by itself either; only `dlq replay` hands it
//     over, and that is a human action.
//   * a configured target whose standing verdict is a FAILURE — the report
//     prints `last_error` and exits 0, while `push` exits 3 for the block
//     that wrote it.
//
// Faces (each a predicate run against ONE tree):
//   D1 status exits 3 while a configured target holds pending dead letters.
//   D2 status exits 3 while a configured target carries a standing failure.
//   D3 status and push agree on the exit code for the same journal state —
//      the "one fact, one verdict per surface" rule the repo already holds
//      for the orphan count itself.
//
// Guards (must be green on BOTH trees):
//   G1 a journal with no debt reports 0 letters and exits 0 (a fix that just
//      makes status exit 3 always is not a fix).
//   G2 an ORPHANED dead letter still exits 3 (the v0.7.29 rule must not
//      regress while the line is redrawn).
//
//   node tools/measure-status-debt-exit.mjs
//   BIN=/path/to/reeflog OUT=/tmp/x.json LABEL=published-v0.7.29 node tools/measure-status-debt-exit.mjs
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { execFileSync, spawnSync } from 'node:child_process';
import { createHash } from 'node:crypto';
import { DatabaseSync } from 'node:sqlite';

const BIN = process.env.BIN || path.resolve(import.meta.dirname, '..', 'target/release/reeflog');
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
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), `reeflog-status-debt-${tag}-`));
  const db = path.join(dir, 'reeflog.db');
  execFileSync(BIN, ['init', '--db', db, '--program', PROGRAM], { cwd: dir, stdio: 'ignore' });
  return { dir, db };
}

function writeConfig(dir, targets) {
  const cfg = path.join(dir, 'wh.json');
  fs.writeFileSync(cfg, JSON.stringify({ targets }));
  return cfg;
}

const LIVE = { id: 't1', url: 'http://127.0.0.1:9971/hook', secret: 's3cret', timeout_ms: 2000, dead_letter_after: 5 };
const DOWN = { id: 't1', url: 'http://127.0.0.1:9/dead', secret: 's3cret', timeout_ms: 2000, dead_letter_after: 5 };

function plantEvents(db, n) {
  const c = new DatabaseSync(db);
  for (let i = 1; i <= n; i++) {
    c.prepare(
      `INSERT INTO events(seq,signature,slot,logs,source,first_seen,seal_status,payload_state)
       VALUES(?,?,?,?,'ws',1700000000000,'finalized','held')`,
    ).run(i, `SIG${'0'.repeat(40)}${i}`, 440000000 + i, JSON.stringify(['log']));
  }
  c.close();
}

/** One pending dead letter for a CONFIGURED target, cursor already past it —
 *  the state `webhook push` leaves behind after a refusal that quarantined. */
function plantConfiguredLetter(db, target) {
  const c = new DatabaseSync(db);
  c.prepare(
    `INSERT INTO webhook_dead_letters(target,kind,seq,body,event_id,reason,status,attempts,
                                      first_failed_at,last_failed_at,state)
     VALUES(?1,'event',1,'{}','EVC','http 403',403,3,1700000000000,1700000000000,'pending')`,
  ).run(target);
  c.prepare(
    `INSERT INTO webhook_state(target,endpoint,next_seq,next_reorg_id,last_error,updated_at)
     VALUES(?1,'http://127.0.0.1:9971/hook',1,0,NULL,1700000000000)`,
  ).run(target);
  c.close();
}

/** A pending dead letter for a target that LEFT the config (the v0.7.29 rule). */
function plantOrphanLetter(db) {
  const c = new DatabaseSync(db);
  c.prepare(
    `INSERT INTO webhook_dead_letters(target,kind,seq,body,event_id,reason,status,attempts,
                                      first_failed_at,last_failed_at,state)
     VALUES('gone','event',1,'{}','EVO','http 500',500,3,1700000000000,1700000000000,'pending')`,
  ).run();
  c.close();
}

// --- D1: a configured target's pending dead letters -------------------------
{
  var d1 = (() => {
    const { dir, db } = mkJournal('configured');
    const cfg = writeConfig(dir, [LIVE]);
    plantConfiguredLetter(db, 't1');
    const status = run(BIN, ['webhook', 'status', '--db', db, '--config', cfg], dir);
    const push = run(BIN, ['webhook', 'push', '--db', db, '--config', cfg], dir);
    // Does the debt drain by itself? Two more passes, same journal, live target.
    const push2 = run(BIN, ['webhook', 'push', '--db', db, '--config', cfg], dir);
    const push3 = run(BIN, ['webhook', 'push', '--db', db, '--config', cfg], dir);
    const statusAfter = run(BIN, ['webhook', 'status', '--db', db, '--config', cfg], dir);
    const printed = status.json?.targets?.[0]?.dead_letters ?? null;
    const pushPending = push.json?.dead_letters_pending ?? null;
    // The quarantine moved the cursor past the row: push re-reads
    // events_after(next_seq) and never touches it again. Only `dlq replay`
    // hands it over, and that is a human action — the same "never drains by
    // itself" property v0.7.29 used to justify the orphan exit code.
    const cursorAfter = statusAfter.json?.targets?.[0]?.next_seq ?? null;
    const stillPendingAfter3Pushes = statusAfter.json?.targets?.[0]?.dead_letters ?? null;
    return { status, push, printed, pushPending, push2, push3, cursorAfter, stillPendingAfter3Pushes, dir };
  })();
}

// --- D2: a configured target whose standing verdict is a failure ------------
{
  var d2 = (() => {
    const { dir, db } = mkJournal('blocked');
    const cfg = writeConfig(dir, [DOWN]);
    plantEvents(db, 1);
    // A REAL blocked pass writes the standing verdict — no hand-planted state.
    const push = run(BIN, ['webhook', 'push', '--db', db, '--config', cfg], dir);
    const status = run(BIN, ['webhook', 'status', '--db', db, '--config', cfg], dir);
    const lastError = status.json?.targets?.[0]?.last_error ?? null;
    return { status, push, lastError, dir };
  })();
}

// --- G1: a clean journal must stay exit 0 ----------------------------------
{
  var g1 = (() => {
    const { dir, db } = mkJournal('clean');
    const cfg = writeConfig(dir, [LIVE]);
    const status = run(BIN, ['webhook', 'status', '--db', db, '--config', cfg], dir);
    return {
      status,
      letters: status.json?.targets?.[0]?.dead_letters ?? null,
      orphans: status.json?.orphaned_dead_letters ?? null,
      dir,
    };
  })();
}

// --- G2: an orphaned letter must still exit 3 (v0.7.29) --------------------
{
  var g2 = (() => {
    const { dir, db } = mkJournal('orphan');
    const cfg = writeConfig(dir, [LIVE]);
    plantOrphanLetter(db);
    const status = run(BIN, ['webhook', 'status', '--db', db, '--config', cfg], dir);
    return { status, orphans: status.json?.orphaned_dead_letters ?? null, dir };
  })();
}

const faces = [
  {
    face: 'D1',
    claim: '`webhook status` exits 3 while a configured target holds pending dead letters (it prints the count and calls it health)',
    expected: { exitCode: 3, printed: 1 },
    observed: { exitCode: d1.status.exitCode, printed: d1.printed },
    ok: d1.status.exitCode === 3 && d1.printed === 1,
    note: `push on the same journal: exit ${d1.push.exitCode}, dead_letters_pending ${d1.pushPending}; after 3 push passes the letter is still pending (exit ${d1.push3.exitCode}) with cursor at ${d1.cursorAfter} — it does not drain by itself, exactly like the orphan`,
  },
  {
    face: 'D2',
    claim: '`webhook status` exits 3 while a configured target carries a standing failure verdict',
    expected: { exitCode: 3, hasLastError: true },
    observed: { exitCode: d2.status.exitCode, hasLastError: typeof d2.lastError === 'string' && d2.lastError.length > 0 },
    ok: d2.status.exitCode === 3 && typeof d2.lastError === 'string' && d2.lastError.length > 0,
    note: `push on the same journal: exit ${d2.push.exitCode}`,
  },
  {
    face: 'D3',
    claim: 'status and push return the SAME exit code for a journal holding pending dead letters',
    expected: 'equal',
    observed: { status: d1.status.exitCode, push: d1.push.exitCode },
    ok: d1.status.exitCode === d1.push.exitCode,
  },
  {
    face: 'G1',
    claim: 'a journal with no debt still reports 0 letters and exits 0',
    expected: { exitCode: 0, letters: 0, orphans: 0 },
    observed: { exitCode: g1.status.exitCode, letters: g1.letters, orphans: g1.orphans },
    ok: g1.status.exitCode === 0 && g1.letters === 0 && g1.orphans === 0,
  },
  {
    face: 'G2',
    claim: 'an ORPHANED dead letter still exits 3 (the v0.7.29 rule holds)',
    expected: { exitCode: 3, orphans: 1 },
    observed: { exitCode: g2.status.exitCode, orphans: g2.orphans },
    ok: g2.status.exitCode === 3 && g2.orphans === 1,
  },
];

const defectFaces = faces.filter((f) => !f.face.startsWith('G'));
const guardFaces = faces.filter((f) => f.face.startsWith('G'));
const verdict = defectFaces.every((f) => f.ok) && guardFaces.every((f) => f.ok)
  ? 'status-debt-exit-held'
  : 'status-debt-exit-blind';

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
    'a pending dead letter does not drain by itself: the quarantine moves the target cursor past the row, and deliver_events reads events_after(next_seq)',
    'the standing verdict last_error is written by the blocked pass that push already turns into exit 3',
    'status is the surface a monitoring caller polls; a debt it prints and then calls health is the same lie as a debt it never printed',
  ],
  faces,
  verdict,
};

if (OUT) {
  fs.writeFileSync(OUT, JSON.stringify(artifact, null, 2) + '\n');
  console.error(`wrote ${OUT}`);
}

// No host paths in the artifact.
const dirs = [d1, d2, g1, g2].map((s) => s.dir);
const serialized = JSON.stringify(artifact);
if (serialized.includes(BIN) || dirs.some((d) => serialized.includes(d)) || /"\/(tmp|home)\//.test(serialized)) {
  console.error('ARTIFACT LEAK: a host path is in the artifact');
  process.exit(1);
}

console.log(JSON.stringify({
  verdict,
  faces: faces.map((f) => ({ face: f.face, ok: f.ok, observed: f.observed })),
}, null, 2));
process.exit(verdict === 'status-debt-exit-held' ? 0 : 1);
