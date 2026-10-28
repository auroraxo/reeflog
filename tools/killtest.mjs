#!/usr/bin/env node
// killtest.mjs — compressed falsification of reeflog's delivery contract.
//
// RFC (openclaw-citizen/visionaer/rfcs/2026-10-23-reeflog.md, section 6):
//
//   "Aurora abonniert ein Programm eine Woche mit reeflog, schiesst den
//    Daemon jeden Tag fuer 30 Minuten ab, und das Vergleichs-Eigenbau-Skript
//    laeuft parallel gegen denselben Ausschnitt. Bedingung: reeflog liefert
//    am Ende jede signierte Transaktion des Programms genau einmal (at-least-
//    once, Dedup nachweisbar, keine Lueckenmeldung); das Skript verliert
//    mindestens eine. Gelingt das nicht, ist der Zustellvertrag falsch
//    konstruiert und die These tot."
//
// This harness compresses that week into one run and drives THREE consumers
// against the same program at the same time:
//
//   1. reeflog      — SIGKILLed every --kill-every seconds for --downtime
//                     seconds; on restart it backfills its tail.
//   2. naive-killed — the typical hand-rolled WS consumer (logsSubscribe,
//                     reconnect, append-only file, no cursor, no backfill),
//                     killed on the SAME schedule.
//   3. naive-live   — the same hand-rolled consumer, never killed. It is the
//                     control that separates "the public WS lost it" from
//                     "the kill made us lose it".
//
// Ground truth is fetched independently from getSignaturesForAddress over the
// exact slot window [slot0, slotEnd] (both sampled with commitment
// 'confirmed' while all consumers are up).
//
// IMPORTANT MEASUREMENT SEMANTICS: truth = every signature the PROGRAM
// ADDRESS shows in the window. logsSubscribe delivers only txs that INVOKE
// the program. So a truth signature whose tx merely references the program as
// an account key can never arrive on a log stream — it is a truth-set
// artifact, not a loss. Every miss is therefore classified via getTransaction:
//   invoked        — the program's logs appear in the tx  => a REAL loss
//   referenced-only— the program is only an account key   => not deliverable
//   unavailable    — could not be fetched                 => treated as loss
//
// Usage:
//   node tools/killtest.mjs [--program <id>] [--seconds 300]
//        [--kill-every 40] [--downtime 4] [--out /tmp/reeflog-killtest]
//        [--bin ./target/release/reeflog] [--label run1]
//
// Exit 0 = the delivery contract survived every check; 1 = falsified;
// 2 = inconclusive (harness could not run).

import { spawn } from 'node:child_process';
import fs from 'node:fs';
import path from 'node:path';
import process from 'node:process';

// ---------------------------------------------------------------------------
// arguments
// ---------------------------------------------------------------------------
const argv = process.argv.slice(2);
function opt(name, dflt) {
  const i = argv.indexOf(name);
  return i >= 0 && argv[i + 1] ? argv[i + 1] : dflt;
}
const PROGRAM = opt('--program', '675kPX9MHTjS2zt1qfr1NYHuzeLXfQM9H24wFSUt1Mp8'); // Raydium AMM v4
const TOTAL_S = Number(opt('--seconds', 300));
const KILL_EVERY_S = Number(opt('--kill-every', 40));
const DOWNTIME_S = Number(opt('--downtime', 4));
const OUT = opt('--out', '/tmp/reeflog-killtest');
const LABEL = opt('--label', 'run');
const BIN = path.resolve(opt('--bin', './target/release/reeflog'));
const GRACE_S = Number(opt('--grace', 20)); // consumers stay up this long after slotEnd
const RPC = process.env.SOLANA_RPC || 'https://api.mainnet-beta.solana.com';
const WS = process.env.SOLANA_WS || 'wss://api.mainnet-beta.solana.com';
const SELF = path.resolve(new URL(import.meta.url).pathname);

const REEFLOG_DB = path.join(OUT, 'reeflog.db');
const NAIVE_KILLED_FILE = path.join(OUT, 'naive-killed.jsonl');
const NAIVE_LIVE_FILE = path.join(OUT, 'naive-live.jsonl');
const REPORT_PATH = path.join(OUT, `killtest-${LABEL}.json`);

function writeReport(report) {
  try { fs.writeFileSync(REPORT_PATH, JSON.stringify(report, null, 2)); }
  catch (e) { console.error('report write failed:', e); }
}

const log = (...a) => console.log(new Date().toISOString().slice(11, 19), ...a);
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

// ---------------------------------------------------------------------------
// the naive consumer (also runs as a child process with --role)
// ---------------------------------------------------------------------------
if (argv.includes('--role')) {
  await naiveConsumerMain(opt('--role', 'live'));
  process.exit(0);
}

async function naiveConsumerMain(role) {
  const file = role === 'live' ? NAIVE_LIVE_FILE : NAIVE_KILLED_FILE;
  const seen = readSeen(file); // restart keeps history but never backfills
  let reported = false;
  for (;;) {
    let ws;
    try {
      ws = new WebSocket(WS);
    } catch {
      await sleep(1500);
      continue;
    }
    await new Promise((resolve) => {
      const done = () => { clearTimeout(timer); resolve(); };
      const timer = setTimeout(done, 60 * 60 * 1000);
      ws.onopen = () => ws.send(JSON.stringify({
        jsonrpc: '2.0', id: 1, method: 'logsSubscribe',
        params: [{ mentions: [PROGRAM] }, { commitment: 'confirmed' }],
      }));
      ws.onmessage = (ev) => {
        if (!reported) { reported = true; console.log('READY'); }
        let v;
        try { v = JSON.parse(ev.data); } catch { return; }
        if (v.method !== 'logsNotification') return;
        const sig = v.params?.result?.value?.signature;
        const slot = v.params?.result?.context?.slot;
        if (!sig || seen.has(sig)) return;
        seen.add(sig);
        fs.appendFileSync(file, JSON.stringify({ signature: sig, slot, t: Date.now() }) + '\n');
      };
      ws.onclose = done;
      ws.onerror = () => { try { ws.close(); } catch { /* already closed */ } };
    });
    await sleep(1500); // typical reconnect backoff
  }
}

// ---------------------------------------------------------------------------
// RPC helpers (ground truth; patient retry on 429)
// ---------------------------------------------------------------------------
async function rpc(method, params, tries = 8) {
  let last;
  for (let i = 0; i < tries; i++) {
    if (i) await sleep(800 * i);
    try {
      const r = await fetch(RPC, {
        method: 'POST',
        headers: { 'content-type': 'application/json' },
        body: JSON.stringify({ jsonrpc: '2.0', id: 1, method, params }),
      });
      if (r.status === 429) { last = new Error('http 429'); continue; }
      const j = await r.json();
      if (j.error) throw new Error(JSON.stringify(j.error));
      return j.result;
    } catch (e) {
      last = e;
    }
  }
  throw last;
}

/** All finalized signatures for PROGRAM with slot0 <= slot <= slotEnd. */
async function chainTruth(slot0, slotEnd) {
  const out = new Map();
  let before = null;
  let pages = 0;
  for (;;) {
    const params = [PROGRAM, { limit: 1000, commitment: 'finalized' }];
    if (before) params[1].before = before;
    const rows = await rpc('getSignaturesForAddress', params);
    pages++;
    if (!rows.length) break;
    let past = false;
    for (const r of rows) {
      if (r.slot < slot0) { past = true; break; }
      if (r.slot <= slotEnd) out.set(r.signature, r.slot);
    }
    if (past || rows.length < 1000) break;
    const next = rows[rows.length - 1].signature;
    if (next === before) break;
    before = next;
    if (pages > 40) throw new Error('truth paging exceeded 40 pages');
  }
  return { sigs: out, pages };
}

/** Did the program INVOKE in this tx, or is it only an account key? */
async function classify(signature) {
  try {
    const tx = await rpc('getTransaction', [
      signature,
      // maxSupportedTransactionVersion MUST be 1: with 0 the RPC rejects
      // versioned transactions with -32015 ("Transaction version (1) is not
      // supported"), which silently demoted real invoked losses to
      // 'unavailable' and under-counted the truth set.
      { encoding: 'jsonParsed', maxSupportedTransactionVersion: 1, commitment: 'finalized' },
    ]);
    if (!tx) return 'unavailable';
    const logs = (tx.meta && tx.meta.logMessages) || [];
    if (logs.some((l) => l.startsWith(`Program ${PROGRAM} `))) return 'invoked';
    return 'referenced-only';
  } catch {
    return 'unavailable';
  }
}

async function classifyAll(list, concurrency = 5) {
  const out = [];
  let i = 0;
  async function worker() {
    while (i < list.length) {
      const idx = i++;
      out[idx] = { ...list[idx], kind: await classify(list[idx].signature) };
    }
  }
  await Promise.all(Array.from({ length: concurrency }, worker));
  return out;
}

// ---------------------------------------------------------------------------
// consumer files (append-only JSONL; a torn last line after SIGKILL is fine)
// ---------------------------------------------------------------------------
function readSeen(file) {
  const seen = new Set();
  if (!fs.existsSync(file)) return seen;
  for (const l of fs.readFileSync(file, 'utf8').split('\n')) {
    if (!l.trim()) continue;
    try { seen.add(JSON.parse(l).signature); } catch { /* torn tail */ }
  }
  return seen;
}

// ---------------------------------------------------------------------------
// child processes
// ---------------------------------------------------------------------------
const children = new Set();
function track(c) { children.add(c); c.on('exit', () => children.delete(c)); return c; }
function spawnNaive(role) {
  const c = track(spawn(
    process.execPath,
    [SELF, '--role', role, '--program', PROGRAM, '--out', OUT],
    { stdio: ['ignore', 'pipe', 'inherit'] },
  ));
  let ready = false;
  c.stdout.on('data', (d) => { if (String(d).includes('READY')) ready = true; });
  c.__isReady = () => ready;
  return c;
}
function spawnReeflog(seconds) {
  const c = track(spawn(BIN, ['start', '--db', REEFLOG_DB, '--program', PROGRAM, '--seconds', String(seconds)],
    { stdio: ['ignore', 'pipe', 'pipe'] }));
  let out = '';
  c.stdout.on('data', (d) => { out += d; });
  c.stderr.on('data', (d) => process.stderr.write(`[reeflog] ${d}`));
  c.__summary = () => out;
  return c;
}
function killAll(...cs) {
  for (const c of cs) { try { c.kill('SIGKILL'); } catch { /* gone */ } }
}
function cli(args) {
  return new Promise((resolve) => {
    const c = spawn(BIN, args, { stdio: ['ignore', 'pipe', 'pipe'] });
    let o = '', e = '';
    c.stdout.on('data', (d) => (o += d));
    c.stderr.on('data', (d) => (e += d));
    c.on('exit', (code) => resolve({ code, out: o, err: e }));
  });
}
function safeJson(s) { try { return JSON.parse(s); } catch { return { raw: s }; } }

// ---------------------------------------------------------------------------
// run
// ---------------------------------------------------------------------------
const report = {
  label: LABEL,
  program: PROGRAM,
  startedAt: new Date().toISOString(),
  config: { TOTAL_S, KILL_EVERY_S, DOWNTIME_S, GRACE_S, rpc: RPC, ws: WS, bin: BIN },
  kills: [],
};
log(`killtest[${LABEL}] program=${PROGRAM} total=${TOTAL_S}s kill-every=${KILL_EVERY_S}s downtime=${DOWNTIME_S}s out=${OUT}`);

fs.mkdirSync(OUT, { recursive: true });
for (const f of [REEFLOG_DB, REEFLOG_DB + '-wal', REEFLOG_DB + '-shm', NAIVE_KILLED_FILE, NAIVE_LIVE_FILE]) {
  fs.rmSync(f, { force: true });
}
{
  const r = await cli(['init', '--db', REEFLOG_DB, '--program', PROGRAM]);
  if (r.code !== 0) { console.error('init failed', r); process.exit(2); }
}

let reeflog = spawnReeflog(TOTAL_S + 600);
let naiveKilled = spawnNaive('killed');
const naiveLive = spawnNaive('live');

for (let i = 0; i < 60; i++) {
  if (naiveKilled.__isReady() && naiveLive.__isReady()) break;
  await sleep(500);
}
if (!naiveKilled.__isReady() || !naiveLive.__isReady()) {
  killAll(reeflog, naiveKilled, naiveLive);
  report.verdict = 'inconclusive';
  report.reason = 'naive consumers did not subscribe within 30s';
  writeReport(report);
  console.error('INCONCLUSIVE:', report.reason);
  process.exit(2);
}
await sleep(3000); // let everyone settle
const slot0 = await rpc('getSlot', [{ commitment: 'confirmed' }]);
report.slot0 = slot0;
log(`window open at confirmed slot ${slot0}`);

const deadline = Date.now() + TOTAL_S * 1000;
const killLoop = (async () => {
  while (Date.now() + KILL_EVERY_S * 1000 < deadline) {
    await sleep(KILL_EVERY_S * 1000);
    const at = new Date().toISOString();
    killAll(reeflog, naiveKilled);
    report.kills.push({ at, downtimeS: DOWNTIME_S });
    log(`KILL #${report.kills.length} (reeflog + naive-killed down ${DOWNTIME_S}s)`);
    await sleep(DOWNTIME_S * 1000);
    naiveKilled = spawnNaive('killed');
    const remain = Math.ceil((deadline - Date.now()) / 1000) + 600;
    reeflog = spawnReeflog(remain > 600 ? 600 : remain);
  }
})();

await sleep(TOTAL_S * 1000);
const slotEnd = await rpc('getSlot', [{ commitment: 'confirmed' }]);
report.slotEnd = slotEnd;
log(`window closed at confirmed slot ${slotEnd}; grace ${GRACE_S}s with consumers up`);
await sleep(GRACE_S * 1000);
await killLoop;
killAll(reeflog, naiveKilled, naiveLive);
await sleep(500);

report.endedAt = new Date().toISOString();
report.killsPerformed = report.kills.length;

log('waiting for finality…');
await sleep(20000);

// ---------------------------------------------------------------------------
// reeflog journal integrity (independent of the truth set)
// ---------------------------------------------------------------------------
const cliStatus = await cli(['status', '--db', REEFLOG_DB]);
const cliVerify = await cli(['verify', '--db', REEFLOG_DB, '--pages', '400']);
const cliRepair = await cli(['repair', '--db', REEFLOG_DB, '--pages', '400']);
const cliVerify2 = await cli(['verify', '--db', REEFLOG_DB, '--pages', '400']);
const cliTail1 = await cli(['tail', '--db', REEFLOG_DB, '--cursor', path.join(OUT, 'c.txt'), '--limit', '500000']);
const cliTail2 = await cli(['tail', '--db', REEFLOG_DB, '--cursor', path.join(OUT, 'c.txt'), '--limit', '500000']);
let sql = null;
let reeflogSigs = new Set();
{
  const { DatabaseSync } = await import('node:sqlite');
  const db = new DatabaseSync(REEFLOG_DB, { readOnly: true });
  sql = db.prepare(
    'SELECT COUNT(*) AS rows, COUNT(DISTINCT signature) AS distinct_sigs, MAX(seq) AS max_seq FROM events',
  ).get();
  for (const r of db.prepare('SELECT signature FROM events').all()) reeflogSigs.add(r.signature);
  db.close();
}
report.integrity = {
  status: safeJson(cliStatus.out),
  verify: safeJson(cliVerify.out),
  verifyExit: cliVerify.code,
  // The product contract is detect -> repair -> prove. The pre-repair verify
  // must SEE the holes (exit 3 with a real count when incomplete, or a
  // non-zero missing_count), the repair must fill them, and the post-repair
  // verify must come back complete with zero missing.
  repair: safeJson(cliRepair.out),
  repairExit: cliRepair.code,
  verifyAfterRepair: safeJson(cliVerify2.out),
  verifyAfterRepairExit: cliVerify2.code,
  tailFirstRunLines: cliTail1.out.split('\n').filter(Boolean).length,
  tailSecondRunLines: cliTail2.out.split('\n').filter(Boolean).length,
  sqlite: sql,
};

// ---------------------------------------------------------------------------
// ground truth vs consumers
// ---------------------------------------------------------------------------
log('fetching chain truth…');
const { sigs: truth, pages } = await chainTruth(slot0, slotEnd);
report.truth = { count: truth.size, pages, slot0, slotEnd };
log(`truth: ${truth.size} signatures over ${pages} pages`);
writeReport(report);

const naiveKilledSigs = readSeen(NAIVE_KILLED_FILE);
const naiveLiveSigs = readSeen(NAIVE_LIVE_FILE);

function missingOf(set) {
  const m = [];
  for (const [sig, slot] of truth) if (!set.has(sig)) m.push({ signature: sig, slot });
  m.sort((a, b) => a.slot - b.slot);
  return m;
}
const missReeflog = missingOf(reeflogSigs);
const missKilled = missingOf(naiveKilledSigs);
const missLive = missingOf(naiveLiveSigs);

// ---------------------------------------------------------------------------
// SOUND loss rule (no per-tx RPC storm):
//
// A signature that ANY consumer received over the log stream is, by
// construction, a transaction that INVOKES the program. Those are exactly
// the transactions a log subscription can deliver. So:
//
//   real loss of X = a truth signature that X missed AND that some consumer
//                    actually received on the log stream.
//
// Everything a consumer received is in the union of the three observed sets.
// The residual (missed by everybody, e.g. a tx that only REFERENCES the
// program as an account key) is not deliverable on a log stream at all; it is
// classified with getTransaction to prove that instead of assuming it.
// ---------------------------------------------------------------------------
const observedUnion = new Set([...naiveLiveSigs, ...naiveKilledSigs, ...reeflogSigs]);
const deliverable = new Map();
for (const [sig, slot] of truth) if (observedUnion.has(sig)) deliverable.set(sig, slot);
report.truth.deliverable = deliverable.size;

const realLosses = (missList) => missList.filter((m) => deliverable.has(m.signature));
const lossReeflog = realLosses(missReeflog);
const lossKilled = realLosses(missKilled);
const lossLive = realLosses(missLive);
const residual = missList => missList.filter((m) => !deliverable.has(m.signature));

log(`classifying residual misses: reeflog=${residual(missReeflog).length} naive-killed=${residual(missKilled).length} naive-live=${residual(missLive).length}`);
const clsResidualReeflog = await classifyAll(residual(missReeflog));
const clsResidualKilled = await classifyAll(residual(missKilled).slice(0, 40));
const clsResidualLive = await classifyAll(residual(missLive).slice(0, 40));
const countKind = (a, k) => a.filter((m) => m.kind === k).length;

report.missing = {
  reeflog: {
    total: missReeflog.length,
    realLosses: lossReeflog.length,
    realLossSample: lossReeflog.slice(0, 10),
    residual: clsResidualReeflog.length,
    residualInvoked: countKind(clsResidualReeflog, 'invoked'),
    residualReferencedOnly: countKind(clsResidualReeflog, 'referenced-only'),
    residualUnavailable: countKind(clsResidualReeflog, 'unavailable'),
    residualSample: clsResidualReeflog.slice(0, 10),
  },
  naiveKilled: {
    total: missKilled.length,
    realLosses: lossKilled.length,
    residual: clsResidualKilled.length,
    residualInvoked: countKind(clsResidualKilled, 'invoked'),
    residualReferencedOnly: countKind(clsResidualKilled, 'referenced-only'),
    residualUnavailable: countKind(clsResidualKilled, 'unavailable'),
    realLossSample: lossKilled.slice(0, 10),
  },
  naiveLive: {
    total: missLive.length,
    realLosses: lossLive.length,
    residual: clsResidualLive.length,
    residualInvoked: countKind(clsResidualLive, 'invoked'),
    residualReferencedOnly: countKind(clsResidualLive, 'referenced-only'),
    residualUnavailable: countKind(clsResidualLive, 'unavailable'),
    realLossSample: lossLive.slice(0, 10),
  },
};
report.consumerSeen = { reeflog: reeflogSigs.size, naiveKilled: naiveKilledSigs.size, naiveLive: naiveLiveSigs.size };
writeReport(report);

const verifyCount = report.integrity.verify?.missing_count;
const v1 = report.integrity.verify || {};
const v2 = report.integrity.verifyAfterRepair || {};
const rp = report.integrity.repair || {};
report.honesty = {
  verifyReportedMissing: verifyCount,
  verifyClaimedComplete: v1.complete,
  verifyExit: report.integrity.verifyExit,
  repairFilled: rp.filled,
  repairFoundMissing: rp.found_missing,
  verifyAfterRepairMissing: v2.missing_count,
  verifyAfterRepairComplete: v2.complete,
  verifyAfterRepairExit: report.integrity.verifyAfterRepairExit,
  independentRealLosses: report.missing.reeflog.realLosses,
  // The exact lie the first kill test caught: a scan that stopped early
  // (truncated/error) printed `missing_count: 0` and exited 0, so an
  // incomplete scan was indistinguishable from a clean one.
  incompleteScanExitsZero:
    v1.complete !== undefined && v1.complete !== true &&
    report.integrity.verifyExit === 0,
  detectorBlindToRealGaps:
    typeof verifyCount === 'number' && verifyCount === 0 && report.missing.reeflog.realLosses > 0,
};

// ---------------------------------------------------------------------------
// verdict
// ---------------------------------------------------------------------------
const checks = [];
const push = (name, pass, detail) => checks.push({ name, pass, detail });

push('reeflog_loses_no_deliverable_event',
  report.missing.reeflog.realLosses === 0 && report.missing.reeflog.residualInvoked === 0,
  `real_losses=${report.missing.reeflog.realLosses} residual_invoked=${report.missing.reeflog.residualInvoked} ` +
  `residual_referenced_only=${report.missing.reeflog.residualReferencedOnly} (deliverable_truth=${deliverable.size}/${truth.size})`);
push('dedup_exactly_once',
  !!sql && sql.rows === sql.distinct_sigs && sql.rows > 0,
  `rows=${sql?.rows} distinct=${sql?.distinct_sigs}`);
push('cursor_no_replay',
  report.integrity.tailSecondRunLines === 0,
  `first=${report.integrity.tailFirstRunLines} second=${report.integrity.tailSecondRunLines}`);
push('holes_are_reported_not_swallowed',
  !report.honesty.detectorBlindToRealGaps,
  `verify_missing=${verifyCount} independent_real_losses=${report.missing.reeflog.realLosses}`);
// Regression for the lie found in run `full`: a truncated verify printed
// missing_count: 0 with exit 0. An unfinished scan must never exit 0.
push('incomplete_verify_never_exits_zero',
  !report.honesty.incompleteScanExitsZero,
  `complete=${v1.complete} truncated=${v1.truncated} error=${v1.error ?? 'none'} exit=${report.integrity.verifyExit}`);
// The product contract: detect the holes, fill them, and prove afterwards
// that the scan is complete AND finds nothing.
push('repair_fills_and_proves_clean',
  v2.complete === true &&
  typeof v2.missing_count === 'number' && v2.missing_count === 0 &&
  report.integrity.verifyAfterRepairExit === 0 &&
  (typeof v1.missing_count !== 'number' || v1.missing_count === 0 ||
    (typeof rp.filled === 'number' && rp.filled >= v1.missing_count)),
  `before_missing=${v1.missing_count} repair_filled=${rp.filled} after_missing=${v2.missing_count} after_complete=${v2.complete} after_exit=${report.integrity.verifyAfterRepairExit}`);
// Only counts as a deliver failure if the journal STILL has holes after
// repair — i.e. what a consumer reads is short of the chain.
push('reeflog_delivers_everything_after_repair',
  report.missing.reeflog.realLosses === 0 && report.missing.reeflog.residualInvoked === 0,
  `post_repair real_losses=${report.missing.reeflog.realLosses} residual_invoked=${report.missing.reeflog.residualInvoked}`);
push('killed_control_loses_deliverable_events',
  report.missing.naiveKilled.realLosses > 0,
  `naive-killed real losses=${report.missing.naiveKilled.realLosses} of ${deliverable.size} across ${report.kills.length} kill windows`);
checks.push({
  name: 'live_control_is_clean',
  pass: report.missing.naiveLive.realLosses === 0,
  detail: `naive-live real losses=${report.missing.naiveLive.realLosses} of ${deliverable.size} (informational: shows the kill is the cause)`,
});

report.checks = checks;
const contractChecks = checks.filter((c) => c.name !== 'live_control_is_clean');
report.verdict = contractChecks.every((c) => c.pass) ? 'contract_survived' : 'falsified';

writeReport(report);
log('='.repeat(70));
for (const c of checks) log(`${c.pass ? 'PASS' : 'FAIL'}  ${c.name}: ${c.detail}`);
log(`VERDICT: ${report.verdict} -> ${REPORT_PATH}`);
process.exit(report.verdict === 'contract_survived' ? 0 : 1);
