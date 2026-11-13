#!/usr/bin/env node
// The PAYLOAD-HOLD measurement (PULS AE, 2026-11-13) — MEASURED with the
// REAL binary, not inferred from the source.
//
// The defect (v0.7.21, published): plain `reeflog tail` delivered a row
// whose payload the journal still OWED (`logs IS NULL AND payload_state IS
// NULL`) and advanced the cursor past it, exit 0. The consumer got a
// signature with `logs: null`; when `hydrate` later filled the payload, the
// next pass reported `0 events after cursor 2` — the finished payload was
// permanently unreachable for that consumer, while a fresh cursor read it
// fine. Silent, permanent data loss behind the DEFAULT path.
//
// The faces (each one a predicate for BOTH trees):
//   F1 the owed row is DELIVERED and CONSUMED (baseline: the defect);
//   F2 the fill that arrives later reaches the SAME cursor (baseline: lost);
//   F3 the hold is named and the cursor stays behind the owed row (fix);
//   F4 a recorded outcome still travels with its marker (guard, both trees);
//   F5 `--no-logs` consumers are never held (guard, both trees).
//
//   node tools/measure-payload-hold.mjs
//   BIN=/path/to/reeflog OUT=/tmp/x.json LABEL=v0.7.21 node tools/measure-payload-hold.mjs
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import crypto from 'node:crypto';
import { execFileSync, spawnSync } from 'node:child_process';

const BIN = process.env.BIN || path.resolve(import.meta.dirname, '..', 'target/release/reeflog');
const OUT = process.env.OUT || null;
const LABEL = process.env.LABEL || 'this-tree';
const PROGRAM = 'whirLbMiicVdio4qvUfM5KAg6Ct8VwpYzGff3uctyCc';

if (!fs.existsSync(BIN)) {
  console.error(`no reeflog binary at ${BIN} — refusing to measure without one`);
  process.exit(2);
}

const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'reeflog-payload-hold-'));
const db = path.join(dir, 'j.db');
const cursor = path.join(dir, 'c.txt');
const cursor2 = path.join(dir, 'c2.txt');

function run(args) {
  const r = spawnSync(BIN, args, { cwd: dir, encoding: 'utf8' });
  return {
    code: r.status,
    out: (r.stdout || '').split('\n').filter(Boolean),
    err: (r.stderr || '').trim(),
  };
}

function init() {
  // Each face starts from an empty journal: the db must not carry rows over.
  for (const f of [db, db + '-wal', db + '-shm', cursor, cursor2]) fs.rmSync(f, { force: true });
  execFileSync(BIN, ['init', '--db', db, '--program', PROGRAM], { cwd: dir, stdio: 'ignore' });
}

const { DatabaseSync } = await import('node:sqlite');
function plant(rows) {
  const conn = new DatabaseSync(db);
  const now = Date.now();
  for (const r of rows) {
    conn
      .prepare(
        `INSERT INTO events(signature, slot, err, logs, source, first_seen, payload_state)
         VALUES (?, ?, NULL, ?, 'ws', ?, ?)`,
      )
      .run(r.sig, r.slot, r.logs, now, r.state);
  }
  conn.close();
}
function pay(sig, logs) {
  const conn = new DatabaseSync(db);
  conn.prepare("UPDATE events SET logs = ?, payload_state = 'held' WHERE signature = ?").run(logs, sig);
  conn.close();
}
function cursorPos(file) {
  if (!fs.existsSync(file)) return 0;
  const raw = fs.readFileSync(file, 'utf8').trim();
  if (raw.startsWith('{')) {
    const m = raw.match(/"value":\s*(\d+)/);
    return m ? Number(m[1]) : 0;
  }
  const n = Number(raw);
  return Number.isFinite(n) ? n : 0;
}
function sigsOf(lines) {
  return lines
    .filter((l) => l.trim().startsWith('{'))
    .map((l) => {
      try {
        return JSON.parse(l).signature;
      } catch {
        return null;
      }
    })
    .filter(Boolean);
}
function logsOf(lines, sig) {
  for (const l of lines) {
    if (!l.trim().startsWith('{')) continue;
    try {
      const j = JSON.parse(l);
      if (j.signature === sig) return j.logs;
    } catch {
      /* a line we cannot read is a face of its own */
    }
  }
  return undefined;
}

const SIG_A = 'SigOne1111111111111111111111111111111111111';
const SIG_B = 'SigTwo1111111111111111111111111111111111111';
const SIG_C = 'SigThree111111111111111111111111111111111111';
const SIG_D = 'SigFour1111111111111111111111111111111111111';

const faces = [];
function face(id, kind, detail) {
  faces.push({ id, kind, ...detail });
}

// ---------------------------------------------------------------- F1/F2/F3
// seq 1 = payload held, seq 2 = payload OWED, seq 3 = payload held.
init();
plant([
  { sig: SIG_A, slot: 44000001, logs: '["log A"]', state: 'held' },
  { sig: SIG_B, slot: 44000002, logs: null, state: null },
  { sig: SIG_C, slot: 44000003, logs: '["log C"]', state: 'held' },
]);

const pass1 = run(['tail', '--db', db, '--cursor', cursor, '--limit', '100']);
const p1sigs = sigsOf(pass1.out);
const p1pos = cursorPos(cursor);
const p1logs = logsOf(pass1.out, SIG_B);
face('F1', 'the owed row and the cursor past it', {
  delivered: p1sigs,
  exitCode: pass1.code,
  cursorAfter: p1pos,
  owedRowDelivered: p1sigs.includes(SIG_B),
  owedRowPayload: p1logs === undefined ? 'not delivered' : p1logs,
  cursorPastTheOwedRow: p1pos >= 2,
  stderr: pass1.err.slice(0, 220),
});
face('F3', 'the hold is named, the cursor stays behind the owed row', {
  exitCode: pass1.code,
  namedTheHold: /HELD at seq 2/.test(pass1.err),
  namesHydrate: /hydrate/.test(pass1.err),
  cursorAfter: p1pos,
  cursorBehindTheOwedRow: p1pos < 2,
  stderr: pass1.err.slice(0, 220),
});

// The debt is paid AFTER the first pass — the moment a consumer's loss
// becomes permanent.
pay(SIG_B, '["log B paid"]');
const pass2 = run(['tail', '--db', db, '--cursor', cursor, '--limit', '100']);
const p2sigs = sigsOf(pass2.out);
const p2logs = logsOf(pass2.out, SIG_B);
face('F2', 'the fill that arrives later reaches the SAME cursor', {
  delivered: p2sigs,
  exitCode: pass2.code,
  cursorAfter: cursorPos(cursor),
  theFilledRowArrived: p2sigs.includes(SIG_B),
  theFilledPayload: p2logs === undefined ? 'never delivered' : p2logs,
  stderr: pass2.err.slice(0, 220),
});

// A FRESH cursor sees the same row — proof that the payload exists and only
// the walked-past cursor lost it.
const passFresh = run(['tail', '--db', db, '--cursor', cursor2, '--limit', '100']);
face('F2b', 'a fresh cursor reads the very same row (the payload exists)', {
  delivered: sigsOf(passFresh.out),
  theFilledPayload: logsOf(passFresh.out, SIG_B),
});

// ---------------------------------------------------------------- F4/F5
// A recorded outcome travels WITH its marker — an answer, not a debt.
init();
plant([
  { sig: SIG_A, slot: 44000001, logs: '["log A"]', state: 'held' },
  { sig: SIG_D, slot: 44000004, logs: null, state: 'unavailable' },
  { sig: SIG_C, slot: 44000003, logs: '["log C"]', state: 'held' },
]);
const pass4 = run(['tail', '--db', db, '--cursor', cursor, '--limit', '100']);
const p4lines = pass4.out.filter((l) => l.trim().startsWith('{')).map((l) => JSON.parse(l));
const decided = p4lines.find((x) => x.signature === SIG_D);
face('F4', 'a recorded outcome is delivered with its marker (guard)', {
  exitCode: pass4.code,
  delivered: p4lines.map((x) => x.signature),
  marker: decided ? { logs: decided.logs, payload_state: decided.payload_state } : null,
  cursorAfter: cursorPos(cursor),
});

// `--no-logs` consumers asked for identities only — never held.
init();
plant([
  { sig: SIG_A, slot: 44000001, logs: null, state: null },
  { sig: SIG_B, slot: 44000002, logs: null, state: null },
]);
const pass5 = run(['tail', '--db', db, '--cursor', cursor, '--limit', '100', '--no-logs']);
face('F5', '--no-logs is never held (guard)', {
  exitCode: pass5.code,
  delivered: sigsOf(pass5.out),
  cursorAfter: cursorPos(cursor),
  compact: !pass5.out.some((l) => l.includes('"logs"')),
});

const byId = Object.fromEntries(faces.map((f) => [f.id, f]));

// Verdict per face: the DEFECT side and the FIXED side. A face whose
// baseline predicate is vacuous would certify nothing.
const DEFECT = (f) =>
  f.id === 'F1'
    ? f.owedRowDelivered && f.cursorPastTheOwedRow && f.exitCode === 0
    : f.id === 'F2'
      ? !f.theFilledRowArrived
      : f.id === 'F3'
        ? !(f.exitCode === 3 && f.namedTheHold && f.cursorBehindTheOwedRow)
        : f.id === 'F2b'
          ? f.theFilledPayload !== undefined && f.theFilledPayload !== null
          : false;
const FIXED = (f) =>
  f.id === 'F1'
    ? !f.owedRowDelivered && !f.cursorPastTheOwedRow
    : f.id === 'F2'
      ? f.theFilledRowArrived && Array.isArray(f.theFilledPayload)
      : f.id === 'F3'
        ? f.exitCode === 3 && f.namedTheHold && f.namesHydrate && f.cursorBehindTheOwedRow
        : f.id === 'F2b'
          ? f.theFilledPayload !== undefined && f.theFilledPayload !== null
          : false;
const GUARD = (f) =>
  f.id === 'F4'
    ? f.exitCode === 0 && f.delivered.length === 3 && f.marker?.payload_state === 'unavailable' && f.marker?.logs === null
    : f.id === 'F5'
      ? f.exitCode === 0 && f.delivered.length === 2 && f.compact && f.cursorAfter === 2
      : false;

const defectFaces = ['F1', 'F2', 'F3'];
const defectVisible = defectFaces.every((id) => DEFECT(byId[id]));
const defectGone = defectFaces.every((id) => !DEFECT(byId[id]));
const fixedHolds = defectFaces.every((id) => FIXED(byId[id]));
const guardsGreen = GUARD(byId.F4) && GUARD(byId.F5);

let verdict;
if (defectVisible && !fixedHolds) verdict = 'payload-hold-defect-present';
else if (!defectVisible && fixedHolds && guardsGreen) verdict = 'payload-hold-fixed';
else if (!defectVisible && fixedHolds && !guardsGreen) verdict = 'payload-hold-fixed-guard-broken';
else verdict = 'payload-hold-inconclusive';

const report = {
  tool: 'measure-payload-hold',
  measuredAt: new Date().toISOString(),
  tree: LABEL,
  // The binary is named by its own version and digest — never by the path
  // it happened to sit at (committed artifacts must not carry host paths).
  binary: path.basename(BIN),
  binarySha256: crypto.createHash('sha256').update(fs.readFileSync(BIN)).digest('hex'),
  binaryVersion: execFileSync(BIN, ['--version'], { encoding: 'utf8' }).trim(),
  verdict,
  faces,
  summary: {
    defectVisible,
    defectGone,
    fixedHolds,
    guardsGreen,
  },
};
// The journal path is an OS temp dir: no committed artifact may carry it.
const clean = JSON.stringify(report, null, 2).replaceAll(dir, '<tmpdir>');
if (OUT) {
  fs.mkdirSync(path.dirname(OUT), { recursive: true });
  fs.writeFileSync(OUT, clean + '\n');
}
for (const f of faces) console.log(`${f.id}  ${f.kind}: ${JSON.stringify(f)}`);
console.log(`VERDICT: ${verdict}`);
fs.rmSync(dir, { recursive: true, force: true });
process.exit(verdict === 'payload-hold-fixed' || verdict === 'payload-hold-defect-present' ? 0 : 1);
