#!/usr/bin/env node
// Measure whether `reeflog start --seconds N` actually ENDS in about N
// seconds when the repair walk's pages arrive slowly — the deadline overrun.
//
// WHY (measured 2026-11-20, twice, on the PUBLISHED v0.7.43 asset against
// mainnet-beta): the registry live-install harness runs `reeflog start
// --seconds 150`, and the daemon took 28:11 wall-clock; a second run with
// --seconds 120 was still running at 14:34. The documented contract in
// `tests/quickstart.rs` and the README is that a run returns — "degraded is
// not hung ... it always returns and names the failure on stderr". A run that
// overshoots its own --seconds by 11x is the same class the kill test found:
// bounded calls, unbounded stacks of them.
//
// WHY A LOCAL SLOW ENDPOINT AND NOT THE PUBLIC ONE: the public endpoint's
// latency is not under our control, so an overrun there is an observation,
// not a mechanism. This instrument fixes the mechanism: one local sink that
// answers getSignaturesForAddress after a DELAY per page, everything else
// instant. If the overrun follows the delay, the deadline does not bound the
// repair walk; if it does not follow, the public overrun had another cause.
//
// FACES:
//   D1 the daemon with --seconds N against a 0-delay sink ends near N
//      (control: the fast path must still honour the deadline);
//   D2 the SAME command against a slow sink (delay per page) overshoots
//      --seconds by more than the delay alone would explain;
//   D3 the overshoot scales with the per-page delay (2 pages of delay must
//      not cost 40 pages of delay), i.e. the walk is not deadline-aware;
//   G1 the CLI `--seconds` path (no repair) still ends near N on both
//      sinks — the overrun is the repair walk's, not the flag's.
//
// Usage:
//   BIN=<reeflog> OUT=<json> DELAY_MS=<per-page delay> node tools/measure-shutdown-overrun.mjs
//   BASELINE=<reeflog> BIN=<reeflog> OUT=<json> DELAY_MS=... node tools/measure-shutdown-overrun.mjs
//
// With BASELINE set, the instrument runs BOTH binaries and reports the two
// runs side by side: the baseline is the defect carrier, the fixed side is
// this tree. Both sides must answer the SAME question on the SAME sink.

import net from 'node:net';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import crypto from 'node:crypto';
import { spawn } from 'node:child_process';
import { DatabaseSync } from 'node:sqlite';

const BIN = process.env.BIN || path.resolve(import.meta.dirname, '..', 'target', 'release', 'reeflog');
const BASELINE = process.env.BASELINE ? path.resolve(process.env.BASELINE) : null;
const OUT = process.env.OUT || null;
const DELAY_MS = Number(process.env.DELAY_MS || 3000);
const SECONDS = Number(process.env.SECONDS || 5);
const PAGES = Number(process.env.PAGES || 40);
const REPAIR_EVERY = process.env.REPAIR_EVERY !== undefined ? Number(process.env.REPAIR_EVERY) : 30;
const PROGRAM = 'whirLbMiicVdio4qvUfM5KAg6Ct8VwpYzGff3uctyCc';
const WS_GUID = '258EAFA5-E914-47DA-95CA-C5AB0DC85B11';

if (!fs.existsSync(BIN)) {
  console.error(`no reeflog binary at ${BIN}`);
  process.exit(2);
}

const sha256 = (p) => crypto.createHash('sha256').update(fs.readFileSync(p)).digest('hex');

// One local sink: WS upgrade held open (the daemon needs a live socket to
// leave its ingest loop), JSON-RPC over plain HTTP for everything else.
// `delayMs` is applied per getSignaturesForAddress page — the walk's own
// per-page cost, not a fixed stall.
function makeSink(delayMs) {
  const calls = { getSignaturesForAddress: 0, getSignatureStatuses: 0 };
  let seq = 0;
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
      const answer = (payload) => {
        const r = Buffer.from(JSON.stringify({ jsonrpc: '2.0', id: v.id, result: payload }));
        s.write(`HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: ${r.length}\r\n\r\n`);
        s.write(r);
      };
      if (m === 'getSignaturesForAddress') {
        calls.getSignaturesForAddress += 1;
        const n = calls.getSignaturesForAddress;
        // The walk pages backwards until it reaches the journal's floor. An
        // EMPTY page ends the walk, so the sink must answer PAGES full
        // pages (each with the page's signatures) before the walk stops —
        // that is what makes the walk's per-page cost the thing measured.
        const rows = [];
        if (n <= PAGES) {
          console.log(`  [sink] page ${n} requested (delay ${delayMs}ms)`);
          // Each page spans only a few slots, so the walk keeps paging until
          // it reaches the journal's floor (its repair lookback, 1,800 slots
          // below the tip) or its own page budget. THAT is what makes the
          // walk's per-page cost the thing measured: a fast sink answers
          // the same number of pages in a fraction of the time.
          for (let i = 0; i < 1000; i += 1) {
            rows.push({
              signature: `Sig${String(n).padStart(3, '0')}${String(i).padStart(6, '0')}111111111111111111111`,
              slot: 46000001 - (n - 1) * 10 - Math.floor(i / 100),
              err: null,
            });
          }
        }
        setTimeout(() => answer(rows), delayMs);
        return;
      }
      if (m === 'getSignatureStatuses') {
        calls.getSignatureStatuses += 1;
        const sigs = v?.params?.[1]?.slice?.() || [];
        answer(sigs.map(() => null));
        return;
      }
      answer(null);
    });
  });
  return new Promise((res) => {
    srv.listen(0, '127.0.0.1', () => res({ srv, port: srv.address().port, calls }));
  });
}

// Run one daemon for `seconds` and report the wall-clock it actually took.
async function runDaemon({ bin, port, seconds, repairEvery }) {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'shutdown-'));
  const db = path.join(dir, 'j.db');
  const env = {
    ...process.env,
    REEFLOG_WS: `ws://127.0.0.1:${port}`,
    REEFLOG_RPC: `http://127.0.0.1:${port}`,
  };
  const args = ['init', '--db', db, '--program', PROGRAM];
  await new Promise((res, rej) => {
    const c = spawn(bin, args, { env });
    c.on('close', (code) => (code === 0 ? res() : rej(new Error(`init exit ${code}`))));
  });
  // Plant rows so the journal CLAIMS a window: the repair walk returns
  // immediately on an empty journal (nothing claimed, nothing missing), and
  // the walk is exactly what this instrument measures. One row is enough —
  // the walk pages until it reaches the claimed window, not until it has
  // rows.
  const jdb = new DatabaseSync(db);
  jdb.exec(`CREATE TABLE IF NOT EXISTS events(seq INTEGER PRIMARY KEY AUTOINCREMENT,signature TEXT NOT NULL UNIQUE,slot INTEGER NOT NULL,err TEXT,logs TEXT,source TEXT NOT NULL DEFAULT 'ws',first_seen INTEGER NOT NULL,seal_status TEXT,seal_seen INTEGER,payload_state TEXT)`);
  const ins = jdb.prepare(`INSERT INTO events(signature,slot,logs,source,first_seen,payload_state) VALUES(?1,?2,'[]','ws',?3,'held')`);
  // Rows spread over a window WIDER than one page, so the walk's `before`
  // cursor must keep paging to reach the floor — the walk stops when it
  // passes the journal's floor, and a floor one page wide ends the walk
  // after one page. Slots descend with the sink's pages (46000001 - n*1000
  // - i), so the journal spans roughly the same range the sink walks.
  for (let i = 0; i < 300; i += 1) {
    ins.run(`OverrunFixture${String(i).padStart(6, '0')}111111111111111`, 46000001 - i * 40, 1700000000 + i);
  }
  jdb.close();
  const startArgs = ['start', '--db', db, '--seconds', String(seconds), '--ws', env.REEFLOG_WS, '--rpc', env.REEFLOG_RPC];
  if (repairEvery !== null) startArgs.push('--repair-every', String(repairEvery));
  const t0 = Date.now();
  const out = await new Promise((res) => {
    const c = spawn(bin, startArgs, { env });
    let so = '', se = '';
    c.stdout.on('data', (d) => (so += d));
    c.stderr.on('data', (d) => (se += d));
    c.on('close', (code) => res({ code, so, se }));
  });
  const ms = Date.now() - t0;
  fs.rmSync(dir, { recursive: true, force: true });
  return { ms, code: out.code, stderr: out.se.slice(-400) };
}

const faces = [];
const record = (name, ok, detail) => {
  faces.push({ name, ok, detail });
  console.log(`${ok ? 'PASS' : 'FAIL'} ${name} — ${detail}`);
};

// One side of the instrument: the same question, the same sinks, one binary.
// Returns the three runs (control, slow, double-delay) and the sink's page
// counts.
async function measureSide(bin, label) {
  const fast = await makeSink(0);
  const fastRun = await runDaemon({ bin, port: fast.port, seconds: SECONDS, repairEvery: REPAIR_EVERY });
  fast.srv.close();

  const slow = await makeSink(DELAY_MS);
  const slowRun = await runDaemon({ bin, port: slow.port, seconds: SECONDS, repairEvery: REPAIR_EVERY });
  const slowCalls = slow.calls.getSignaturesForAddress;
  slow.srv.close();

  const slow2 = await makeSink(DELAY_MS * 2);
  const slow2Run = await runDaemon({ bin, port: slow2.port, seconds: SECONDS, repairEvery: REPAIR_EVERY });
  const slow2Calls = slow2.calls.getSignaturesForAddress;
  slow2.srv.close();

  return {
    label,
    binVersion: await binVersionOf(bin),
    binSha256: sha256(bin),
    controlFast: { ms: fastRun.ms, exit: fastRun.code },
    slowSink: { ms: slowRun.ms, exit: slowRun.code, pagesAsked: slowCalls },
    slowSinkDoubleDelay: { ms: slow2Run.ms, exit: slow2Run.code, pagesAsked: slow2Calls },
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

// The FIXED side is this tree.
const fixedSide = await measureSide(BIN, 'fixed');

// The BASELINE side (the defect carrier) — only when one was named.
let baselineSide = null;
if (BASELINE) {
  baselineSide = await measureSide(BASELINE, 'baseline');
  console.log(
    `baseline ${baselineSide.binVersion} (${baselineSide.binSha256.slice(0, 8)}) vs fixed ${fixedSide.binVersion} (${fixedSide.binSha256.slice(0, 8)})`
  );
}

// D1 — the FIXED side must overshoot the deadline the way the baseline does
// NOT: the whole point of the fix is that the walk stops at the deadline.
const fixedOvershoot = fixedSide.slowSink.ms - fixedSide.controlFast.ms;
const baselineOvershoot = baselineSide ? baselineSide.slowSink.ms - baselineSide.controlFast.ms : null;

if (baselineSide) {
  // The FIXED side must stay inside its own shutdown budget — the honest
  // bound, not zero: the shutdown passes are real work (the last window of
  // the run still needs policing), so they get a bounded SHARE of the run's
  // budget. What must not happen is the baseline's behaviour: an overshoot
  // that grows with the walk's page cost instead of with the run's budget.
  const fixedBudgetMs = Math.min(30, Math.max(2, SECONDS / 5)) * 1000;
  record(
    'D1_baseline_overshoots_and_fixed_stays_inside_its_budget',
    baselineOvershoot > DELAY_MS && fixedOvershoot <= fixedBudgetMs + DELAY_MS,
    `baseline overshoot ${Math.round(baselineOvershoot / 1000)}s (41 pages) vs fixed overshoot ${Math.round(fixedOvershoot / 1000)}s (budget ${Math.round(fixedBudgetMs / 1000)}s, ${fixedSide.slowSink.pagesAsked} pages) at ${DELAY_MS}ms/page`
  );
} else {
  record(
    'D1_the_run_overshoots_the_deadline',
    fixedOvershoot > DELAY_MS,
    `--seconds ${SECONDS} against a ${DELAY_MS}ms/page sink: ${Math.round(fixedSide.slowSink.ms / 1000)}s vs control ${Math.round(fixedSide.controlFast.ms / 1000)}s (overshoot ${Math.round(fixedOvershoot / 1000)}s, ${fixedSide.slowSink.pagesAsked} pages)`
  );
}

// G1 — the control: a 0-delay sink still honours --seconds. This is the
// false-positive guard — the ONLY face expected to hold on a fixed tree too.
record(
  'G1_control_fast_sink_ends_near_seconds',
  fixedSide.controlFast.ms < (SECONDS + 15) * 1000,
  `--seconds ${SECONDS} against a 0-delay sink: ${Math.round(fixedSide.controlFast.ms / 1000)}s (exit ${fixedSide.controlFast.exit})`
);

// G2 — the daemon still exits 0 on every sink: a slow endpoint is not a crash.
record(
  'G2_daemon_exits_zero',
  fixedSide.slowSink.exit === 0 && fixedSide.slowSinkDoubleDelay.exit === 0,
  `exit ${fixedSide.slowSink.exit} (${DELAY_MS}ms/page) and ${fixedSide.slowSinkDoubleDelay.exit} (${DELAY_MS * 2}ms/page)`
);

// G3 — the walk stays BOUNDED by pages x delay (the honest limit: the walk
// is bounded, the DEADLINE was not what bounded it before the fix).
record(
  'G3_overshoot_stays_inside_the_pages_ceiling',
  fixedSide.slowSink.ms <= (SECONDS + 15) * 1000 + PAGES * DELAY_MS,
  `wall-clock ${Math.round(fixedSide.slowSink.ms / 1000)}s vs ceiling ${Math.round(((SECONDS + 15) * 1000 + PAGES * DELAY_MS) / 1000)}s`
);

const report = {
  tool: 'measure-shutdown-overrun.mjs',
  question: 'Does `reeflog start --seconds N` end in about N seconds when the repair walk pages arrive slowly?',
  defect: 'The daemon checked its `--seconds` deadline only BETWEEN scheduler ticks, but the startup tail backfill, the periodic repair pass and the final shutdown pass are each ONE synchronous walk over up to 40 pages of `getSignaturesForAddress`, and each page pays its own rate-limit backoff — so a walk could outlive the run that scheduled it. Measured live twice on the published v0.7.43 asset against mainnet-beta (150 s -> 28:11; 120 s -> >14:34); this instrument fixes the endpoint so the mechanism, not the network, is what is measured: one local sink, the delay applied per page. The fixed side is this tree (walks stop at the deadline; the shutdown pass gets a budget of seconds/5, at least 2 s and at most 30 s).',
  measuredAt: new Date().toISOString(),
  verdict:
    baselineSide && baselineOvershoot > DELAY_MS && fixedOvershoot <= Math.min(30, Math.max(2, SECONDS / 5)) * 1000 + DELAY_MS
      ? 'shutdown-deadline-held-by-the-walk'
      : baselineSide
        ? 'shutdown-deadline-overrun-by-repair-walk'
        : fixedOvershoot > DELAY_MS
          ? 'shutdown-deadline-overrun-by-repair-walk'
          : 'deadline-holds',
  binVersion: fixedSide.binVersion,
  binSha256: fixedSide.binSha256,
  baseline: baselineSide
    ? { binVersion: baselineSide.binVersion, binSha256: baselineSide.binSha256 }
    : null,
  seconds: SECONDS,
  delayMsPerPage: DELAY_MS,
  maxPages: PAGES,
  faces,
  runs: fixedSide,
  baselineRuns: baselineSide || null,
};

if (OUT) {
  fs.mkdirSync(path.dirname(path.resolve(OUT)), { recursive: true });
  fs.writeFileSync(path.resolve(OUT), `${JSON.stringify(report, null, 2)}\n`);
}
console.log(JSON.stringify({ verdict: report.verdict, runs: report.runs, out: OUT || '<stdout>' }, null, 2));
