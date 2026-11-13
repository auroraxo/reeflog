#!/usr/bin/env node
// The WEBHOOK-LIMIT-ZERO / PAGE-DOMAIN measurement (PULS AI, 2026-11-14) —
// MEASURED with the REAL binary against a REAL local receiver, not inferred
// from source.
//
// The defect (v0.7.25, this tree before the fix): three rewrites of an
// operator's explicit budget into something else:
//   1. `webhook push --limit 0` POSTED one signed event per pass and
//      advanced the target cursor — two passes with `--limit 0` consumed 2
//      of 5 planted rows, exit 0. The value is legal (flags.rs NUMERIC
//      keeps 0; `tail --limit 0` delivers ZERO rows) yet `deliver_events` /
//      `deliver_reorgs` bound `limit.max(1)`.
//   2. `webhook dlq replay --limit 0` attempted ONE quarantined letter
//      (`webhook_dead_letters` bound `limit.max(1)`): bytes left the
//      machine on a pass explicitly asked to move nothing.
//   3. `webhook push --limit 2` against 5 pending rows reported
//      `delivered: 2, exit 0` with NOTHING naming the three behind the
//      page — the exact lie v0.7.12 removed from `dlq replay` ("the report
//      describes the QUEUE, not the PAGE"), never imported into push.
//      A cron reading the JSON believed the mailbox drained.
//
// The faces (each one a predicate for BOTH trees):
//   Z1 `push --limit 0` delivers nothing, cursor stays (fix);
//   Z2 `push --limit 2` of 5 names the rows behind the page
//      (`remaining_behind_cursor`, `truncated`) (fix);
//   Z3 `dlq replay --limit 0` moves no letter and exits 3 naming the
//      pending count (fix);
//   G1 a positive limit delivers exactly that many (guard, both trees);
//   G2 the default pass drains the queue and says so (guard, both trees).
//
//   node tools/measure-webhook-limit-zero.mjs
//   BIN=/path/to/reeflog OUT=/tmp/x.json LABEL=v0.7.25 node tools/measure-webhook-limit-zero.mjs
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

const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'reeflog-limit0-'));
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
function plant(n, prefix = 'Sig') {
  const conn = new DatabaseSync(db);
  for (let i = 1; i <= n; i++) {
    conn
      .prepare(
        `INSERT INTO events(signature, slot, err, logs, source, first_seen, payload_state)
         VALUES (?, ?, NULL, ?, 'ws', ?, 'held')`
      )
      .run(`${prefix}${i}`, 44000000 + i, `["log ${i}"]`, Date.now());
  }
  conn.close();
}
async function run(args) {
  const before = received.length;
  const { code, stdout, stderr } = await new Promise((res) => {
    const c = spawn(BIN, args, { cwd: dir });
    let out = '';
    let err = '';
    c.stdout.on('data', (d) => (out += d));
    c.stderr.on('data', (d) => (err += d));
    c.on('exit', (code) => res({ code, stdout: out, stderr: err }));
  });
  // let in-flight connections finish
  await new Promise((r) => setTimeout(r, 150));
  let report = null;
  try {
    report = JSON.parse(stdout.trim());
  } catch {
    report = null;
  }
  return { code, stdout, stderr, report, bodies: received.slice(before) };
}
function push(args) {
  return run(['webhook', 'push', '--db', db, '--config', cfgPath, ...args]);
}
function targetState() {
  const conn = new DatabaseSync(db);
  const s = conn.prepare('SELECT next_seq FROM webhook_state WHERE target = ?').get('t1');
  const r = conn.prepare('SELECT COUNT(*) AS n FROM webhook_receipts WHERE target = ?').get('t1');
  conn.close();
  return { nextSeq: s ? s.next_seq : 0, receipts: r.n };
}
function plantReorg() {
  const conn = new DatabaseSync(db);
  conn
    .prepare(
      `INSERT INTO reorgs(signature, slot, detected_at, status_source, tx_source, detail)
       VALUES ('REORG1', 44000001, 1700000000000, 'ws', 'ws', '')`
    )
    .run();
  conn.close();
}
function plantLetters(n) {
  const conn = new DatabaseSync(db);
  for (let i = 1; i <= n; i++) {
    conn
      .prepare(
        `INSERT INTO webhook_dead_letters(target,kind,seq,body,event_id,reason,status,attempts,first_failed_at,last_failed_at,state)
         VALUES ('t1','event',?, '{}', ?, 'refused with http 403', 403, 1, ?, ?, 'pending')`
      )
      .run(i, `t1:event:${i}`, Date.now(), Date.now());
  }
  conn.close();
}
function pendingLetters() {
  const conn = new DatabaseSync(db);
  const r = conn
    .prepare(`SELECT COUNT(*) AS n FROM webhook_dead_letters WHERE target='t1' AND state='pending'`)
    .get();
  conn.close();
  return r.n;
}

const faces = [];
const face = (id, kind, detail) => faces.push({ id, kind, ...detail });

// ------------------------------------------------------------ Z1
// 5 planted rows, `push --limit 0` twice: zero bytes may leave, the cursor
// may not move.
init();
plant(5);
plantReorg();
const z1a = await push(['--limit', '0']);
const z1b = await push(['--limit', '0']);
const z1s = targetState();
face('Z1', '`push --limit 0` attempts nothing and moves no cursor', {
  bodies: z1a.bodies.length + z1b.bodies.length,
  delivered: [z1a.report?.targets?.[0]?.delivered, z1b.report?.targets?.[0]?.delivered],
  reorgDelivered: z1b.report?.targets?.[0]?.reorg_delivered ?? 'missing',
  pageLimit: z1a.report?.targets?.[0]?.page_limit ?? 'missing',
  exitCodes: [z1a.code, z1b.code],
  cursor: z1s.nextSeq,
  reorgCursor: z1b.report?.targets?.[0]?.next_reorg_id ?? 'missing',
  zeroDelivered: (z1a.report?.targets?.[0]?.delivered ?? 99) === 0 && (z1b.report?.targets?.[0]?.delivered ?? 99) === 0,
  noRetractionLeft: (z1b.report?.targets?.[0]?.reorg_delivered ?? 99) === 0,
  noBytesLeft: z1a.bodies.length === 0 && z1b.bodies.length === 0,
  cursorUnmoved: z1s.nextSeq === 0,
});

// ------------------------------------------------------------ Z2
// 5 pending, `--limit 2`: the report must name the three behind the page.
init();
plant(5);
const z2 = await push(['--limit', '2']);
const z2t = z2.report?.targets?.[0] || {};
const z2s = targetState();
face('Z2', 'a truncated push names the rows behind the page', {
  delivered: z2t.delivered ?? 'missing',
  remainingBehindCursor: z2t.remaining_behind_cursor ?? 'missing',
  truncated: z2t.truncated ?? 'missing',
  exitCode: z2.code,
  cursor: z2s.nextSeq,
  namesTheRest:
    typeof z2t.remaining_behind_cursor === 'number' &&
    z2t.remaining_behind_cursor === 3 &&
    z2t.truncated === true,
  pageDeliveredTwo: (z2t.delivered ?? 0) === 2,
});

// ------------------------------------------------------------ Z3
// 2 pending letters, `dlq replay --limit 0`: zero may move, the exit must
// still hold the truth.
init();
plant(0);
plantLetters(2);
const z3 = await run(['webhook', 'dlq', 'replay', '--db', db, '--config', cfgPath, '--limit', '0']);
const left = pendingLetters();
face('Z3', '`dlq replay --limit 0` moves no letter and holds the exit code', {
  replayed: z3.report?.replayed ?? 'missing',
  pendingBefore: z3.report?.pending_before ?? 'missing',
  remainingPending: z3.report?.remaining_pending ?? 'missing',
  exitCode: z3.code,
  lettersLeft: left,
  bodies: z3.bodies.length,
  namesThePending: z3.code === 3 && /\bnot reached\b/.test(z3.stderr),
  zeroReplayed: (z3.report?.replayed ?? 99) === 0,
  nothingLeftTheMachine: z3.bodies.length === 0 && left === 2,
});

// ------------------------------------------------------------ G1
init();
plant(5);
const g1 = await push(['--limit', '3']);
const g1t = g1.report?.targets?.[0] || {};
face('G1', 'a positive limit still delivers exactly that many (guard)', {
  delivered: g1t.delivered ?? 'missing',
  remainingBehindCursor: g1t.remaining_behind_cursor ?? 'missing',
  bodies: g1.bodies.length,
  exact: (g1t.delivered ?? 0) === 3 && g1.bodies.length === 3,
});

// ------------------------------------------------------------ G2
init();
plant(4);
const g2 = await push([]);
const g2t = g2.report?.targets?.[0] || {};
face('G2', 'the default pass drains the queue and says so (guard)', {
  delivered: g2t.delivered ?? 'missing',
  remainingBehindCursor: g2t.remaining_behind_cursor ?? 'missing',
  truncated: g2t.truncated ?? 'missing',
  drained: (g2t.delivered ?? 0) === 4 && (g2t.remaining_behind_cursor ?? 99) === 0 && g2t.truncated === false,
});

srv.close();
for (const f of [db, `${db}-wal`, `${db}-shm`]) fs.rmSync(f, { force: true });
fs.rmSync(dir, { recursive: true, force: true });

const DEFECT = (f) =>
  f.id === 'Z1'
    ? !(f.zeroDelivered && f.noBytesLeft && f.cursorUnmoved && f.noRetractionLeft && f.reorgCursor === 0)
    : f.id === 'Z2'
      ? !f.namesTheRest
      : f.id === 'Z3'
        ? !(f.zeroReplayed && f.nothingLeftTheMachine && f.namesThePending)
        : false;
const GUARD = (f) => (f.id === 'Z1' || f.id === 'Z2' || f.id === 'Z3' ? undefined : f.id === 'G1' ? f.exact : f.drained);

const result = {
  kind: 'webhook-limit-zero-page-domain',
  label: LABEL,
  // The binary is named by its VERSION and its sha256, never by the path it
  // happens to sit at: a committed artifact must not carry the machine it
  // was measured on (`tests/research_artifacts.rs`).
  binary: path.basename(BIN),
  binarySha256: crypto.createHash('sha256').update(fs.readFileSync(BIN)).digest('hex'),
  binaryVersion: execFileSync(BIN, ['--version']).toString().trim(),
  measuredAt: new Date().toISOString(),
  faces,
  verdict: faces.some((f) => DEFECT(f))
    ? 'defect-present'
    : faces.filter((f) => GUARD(f) !== undefined).every((f) => GUARD(f))
      ? 'fixed-and-guards-held'
      : 'guard-failed',
};
console.log(JSON.stringify(result, null, 2));
if (OUT) fs.writeFileSync(OUT, JSON.stringify(result, null, 2) + '\n');
process.exit(result.verdict === 'defect-present' ? 1 : result.verdict === 'guard-failed' ? 3 : 0);
