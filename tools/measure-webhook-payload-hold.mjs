#!/usr/bin/env node
// The WEBHOOK-PAYLOAD-HOLD measurement (PULS AF, 2026-11-13) — MEASURED with
// the REAL binary against a REAL local receiver, not inferred from source.
//
// The defect (v0.7.22, this tree before the fix): `deliver_events` posted a
// row whose payload the journal still owes (`logs IS NULL AND payload_state
// IS NULL`) as a full, SIGNED event with `logs: null`, advanced the
// target's `next_seq` past it, and wrote a receipt. When `hydrate` later
// paid the debt, the next pass delivered NOTHING — the finished event was
// permanently unreachable for that consumer, while a fresh target read it
// fine. Exactly the loss PULS AE stopped on the CLI `tail` path, one
// channel over: the webhook is the channel for the consumer that is NOT on
// this machine, and its body is signed, so the receiver cannot tell a
// signature without content from a real event.
//
// The faces (each one a predicate for BOTH trees):
//   W1 the owed row is POSTED and CONSUMED (baseline: the defect);
//   W2 the fill that arrives later never reaches the SAME target (baseline:
//      lost; fixed: delivered exactly once, with its payload);
//   W3 the hold is named and the cursor stays behind the owed row (fix);
//   G1 a full row still delivers and receipts (guard, both trees);
//   G2 a recorded outcome travels with its marker (guard, both trees).
//
//   node tools/measure-webhook-payload-hold.mjs
//   BIN=/path/to/reeflog OUT=/tmp/x.json LABEL=v0.7.22 node tools/measure-webhook-payload-hold.mjs
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import http from 'node:http';
import crypto from 'node:crypto';
import { execFileSync, spawn } from 'node:child_process';
import { DatabaseSync } from 'node:sqlite';

const BIN = process.env.BIN || path.resolve(import.meta.dirname, '..', 'target/release/reeflog');
const OUT = process.env.OUT || null;
const LABEL = process.env.LABEL || 'this-tree';
const PROGRAM = 'whirLbMiicVdio4qvUfM5KAg6Ct8VwpYzGff3uctyCc';

if (!fs.existsSync(BIN)) {
  console.error(`no reeflog binary at ${BIN} — refusing to measure without one`);
  process.exit(2);
}

const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'reeflog-whhold-'));
const db = path.join(dir, 'j.db');
const cfgPath = path.join(dir, 'wh.json');

const received = [];
const srv = http.createServer((req, res) => {
  const chunks = [];
  req.on('data', (c) => chunks.push(c));
  req.on('end', () => {
    received.push(Buffer.concat(chunks).toString('utf8'));
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
  const { code, stderr } = await new Promise((res) => {
    const c = spawn(BIN, ['webhook', 'push', '--db', db, '--config', cfgPath], { cwd: dir });
    let err = '';
    c.stderr.on('data', (d) => (err += d));
    c.on('exit', (code) => res({ code, stderr: err }));
  });
  // let in-flight connections finish
  await new Promise((r) => setTimeout(r, 150));
  return { code, stderr, bodies: received.slice(before).map((b) => tryJson(b)) };
}
function tryJson(b) {
  try {
    return JSON.parse(b);
  } catch {
    return null;
  }
}
function targetState() {
  const conn = new DatabaseSync(db);
  const s = conn.prepare('SELECT next_seq FROM webhook_state WHERE target = ?').get('t1');
  const r = conn.prepare('SELECT COUNT(*) AS n FROM webhook_receipts WHERE target = ?').get('t1');
  conn.close();
  return { nextSeq: s ? s.next_seq : 0, receipts: r.n };
}

const SIG_A = 'SigOne1111111111111111111111111111111111111';
const SIG_B = 'SigTwo1111111111111111111111111111111111111';

const faces = [];
const face = (id, kind, detail) => faces.push({ id, kind, ...detail });

// ------------------------------------------------------------ W1/W2/W3
// seq 1 full, seq 2 OWED.
init();
plant([
  { sig: SIG_A, slot: 44000001, logs: '["log A"]', state: 'held' },
  { sig: SIG_B, slot: 44000002, logs: null, state: null },
]);
const p1 = await push();
const postedB = p1.bodies.some((b) => b && b.signature === SIG_B);
const st1 = targetState();
face('W1', 'the owed row is posted and consumed', {
  posted: p1.bodies.map((b) => (b ? b.signature : '<unreadable>')),
  exitCode: p1.code,
  ...st1,
  owedRowPosted: postedB,
  cursorPastTheOwedRow: st1.nextSeq >= 2,
  owedPayloadInBody: postedB ? p1.bodies.find((b) => b?.signature === SIG_B).logs : 'not posted',
});
face('W3', 'the hold is named and the cursor stays behind the owed row', {
  exitCode: p1.code,
  namedTheHold: /blocked|payload|hydrate/i.test(p1.stderr),
  namesHydrate: /hydrate/.test(p1.stderr),
  stderr: p1.stderr.trim().slice(0, 220),
  cursorAfter: st1.nextSeq,
  cursorBehindTheOwedRow: st1.nextSeq < 2,
});

// The debt is paid AFTER the delivery — the moment a consumer's loss becomes
// permanent.
pay(SIG_B, '["log B paid"]');
const p2 = await push();
const filledB = p2.bodies.find((b) => b && b.signature === SIG_B);
const st2 = targetState();
face('W2', 'the fill that arrives later reaches the SAME target', {
  posted: p2.bodies.map((b) => (b ? b.signature : '<unreadable>')),
  exitCode: p2.code,
  theFilledRowArrived: Boolean(filledB),
  theFilledPayload: filledB ? filledB.logs : 'never delivered',
  ...st2,
});

// ------------------------------------------------------------ G1
init();
plant([{ sig: SIG_A, slot: 44000001, logs: '["log A"]', state: 'held' }]);
const g1 = await push();
const stG1 = targetState();
face('G1', 'a full row still delivers and receipts (guard)', {
  posted: g1.bodies.map((b) => (b ? b.signature : '<unreadable>')),
  exitCode: g1.code,
  ...stG1,
});

// ------------------------------------------------------------ G2
init();
plant([{ sig: SIG_A, slot: 44000001, logs: '["log A"]', state: 'held' }, { sig: SIG_B, slot: 44000002, logs: null, state: 'unavailable' }]);
const g2 = await push();
const decided = g2.bodies.find((b) => b && b.signature === SIG_B);
face('G2', 'a recorded outcome is delivered with its marker (guard)', {
  posted: g2.bodies.map((b) => (b ? b.signature : '<unreadable>')),
  exitCode: g2.code,
  marker: decided ? { logs: decided.logs, payload_state: decided.payload_state } : null,
});

srv.close();

const byId = Object.fromEntries(faces.map((f) => [f.id, f]));
const DEFECT = (f) =>
  f.id === 'W1'
    ? f.owedRowPosted && f.cursorPastTheOwedRow
    : f.id === 'W2'
      ? !f.theFilledRowArrived
      : f.id === 'W3'
        ? !(f.exitCode === 3 && f.cursorBehindTheOwedRow)
        : false;
const FIXED = (f) =>
  f.id === 'W1'
    ? !f.owedRowPosted && !f.cursorPastTheOwedRow
    : f.id === 'W2'
      ? f.theFilledRowArrived && Array.isArray(f.theFilledPayload)
      : f.id === 'W3'
        ? f.exitCode === 3 && f.cursorBehindTheOwedRow
        : false;
const GUARD = (f) =>
  f.id === 'G1'
    ? f.exitCode === 0 && f.posted.length === 1 && f.nextSeq === 1 && f.receipts === 1
    : f.id === 'G2'
      ? f.exitCode === 0 && f.posted.length === 2 && f.marker?.payload_state === 'unavailable' && f.marker?.logs === null
      : false;

const defectIds = ['W1', 'W2', 'W3'];
const defectVisible = defectIds.every((id) => DEFECT(byId[id]));
const fixedHolds = defectIds.every((id) => FIXED(byId[id]));
const guardsGreen = GUARD(byId.G1) && GUARD(byId.G2);

let verdict;
if (defectVisible && !fixedHolds) verdict = 'webhook-payload-hold-defect-present';
else if (!defectVisible && fixedHolds && guardsGreen) verdict = 'webhook-payload-hold-fixed';
else if (!defectVisible && fixedHolds && !guardsGreen) verdict = 'webhook-payload-hold-fixed-guard-broken';
else verdict = 'webhook-payload-hold-inconclusive';

const report = {
  tool: 'measure-webhook-payload-hold',
  measuredAt: new Date().toISOString(),
  tree: LABEL,
  binary: path.basename(BIN),
  binarySha256: crypto.createHash('sha256').update(fs.readFileSync(BIN)).digest('hex'),
  binaryVersion: execFileSync(BIN, ['--version'], { encoding: 'utf8' }).trim(),
  verdict,
  faces,
  summary: { defectVisible, fixedHolds, guardsGreen },
};
const clean = JSON.stringify(report, null, 2).replaceAll(dir, '<tmpdir>');
if (OUT) {
  fs.mkdirSync(path.dirname(OUT), { recursive: true });
  fs.writeFileSync(OUT, `${clean}\n`);
}
for (const f of faces) console.log(`${f.id}  ${f.kind}: ${JSON.stringify(f)}`);
console.log(`VERDICT: ${verdict}`);
fs.rmSync(dir, { recursive: true, force: true });
process.exit(verdict === 'webhook-payload-hold-fixed' || verdict === 'webhook-payload-hold-defect-present' ? 0 : 1);
