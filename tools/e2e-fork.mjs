#!/usr/bin/env node
// End-to-end proof of the reorg detector against a REAL fork.
//
// What this closes (the reeflog README's open item): "the detector has only
// ever been proven against a planted signature, never against a real fork."
// Every previous positive case planted a signature the chain had never seen.
// That proves the two-source rule fires on an unknown key. It does NOT prove
// that the rule fires on a transaction that really WAS executed, really was
// FINALIZED, and was then taken back by the chain — the only case a consumer
// can actually lose money on.
//
// This harness manufactures that case honestly, on a real chain:
//
//   1. A local agave validator produces real slots with real consensus
//      (root/finalized).
//   2. A REAL transaction is sent and observed as confirmationStatus=finalized
//      with the finalized root past its slot. reeflog's daemon is already
//      running, so the event arrives live over the socket and is sealed as
//      finalized — a promise to the consumer. This is the SURVIVOR: the
//      ledger is snapshotted AFTER it, so the fork must not touch it.
//   3. A SECOND real transaction is sent after the snapshot, ingested and
//      sealed the same way. This is the one the fork will unmake.
//   4. CONTROL: with the chain intact, both sources serve both transactions
//      and reeflog must find ZERO reverts — in the CLI and inside the daemon.
//   5. THE FORK: the validator is stopped and its ledger rolled back to the
//      pre-second-transaction copy, then restarted. The chain re-produces
//      those slots without that transaction. Nothing is planted: the
//      signature is a real one the chain made and then unmade.
//   6. Both independent sources must now refuse it, the block at its slot
//      must be a DIFFERENT block, and the survivor must still be served.
//      Only then may reeflog record a reorg — detected by the RUNNING daemon,
//      not just the CLI — and deliver it exactly once on the retraction
//      channel so the consumer can undo exactly the event it acted on.
//   7. NEGATIVE CONTROLS: a transaction from before the rollback stays
//      intact in the same check, a second read of the retraction channel
//      delivers nothing, and a NEW transaction on the forked chain must be
//      ingested, sealed, and NOT retracted.
//
// Exit codes: 0 all checks passed, 1 a check failed, 3 skipped (missing
// toolchain — a missing binary is never a passing test).
//
// Usage:
//   node tools/e2e-fork.mjs
//   AGAVE_BIN=/path/to/bin node tools/e2e-fork.mjs
//   REEFLOG_FORK_DAEMON_SECONDS=60 node tools/e2e-fork.mjs
import { spawn, spawnSync } from 'node:child_process';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { watchRetractions } from './fill-watcher.mjs';

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const ROOT = path.resolve(__dirname, '..');
const REEFLOG = path.join(ROOT, 'target', 'release', 'reeflog');

const AGAVE_BIN = process.env.AGAVE_BIN || '/tmp/agave/solana-release/bin';
const VALIDATOR = path.join(AGAVE_BIN, 'solana-test-validator');
const KEYGEN = path.join(AGAVE_BIN, 'solana-keygen');
const SOLANA = path.join(AGAVE_BIN, 'solana');
// A port nobody else uses: the harness owns its own chain.
const RPC_PORT = Number(process.env.REEFLOG_FORK_RPC_PORT || 18999);
const WS_PORT = RPC_PORT + 1;
const FAUCET_PORT = RPC_PORT + 2;
const RPC = `http://127.0.0.1:${RPC_PORT}`;
const WS = `ws://127.0.0.1:${WS_PORT}`;
const DAEMON_SECONDS = Number(process.env.REEFLOG_FORK_DAEMON_SECONDS || 60);
// Every transaction in this harness is a system transfer, so the program the
// mailbox watches is the System Program itself: its logs contain the program
// id, which is what logsSubscribe mentions= and getSignaturesForAddress agree
// on, and it exists on every chain without deploying anything.
const PROGRAM = '11111111111111111111111111111111';

const DIR = process.env.REEFLOG_FORK_DIR || path.join(os.tmpdir(), 'reeflog-fork-e2e');
const LEDGER = path.join(DIR, 'ledger');
const PRE_FORK = path.join(DIR, 'ledger-prefork');
const DB = path.join(DIR, 'reeflog.db');
const REORG_CURSOR = path.join(DIR, 'reorg-cursor.txt');
const REORG_INSPECT = path.join(DIR, 'reorg-inspect-cursor.txt');
const CONSUMER_CURSOR = path.join(DIR, 'reorg-cursor-consumer.txt');
const LEDGER_JSON = path.join(DIR, 'consumer-ledger.json');
const REPORT = process.env.REEFLOG_FORK_REPORT || path.join(ROOT, 'research', 'fork-e2e-report.json');

const TX_OPTS = { encoding: 'json', maxSupportedTransactionVersion: 1, commitment: 'finalized' };

const results = [];
const check = (name, pass, detail) => {
  const p = !!pass;
  results.push({ name, pass: p, detail: detail ?? null });
  console.log(`${p ? 'PASS' : 'FAIL'} ${name}${detail ? ` — ${detail}` : ''}`);
};

function skip(reason) {
  console.log(`SKIPPED — ${reason}`);
  fs.mkdirSync(path.dirname(REPORT), { recursive: true });
  fs.writeFileSync(
    REPORT,
    JSON.stringify({ verdict: 'skipped', reason, at: new Date().toISOString() }, null, 2),
  );
  process.exit(3);
}

function binExists(p) {
  try {
    fs.accessSync(p, fs.constants.X_OK);
    return true;
  } catch {
    return false;
  }
}

async function rpc(method, params = [], tries = 12) {
  let last;
  for (let i = 0; i < tries; i++) {
    if (i) await new Promise((r) => setTimeout(r, 500 * i));
    try {
      const res = await fetch(RPC, {
        method: 'POST',
        headers: { 'content-type': 'application/json' },
        body: JSON.stringify({ 'jsonrpc': '2.0', id: 1, method, params }),
      });
      if (res.status === 429) {
        last = new Error('429');
        continue;
      }
      const j = await res.json();
      // Keep the raw envelope distinction: an RPC *error* is never a verdict.
      // `{result}` only exists when the node actually answered.
      if (j.error) return { error: j.error };
      return { result: j.result };
    } catch (e) {
      last = e;
    }
  }
  return { error: { message: String((last && last.message) || last) } };
}

// getTransaction answers with the transaction object in `result` itself
// (blockTime/meta/slot/transaction) — NOT wrapped in `.value` like
// getBalance and getSignatureStatuses are. Confirmed against agave 4.3.2:
//   {result:{blockTime,meta,slot,transaction}}
// and the refusal form is `{result:null}`. Reading `.value` here silently
// inverted every "served?" answer, so the harness now normalises it once.
async function getTx(sig) {
  const r = await rpc('getTransaction', [sig, TX_OPTS]);
  if (!('result' in r)) return { state: 'error', value: null, detail: `rpc error ${JSON.stringify(r.error)}` };
  if (r.result === null) return { state: 'refused', value: null, detail: 'result=null' };
  return { state: 'served', value: r.result, detail: 'served' };
}

async function getStatus(sig) {
  const r = await rpc('getSignatureStatuses', [[sig], { searchTransactionHistory: true }]);
  if (!('result' in r)) return { state: 'error', value: null, detail: `rpc error ${JSON.stringify(r.error)}` };
  const v = r.result && r.result.value ? r.result.value[0] : undefined;
  if (v === null) return { state: 'refused', value: null, detail: 'value[0]=null' };
  if (!v) return { state: 'refused', value: null, detail: `value[0]=${JSON.stringify(v)}` };
  return { state: 'served', value: v, detail: `confirmationStatus=${v.confirmationStatus} slot=${v.slot}` };
}

async function getBlockHash(slot) {
  const r = await rpc('getBlock', [
    slot,
    { commitment: 'finalized', transactionDetails: 'none', maxSupportedTransactionVersion: 0 },
  ]);
  if (!('result' in r)) return { state: 'error', hash: null, detail: `rpc error ${JSON.stringify(r.error)}` };
  if (!r.result) return { state: 'missing', hash: null, detail: 'result=null' };
  const h = r.result.blockhash || null;
  return { state: h ? 'ok' : 'error', hash: h, detail: h || JSON.stringify(r.result).slice(0, 120) };
}

function run(cmd, args) {
  const r = spawnSync(cmd, args, { encoding: 'utf8', maxBuffer: 32 * 1024 * 1024 });
  return { code: r.status, out: r.stdout || '', err: r.stderr || '' };
}

function sol(subcommand, args) {
  // Pass --url and --commitment globally before the subcommand, so every
  // subcommand (balance, transfer, airdrop, etc.) inherits them
  // deterministically without subcommand-specific flag quirks. Put them after
  // the subcommand and `solana` silently falls back to ~/.config/solana/
  // cli/config.yml (mainnet) and reports "insufficient funds" for an account
  // that is rich on the local chain.
  return run(SOLANA, ['--url', RPC, '--commitment', 'finalized', subcommand, ...args]);
}

function reeflog(args) {
  const r = run(REEFLOG, args);
  let json = null;
  try {
    json = JSON.parse(r.out);
  } catch {
    /* plain-text commands (init, tail, reorg tail) */
  }
  return { ...r, json };
}

// A daemon handle that keeps its output readable WHILE it runs: the reorg
// detection proof needs to see the "REVERTED" line the running daemon prints,
// and SQLite polling is not the signal under test.
function startProc(cmd, args) {
  const child = spawn(cmd, args, { stdio: ['ignore', 'pipe', 'pipe'] });
  const state = { out: '', err: '' };
  child.stdout.on('data', (d) => (state.out += d));
  child.stderr.on('data', (d) => (state.err += d));
  // 'close' (stdio drained), not 'exit' (process died): piped output can
  // still be unread at 'exit', which loses the last lines — and here the
  // lost line would be the daemon's REVERTED verdict.
  const done = new Promise((res) => {
    child.on('close', (code) => res(code));
    // Without this a launch failure would leave the harness waiting forever.
    child.on('error', () => res(null));
  });
  return { child, done, state };
}

function spawnAsync(cmd, args) {
  return new Promise((resolve) => {
    const p = startProc(cmd, args);
    p.done.then((code) => resolve({ code, out: p.state.out, err: p.state.err }));
  });
}

async function waitHealth(timeoutMs = 90000) {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    const r = await rpc('getHealth', [], 1);
    if (r.result === 'ok') return true;
    await new Promise((s) => setTimeout(s, 800));
  }
  return false;
}

async function waitFinalized(sig, timeoutMs = 120000) {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    const r = await rpc('getSignatureStatuses', [[sig], { searchTransactionHistory: true }], 1);
    const v = r.result && r.result.value && r.result.value[0];
    if (v && v.confirmationStatus === 'finalized') return v;
    await new Promise((s) => setTimeout(s, 1000));
  }
  return null;
}

// The finalized ROOT must be strictly past a slot before we may claim that
// slot is settled — root == slot is the common instant right after
// finalization and is not enough to prove the block is rooted.
async function waitRootPast(slot, timeoutMs = 180000) {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    const r = await rpc('getSlot', [{ commitment: 'finalized' }], 1);
    if (typeof r.result === 'number' && r.result > slot) return r.result;
    await new Promise((s) => setTimeout(s, 1000));
  }
  return null;
}

// Poll until `fn` returns a truthy value. Used for journal/daemon states that
// only become true a few seconds after an action.
async function waitFor(fn, timeoutMs = 90000, intervalMs = 1000) {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    try {
      const v = await fn();
      if (v) return v;
    } catch {
      /* keep polling through transient failures */
    }
    await new Promise((r) => setTimeout(r, intervalMs));
  }
  return null;
}

function startValidator(ledgerDir, logFile) {
  const out = fs.openSync(logFile, 'a');
  const child = spawn(
    VALIDATOR,
    ['--ledger', ledgerDir, '--rpc-port', String(RPC_PORT), '--faucet-port', String(FAUCET_PORT)],
    { stdio: ['ignore', out, out] },
  );
  return child;
}

async function stopValidator(child) {
  if (!child || child.exitCode !== null) return;
  const exited = new Promise((r) => child.once('exit', r));
  child.kill('SIGTERM');
  const timer = setTimeout(() => child.kill('SIGKILL'), 20000);
  await exited;
  clearTimeout(timer);
  await new Promise((r) => setTimeout(r, 2000));
}

function extractSig(text) {
  const m = text.match(/Signature:\s*([1-9A-HJ-NP-Za-km-z]{60,120})/);
  if (m) return m[1];
  try {
    const j = JSON.parse(text);
    if (j.result) return j.result;
  } catch {
    /* not json */
  }
  return null;
}

// Every read gets a FRESH cursor file: `tail` advances its cursor, so a shared
// one makes every read after the first return nothing (which looked like rows
// vanishing from the journal when they were only already-read).
let cursorSeq = 0;
function journalRows(db) {
  const cursor = path.join(DIR, `inspect-cursor-${process.pid}-${cursorSeq++}.txt`);
  const t = reeflog(['tail', '--db', db, '--cursor', cursor, '--limit', '10000', '--no-logs']);
  return t.out
    .split('\n')
    .filter(Boolean)
    .map((l) => {
      try {
        return JSON.parse(l);
      } catch {
        return null;
      }
    })
    .filter(Boolean);
}

function rowFor(db, sig) {
  return journalRows(db).find((r) => r.signature === sig) || null;
}

function daemonArgs(seconds) {
  return [
    'start',
    '--db',
    DB,
    '--program',
    PROGRAM,
    '--rpc',
    RPC,
    '--ws',
    WS,
    '--seconds',
    String(seconds),
    '--seal-every',
    '5',
    '--reorg-every',
    '5',
    '--reorg-lookback',
    '1000',
    '--repair-every',
    '5',
    '--payload-limit',
    '100',
  ];
}

function parseJson(text) {
  try {
    return JSON.parse(text);
  } catch {
    return {};
  }
}

// ---------------------------------------------------------------------------

if (!binExists(VALIDATOR) || !binExists(KEYGEN) || !binExists(SOLANA)) {
  skip(
    `agave release binaries not found in ${AGAVE_BIN} (need solana-test-validator, solana-keygen, solana). ` +
      'Set AGAVE_BIN to a directory holding them.',
  );
}
if (!binExists(REEFLOG)) {
  skip(`reeflog binary not built: ${REEFLOG} (cargo build --release)`);
}

// Fresh state: a ledger or journal left by a previous run would make every
// "first time" claim below a claim about the last run.
for (const p of [LEDGER, PRE_FORK, DB, REORG_CURSOR, REORG_INSPECT, CONSUMER_CURSOR, LEDGER_JSON]) {
  fs.rmSync(p, { recursive: true, force: true });
}
fs.mkdirSync(DIR, { recursive: true });

let validator = null;
const report = {
  at: new Date().toISOString(),
  rpc: RPC,
  program: PROGRAM,
  checks: [],
  facts: {},
};

try {
  // ---- 1. a real chain ---------------------------------------------------
  validator = startValidator(LEDGER, path.join(DIR, 'validator-1.log'));
  const up = await waitHealth();
  check('validator_starts', up, up ? `local agave chain answering on ${RPC}` : 'never became healthy');
  if (!up) throw new Error('validator did not start');

  const payerFile = path.join(DIR, 'payer.json');
  const destFile = path.join(DIR, 'dest.json');
  spawnSync(KEYGEN, ['new', '--no-bip39-passphrase', '--silent', '-o', payerFile, '--force']);
  spawnSync(KEYGEN, ['new', '--no-bip39-passphrase', '--silent', '-o', destFile, '--force']);
  const payerPub = run(KEYGEN, ['pubkey', payerFile]).out.trim();
  const destPub = run(KEYGEN, ['pubkey', destFile]).out.trim();

  // solana airdrop takes <AMOUNT> [RECIPIENT_ADDRESS]
  const air = sol('airdrop', ['10', payerFile]);
  let balance = 0;
  for (let attempt = 0; attempt < 5 && balance < 2; attempt++) {
    for (let i = 0; i < 30 && balance < 2; i++) {
      const b = await rpc('getBalance', [payerPub, { commitment: 'finalized' }], 1);
      balance = (b.result && b.result.value) || 0;
      if (balance < 2) await new Promise((s) => setTimeout(s, 1000));
    }
    if (balance < 2) sol('airdrop', ['10', payerFile]);
  }
  check(
    'payer_funded',
    balance >= 2,
    `${balance / 1e9} SOL (${air.out.trim().split('\n').slice(-1)[0] || air.err.trim().slice(-120) || ''})`,
  );

  const airSig = extractSig(air.out) || extractSig(air.err);
  if (airSig) await waitFinalized(airSig);

  // ---- 2. the survivor transaction, ingested by a RUNNING daemon ---------
  const init = run(REEFLOG, ['init', '--db', DB, '--program', PROGRAM]);
  check('journal_initialized', init.code === 0, init.out.trim() || init.err.trim());

  // The daemon must be alive BEFORE the transaction happens: an empty journal
  // has no tail boundary and no seal window, so a daemon started afterwards
  // can never learn about an event that already happened. The survivor has to
  // be in the journal for the post-fork "still intact" control to mean
  // anything at all.
  const d1 = startProc(REEFLOG, daemonArgs(30));
  await new Promise((r) => setTimeout(r, 5000));

  const sigA = sol('transfer', [destPub, '1', '--allow-unfunded-recipient', '--keypair', payerFile]);
  const txA = extractSig(sigA.out) || extractSig(sigA.err);
  check('survivor_transaction_sent', !!txA, txA || `${sigA.out}${sigA.err}`.trim().slice(-400));
  if (!txA) throw new Error('could not send the survivor transaction');

  const statusA = await waitFinalized(txA);
  check('survivor_transaction_reaches_finalized', !!statusA, statusA ? statusA.detail || `slot=${statusA.slot}` : 'never finalized');
  if (!statusA) throw new Error('survivor transaction never finalized');

  const rootA = await waitRootPast(statusA.slot);
  check(
    'finalized_root_past_the_survivor_slot',
    typeof rootA === 'number' && rootA > statusA.slot,
    `root=${rootA} tx_slot=${statusA.slot}`,
  );

  // CONTROL before the fork: both independent sources must serve it.
  const txAInfo = await getTx(txA);
  check('both_sources_serve_the_survivor_before_the_fork', txAInfo.state === 'served', txAInfo.detail);
  check(
    'survivor_transaction_succeeded_on_chain',
    txAInfo.value && txAInfo.value.meta && txAInfo.value.meta.err === null,
    txAInfo.value ? `meta.err=${JSON.stringify(txAInfo.value.meta.err)}` : 'no tx object',
  );

  const rowA = await waitFor(() => {
    const r = rowFor(DB, txA);
    return r && r.seal === 'finalized' ? r : null;
  }, 90000);
  check(
    'survivor_arrived_over_the_socket_and_was_sealed_finalized',
    !!rowA,
    rowA ? `seq=${rowA.seq} source=${rowA.source} seal=${rowA.seal}` : 'row missing or not sealed',
  );

  const code1 = await d1.done;
  const s1 = parseJson(d1.state.out);
  check(
    'daemon_phase_1_completed_cleanly',
    code1 === 0 && (s1.ws_new || 0) >= 1 && (s1.seal_passes || 0) >= 1,
    `ws_new=${s1.ws_new} seal_passes=${s1.seal_passes} reorg_passes=${s1.reorg_passes} reorg_reverted=${s1.reorg_reverted} exit=${code1}`,
  );

  // ---- 3. snapshot AFTER the survivor (it must survive the fork) ---------
  await stopValidator(validator);
  validator = null;
  fs.cpSync(LEDGER, PRE_FORK, { recursive: true });
  check(
    'ledger_snapshot_taken_after_the_survivor',
    fs.existsSync(path.join(PRE_FORK, 'rocksdb')),
    `rocksdb copied at root=${rootA} (survivor slot ${statusA.slot} is inside the snapshot)`,
  );

  validator = startValidator(LEDGER, path.join(DIR, 'validator-2.log'));
  const up2 = await waitHealth();
  check('validator_resumes_after_snapshot', up2, up2 ? 'healthy' : 'never healthy');
  if (!up2) throw new Error('validator did not resume');

  const bAfter = await rpc('getBalance', [payerPub, { commitment: 'finalized' }]);
  const balAfter = (bAfter.result && bAfter.result.value) || 0;
  check('funding_survived_the_ledger_snapshot', balAfter >= 2, `${balAfter / 1e9} SOL after resume`);

  // ---- 4. the transaction the fork will unmake ---------------------------
  const d2 = startProc(REEFLOG, daemonArgs(40));
  await new Promise((r) => setTimeout(r, 5000));

  const sigB = sol('transfer', [destPub, '2', '--allow-unfunded-recipient', '--keypair', payerFile]);
  const txB = extractSig(sigB.out) || extractSig(sigB.err);
  check('forked_transaction_sent', !!txB, txB || `${sigB.out}${sigB.err}`.trim().slice(-400));
  if (!txB) throw new Error('could not send the forked transaction');

  const statusB = await waitFinalized(txB);
  check('forked_transaction_reaches_finalized', !!statusB, statusB ? `slot=${statusB.slot}` : 'never finalized');
  if (!statusB) throw new Error('forked transaction never finalized');
  const forkedSlot = statusB.slot;

  const rootB = await waitRootPast(forkedSlot);
  check(
    'finalized_root_past_the_forked_slot',
    typeof rootB === 'number' && rootB > forkedSlot,
    `root=${rootB} tx_slot=${forkedSlot}`,
  );

  // The block at the forked slot, recorded BEFORE the rollback: the fork is
  // proven when the same slot number holds a different block afterwards.
  const blockBefore = await getBlockHash(forkedSlot);
  check(
    'recorded_the_block_at_the_forked_slot_before_the_fork',
    blockBefore.state === 'ok',
    `${blockBefore.detail} (slot ${forkedSlot})`,
  );
  const survivorBlockBefore = await getBlockHash(statusA.slot);

  // CONTROL: with the chain intact, both sources serve it too.
  const txBInfo = await getTx(txB);
  const stBInfo = await getStatus(txB);
  check(
    'both_sources_serve_the_forked_tx_before_the_fork',
    txBInfo.state === 'served' && stBInfo.state === 'served',
    `getTransaction=${txBInfo.state} getSignatureStatuses=${stBInfo.state}`,
  );

  const rowB = await waitFor(() => {
    const r = rowFor(DB, txB);
    return r && r.seal === 'finalized' ? r : null;
  }, 90000);
  check(
    'forked_event_arrived_over_the_socket_and_was_sealed_finalized',
    !!rowB,
    rowB ? `seq=${rowB.seq} source=${rowB.source} seal=${rowB.seal}` : 'row missing or not sealed',
  );
  check('the_forked_event_arrived_over_the_socket', rowB && rowB.source === 'ws', rowB ? `source=${rowB.source}` : 'row missing');

  const rowsBoth = journalRows(DB);
  const rowA2 = rowsBoth.find((r) => r.signature === txA);
  const rowB2 = rowsBoth.find((r) => r.signature === txB);
  check(
    'journal_holds_both_real_transactions',
    !!rowA2 && !!rowB2,
    `A=${rowA2 ? `seq=${rowA2.seq} source=${rowA2.source} seal=${rowA2.seal}` : 'missing'} B=${rowB2 ? `seq=${rowB2.seq} source=${rowB2.source} seal=${rowB2.seal}` : 'missing'}`,
  );

  const stBefore = reeflog(['status', '--db', DB]);
  check(
    'journal_promised_both_as_final',
    stBefore.json && stBefore.json.finalized >= 2 && stBefore.json.reverted === 0,
    `events=${stBefore.json && stBefore.json.events} finalized=${stBefore.json && stBefore.json.finalized} reverted=${stBefore.json && stBefore.json.reverted}`,
  );

  // CONTROL 1: with the chain intact, the detector must find nothing. A
  // false positive here would make a consumer undo a real fill.
  const pre = reeflog(['reorg', 'check', '--db', DB, '--rpc', RPC]);
  check(
    'control_no_false_positive_while_the_chain_is_intact',
    pre.json && pre.json.reverted === 0 && pre.json.considered >= 2 && pre.json.complete === true,
    `reverted=${pre.json && pre.json.reverted} considered=${pre.json && pre.json.considered} intact=${pre.json && pre.json.intact} complete=${pre.json && pre.json.complete} exit=${pre.code}`,
  );

  const code2 = await d2.done;
  const s2 = parseJson(d2.state.out);
  check(
    'control_daemon_found_no_reverts_while_the_chain_was_intact',
    code2 === 0 && (s2.reorg_reverted || 0) === 0 && (s2.reorg_passes || 0) >= 1,
    `reorg_reverted=${s2.reorg_reverted} considered=${s2.reorg_considered} passes=${s2.reorg_passes} incomplete=${s2.reorg_incomplete} exit=${code2}`,
  );

  // ---- 5. THE REAL FORK --------------------------------------------------
  // Roll the chain back to the copy taken after the survivor. The survivor's
  // block is inside that copy, so it survives; the second transaction's block
  // is not, so the chain re-produces those slots without it.
  await stopValidator(validator);
  validator = null;
  fs.rmSync(LEDGER, { recursive: true, force: true });
  fs.renameSync(PRE_FORK, LEDGER);
  validator = startValidator(LEDGER, path.join(DIR, 'validator-3.log'));
  const up3 = await waitHealth();
  check('validator_restarts_on_the_rolled_back_ledger', up3, up3 ? 'healthy after rollback' : 'never healthy again');
  if (!up3) throw new Error('validator did not restart after the rollback');

  // The chain must re-produce the forked slot before we may claim the block
  // there changed: until then the slot simply does not exist on this chain.
  const newRoot = await waitRootPast(forkedSlot);
  check(
    'chain_re_produced_the_slots_past_the_fork',
    typeof newRoot === 'number',
    `finalized root now ${newRoot} (forked slot was ${forkedSlot})`,
  );

  // ---- 6. both independent sources must now refuse it ---------------------
  const stAfter = await getStatus(txB);
  const txAfter = await getTx(txB);
  check(
    'status_source_refuses_after_the_fork',
    stAfter.state === 'refused',
    `getSignatureStatuses=${stAfter.state} (${stAfter.detail})`,
  );
  check(
    'transaction_source_refuses_after_the_fork',
    txAfter.state === 'refused',
    `getTransaction=${txAfter.state} (${txAfter.detail})`,
  );

  // NEGATIVE CONTROL: the survivor must still be there, with the same block.
  const survivorAfter = await getTx(txA);
  check(
    'the_pre_rollback_transaction_is_still_served',
    survivorAfter.state === 'served',
    survivorAfter.state === 'served' ? 'survived the rollback (control)' : `${survivorAfter.detail} — control broken`,
  );

  const survivorBlockAfter = await getBlockHash(statusA.slot);
  check(
    'the_survivor_kept_the_same_block_across_the_fork',
    survivorBlockBefore.state === 'ok' &&
      survivorBlockAfter.state === 'ok' &&
      survivorBlockAfter.hash === survivorBlockBefore.hash,
    `before=${survivorBlockBefore.hash} after=${survivorBlockAfter.hash}`,
  );

  const blockAfter = await getBlockHash(forkedSlot);
  check(
    'the_forked_slot_has_a_different_block_now',
    blockBefore.state === 'ok' && blockAfter.state === 'ok' && blockAfter.hash !== blockBefore.hash,
    `slot=${forkedSlot} before=${blockBefore.hash} after=${blockAfter.hash} (${blockAfter.detail})`,
  );

  // The walk the repair pass uses must no longer return the forked
  // signature, otherwise the mailbox would re-learn it as if it were new —
  // and it must still return the survivor.
  const walk = await rpc('getSignaturesForAddress', [PROGRAM, { limit: 500, commitment: 'finalized', searchTransactionHistory: true }]);
  const listed = (walk.result || []).map((r) => r.signature);
  check(
    'the_signature_walk_no_longer_returns_the_forked_signature',
    !!txB && !listed.includes(txB),
    `listed=${listed.length} contains_forked=${listed.includes(txB)}`,
  );
  check(
    'the_signature_walk_still_returns_the_survivor',
    !!txA && listed.includes(txA),
    `contains_survivor=${listed.includes(txA)}`,
  );

  // ---- 7. the RUNNING daemon detects the fork, then a new tx arrives ------
  // Detection happens inside the product, not in a one-shot CLI call: the
  // periodic reorg pass is what a real consumer runs.
  const d3 = startProc(REEFLOG, daemonArgs(DAEMON_SECONDS));
  const detected = await waitFor(() => /REVERTED/.test(d3.state.err), 90000);
  check(
    'the_running_daemon_detects_the_real_fork',
    !!detected,
    detected
      ? (d3.state.err.match(/reeflog: [^\n]*REVERTED[^\n]*/) || ['matched'])[0]
      : `no REVERTED line within timeout; stderr tail: ${d3.state.err.slice(-400)}`,
  );

  // ---- 8. NEGATIVE CONTROL: a new transaction on the forked chain ---------
  await new Promise((r) => setTimeout(r, 3000));
  const sigC = sol('transfer', [destPub, '3', '--allow-unfunded-recipient', '--keypair', payerFile]);
  const txC = extractSig(sigC.out) || extractSig(sigC.err);
  const statusC = txC ? await waitFinalized(txC) : null;
  check('a_new_transaction_finalizes_on_the_forked_chain', !!statusC, statusC ? `slot=${statusC.slot}` : 'never finalized');

  const rowC = await waitFor(() => {
    const r = rowFor(DB, txC);
    return r && r.seal === 'finalized' ? r : null;
  }, 60000);
  check(
    'the_new_transaction_is_in_the_journal_sealed_finalized',
    !!rowC,
    rowC ? `seq=${rowC.seq} source=${rowC.source} seal=${rowC.seal}` : 'row missing or not finalized',
  );

  const code3 = await d3.done;
  const s3 = parseJson(d3.state.out);
  check(
    'the_daemons_own_reorg_pass_reports_exactly_the_real_fork',
    code3 === 0 && (s3.reorg_reverted || 0) === 1,
    `reorg_reverted=${s3.reorg_reverted} considered=${s3.reorg_considered} passes=${s3.reorg_passes} incomplete=${s3.reorg_incomplete} exit=${code3}`,
  );

  const rowsFinal = journalRows(DB);
  const rowA3 = rowsFinal.find((r) => r.signature === txA);
  const rowB3 = rowsFinal.find((r) => r.signature === txB);
  const rowC3 = rowsFinal.find((r) => r.signature === txC);
  check(
    'the_journal_holds_all_three_real_transactions_with_the_right_verdicts',
    rowA3 && rowA3.seal === 'finalized' && rowB3 && rowB3.seal === 'reverted' && rowC3 && rowC3.seal === 'finalized',
    `A=${rowA3 && rowA3.seal} B=${rowB3 && rowB3.seal} C=${rowC3 && rowC3.seal}`,
  );

  const statusFinal = reeflog(['status', '--db', DB]);
  check(
    'exactly_one_reorg_is_recorded_in_the_end',
    statusFinal.json && statusFinal.json.reverted === 1,
    `reverted=${statusFinal.json && statusFinal.json.reverted} events=${statusFinal.json && statusFinal.json.events}`,
  );

  // The reorg record itself, read through its own cursor so the consumer's
  // cursor further down stays untouched.
  const rInspect = run(REEFLOG, ['reorg', 'tail', '--db', DB, '--cursor', REORG_INSPECT, '--limit', '100']);
  const rInspectRows = rInspect.out
    .split('\n')
    .filter(Boolean)
    .map((l) => {
      try {
        return JSON.parse(l);
      } catch {
        return null;
      }
    })
    .filter(Boolean);
  check(
    'the_reorg_record_names_the_real_forked_signature',
    rInspectRows.length === 1 && rInspectRows[0].signature === txB,
    rInspectRows.length === 1
      ? `${rInspectRows[0].signature} status=${rInspectRows[0].status_source} tx=${rInspectRows[0].tx_source}`
      : `records=${rInspectRows.length}`,
  );

  // CONTROL 2: a second pass over the same sealed rows must not re-report
  // the recorded fork, and must still call the survivor intact.
  const post = reeflog(['reorg', 'check', '--db', DB, '--rpc', RPC]);
  check(
    'control_after_the_fork_the_survivor_is_intact_and_nothing_is_re_reported',
    post.json &&
      post.json.reverted === 0 &&
      post.json.intact >= 2 &&
      post.json.complete === true &&
      post.json.journal &&
      post.json.journal.reorg_records === 1,
    `reverted=${post.json && post.json.reverted} intact=${post.json && post.json.intact} considered=${post.json && post.json.considered} reorg_records=${post.json && post.json.journal && post.json.journal.reorg_records} complete=${post.json && post.json.complete} exit=${post.code}`,
  );

  // ---- 9. the retraction reaches the consumer exactly once ----------------
  const r1 = run(REEFLOG, ['reorg', 'tail', '--db', DB, '--cursor', REORG_CURSOR, '--limit', '100']);
  const rr1 = r1.out
    .split('\n')
    .filter(Boolean)
    .map((l) => {
      try {
        return JSON.parse(l);
      } catch {
        return null;
      }
    })
    .filter(Boolean);
  check('retraction_delivered_once', rr1.length === 1, `rows=${rr1.length}`);
  check('the_retraction_names_the_real_signature', rr1.length === 1 && rr1[0].signature === txB, rr1.length ? rr1[0].signature : 'none');
  const r2 = run(REEFLOG, ['reorg', 'tail', '--db', DB, '--cursor', REORG_CURSOR, '--limit', '100']);
  check('second_read_delivers_nothing', r2.out.split('\n').filter(Boolean).length === 0, `rows=${r2.out.split('\n').filter(Boolean).length}`);

  // ---- 10. the consumer undoes exactly what it acted on -------------------
  // The consumer read the event over `reeflog tail` before the fork (the same
  // rows above) and recorded it in its ledger. The "action" is modeled by the
  // ledger entry built from that real row: the local chain cannot produce a
  // Raydium fill, and the fill-decoding side of the join is already proven by
  // e2e-retraction against real mainnet logs. What is NOT modeled is the
  // join itself — that is reeflog's own reorg tail against this consumer's
  // ledger, on a real forked-out signature.
  const actingRow = rowB2;
  if (actingRow) {
    fs.mkdirSync(path.dirname(LEDGER_JSON), { recursive: true });
    fs.writeFileSync(
      LEDGER_JSON,
      JSON.stringify({
        fills: {
          [actingRow.signature]: {
            signature: actingRow.signature,
            slot: actingRow.slot,
            err: actingRow.err ?? null,
            source: actingRow.source,
            seal: actingRow.seal,
            seq: actingRow.seq,
            read_at: Date.now(),
            note: 'consumer acted on this event before the fork',
          },
        },
        retracted: {},
      }),
    );
  }
  const joined = await watchRetractions({
    db: DB,
    cursor: CONSUMER_CURSOR,
    bin: REEFLOG,
    limit: 100,
    ledgerPath: LEDGER_JSON,
  });
  check('consumer_sees_the_retraction', joined.retractions.length === 1, `retractions=${joined.retractions.length} exit=${joined.exit}`);
  check(
    'the_retraction_is_undoable_for_this_consumer',
    joined.retractions.length === 1 && joined.retractions[0].undoable === true,
    JSON.stringify((joined.retractions[0] || {}).signature || null),
  );
  const afterLedger = JSON.parse(fs.readFileSync(LEDGER_JSON, 'utf8'));
  check(
    'the_acted_event_is_undone_and_remembered_as_retracted',
    !afterLedger.fills[txB] && !!afterLedger.retracted[txB],
    `fills=${Object.keys(afterLedger.fills).length} retracted=${Object.keys(afterLedger.retracted).length}`,
  );

  report.facts = {
    program: PROGRAM,
    survivor_transaction: txA,
    survivor_slot: statusA && statusA.slot,
    survivor_block_before: survivorBlockBefore.hash,
    survivor_block_after: survivorBlockAfter.hash,
    forked_transaction: txB,
    forked_slot: forkedSlot,
    forked_block_before: blockBefore.hash,
    forked_block_after: blockAfter.hash,
    new_transaction_after_fork: txC,
    finalized_root_before_fork: rootB,
    finalized_root_after_fork: newRoot,
    daemon_phase_1: { ws_new: s1.ws_new, seal_passes: s1.seal_passes, reorg_reverted: s1.reorg_reverted, exit: code1 },
    daemon_phase_2: { ws_new: s2.ws_new, seal_passes: s2.seal_passes, reorg_passes: s2.reorg_passes, reorg_reverted: s2.reorg_reverted, exit: code2 },
    daemon_final: {
      ws_new: s3.ws_new,
      seal_passes: s3.seal_passes,
      reorg_passes: s3.reorg_passes,
      reorg_considered: s3.reorg_considered,
      reorg_reverted: s3.reorg_reverted,
      reorg_incomplete: s3.reorg_incomplete,
      exit: code3,
    },
    reorg_check_intact_chain: pre.json,
    reorg_check_after_fork: post.json,
    journal_status: statusFinal.json,
    ledger_dir: DIR,
    daemon_seconds: DAEMON_SECONDS,
  };
} catch (e) {
  report.error = String((e && e.stack) || e);
  console.error(`ERROR ${report.error}`);
} finally {
  if (validator) await stopValidator(validator);
}

report.checks = results;
report.verdict = results.length > 0 && results.every((r) => r.pass) ? 'passed' : 'failed';
report.finishedAt = new Date().toISOString();
fs.mkdirSync(path.dirname(REPORT), { recursive: true });
fs.writeFileSync(REPORT, JSON.stringify(report, null, 2));
console.log(`\nverdict: ${report.verdict} (${results.filter((r) => r.pass).length}/${results.length} checks passed)`);
console.log(`report: ${REPORT}`);
process.exit(report.verdict === 'passed' ? 0 : 1);
