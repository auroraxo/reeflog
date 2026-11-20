#!/usr/bin/env node
// Measure whether the seal pass can drain a backlog at all — two real
// binaries against the same local chain sink.
//
// THE QUESTION (measured 2026-11-20): the seal pass asked for the OLDEST
// 150 pending rows every time. Two consequences, both checked here:
//
//   * the daemon tick compared the size of that window before and after
//     each batch, and both were clamped to 150 — so a backlog of 300 or
//     more could never show progress and the tick stopped after a single
//     batch per cadence, no matter how much work was left;
//
//   * the window never moved past rows the chain could not decide, so a
//     head of undecidable rows blocked every row behind it from ever being
//     asked. The rows behind a stuck head were not slow — they were never
//     asked at all, and `status` reported a healthy exit 0 over it.
//
// Faces (each run against BOTH binaries with an identical fixture):
//   D1 daemon drain: 1,000 planted pending rows, `start --seal-every 1`
//      for 3 s against a chain that answers every status → the backlog
//      must reach 0 unsealed inside one run.
//   D2 CLI drain: `seal` on 1,000 pending rows → every row asked in one
//      pass, and the summary says whether the pass actually finished.
//   D3 head block: 150 undecidable rows (status source refuses, second
//      source still serves → no verdict, stay pending) in front of 150
//      decidable rows → the decidable tail must still get its verdict.
//
// Guards (must hold on BOTH trees — the honest forms must not move):
//   G1 a backlog of one batch (150 rows) drains on both trees;
//   G2 the two-source rule holds: undecidable rows are never sealed as
//      missing, on either tree;
//   G3 a dead endpoint never looks like a pass: neither tree exits 0 or
//      seals rows when the RPC cannot answer.
//
// Usage:
//   node tools/measure-seal-pass-backlog-drain.mjs \
//     --baseline <path/to/baseline/reeflog> \
//     --fixed <path/to/fixed/reeflog> \
//     --out research/seal-pass-backlog-drain-2026-11-20.json

import fs from 'node:fs';
import net from 'node:net';
import crypto from 'node:crypto';
import os from 'node:os';
import path from 'node:path';
import { spawn } from 'node:child_process';
import { DatabaseSync } from 'node:sqlite';

const PROGRAM = 'whirLbMiicVdio4qvUfM5KAg6Ct8VwpYzGff3uctyCc';
const WS_GUID = '258EAFA5-E914-47DA-95CA-C5AB0DC85B11';

function arg(name, fallback = null) {
  const i = process.argv.indexOf(name);
  return i >= 0 && process.argv[i + 1] ? process.argv[i + 1] : fallback;
}
const BASELINE = arg('--baseline') && path.resolve(arg('--baseline'));
const FIXED = arg('--fixed') && path.resolve(arg('--fixed'));
const OUT = arg('--out');
if (!BASELINE || !FIXED || !OUT) {
  console.error('usage: --baseline <bin> --fixed <bin> --out <json>');
  process.exit(2);
}

const sha256 = (p) => crypto.createHash('sha256').update(fs.readFileSync(p)).digest('hex');
const binVersion = (bin) =>
  new Promise((res) => {
    const c = spawn(bin, ['--version']);
    let o = '';
    c.stdout.on('data', (d) => (o += d));
    c.on('close', () => res(o.trim()));
  });

// One port, three roles (the house fixture): a real WS upgrade held open,
// JSON-RPC answers for the periodic passes, counted per method.
// mode: 'finalized'  — every status is finalized (the chain decides)
//       'traps:<n>'  — the FIRST n signatures get a null status whose
//                      second source still serves them (undecidable: the
//                      row must stay pending), the rest are finalized
//       'dead'       — every call is refused with HTTP 500
function makeSink(mode) {
  const calls = { getSignatureStatuses: [], getTransaction: 0, dead: 0 };
  const srv = net.createServer((s) => {
    let acc = Buffer.alloc(0);
    s.on('error', () => {});
    s.on('data', (d) => {
      acc = Buffer.concat([acc, d]);
      const p = acc.indexOf('\r\n\r\n');
      if (p < 0) return;
      const head = acc.subarray(0, p).toString();
      const cl = Number((head.match(/content-length:\s*(\d+)/i) || [])[1] || 0);
      const he = p + 4;
      if (acc.length < he + cl) return;
      const body = acc.subarray(he, he + cl).toString();
      acc = Buffer.alloc(0);
      if (head.toLowerCase().includes('upgrade: websocket')) {
        const key = (head.match(/sec-websocket-key:\s*(.+)/i) || [])[1]?.trim();
        const accept = crypto.createHash('sha1').update(key + WS_GUID).digest('base64');
        s.write(
          `HTTP/1.1 101 Switching Protocols\r\nupgrade: websocket\r\nconnection: upgrade\r\nsec-websocket-accept: ${accept}\r\n\r\n`
        );
        return; // held open for the rest of the run
      }
      let v;
      try {
        v = JSON.parse(body);
      } catch {
        return;
      }
      const m = v?.method || '';
      if (mode === 'dead') {
        calls.dead += 1;
        s.write('HTTP/1.1 500 Internal Server Error\r\ncontent-length: 0\r\nconnection: close\r\n\r\n');
        return;
      }
      let result;
      if (m === 'getSignatureStatuses') {
        const sigs = v?.params?.[0] || [];
        calls.getSignatureStatuses.push(sigs.length);
        const trapCount = mode.startsWith('traps:') ? Number(mode.slice(6)) : 0;
        // Trap identity is the signature's own index, not its position in
        // the batch: the same signature must be a trap in every batch it
        // appears in, and rows behind it must stay decidable.
        const idxOf = (sig) => {
          const n = Number(String(sig).replace(/^sig/, ''));
          return Number.isFinite(n) ? n : -1;
        };
        result = {
          value: sigs.map((sig) =>
            idxOf(sig) < trapCount ? null : { confirmationStatus: 'finalized', err: null }
        ),
        };
      } else if (m === 'getTransaction') {
        calls.getTransaction += 1;
        // Traps: the status source refused but this source serves the
        // transaction → NO verdict is allowed (the two-source rule).
        const trapCount = mode.startsWith('traps:') ? Number(mode.slice(6)) : 0;
        const sig = v?.params?.[0] || '';
        const n = Number(String(sig).replace(/^sig/, ''));
        result = Number.isFinite(n) && n < trapCount ? { slot: 1000 + n } : null;
      } else if (m === 'getSignaturesForAddress') {
        result = [];
      } else {
        result = {};
      }
      const payload = JSON.stringify({ jsonrpc: '2.0', id: v?.id ?? 1, result });
      s.write(
        `HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: ${payload.length}\r\nconnection: close\r\n\r\n${payload}`
      );
    });
  });
  return { srv, calls };
}

function plant(dir, rows) {
  fs.mkdirSync(dir, { recursive: true });
  const db = new DatabaseSync(path.join(dir, 'reeflog.db'));
  db.exec(`CREATE TABLE IF NOT EXISTS events(seq INTEGER PRIMARY KEY AUTOINCREMENT,signature TEXT NOT NULL UNIQUE,slot INTEGER NOT NULL,err TEXT,logs TEXT,source TEXT NOT NULL DEFAULT 'ws',first_seen INTEGER NOT NULL,seal_status TEXT,seal_seen INTEGER,payload_state TEXT)`);
  const ins = db.prepare(`INSERT INTO events(signature,slot,logs,source,first_seen,payload_state) VALUES(?1,?2,'[]','ws',?3,'held')`);
  for (let i = 0; i < rows; i++) ins.run(`sig${String(i).padStart(5, '0')}`, 1000 + i, Date.now());
  db.close();
}

function counts(dir) {
  const db = new DatabaseSync(path.join(dir, 'reeflog.db'), { readOnly: true });
  const one = (q) => db.prepare(q).get().n;
  const out = {
    unsealed: one(`SELECT COUNT(*) n FROM events WHERE seal_status IS NULL`),
    finalized: one(`SELECT COUNT(*) n FROM events WHERE seal_status='finalized'`),
    missing: one(`SELECT COUNT(*) n FROM events WHERE seal_status='missing'`),
    confirmed: one(`SELECT COUNT(*) n FROM events WHERE seal_status='confirmed'`),
  };
  db.close();
  return out;
}

function exec(cmd, args, cwd, timeoutMs) {
  return new Promise((res) => {
    const c = spawn(cmd, args, { cwd, timeout: timeoutMs, killSignal: 'SIGKILL' });
    let so = '',
      se = '';
    c.stdout.on('data', (d) => (so += d));
    c.stderr.on('data', (d) => (se += d));
    c.on('close', (st, sig) => res({ exit: st, signal: sig, stdout: so, stderr: se }));
    c.on('error', (e) => res({ exit: -1, signal: null, stdout: so, stderr: String(e) }));
  });
}

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
let tmpRoot = null;
function tmpdir(tag) {
  if (!tmpRoot) tmpRoot = fs.mkdtempSync(path.join(os.tmpdir(), 'reeflog-drain-'));
  const dir = path.join(tmpRoot, `${tag}-${Math.random().toString(36).slice(2, 8)}`);
  return dir;
}
async function listen(srv) {
  await new Promise((r) => srv.listen(0, '127.0.0.1', r));
  return srv.address().port;
}

// --- D1: the daemon must drain a 1,000-row backlog inside one run ----------
async function d1_daemon_drain(bin, tag) {
  const { srv, calls } = makeSink('finalized');
  const port = await listen(srv);
  const dir = tmpdir(`d1-${tag}`);
  plant(dir, 1000);
  const run = await exec(
    bin,
    [
      'start', '--db', 'reeflog.db', '--program', PROGRAM,
      '--rpc', `http://127.0.0.1:${port}`, '--ws', `ws://127.0.0.1:${port}`,
      '--seconds', '3', '--seal-every', '1',
      '--reorg-every', '0', '--repair-every', '0', '--webhook-every', '0',
    ],
    dir,
    30_000
  );
  await sleep(50);
  const c = counts(dir);
  srv.close();
  fs.rmSync(dir, { recursive: true, force: true });
  return {
    pending: 1000,
    sealed: c.finalized,
    still_unsealed: c.unsealed,
    rpc_batches: calls.getSignatureStatuses.length,
    sigs_per_batch: calls.getSignatureStatuses,
    exit: run.exit,
  };
}

// --- D2: the CLI must ask the whole backlog in one pass --------------------
async function d2_cli_drain(bin, tag, rows = 1000) {
  const { srv, calls } = makeSink('finalized');
  const port = await listen(srv);
  const dir = tmpdir(`d2-${tag}`);
  plant(dir, rows);
  const run = await exec(bin, ['seal', '--db', 'reeflog.db', '--rpc', `http://127.0.0.1:${port}`], dir, 60_000);
  const c = counts(dir);
  let summary = null;
  try {
    summary = JSON.parse(run.stdout);
  } catch {
    summary = null;
  }
  srv.close();
  fs.rmSync(dir, { recursive: true, force: true });
  return {
    pending: rows,
    sealed: c.finalized,
    still_unsealed: c.unsealed,
    rpc_batches: calls.getSignatureStatuses.length,
    sigs_per_batch: calls.getSignatureStatuses,
    exit: run.exit,
    complete: summary?.seal_pass?.complete ?? null,
    seal_pass: summary?.seal_pass ?? null,
    stderr: run.stderr.trim().slice(0, 300),
  };
}

// --- D3: rows behind an undecidable head must still be asked ---------------
async function d3_head_block(bin, tag) {
  const { srv, calls } = makeSink('traps:150');
  const port = await listen(srv);
  const dir = tmpdir(`d3-${tag}`);
  plant(dir, 300);
  const run = await exec(bin, ['seal', '--db', 'reeflog.db', '--rpc', `http://127.0.0.1:${port}`], dir, 60_000);
  const c = counts(dir);
  let summary = null;
  try {
    summary = JSON.parse(run.stdout);
  } catch {
    summary = null;
  }
  srv.close();
  fs.rmSync(dir, { recursive: true, force: true });
  return {
    pending: 300,
    undecidable_head: 150,
    decidable_tail: 150,
    sealed: c.finalized,
    still_unsealed: c.unsealed,
    missing: c.missing,
    rpc_batches: calls.getSignatureStatuses.length,
    sigs_per_batch: calls.getSignatureStatuses,
    exit: run.exit,
    complete: summary?.seal_pass?.complete ?? null,
    seal_pass: summary?.seal_pass ?? null,
  };
}

// --- G1: a one-batch backlog drains on both trees --------------------------
async function g1_small_backlog(bin, tag) {
  const r = await d2_cli_drain(bin, `g1-${tag}`, 150);
  return { pending: r.pending, sealed: r.sealed, still_unsealed: r.still_unsealed, exit: r.exit };
}

// --- G2: the two-source rule: an undecidable row is never "missing" --------
async function g2_two_source(bin, tag) {
  const { srv, calls } = makeSink('traps:10');
  const port = await listen(srv);
  const dir = tmpdir(`g2-${tag}`);
  plant(dir, 10);
  const run = await exec(bin, ['seal', '--db', 'reeflog.db', '--rpc', `http://127.0.0.1:${port}`], dir, 60_000);
  const c = counts(dir);
  srv.close();
  fs.rmSync(dir, { recursive: true, force: true });
  return {
    rows: 10,
    sealed: c.finalized,
    missing: c.missing,
    still_unsealed: c.unsealed,
    exit: run.exit,
    rpc_batches: calls.getSignatureStatuses.length,
  };
}

// --- G3: a dead endpoint never looks like a pass ---------------------------
async function g3_dead_endpoint(bin, tag) {
  const { srv, calls } = makeSink('dead');
  const port = await listen(srv);
  const dir = tmpdir(`g3-${tag}`);
  plant(dir, 10);
  const run = await exec(bin, ['seal', '--db', 'reeflog.db', '--rpc', `http://127.0.0.1:${port}`], dir, 180_000);
  const c = counts(dir);
  srv.close();
  fs.rmSync(dir, { recursive: true, force: true });
  return {
    rows: 10,
    sealed: c.finalized,
    still_unsealed: c.unsealed,
    exit: run.exit,
    stdout_has_summary: run.stdout.trim().length > 0,
    stderr: run.stderr.trim().slice(0, 300),
    refused_calls: calls.dead,
  };
}

// --- evaluation -------------------------------------------------------------
// A face is DEFECTIVE on a tree when the honest outcome did not hold.
const evaluate = {
  d1: (r) => r.still_unsealed > 0,                       // backlog survived the run
  d2: (r) => r.sealed < r.pending,                       // rows left unasked
  d3: (r) => r.sealed < r.decidable_tail,                // the tail was never asked
};
const guardOk = {
  g1: (r) => r.sealed === r.pending && r.still_unsealed === 0 && r.exit === 0,
  g2: (r) => r.missing === 0 && r.still_unsealed === r.rows && r.sealed === 0,
  g3: (r) => r.sealed === 0 && r.still_unsealed === r.rows && r.exit !== 0,
};

async function measureTree(bin, tag) {
  const [d1, d2, d3, g1, g2, g3] = [
    await d1_daemon_drain(bin, tag),
    await d2_cli_drain(bin, tag),
    await d3_head_block(bin, tag),
    await g1_small_backlog(bin, tag),
    await g2_two_source(bin, tag),
    await g3_dead_endpoint(bin, tag),
  ];
  return {
    binSha256: sha256(bin),
    binVersion: await binVersion(bin),
    faces: {
      D1_daemon_drain: { ...d1, defect: evaluate.d1(d1) },
      D2_cli_drain: { ...d2, defect: evaluate.d2(d2) },
      D3_head_block: { ...d3, defect: evaluate.d3(d3) },
    },
    guards: {
      G1_one_batch_drains: { ...g1, pass: guardOk.g1(g1) },
      G2_two_source_holds: { ...g2, pass: guardOk.g2(g2) },
      G3_dead_is_not_a_pass: { ...g3, pass: guardOk.g3(g3) },
    },
  };
}

const count = (t, pick) =>
  Object.values(t.faces).filter(pick).length;

async function main() {
  console.error(`baseline: ${BASELINE}`);
  const baseline = await measureTree(BASELINE, 'baseline');
  console.error(`fixed: ${FIXED}`);
  const fixed = await measureTree(FIXED, 'fixed');

  const baselineDefects = count(baseline, (f) => f.defect);
  const fixedDefects = count(fixed, (f) => f.defect);
  const guardsPass = (t) => Object.values(t.guards).every((g) => g.pass);
  const verdict =
    baselineDefects === 3 && fixedDefects === 0 && guardsPass(baseline) && guardsPass(fixed)
      ? 'seal-pass-backlog-drained'
      : 'inconclusive';

  const artifact = {
    tool: 'measure-seal-pass-backlog-drain.mjs',
    question:
      'Can the seal pass drain a backlog larger than one batch, and does a row the chain cannot decide block the rows behind it?',
    defect:
      'The seal pass asked for the OLDEST 150 pending rows each time; the tick compared that window before/after (both clamped to 150) so a backlog of 300+ could never show progress, and the window never moved past undecidable head rows — rows behind a stuck head were never asked while status reported exit 0.',
    measuredAt: new Date().toISOString(),
    verdict,
    baselineDefectFaces: baselineDefects,
    fixedDefectFaces: fixedDefects,
    baselineGuards: guardsPass(baseline) ? 'all pass' : 'FAILED',
    fixedGuards: guardsPass(fixed) ? 'all pass' : 'FAILED',
    trees: {
      baseline: {
        source: 'release binary of the tree at 5550f5b; src/ byte-identical to tag v0.7.41 (git diff v0.7.41..5550f5b -- src/ is empty)',
        binSha256: baseline.binSha256,
        binVersion: baseline.binVersion,
        faces: baseline.faces,
        guards: baseline.guards,
      },
      fixed: {
        source: 'this tree',
        binSha256: fixed.binSha256,
        binVersion: fixed.binVersion,
        faces: fixed.faces,
        guards: fixed.guards,
      },
    },
  };
  fs.mkdirSync(path.dirname(OUT), { recursive: true });
  fs.writeFileSync(OUT, JSON.stringify(artifact, null, 2) + '\n');
  console.log(JSON.stringify({
    verdict,
    baselineDefectFaces: baselineDefects,
    fixedDefectFaces: fixedDefects,
    baselineGuards: artifact.baselineGuards,
    fixedGuards: artifact.fixedGuards,
    out: OUT,
  }, null, 1));
  if (tmpRoot) fs.rmSync(tmpRoot, { recursive: true, force: true });
  process.exit(verdict === 'seal-pass-backlog-drained' ? 0 : 1);
}

main().catch((e) => {
  console.error(e);
  process.exit(2);
});
