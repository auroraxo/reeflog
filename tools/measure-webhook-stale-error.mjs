#!/usr/bin/env node
// The WEBHOOK-STALE-ERROR measurement (PULS AG, 2026-11-13) — MEASURED with
// the REAL binary against a REAL local receiver, not inferred from source.
//
// The defect (v0.7.23, this tree before the fix): `webhook_state.last_error`
// is written by every blocked pass and read by `webhook status` — and
// nothing ever clears it. So a target that RECOVERED kept reporting the
// failure it recovered from:
//
//   pass 1  the endpoint refuses (500) -> exit 3, last_error = "http 500"
//   pass 2  the endpoint is healthy   -> exit 0, both rows delivered
//   status  last_error = "http 500"   <-- the failure ended, the report
//                                           does not
//
// The faces (each one a predicate for BOTH trees):
//   S1 a pass that delivers everything leaves no standing error behind;
//   S2 the same after a payload-hold was paid (the hold reason must not
//      stand either);
//   G1 a standing failure IS reported (guard, both trees);
//   G2 a target that never failed reports no error (guard, both trees).
//
//   node tools/measure-webhook-stale-error.mjs
//   BIN=/path/to/reeflog OUT=/tmp/x.json LABEL=v0.7.23 node tools/measure-webhook-stale-error.mjs
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import http from 'node:http';
import { execFileSync, spawn } from 'node:child_process';
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

const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'reeflog-staleerr-'));
const db = path.join(dir, 'j.db');
const cfgPath = path.join(dir, 'wh.json');

let failNext = 0;
const received = [];
const srv = http.createServer((req, res) => {
  const chunks = [];
  req.on('data', (c) => chunks.push(c));
  req.on('end', () => {
    received.push(Buffer.concat(chunks).toString('utf8'));
    if (failNext > 0) {
      failNext -= 1;
      res.writeHead(500).end('{"e":1}');
      return;
    }
    res.writeHead(200).end('{}');
  });
});
await new Promise((res) => srv.listen(0, '127.0.0.1', res));
const url = `http://127.0.0.1:${srv.address().port}/hook`;
fs.writeFileSync(
  cfgPath,
  JSON.stringify({ targets: [{ id: 't1', url, secret: 's3cret', timeout_ms: 5000, dead_letter_after: 0 }] })
);

function init() {
  for (const f of [db, `${db}-wal`, `${db}-shm`]) fs.rmSync(f, { force: true });
  execFileSync(BIN, ['init', '--db', db, '--program', PROGRAM], { cwd: dir, stdio: 'ignore' });
}
function plant(rows) {
  const conn = new DatabaseSync(db);
  for (const r of rows) {
    conn
      .prepare(
        `INSERT INTO events(signature, slot, err, logs, source, first_seen, payload_state)
         VALUES (?, ?, NULL, ?, 'ws', ?, ?)`
      )
      .run(r.sig, r.slot, r.logs, Date.now(), r.state);
  }
  conn.close();
}
function pay(sig, logs) {
  const conn = new DatabaseSync(db);
  conn.prepare("UPDATE events SET logs = ?, payload_state = 'held' WHERE signature = ?").run(logs, sig);
  conn.close();
}
async function push() {
  const before = received.length;
  const { code, stderr, stdout } = await new Promise((res) => {
    const c = spawn(BIN, ['webhook', 'push', '--db', db, '--config', cfgPath], { cwd: dir });
    let err = '', out = '';
    c.stderr.on('data', (d) => (err += d));
    c.stdout.on('data', (d) => (out += d));
    c.on('exit', (code) => res({ code, stderr: err, stdout: out }));
  });
  await new Promise((r) => setTimeout(r, 150));
  let rep = null;
  try {
    rep = JSON.parse(stdout);
  } catch {
    rep = null;
  }
  return { code, stderr, report: rep, bodies: received.slice(before).map((b) => tryJson(b)) };
}
function tryJson(b) {
  try {
    return JSON.parse(b);
  } catch {
    return null;
  }
}
function statusOf() {
  const out = execFileSync(BIN, ['webhook', 'status', '--db', db, '--config', cfgPath], { cwd: dir }).toString();
  return JSON.parse(out).targets[0];
}
function cursorOf() {
  const conn = new DatabaseSync(db);
  const s = conn.prepare('SELECT next_seq FROM webhook_state WHERE target = ?').get('t1');
  conn.close();
  return s ? s.next_seq : 0;
}

const SIG_A = 'SigStale1111111111111111111111111111111111111';
const SIG_B = 'SigStale2222222222222222222222222222222222222';

const faces = [];
const face = (id, kind, detail) => faces.push({ id, kind, ...detail });

// ------------------------------------------------------------ S1 / G1
// The endpoint refuses once; the target recovers; the report must follow.
init();
plant([
  { sig: SIG_A, slot: 44000001, logs: '["log one"]', state: 'held' },
  { sig: SIG_B, slot: 44000002, logs: '["log two"]', state: 'held' },
]);
failNext = 1;
const p1 = await push();
const st1 = statusOf();
face('G1', 'a standing failure IS reported (guard)', {
  exitCode: p1.code,
  lastError: st1.last_error,
  reported: typeof st1.last_error === 'string' && st1.last_error.length > 0,
});

const p2 = await push();
const st2 = statusOf();
face('S1', 'a pass that delivers everything leaves no standing error behind', {
  exitCode: p2.code,
  delivered: p2.report ? p2.report.targets[0].delivered : null,
  lastError: st2.last_error,
  cursor: cursorOf(),
  standingError: typeof st2.last_error === 'string' && st2.last_error.length > 0,
});

// ------------------------------------------------------------ S2
// The payload-hold channel: the hold reason must not stand after the debt
// is paid.
init();
plant([{ sig: SIG_A, slot: 44000001, logs: null, state: null }]);
const h1 = await push();
const stHold = statusOf();
const holdReason = stHold.last_error;
pay(SIG_A, '["log paid"]');
const h2 = await push();
const stPaid = statusOf();
face('S2', 'the paid hold does not stand as a failure any more', {
  holdExitCode: h1.code,
  holdReason,
  paidExitCode: h2.code,
  delivered: h2.report ? h2.report.targets[0].delivered : null,
  lastError: stPaid.last_error,
  standingError: typeof stPaid.last_error === 'string' && stPaid.last_error.length > 0,
});

// ------------------------------------------------------------ G2
init();
plant([{ sig: SIG_A, slot: 44000001, logs: '["log one"]', state: 'held' }]);
const g = await push();
const stClean = statusOf();
face('G2', 'a target that never failed reports no error (guard)', {
  exitCode: g.code,
  lastError: stClean.last_error,
  delivered: g.report ? g.report.targets[0].delivered : null,
});

srv.close();

const byId = Object.fromEntries(faces.map((f) => [f.id, f]));
const DEFECT = (f) =>
  f.id === 'S1'
    ? f.standingError === true
    : f.id === 'S2'
      ? f.standingError === true
      : false;
const FIXED = (f) =>
  f.id === 'S1'
    ? f.standingError === false && f.delivered === 2
    : f.id === 'S2'
      ? f.standingError === false && f.delivered === 1
      : false;
const GUARD = (f) =>
  f.id === 'G1'
    ? f.reported === true
    : f.id === 'G2'
      ? f.lastError === null && f.delivered === 1
      : false;

const defects = faces.filter((f) => DEFECT(f)).map((f) => f.id);
const fixed = faces.filter((f) => FIXED(f)).map((f) => f.id);
const guardBroken = faces.filter((f) => GUARD(f) && !GUARD(f)).map((f) => f.id);
const guardOk = faces.filter((f) => GUARD(f)).map((f) => f.id);

const verdict =
  defects.length > 0
    ? 'stale-error-verdict-present'
    : fixed.length === 2 && guardBroken.length === 0
      ? 'stale-error-verdict-cleared'
      : 'no-verdict';

const report = {
  tool: 'measure-webhook-stale-error',
  measuredAt: new Date().toISOString(),
  tree: LABEL,
  // The binary is named by its OWN BYTES, never by a host path — the
  // artifact must be readable on a machine that has no /tmp/reeflog-base.
  binary: 'reeflog',
  binarySha256: sha256(BIN),
  binaryVersion: binaryVersion(),
  verdict,
  verdict,
  faces,
  defectFaces: defects,
  fixedFaces: fixed,
  guards: guardOk,
  guardsBroken: guardBroken,
  summary: {
    defectVisible: defects.length > 0,
    defectGone: fixed.length === 2,
    standingFailureReported: guardOk.includes('G1'),
    cleanTargetSilent: guardOk.includes('G2'),
  },
};

console.log(JSON.stringify(report, null, 2));
if (OUT) {
  fs.writeFileSync(OUT, JSON.stringify(report, null, 2) + '\n');
  console.error(`wrote ${OUT}`);
}
process.exit(defects.length > 0 ? 1 : fixed.length === 2 && guardBroken.length === 0 ? 0 : 3);

function sha256(file) {
  return createHash('sha256').update(fs.readFileSync(file)).digest('hex');
}

function binaryVersion() {
  try {
    return execFileSync(BIN, ['--version'], { encoding: 'utf8' }).trim();
  } catch {
    return 'unknown';
  }
}
