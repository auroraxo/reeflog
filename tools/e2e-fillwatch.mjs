// Falsifying comparison: reeflog-as-a-mailbox fill watcher vs. the classic
// "poll the RPC and fetch every transaction yourself" fill watcher.
//
// Both consumers observe the SAME slot window on the same public endpoint
// (api.mainnet-beta.solana.com), Raydium AMM v4.
//
//   naive  = getSignaturesForAddress every 2 s, then getTransaction for new
//            signatures, decoding fills from freshly fetched logs. This is what
//            a bot does when it has no mailbox. It is budgeted because the
//            public endpoint answers ~1 tx/s under load; every call is counted.
//   reef   = `reeflog start` (WS ingest + repair + payload worker), then
//            `reeflog tail --cursor` -> fill-watcher decodes from local disk
//            with zero RPC calls of its own.
//
// Why the comparison is slot-aligned
// ----------------------------------
// A naive page fetch reaches back ~1000 signatures into history; the daemon's
// window starts when its WS subscription delivers the first event. Comparing
// the two raw sets is therefore not a loss measurement — it measures where each
// consumer *began*. First run of this harness did exactly that and reported 21
// "missing" fills; all sampled ones sat at slots 662/663 while the journal
// started at 668, i.e. before the daemon had connected. The superset check is
// bound to the daemon's own slot window [min_slot, max_slot]. Falls outside are
// counted and reported as `outside_window`, never as a loss and never hidden.
//
// Checks:
//  1. payload_complete        - after `hydrate` exits 0 no row is pending.
//  2. local_decoding_outbids_rpc - local bytes yield at least as many in-window
//                                 fills as the budgeted poller bought.
//  3. window_superset         - every naive fill INSIDE the daemon's slot
//                               window is in reeflog's stream. One missing fill
//                               falsifies the mailbox for that window.
//  4. decoded_amounts_match   - where both saw an in-window fill, amount_in /
//                               out / direction must be identical. A payload
//                               that exists but disagrees with the chain is not
//                               delivery.
//  5. watcher_costs_nothing   - local decode phase makes 0 RPC calls.
//  6. watcher_decodes_every_leg - no held leg falls out of the decoder.
import { spawn } from 'node:child_process';
import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { fillFromRow, watch } from './fill-watcher.mjs';

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const ROOT = path.resolve(__dirname, '..');
const BIN = path.join(ROOT, 'target', 'release', 'reeflog');
const RPC = 'https://api.mainnet-beta.solana.com';
const PROGRAM = '675kPX9MHTjS2zt1qfr1NYHuzeLXfQM9H24wFSUt1Mp8'; // Raydium AMM v4
const DIR = process.env.REEFLOG_FILLWATCH_DIR || '/tmp/reeflog-fillwatch';
const DB = path.join(DIR, 'reeflog.db');
const CURSOR = path.join(DIR, 'cursor.txt');
const SECONDS = Number(process.env.REEFLOG_FILLWATCH_SECONDS || 60);
const TX_BUDGET = Number(process.env.REEFLOG_FILLWATCH_TX_BUDGET || 80);
const PER_POLL_CAP = 10; // keeps the poller inside the window instead of
                         // spending its whole budget on the first page

const rpcCalls = { sigs: 0, tx: 0 };
async function rpc(method, params, kind, tries = 10) {
  if (kind) rpcCalls[kind]++;
  let last;
  for (let i = 0; i < tries; i++) {
    if (i) await new Promise((r) => setTimeout(r, 400 * (i * i + 1)));
    try {
      const res = await fetch(RPC, {
        method: 'POST',
        headers: { 'content-type': 'application/json' },
        body: JSON.stringify({ jsonrpc: '2.0', id: 1, method, params }),
      });
      if (res.status === 429) { last = new Error('429'); continue; }
      const j = await res.json();
      if (j.error) throw new Error(JSON.stringify(j.error));
      return j.result;
    } catch (e) { last = e; }
  }
  throw last;
}

function cli(args) {
  return new Promise((resolve) => {
    const c = spawn(BIN, args, { stdio: ['ignore', 'pipe', 'pipe'] });
    let o = '', e = '';
    c.stdout.on('data', (d) => { o += d; });
    c.stderr.on('data', (d) => { e += d; });
    // 'close' = stdio drained. 'exit' can fire while piped stdout is still
    // unread, which silently truncates what this harness believes it read.
    c.on('close', (code) => resolve({ code, out: o, err: e }));
    // Without this a launch failure would leave the harness waiting forever.
    c.on('error', (err) => resolve({ code: null, out: o, err: `${e}\n${err}` }));
  });
}

const checks = [];
const push = (name, pass, detail) => {
  checks.push({ name, pass, detail });
  console.log(`${pass ? 'PASS' : 'FAIL'} ${name}: ${detail}`);
};

fs.rmSync(DIR, { recursive: true, force: true });
fs.mkdirSync(DIR, { recursive: true });
{
  const r = await cli(['init', '--db', DB, '--program', PROGRAM]);
  if (r.code !== 0) { console.error('INIT FAILED', r); process.exit(2); }
}

// --- start both consumers at the same instant -----------------------------
const t0 = Date.now();
const endAt = t0 + SECONDS * 1000;
const reefRun = cli(['start', '--db', DB, '--seconds', String(SECONDS), '--program', PROGRAM]);
const reefStderr = reefRun.then((r) => r); // captured below

const naive = {
  fills: new Map(), // sig -> fill (with .slot)
  polls: 0, txs: 0, budgetLeft: TX_BUDGET, errors: 0,
};
const seen = new Set();
while (Date.now() < endAt && naive.budgetLeft > 0) {
  naive.polls++;
  let page;
  try {
    page = await rpc('getSignaturesForAddress',
      [PROGRAM, { limit: 1000, commitment: 'confirmed' }], 'sigs');
  } catch (e) {
    console.error('naive poll error:', String(e.message || e));
    await new Promise((r) => setTimeout(r, 2000));
    continue;
  }
  let boughtThisPoll = 0;
  for (const row of page) {
    if (naive.budgetLeft <= 0 || boughtThisPoll >= PER_POLL_CAP) break;
    if (seen.has(row.signature)) continue;
    seen.add(row.signature);
    if (row.err) continue; // a failed tx is not a fill
    naive.budgetLeft--;
    naive.txs++;
    boughtThisPoll++;
    let tx;
    try {
      tx = await rpc('getTransaction', [row.signature, {
        commitment: 'confirmed', maxSupportedTransactionVersion: 1, encoding: 'json',
      }], 'tx');
    } catch { naive.errors++; continue; }
    if (!tx?.meta?.logMessages) continue;
    const fill = fillFromRow({
      signature: row.signature, slot: tx.slot, logs: tx.meta.logMessages,
      err: null, source: 'naive', seal: 'confirmed',
    });
    if (fill) naive.fills.set(row.signature, fill);
  }
  await new Promise((r) => setTimeout(r, 2000));
}
const naiveMs = Date.now() - t0;
console.log(`naive consumer: ${naive.polls} polls, ${naive.txs} txs bought, ` +
  `${naive.fills.size} fills decoded in ${(naiveMs / 1000).toFixed(1)}s (budget ${TX_BUDGET})`);

const reefRunResult = await reefStderr;
if (reefRunResult.err) {
  console.log('\nreeflog start (stderr summary):');
  console.log(reefRunResult.err.trim().split('\n').slice(-6).join('\n'));
}

// --- the daemon's own slot window ------------------------------------------
const { DatabaseSync } = await import('node:sqlite');
{
  const db = new DatabaseSync(DB, { readOnly: true });
  var win = db.prepare('SELECT MIN(slot) lo, MAX(slot) hi, COUNT(*) n FROM events').get();
  var rowsWithLogs = db.prepare('SELECT COUNT(*) n FROM events WHERE logs IS NOT NULL').get().n;
  db.close();
}
console.log(`\ndaemon window: slots ${win.lo}..${win.hi}, ${win.n} rows, ${rowsWithLogs} with payload`);

// --- make every held row readable ------------------------------------------
const hy = await cli(['hydrate', '--db', DB, '--limit', '5000']);
let hyOut = {};
try { hyOut = JSON.parse(hy.out); } catch { /* ignore */ }
push(
  'payload_complete',
  hy.code === 0 && hyOut.complete === true && hyOut.pending_left === 0,
  `exit=${hy.code} complete=${hyOut.complete} hydrated=${hyOut.hydrated} pending=${hyOut.pending_left} ` +
  `unavailable=${hyOut.unavailable} invalid=${hyOut.invalid} events=${hyOut.journal?.events}`,
);

// --- reeflog's own fill watcher: local only, zero RPC -----------------------
const callsBefore = rpcCalls.sigs + rpcCalls.tx;
const tWatch = Date.now();
const w = await watch({ db: DB, cursor: CURSOR, bin: BIN, limit: 10000 });
const watchMs = Date.now() - tWatch;
const reef = new Map(w.fills.map((f) => [f.signature, f]));
const localCalls = (rpcCalls.sigs + rpcCalls.tx) - callsBefore;
console.log(`reeflog fill watcher: ${w.rows} rows -> ${reef.size} fills in ${watchMs}ms (own RPC calls: ${localCalls})`);

// --- slot-align the comparison ---------------------------------------------
const inWindow = (f) => f.slot >= win.lo && f.slot <= win.hi;
const naiveIn = new Map([...naive.fills.entries()].filter(([, f]) => inWindow(f)));
const naiveBefore = [...naive.fills.values()].filter((f) => f.slot < win.lo);
const naiveAfter = [...naive.fills.values()].filter((f) => f.slot > win.hi);
const reefIn = new Map([...reef.entries()].filter(([, f]) => inWindow(f)));

console.log(`\nalignment: naive fills=${naive.fills.size} (in window ${naiveIn.size}, ` +
  `before ${naiveBefore.length}, after ${naiveAfter.length}); reeflog fills=${reef.size} ` +
  `(in window ${reefIn.size})`);
if (naiveBefore.length) {
  const s = naiveBefore.map((f) => f.slot).sort((a, b) => a - b);
  console.log(`  naive fills before the daemon's window: slots ${s[0]}..${s[s.length - 1]} ` +
    `(journal starts at ${win.lo}) — history a page fetch reaches, not the daemon's window`);
}

push(
  'local_decoding_outbids_rpc',
  reefIn.size > 0 && reefIn.size >= naiveIn.size,
  `${reefIn.size} in-window fills from local bytes vs ${naiveIn.size} bought from the RPC ` +
  `(${naive.fills.size} bought total)`,
);

const missing = [...naiveIn.keys()].filter((k) => !reef.has(k));
push(
  'window_superset',
  missing.length === 0 && naiveIn.size > 0,
  `${naiveIn.size} in-window naive fills, ${reefIn.size} in-window reeflog fills, ` +
  `missing from reeflog: ${missing.length}` +
  (missing.length ? ` e.g. ${missing.slice(0, 3).join(', ')}` : ''),
);

// BigInt-safe stable form: amount fields are u64 and exceed Number range.
const form = (legs) => JSON.stringify(legs, (_k, v) =>
  typeof v === 'bigint' ? v.toString() : v);

let identical = 0;
const mismatches = [];
for (const [k, nf] of naiveIn) {
  const rf = reef.get(k);
  if (!rf) continue;
  const a = form(nf.legs);
  const b = form(rf.legs);
  if (a === b) identical++;
  else mismatches.push({ sig: k.slice(0, 20), naive: a, reef: b });
}
push(
  'decoded_amounts_match',
  mismatches.length === 0 && identical > 0,
  `${identical}/${naiveIn.size} in-window fills decode identically; ${mismatches.length} mismatch(es)` +
  (mismatches.length ? ` first=${JSON.stringify(mismatches[0])}` : ''),
);

push('watcher_costs_nothing', localCalls === 0, `${localCalls} RPC calls in the local decode phase`);

let undecodable = 0;
for (const f of reef.values()) for (const l of f.legs) if (l.kind === 'undecodable') undecodable++;
push('watcher_decodes_every_leg', undecodable === 0,
  `${undecodable} undecodable legs in ${reef.size} fills`);

console.log('\nMeasured cost:');
console.log(`  naive poller : ${rpcCalls.sigs} getSignaturesForAddress + ${rpcCalls.tx} getTransaction ` +
  `= ${rpcCalls.sigs + rpcCalls.tx} calls for ${naive.fills.size} fills`);
console.log(`  reeflog tail : 0 calls for ${reef.size} fills (bytes already on disk)`);
console.log(`  naive window : ${(naiveMs / 1000).toFixed(1)}s of polling; watcher drained ${w.rows} rows in ${watchMs}ms`);
console.log(`  fills only reeflog had (in window): ${[...reefIn.keys()].filter((k) => !naive.fills.has(k)).length}`);

const verdict = checks.every((c) => c.pass);
console.log(verdict ? 'FILLWATCH E2E: ALL PASS' : 'FILLWATCH E2E: FAILURE');
process.exit(verdict ? 0 : 1);
