#!/usr/bin/env node
// Measure whether the webhook delivery pass stays inside the run (or the
// command) that scheduled it.
//
// THE DEFECT CLASS (PULS BF): PULS BD bounded every signature WALK
// (backfill/repair/seal/reorg) at the run's `--seconds`; the webhook
// delivery pass was never looked at. Per POST the endpoint's own timeout
// applies (default 5 s, config ceiling 1 h), but the PASS has no budget of
// its own — `deliver_events` + `deliver_reorgs` loop over up to
// `--webhook-limit` (default 500) rows synchronously INSIDE the scheduler
// tick, so a run that is N seconds by contract can end at N + backlog x
// per-POST cost: the run loop cannot check its deadline again until the
// pass hands control back.
//
// WHY A LOCAL SINK AND NOT A REAL ENDPOINT: a real endpoint's latency is an
// observation, not a mechanism. The sink answers after an exact delay — the
// pass's own per-row cost is what is measured, everything else instant.
//
// FACES:
//   D1  the daemon `--seconds N --webhook-config` against a delaying sink
//       ends at N + backlog x delay + shutdown budget on the defect carrier,
//       and inside N + one in-flight POST + the shutdown budget on a fixed
//       tree: the delivery pass either outlives the run it was scheduled
//       into or it does not. THE CADENCE MUST GO DUE INSIDE THE RUN: an
//       instrument that passes `--webhook-every 3600` on a short run
//       measures nothing — the tick is never due before the deadline ends
//       (falsified 2026-11-21: the first draft of this instrument could
//       never trigger its own D1; EVERY_S defaults to well under SECONDS).
//   D2  the overrun SCALES WITH THE BACKLOG on the defect carrier: the same
//       delay per POST with 4x the rows costs the run ~4x the overrun. A
//       fixed tree's overrun does not grow with the queue — the pass
//       budget, not the queue size, decides how much of the delivery cost
//       the run pays. (The first draft framed `webhook push`'s wall clock
//       as a defect; that reading is wrong and is now pinned as G3: push is
//       a DRAIN command — its wall clock IS the delivery work it completed,
//       every row answered, exit 0. A stalled endpoint stops push at the
//       FIRST row (transient failure blocks the queue in order), so push
//       never scales with queue x timeout either.)
//   G1  control: a 0-delay endpoint keeps the daemon near its seconds (the
//       false-positive guard — must pass on both sides of any fix);
//   G2  exit voices: the daemon still exits 0 (a stalled/slow webhook
//       endpoint is not a crash), push still exits 0 with every planted row
//       delivered;
//   G3  delivery correctness: the sink received every planted signed body
//       on the slow push AND the fixed daemon delivered at least one body
//       before its budget ran out — slowness must not lose deliveries, and
//       a budget that delivers NOTHING is a defect of its own.
//
// Usage:
//   BIN=<reeflog> OUT=<json> node tools/measure-webhook-pass-overrun.mjs
//   BASELINE=<defect> BIN=<fixed> OUT=<json> node tools/measure-webhook-pass-overrun.mjs
//   DELAY_MS=250 LIMIT=200 SECONDS=30 node tools/measure-webhook-pass-overrun.mjs
//

import net from 'node:net';
import http from 'node:http';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import crypto from 'node:crypto';
import { spawn } from 'node:child_process';
import { DatabaseSync } from 'node:sqlite';

const BIN = process.env.BIN || path.resolve(import.meta.dirname, '..', 'target', 'release', 'reeflog');
const BASELINE = process.env.BASELINE ? path.resolve(process.env.BASELINE) : null;
const OUT = process.env.OUT || null;
const DELAY_MS = Number(process.env.DELAY_MS || 250);
const LIMIT = Number(process.env.LIMIT || 200);
const SECONDS = Number(process.env.SECONDS || 30);
// The webhook cadence must go DUE inside the run, or D1 can never fire
// (falsified: `--webhook-every 3600` on a 120 s run = zero webhook passes).
const EVERY_S = Number(process.env.EVERY_S || Math.max(1, Math.min(5, Math.floor(SECONDS / 6))));
// A second, smaller backlog: D2 compares the overrun at two queue sizes at
// the SAME per-POST delay.
const LIMIT_SMALL = Number(process.env.LIMIT_SMALL || Math.max(1, Math.floor(LIMIT / 4)));
const PROGRAM = 'whirLbMiicVdio4qvUfM5KAg6Ct8VwpYzGff3uctyCc';
const WS_GUID = '258EAFA5-E914-47DA-95CA-C5AB0DC85B11';

for (const [name, p] of [['BIN', BIN], ...(BASELINE ? [['BASELINE', BASELINE]] : [])]) {
  if (!fs.existsSync(p)) {
    console.error(`no reeflog binary at ${name}=${p}`);
    process.exit(2);
  }
}

const sha256 = (p) => crypto.createHash('sha256').update(fs.readFileSync(p)).digest('hex');

function safeJson(s) {
  try { return JSON.parse(s); } catch { return {}; }
}

// One webhook sink: an HTTP server that answers every POST after
// `delayMs`. Records every request so delivery CORRECTNESS is measured
// (bodies arrived, signatures present), not just the wall clock.
function makeSink(delayMs) {
  const seen = [];
  const srv = http.createServer((req, res) => {
    const chunks = [];
    req.on('data', (c) => chunks.push(c));
    req.on('end', () => {
      const raw = Buffer.concat(chunks).toString('utf8');
      const body = safeJson(raw);
      seen.push({
        type: body.type ?? null,
        seq: body.seq ?? null,
        hasSignature: Boolean(req.headers['x-reeflog-signature']),
        at: Date.now(),
      });
      setTimeout(() => {
        res.writeHead(200, { 'content-type': 'application/json' });
        res.end('{"ok":true}');
      }, delayMs);
    });
  });
  return new Promise((res) => {
    srv.listen(0, '127.0.0.1', () => res({ srv, port: srv.address().port, seen }));
  });
}

// RPC + WS sink: the daemon needs a live socket to stay in its ingest
// loop; every RPC answer is instant (the webhook endpoint is the only
// thing that is slow — the defect is isolated to one pass, not the
// network).
function makeRpcSink() {
  let seq = 0;
  const calls = {};
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
      try { v = JSON.parse(body); } catch { return; }
      const m = v?.method || '';
      calls[m] = (calls[m] || 0) + 1;
      const answer = (payload) => {
        const r = Buffer.from(JSON.stringify({ jsonrpc: '2.0', id: v.id, result: payload }));
        s.write(`HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: ${r.length}\r\n\r\n`);
        s.write(r);
      };
      if (m === 'getSignaturesForAddress') answer([]);   // empty page: no walk pages
      else if (m === 'getSignatureStatuses') answer([]);
      else answer(null);
    });
  });
  return new Promise((res) => {
    srv.listen(0, '127.0.0.1', () => res({ srv, port: srv.address().port, calls }));
  });
}

// One journal with an n-row webhook backlog: the journal is created by
// `reeflog init` (the program is recorded, the full schema exists) and the
// queue is then planted DIRECTLY (the queue is the thing under test; the
// endpoint type does not matter).
async function makeJournal(bin, dir, n) {
  const db = path.join(dir, 'j.db');
  await new Promise((res, rej) => {
    const c = spawn(bin, ['init', '--db', db, '--program', PROGRAM], { stdio: 'ignore' });
    c.on('close', (code) => (code === 0 ? res() : rej(new Error(`init exit ${code}`))));
  });
  const jdb = new DatabaseSync(db);
  const ins = jdb.prepare(
    `INSERT INTO events(signature,slot,logs,source,first_seen,seal_status,seal_seen,payload_state) VALUES(?1,?2,?3,'ws',?4,'finalized',1,NULL)`
  );
  for (let i = 0; i < n; i += 1) {
    ins.run(
      `OvrFixture${String(i).padStart(6, '0')}111111111111111111111111111`,
      46000001 - i,
      JSON.stringify(['Program whirLbMiicVdio4qvUfM5KAg6Ct8VwpYzGff3uctyCc invoke [1]']),
      1700000000 + i
    );
  }
  jdb.close();
  return db;
}

async function run(cmd, args, env, killAfterMs) {
  return await new Promise((res) => {
    const c = spawn(cmd, args, { env });
    let so = '', se = '';
    let killed = false;
    const timer = killAfterMs
      ? setTimeout(() => { killed = true; c.kill('SIGKILL'); }, killAfterMs)
      : null;
    c.stdout.on('data', (d) => (so += d));
    c.stderr.on('data', (d) => (se += d));
    c.on('close', (code) => {
      if (timer) clearTimeout(timer);
      res({ code, so, se, killed });
    });
  });
}

// One side of the instrument: the same questions, the same sinks, one
// binary, FIVE runs:
//   1. daemon SLOW (backlog n, per-POST delay)          — D1, G2
//   2. daemon CONTROL (same backlog, 0-delay sink)      — G1
//   3. daemon SMALL (backlog n/4, same delay)           — D2
//   4. push SLOW (fresh copy of backlog n)              — G3
//   5. push SMALL (fresh copy of backlog n/4)           — G3
async function measureSide(bin, label) {
  const rpc = await makeRpcSink();
  const killMs = (SECONDS + Math.ceil((LIMIT * DELAY_MS) / 1000) + 90) * 1000;

  const daemonRun = async (n, delayMs, tag) => {
    const dir = fs.mkdtempSync(path.join(os.tmpdir(), `hook-overrun-${tag}-`));
    const db = await makeJournal(bin, dir, n);
    const sink = await makeSink(delayMs);
    const cfgPath = path.join(dir, 'webhooks.json');
    fs.writeFileSync(cfgPath, JSON.stringify({ targets: [{ id: 'probe', url: `http://127.0.0.1:${sink.port}/hook`, secret: 'whsec_overrun_probe', timeout_ms: 5000 }] }));
    const args = ['start', '--db', db, '--seconds', String(SECONDS), '--ws', `ws://127.0.0.1:${rpc.port}`, '--rpc', `http://127.0.0.1:${rpc.port}`, '--webhook-config', cfgPath, '--webhook-every', String(EVERY_S), '--webhook-limit', String(n)];
    const t0 = Date.now();
    const r = await run(bin, args, { ...process.env }, killMs);
    const ms = Date.now() - t0;
    sink.srv.close();
    const summary = safeJson(r.so);
    const wh = summary.webhook || {};
    fs.rmSync(dir, { recursive: true, force: true });
    return {
      ms, exit: r.code, killed: r.killed,
      // Fields a pre-fix binary simply does not carry are recorded as
      // 'missing' — never as 0, which would read as "nothing owed".
      delivered: wh.delivered ?? 'missing',
      budgetStops: wh.budget_stops ?? 'missing',
      rowsOwed: wh.rows_owed ?? 'missing',
      stderrTail: r.se.slice(-300),
    };
  };

  const pushRun = async (n, delayMs, tag) => {
    const dir = fs.mkdtempSync(path.join(os.tmpdir(), `hook-overrun-push-${tag}-`));
    const db = await makeJournal(bin, dir, n);
    const sink = await makeSink(delayMs);
    const cfgPath = path.join(dir, 'webhooks.json');
    fs.writeFileSync(cfgPath, JSON.stringify({ targets: [{ id: 'probe', url: `http://127.0.0.1:${sink.port}/hook`, secret: 'whsec_overrun_probe', timeout_ms: 5000 }] }));
    const t0 = Date.now();
    const r = await run(bin, ['webhook', 'push', '--db', db, '--config', cfgPath, '--limit', String(n)], { ...process.env }, killMs);
    const ms = Date.now() - t0;
    const bodies = sink.seen.length;
    sink.srv.close();
    fs.rmSync(dir, { recursive: true, force: true });
    return { ms, exit: r.code, killed: r.killed, deliveredBodies: bodies };
  };

  const daemonSlow = await daemonRun(LIMIT, DELAY_MS, 'slow');
  const daemonFast = await daemonRun(LIMIT, 0, 'control');
  const daemonSmall = await daemonRun(LIMIT_SMALL, DELAY_MS, 'small');
  const pushSlow = await pushRun(LIMIT, DELAY_MS, 'slow');
  const pushSmall = await pushRun(LIMIT_SMALL, DELAY_MS, 'small');
  rpc.srv.close();

  return {
    label,
    binVersion: await binVersionOf(bin),
    binSha256: sha256(bin),
    daemonSlow,
    daemonFast,
    daemonSmall,
    pushSlow,
    pushSmall,
  };
}

async function binVersionOf(bin) {
  return await new Promise((res) => {
    const c = spawn(bin, ['--version']);
    let o = '';
    c.stdout.on('data', (d) => (o += d));
    c.on('close', () => res(o.trim()));
  });
}

const fixedSide = await measureSide(BIN, 'fixed');

let baselineSide = null;
if (BASELINE) {
  baselineSide = await measureSide(BASELINE, 'baseline');
  console.log(
    `baseline ${baselineSide.binVersion} (${baselineSide.binSha256.slice(0, 8)}) vs fixed ${fixedSide.binVersion} (${fixedSide.binSha256.slice(0, 8)})`
  );
}

const faces = [];
const record = (name, ok, detail) => {
  faces.push({ name, ok, detail });
  console.log(`${ok ? 'PASS' : 'FAIL'} ${name} — ${detail}`);
};

const budgetMs = Math.min(30, Math.max(2, SECONDS / 5)) * 1000;
// The honest overshoot after a budgeted pass: the run's shutdown budget,
// plus the one POST that was IN FLIGHT when the budget expired (the sink's
// own delay here; the endpoint's timeout_ms is the ceiling in production),
// plus process exit slack.
const honestTail = budgetMs + DELAY_MS + 5000;
const overrun = (side) => side.daemonSlow.ms - SECONDS * 1000;
const overrunSmall = (side) => side.daemonSmall.ms - SECONDS * 1000;
// How much of the queue's own cost (rows x delay) the run actually paid.
const expectedGrowth = (LIMIT - LIMIT_SMALL) * DELAY_MS;

const fD1 = baselineSide
  ? `D1_the_webhook_pass_outlives_the_run_deadline_on_the_carrier_and_stays_in_budget_on_the_fixed_side`
  : 'D1_the_webhook_pass_outlives_the_run_deadline';
if (baselineSide) {
  const bOver = overrun(baselineSide);
  const fOver = overrun(fixedSide);
  record(
    fD1,
    bOver > DELAY_MS * 4 && fOver <= honestTail,
    `baseline overshoot ${Math.round(bOver / 1000)}s vs fixed overshoot ${Math.round(fOver / 1000)}s (honest tail = shutdown budget ${Math.round(budgetMs / 1000)}s + one in-flight POST ${DELAY_MS}ms + exit slack) at ${DELAY_MS}ms/POST on a ${LIMIT}-row backlog, --seconds ${SECONDS}, --webhook-every ${EVERY_S}`
  );
} else {
  const over = overrun(fixedSide);
  record(
    fD1,
    over > DELAY_MS * 4,
    `--seconds ${SECONDS} against a ${DELAY_MS}ms/POST sink with a ${LIMIT}-row backlog: ${Math.round(fixedSide.daemonSlow.ms / 1000)}s wall (overshoot ${Math.round(over / 1000)}s), exit ${fixedSide.daemonSlow.exit}, --webhook-every ${EVERY_S}`
  );
}

// D2 — the overrun scales with the QUEUE at a fixed per-POST cost on the
// defect carrier; on a fixed tree it must not (both queue sizes stay inside
// the same honest tail).
const baseGrowth = baselineSide ? overrun(baselineSide) - overrunSmall(baselineSide) : overrun(fixedSide) - overrunSmall(fixedSide);
const fixedGrowth = baselineSide ? overrun(fixedSide) - overrunSmall(fixedSide) : null;
if (baselineSide) {
  record(
    'D2_the_overrun_scales_with_the_backlog_on_the_carrier_and_not_on_the_fixed_side',
    baseGrowth >= expectedGrowth * 0.4 && fixedGrowth !== null && Math.abs(fixedGrowth) <= honestTail,
    `same ${DELAY_MS}ms/POST, ${LIMIT} vs ${LIMIT_SMALL} rows: baseline overrun grows by ${Math.round(baseGrowth / 1000)}s (queue expects ${Math.round(expectedGrowth / 1000)}s) — fixed grows by ${Math.round(fixedGrowth / 1000)}s (≤ honest tail ${Math.round(honestTail / 1000)}s)`
  );
} else {
  record(
    'D2_the_overrun_scales_with_the_backlog',
    baseGrowth >= expectedGrowth * 0.4,
    `same ${DELAY_MS}ms/POST, ${LIMIT} vs ${LIMIT_SMALL} rows: overrun grows by ${Math.round(baseGrowth / 1000)}s for ${expectedGrowth}ms of queue cost expected`
  );
}

// G1 — the control: a 0-delay endpoint keeps the daemon near its seconds.
// The ONLY face expected to pass identically on both sides.
record(
  'G1_control_fast_endpoint_keeps_the_daemon_near_its_seconds',
  fixedSide.daemonFast.ms < (SECONDS + 15) * 1000 &&
    (!baselineSide || baselineSide.daemonFast.ms < (SECONDS + 15) * 1000),
  `--seconds ${SECONDS} against a 0-delay endpoint: fixed ${Math.round(fixedSide.daemonFast.ms / 1000)}s${baselineSide ? `, baseline ${Math.round(baselineSide.daemonFast.ms / 1000)}s` : ''} (exit ${fixedSide.daemonFast.exit})`
);

// G2 — exit voices: a slow webhook endpoint is not a crash (daemon 0),
// and push answers every attempt (0 with every planted row delivered).
record(
  'G2_daemon_exits_zero_on_a_slow_endpoint',
  fixedSide.daemonSlow.exit === 0 && (!baselineSide || baselineSide.daemonSlow.exit === 0),
  `exit ${fixedSide.daemonSlow.exit}${baselineSide ? ` / baseline ${baselineSide.daemonSlow.exit}` : ''}, killed=${fixedSide.daemonSlow.killed}`
);
record(
  'G3_push_drains_every_planted_row_and_exits_zero',
  fixedSide.pushSlow.exit === 0 && fixedSide.pushSlow.deliveredBodies === LIMIT &&
    fixedSide.pushSmall.deliveredBodies === LIMIT_SMALL &&
    (!baselineSide || (baselineSide.pushSlow.deliveredBodies === LIMIT && baselineSide.pushSmall.deliveredBodies === LIMIT_SMALL)),
  `push wall ${Math.round(fixedSide.pushSlow.ms / 1000)}s for ${LIMIT} rows = ${Math.round(fixedSide.pushSlow.ms / LIMIT)}ms/row at ${DELAY_MS}ms/POST (the work itself); bodies ${fixedSide.pushSlow.deliveredBodies}/${LIMIT} + ${fixedSide.pushSmall.deliveredBodies}/${LIMIT_SMALL} (baseline ${baselineSide ? `${baselineSide.pushSlow.deliveredBodies}/${LIMIT}` : 'n/a'}), exit ${fixedSide.pushSlow.exit}`
);
// G4 — the budget must not starve delivery: the fixed daemon delivered at
// least one body before its budget ran out, and (when both sides run) the
// carrier delivered the whole backlog it was handed.
record(
  'G4_the_budget_does_not_starve_delivery',
  fixedSide.daemonSlow.delivered !== 'missing' && Number(fixedSide.daemonSlow.delivered) >= 1 &&
    (!baselineSide || Number(baselineSide.daemonSlow.delivered) >= LIMIT),
  `daemon delivered: fixed ${fixedSide.daemonSlow.delivered}, budget_stops=${fixedSide.daemonSlow.budgetStops}, rows_owed=${fixedSide.daemonSlow.rowsOwed}${baselineSide ? ` — baseline delivered ${baselineSide.daemonSlow.delivered} (carrier pays the whole queue)` : ''}`
);

const verdict = baselineSide
  ? overrun(baselineSide) > DELAY_MS * 4 && overrun(fixedSide) <= honestTail &&
      (overrun(baselineSide) - overrunSmall(baselineSide)) >= expectedGrowth * 0.4 &&
      Math.abs(overrun(fixedSide) - overrunSmall(fixedSide)) <= honestTail
    ? 'webhook-pass-bounded-by-the-pass-budget'
    : 'webhook-pass-overruns-the-run'
  : overrun(fixedSide) > DELAY_MS * 4
    ? 'webhook-pass-overruns-the-run'
    : 'webhook-pass-bounded-by-the-pass-budget';

const report = {
  tool: 'measure-webhook-pass-overrun.mjs',
  question: 'Does the webhook delivery pass stay inside the run (or command) that scheduled it?',
  defect:
    'The webhook pass is bounded per POST by the target timeout_ms but has NO pass budget: deliver_events + deliver_reorgs loop over up to --webhook-limit (default 500) rows synchronously inside the scheduler tick, so the run loop cannot check its deadline until the pass hands control back — the run pays backlog x per-POST cost past its --seconds. Every signature walk was deadline-bounded in PULS BD; the webhook pass was the one scheduler consumer left. `webhook push` is a DRAIN command: its wall clock is the delivery work it completed (G3), and a stalled endpoint stops it at the first row — that is not this defect.',
  measuredAt: new Date().toISOString(),
  binVersion: fixedSide.binVersion,
  binSha256: fixedSide.binSha256,
  baseline: baselineSide ? { binVersion: baselineSide.binVersion, binSha256: baselineSide.binSha256 } : null,
  seconds: SECONDS,
  webhookEveryS: EVERY_S,
  delayMsPerPost: DELAY_MS,
  limit: LIMIT,
  limitSmall: LIMIT_SMALL,
  honestTailMs: honestTail,
  faces,
  runs: fixedSide,
  baselineRuns: baselineSide,
  verdict,
};

if (OUT) {
  fs.mkdirSync(path.dirname(path.resolve(OUT)), { recursive: true });
  fs.writeFileSync(path.resolve(OUT), `${JSON.stringify(report, null, 2)}\n`);
}
console.log(JSON.stringify({ verdict: report.verdict, out: OUT || '<stdout>' }, null, 2));
