// Falsifying end-to-end proof of the retraction path, from the CONSUMER's
// side, against live mainnet.
//
// The daemon-side detector is already proven (tools/e2e-reorg.mjs): two
// sources must refuse the same transaction before anything is called a
// revert. What was NOT proven is the part a product is judged on: that a
// consumer who already acted on an event finds out that the event was taken
// back, and can undo exactly that action — and only that action.
//
// The chain rarely forks on demand, so a positive reorg cannot be waited for.
// The harness therefore separates two claims and proves each honestly:
//
//   A. NEGATIVE, live: during a real run against mainnet the daemon's own
//      reorg check must find ZERO reverts. A false positive is worse than no
//      detector: a consumer would undo a real fill. The run must also show
//      that the check actually RAN (reorg_passes > 0, rows considered > 0),
//      because "0 reverts" from a check that never executed proves nothing.
//
//   B. POSITIVE, planted: a well-formed signature the chain does not know is
//      planted into the journal exactly as the daemon would have delivered it
//      (sealed 'finalized', with the real logs of a real transaction, so a
//      consumer that reads it decodes a real fill). The chain must refuse it
//      in BOTH sources. The daemon's reorg check must then mark it reverted,
//      and the retraction must arrive on the retraction channel — joined
//      against the consumer's own ledger.
//
//   C. THE LEDGER: the consumer must be able to answer "which of MY fills
//      must be undone?". A retraction for a signature this consumer never
//      acted on must be reported as such and must NOT un-fill some other
//      fill. The undo must carry the original fill's data, so the consumer can
//      reverse precisely what it did.
//
//   D. NO REPLAY: reading the retraction channel twice must deliver the
//      retraction once. A retraction applied twice is a double reversal.
//
//   E. THE DAEMON LOOP: the reorg check must run inside `reeflog start`
//      without the operator asking for it. The summary must report the
//      passes and the rows they considered.
//
// The fixture is planted with the logs of a REAL transaction, because a
// retraction is only meaningful for an event the consumer could read. A
// planted row with empty logs would decode to no fill, and the ledger join
// would prove nothing.
import { spawn } from 'node:child_process';
import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { DatabaseSync } from 'node:sqlite';
import { fillFromRow, watch, watchRetractions, loadLedger } from './fill-watcher.mjs';

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const ROOT = path.resolve(__dirname, '..');
const BIN = path.join(ROOT, 'target', 'release', 'reeflog');
const RPC = 'https://api.mainnet-beta.solana.com';
const PROGRAM = '675kPX9MHTjS2zt1qfr1NYHuzeLXfQM9H24wFSUt1Mp8'; // Raydium AMM v4
const DIR = process.env.REEFLOG_RETRACTION_DIR || '/tmp/reeflog-retraction';
const DB = path.join(DIR, 'reeflog.db');
const CURSOR = path.join(DIR, 'cursor.txt');
const REORG_CURSOR = path.join(DIR, 'reorg-cursor.txt');
const LEDGER = path.join(DIR, 'ledger.json');
const SECONDS = Number(process.env.REEFLOG_RETRACTION_SECONDS || 30);

async function rpc(method, params, tries = 8) {
  let last;
  for (let i = 0; i < tries; i++) {
    if (i) await new Promise((r) => setTimeout(r, 700 * i));
    try {
      const r = await fetch(RPC, {
        method: 'POST',
        headers: { 'content-type': 'application/json' },
        body: JSON.stringify({ jsonrpc: '2.0', id: 1, method, params }),
      });
      if (r.status === 429) { last = new Error('429'); continue; }
      const j = await r.json();
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
const json = (s) => { try { return JSON.parse(s); } catch { return { raw: s }; } };

const B58 = '123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz';
function b58decode(s) {
  let n = 0n;
  for (const c of s) n = n * 58n + BigInt(B58.indexOf(c));
  const bytes = [];
  while (n > 0n) { bytes.unshift(Number(n & 255n)); n >>= 8n; }
  for (const c of s) { if (c === '1') bytes.unshift(0); else break; }
  return Uint8Array.from(bytes);
}
function b58encode(bytes) {
  let n = 0n;
  for (const b of bytes) n = (n << 8n) | BigInt(b);
  let out = '';
  while (n > 0n) { out = B58[Number(n % 58n)] + out; n /= 58n; }
  for (const b of bytes) { if (b === 0) out = '1' + out; else break; }
  return out;
}

const results = [];
const check = (name, pass, detail) => {
  results.push(pass);
  console.log(`${pass ? 'PASS' : 'FAIL'} ${name}${detail ? ` — ${detail}` : ''}`);
};

fs.rmSync(DIR, { recursive: true, force: true });
fs.mkdirSync(DIR, { recursive: true });
{
  const r = await cli(['init', '--db', DB, '--program', PROGRAM]);
  if (r.code !== 0) { console.error('INIT FAILED', r); process.exit(2); }
}

// --- the daemon, with the reorg check running inside it -------------------
// Short cadences so the check actually fires inside the window; the point of
// the claim is that it runs unattended, not how often.
const t0 = Date.now();
const run = await cli([
  'start', '--db', DB, '--seconds', String(SECONDS), '--program', PROGRAM,
  '--seal-every', '5', '--reorg-every', '10', '--reorg-lookback', '2000',
  '--payload-limit', '20',
]);
const summary = json(run.out);
console.log(`daemon: exit=${run.code} events=${summary.journal?.events} ` +
  `seal_passes=${summary.seal_passes} reorg_passes=${summary.reorg_passes} ` +
  `reorg_considered=${summary.reorg_considered} reorg_reverted=${summary.reorg_reverted} ` +
  `reorg_incomplete_passes=${summary.reorg_incomplete_passes}`);
// Note on the exit code: `start` exits 3 when payload hydration could not
// finish (its documented rule — an unfinished pass must never look clean),
// which is unrelated to the reorg claim and is reported, not hidden. The
// reorg checks below therefore assert the summary's own fields.
console.log(`daemon hydrate: complete=${summary.hydrate?.complete} ` +
  `pending_left=${summary.hydrate?.pending_left} exit=${run.code}`);

// --- A. NEGATIVE, live ------------------------------------------------------
// The check must have run AND found nothing. Both halves matter: a check
// that never executed produces 0 reverts too, and that would prove nothing.
check('the_daemon_ran_the_reorg_check_itself',
  (summary.reorg_passes || 0) >= 1 && (summary.reorg_considered || 0) > 0 &&
  (summary.seal_passes || 0) >= 1,
  `seal_passes=${summary.seal_passes} reorg_passes=${summary.reorg_passes} ` +
  `considered=${summary.reorg_considered} (daemon exit=${run.code}, ` +
  `hydrate complete=${summary.hydrate?.complete})`);

check('alive_transactions_never_become_reorgs_live',
  (summary.reorg_reverted || 0) === 0,
  `reverted=${summary.reorg_reverted} of ${summary.reorg_considered} considered`);

// --- the consumer reads its fills and records them -------------------------
const w = await watch({ db: DB, cursor: CURSOR, bin: BIN, limit: 100000, ledgerPath: LEDGER });
console.log(`consumer: ${w.rows} rows -> ${w.fills.length} fills`);
const ledgerAfterRead = loadLedger(LEDGER);
check('the_consumer_recorded_its_fills_in_a_ledger',
  w.fills.length > 0 && Object.keys(ledgerAfterRead.fills).length === w.fills.length,
  `${Object.keys(ledgerAfterRead.fills).length} ledger entries for ${w.fills.length} fills`);

// --- B. POSITIVE, planted ---------------------------------------------------
// Take a REAL transaction from the journal — one the consumer already read
// and acted on — and turn it into a signature the chain does not know: flip
// one byte and re-encode. Same wire shape as a forked-out transaction, but a
// key the chain has never seen.
//
// The planted row carries the REAL logs of the real transaction, so a
// consumer reading it decodes a real fill — which is what makes the ledger
// join meaningful rather than a formality.
const templateRow = w.fills[0];
if (!templateRow) {
  console.error('no fill to use as a template — the run produced nothing');
  process.exit(2);
}
const realSig = templateRow.signature;
const bytes = b58decode(realSig);
bytes[10] ^= 0x01;
const ghost = b58encode(bytes);
console.log(`ghost signature (well-formed, never broadcast): ${ghost.slice(0, 16)}…`);

// The chain must refuse it in BOTH sources, else the test measures nothing.
const ghostStatus = await rpc('getSignatureStatuses', [[ghost], { searchTransactionHistory: true }]);
const ghostTx = await rpc('getTransaction', [ghost, { encoding: 'json', maxSupportedTransactionVersion: 1, commitment: 'finalized' }]);
check('fixture_precondition_both_sources_refuse_the_ghost',
  ghostStatus?.value?.[0] === null && ghostTx === null,
  `status=${JSON.stringify(ghostStatus?.value?.[0])} tx=${ghostTx === null ? 'null' : 'FOUND'}`);

// Plant it: same shape as a delivered, sealed, hydrated event — but with the
// ghost's own signature and the real logs, so it decodes to the same fill.
{
  const db = new DatabaseSync(DB);
  const src = db.prepare('SELECT slot, logs FROM events WHERE signature=?1').get(realSig);
  db.prepare('INSERT OR IGNORE INTO events(signature,slot,err,logs,source,seal_status,seal_seen,first_seen,payload_state) VALUES(?,?,?,?,?,?,?,?,?)')
    .run(ghost, src.slot - 1, null, src.logs, 'ws', 'finalized', Date.now(), Date.now(), 'held');
  db.close();
}

// The consumer reads the planted row too — it must decode to a fill, or the
// ledger join below would be a formality instead of a proof.
const w2 = await watch({ db: DB, cursor: CURSOR, bin: BIN, limit: 100000, ledgerPath: LEDGER });
const ghostFill = w2.fills.find((f) => f.signature === ghost);
check('the_planted_event_is_readable_and_decodes_to_a_fill',
  !!ghostFill && ghostFill.legs.length > 0,
  ghostFill ? `legs=${ghostFill.legs.length}` : 'the planted row did not decode');

const ledgerBefore = loadLedger(LEDGER);
check('the_ledger_now_holds_the_fill_that_will_be_retracted',
  !!ledgerBefore.fills[ghost],
  `ledger entries=${Object.keys(ledgerBefore.fills).length}`);

// --- the daemon's own check finds the revert --------------------------------
const check1 = await cli(['reorg', 'check', '--db', DB, '--limit', '2000']);
const c1 = json(check1.out);
console.log(`REORG CHECK exit=${check1.code} complete=${c1.complete} considered=${c1.considered} reverted=${c1.reverted}`);
check('the_detector_marks_the_planted_event_as_reverted',
  check1.code === 0 && c1.reverted >= 1 && (c1.reverted_sample || []).includes(ghost),
  `reverted=${c1.reverted} sample=${JSON.stringify(c1.reverted_sample)}`);

// --- C. THE LEDGER JOIN -----------------------------------------------------
const retr = await watchRetractions({ db: DB, cursor: REORG_CURSOR, bin: BIN, limit: 1000, ledgerPath: LEDGER });
console.log(`retraction channel: ${retr.rows} row(s), ${retr.retractions.filter((x) => x.undoable).length} undoable`);
const rec = retr.retractions.find((x) => x.signature === ghost);
check('the_retraction_reaches_the_consumer_on_its_own_channel',
  !!rec, rec ? `reorg_id=${rec.reorg_id}` : 'no retraction delivered');

check('the_retraction_is_marked_undoable_and_carries_the_original_fill',
  !!rec && rec.undoable === true && !!rec.fill &&
  rec.fill.signature === ghost &&
  rec.fill.legs.length > 0,
  rec ? `undoable=${rec.undoable} legs=${rec.fill?.legs?.length}` : 'no retraction');

const form = (v) => JSON.stringify(v, (_k, x) => (typeof x === 'bigint' ? x.toString() : x));
check('the_undo_carries_exactly_the_fill_the_consumer_acted_on',
  !!rec && !!rec.fill && form(rec.fill.legs) === form(ghostFill.legs),
  rec ? `legs match=${!!rec.fill && form(rec.fill.legs) === form(ghostFill.legs)}` : 'no retraction');

const ledgerAfter = loadLedger(LEDGER);
check('the_retracted_fill_leaves_the_open_ledger',
  !ledgerAfter.fills[ghost] && !!ledgerAfter.retracted[ghost],
  `open=${Object.keys(ledgerAfter.fills).length} undone=${Object.keys(ledgerAfter.retracted).length}`);

// --- a retraction for something this consumer never read --------------------
// It must be reported and must NOT un-fill another fill.
const foreign = b58encode((() => { const b = b58decode(realSig); b[12] ^= 0x02; return b; })());
{
  const db = new DatabaseSync(DB);
  const src = db.prepare('SELECT slot, logs FROM events WHERE signature=?1').get(realSig);
  db.prepare('INSERT OR IGNORE INTO events(signature,slot,err,logs,source,seal_status,seal_seen,first_seen,payload_state) VALUES(?,?,?,?,?,?,?,?,?)')
    .run(foreign, src.slot - 2, null, src.logs, 'ws', 'finalized', Date.now(), Date.now(), 'held');
  db.close();
}
await cli(['reorg', 'check', '--db', DB, '--limit', '2000']);
const retr2 = await watchRetractions({ db: DB, cursor: REORG_CURSOR, bin: BIN, limit: 1000, ledgerPath: LEDGER });
const foreignRec = retr2.retractions.find((x) => x.signature === foreign);
const ledgerAfter2 = loadLedger(LEDGER);
check('a_retraction_for_an_unread_fill_is_reported_but_not_unfilled',
  !!foreignRec && foreignRec.undoable === false &&
  Object.keys(ledgerAfter2.fills).length === Object.keys(ledgerAfter.fills).length,
  foreignRec
    ? `undoable=${foreignRec.undoable} open before=${Object.keys(ledgerAfter.fills).length} after=${Object.keys(ledgerAfter2.fills).length}`
    : 'no retraction delivered');

// --- D. NO REPLAY ------------------------------------------------------------
const retr3 = await watchRetractions({ db: DB, cursor: REORG_CURSOR, bin: BIN, limit: 1000, ledgerPath: LEDGER });
check('the_retraction_channel_never_replays',
  retr3.retractions.length === 0,
  `second read delivered ${retr3.retractions.length}`);

// --- E. the run's own summary accounts for the reorg work --------------------
// The status check must report exactly the reverts this harness planted: the
// ghost (the consumer acted on it) and the foreign one (it did not). A count
// of 1 would mean one of them went missing.
const journalStatus = json((await cli(['status', '--db', DB])).out);
check('status_reports_exactly_the_planted_reverts',
  journalStatus.reverted === 2,
  `reverted=${journalStatus.reverted} events=${journalStatus.events} (expected 2 planted)`);

// The retraction is visible in the event stream too: the event keeps its
// identity and reports its current seal.
const ev = await cli(['tail', '--db', DB, '--cursor', path.join(DIR, 'ev.txt'), '--limit', '100000']);
const evLines = ev.out.trim() ? ev.out.trim().split('\n').map(json) : [];
const ghostRow = evLines.find((x) => x.signature === ghost);
check('the_retracted_event_keeps_its_identity_and_reports_its_seal',
  !!ghostRow && ghostRow.seal === 'reverted' &&
  evLines.filter((x) => x.signature === ghost).length === 1,
  `seal=${ghostRow ? ghostRow.seal : 'MISSING'}`);

console.log(`\nmeasured: ${w.fills.length + w2.fills.length} fill(s) read, ` +
  `${retr.retractions.length + retr2.retractions.length} retraction(s) delivered, ` +
  `${retr.retractions.filter((x) => x.undoable).length} undoable, ` +
  `reorg_passes=${summary.reorg_passes} considered=${summary.reorg_considered}`);

const verdict = results.every(Boolean);
console.log(verdict ? 'RETRACTION E2E: ALL PASS' : 'RETRACTION E2E: FAILURE');
process.exit(verdict ? 0 : 1);
