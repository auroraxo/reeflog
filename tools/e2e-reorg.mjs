// End-to-end probe of reorg detection on live mainnet.
//
// The dangerous direction is the false positive. A detector that says "reverted"
// for a transaction that is alive on chain is worse than no detector: a
// consumer would undo a real fill. So the harness proves BOTH directions:
//
//  A. NEGATIVE (the falsifiable direction): a journal full of REAL, finalized
//     transactions must produce ZERO reorgs. If a single one flips, the
//     two-source rule is not holding.
//
//  B. POSITIVE: a well-formed signature that the chain does not know (the
//     exact shape of a forked-out transaction) must be detected as reverted
//     and delivered on the reorg channel — once, with a strict cursor.
//
//  C. HONESTY: an unreachable endpoint must produce an incomplete pass and a
//     non-zero exit, never a revert and never exit 0.
//
// The planted row is a genuine, well-formed 64-byte signature that was never
// broadcast (one byte of a real signature flipped, then re-encoded in
// base58). It has the same wire shape as a transaction that was forked out,
// which is what the detector has to recognize.
import { spawn } from 'node:child_process';
import fs from 'node:fs';
import path from 'node:path';
import { DatabaseSync } from 'node:sqlite';

const RPC = 'https://api.mainnet-beta.solana.com';
const PROGRAM = '675kPX9MHTjS2zt1qfr1NYHuzeLXfQM9H24wFSUt1Mp8';
const BIN = path.resolve('./target/release/reeflog');
const DIR = '/tmp/reeflog-reorg-e2e';
const DB = path.join(DIR, 'reeflog.db');

async function rpc(method, params, tries = 8) {
  let last;
  for (let i = 0; i < tries; i++) {
    if (i) await new Promise((r) => setTimeout(r, 800 * i));
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

// base58 for a 64-byte signature: flip one byte of a real one, re-encode.
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

// --- harvest a real window ------------------------------------------------
let rawRows = await rpc('getSignaturesForAddress', [PROGRAM, { limit: 200, commitment: 'finalized' }]);
const cutSlot = rawRows[rawRows.length - 1].slot;
const rows = rawRows.filter((r) => r.slot > cutSlot);
console.log(`chain page: ${rawRows.length} raw, ${rows.length} clean (discarded partial slot ${cutSlot})`);

fs.rmSync(DIR, { recursive: true, force: true });
fs.mkdirSync(DIR, { recursive: true });

let r = await cli(['init', '--db', DB, '--program', PROGRAM]);
if (r.code !== 0) { console.error('INIT FAILED', r); process.exit(2); }

// Plant the real rows as sealed 'finalized' events — exactly the state a
// consumer has been promised. The newest slot is excluded: a transaction
// finalizing there after the harvest would be a race, not a reorg.
const ceilSlot = rows[0].slot;
const plantedReal = [];
{
  const db = new DatabaseSync(DB);
  const ins = db.prepare('INSERT OR IGNORE INTO events(signature,slot,err,logs,source,seal_status,seal_seen,first_seen,payload_state) VALUES(?,?,?,?,?,?,?,?,?)');
  for (const row of rows) {
    if (row.slot >= ceilSlot - 1) continue;
    const err = row.err == null ? null : JSON.stringify(row.err);
    ins.run(row.signature, row.slot, err, '[]', 'ws', 'finalized', Date.now(), Date.now(), 'held');
    plantedReal.push(row.signature);
  }
  db.close();
}
console.log(`planted ${plantedReal.length} real finalized events`);

// --- A. NEGATIVE: alive transactions must NOT become reorgs --------------
const c1 = await cli(['reorg', 'check', '--db', DB, '--limit', '500']);
const c1j = json(c1.out);
console.log(`REORG CHECK#1 exit=${c1.code} complete=${c1j.complete} considered=${c1j.considered} intact=${c1j.intact} reverted=${c1j.reverted} invalid=${c1j.invalid} incomplete=${c1j.incomplete}`);
check('alive_transactions_never_become_reorgs',
  c1.code === 0 && c1j.complete === true && c1j.reverted === 0 && c1j.considered === plantedReal.length,
  `reverted=${c1j.reverted} of ${c1j.considered} considered`);

// --- B. POSITIVE: a signature the chain does not know is a reorg ---------
const template = plantedReal[0];
const bytes = b58decode(template);
bytes[10] ^= 0x01;
const ghost = b58encode(bytes);
console.log(`ghost signature (well-formed, never broadcast): ${ghost.slice(0, 16)}…`);

// The chain must agree it does not know this signature — otherwise the test
// would be measuring nothing.
const ghostStatus = await rpc('getSignatureStatuses', [[ghost], { searchTransactionHistory: true }]);
const ghostTx = await rpc('getTransaction', [ghost, { encoding: 'json', maxSupportedTransactionVersion: 1, commitment: 'finalized' }]);
check('fixture_precondition_both_sources_refuse_the_ghost',
  ghostStatus.value[0] === null && ghostTx === null,
  `status=${JSON.stringify(ghostStatus.value[0])} tx=${ghostTx === null ? 'null' : 'FOUND'}`);

// Plant it as a sealed event, exactly like a real one the daemon delivered.
{
  const db = new DatabaseSync(DB);
  db.prepare('INSERT OR IGNORE INTO events(signature,slot,err,logs,source,seal_status,seal_seen,first_seen,payload_state) VALUES(?,?,?,?,?,?,?,?,?)')
    .run(ghost, rows[0].slot - 1, null, '[]', 'ws', 'finalized', Date.now(), Date.now(), 'held');
  db.close();
}

const c2 = await cli(['reorg', 'check', '--db', DB, '--limit', '500']);
const c2j = json(c2.out);
console.log(`REORG CHECK#2 exit=${c2.code} complete=${c2j.complete} considered=${c2j.considered} intact=${c2j.intact} reverted=${c2j.reverted}`);
check('forked_out_signature_is_detected_as_reverted',
  c2.code === 0 && c2j.reverted === 1 && c2j.reverted_sample.includes(ghost),
  `reverted=${c2j.reverted} sample=${JSON.stringify(c2j.reverted_sample)}`);

// The delivery: the reorg channel must hand it to a consumer exactly once.
const cur = path.join(DIR, 'reorg-cursor.txt');
const t1 = await cli(['reorg', 'tail', '--db', DB, '--cursor', cur]);
const t1lines = t1.out.trim() ? t1.out.trim().split('\n').map(json) : [];
console.log(`REORG TAIL#1 lines=${t1lines.length}`);
check('reorg_is_delivered_on_its_own_channel',
  t1lines.length === 1 && t1lines[0].signature === ghost && t1lines[0].status_source === 'refused' && t1lines[0].tx_source === 'refused',
  JSON.stringify(t1lines[0] || {}));

// Strict cursor: a second read must deliver nothing.
const t2 = await cli(['reorg', 'tail', '--db', DB, '--cursor', cur]);
const t2lines = t2.out.trim() ? t2.out.trim().split('\n') : [];
check('reorg_channel_never_replays', t2lines.length === 0, `second read delivered ${t2lines.length}`);

// The event itself keeps its identity and is not delivered twice: `tail`
// reports the current seal.
const evCur = path.join(DIR, 'event-cursor.txt');
const ev1 = await cli(['tail', '--db', DB, '--cursor', evCur, '--limit', '10000']);
const evLines = ev1.out.trim() ? ev1.out.trim().split('\n').map(json) : [];
const ghostRow = evLines.find((x) => x.signature === ghost);
check('the_retracted_event_keeps_its_identity_and_reports_its_seal',
  !!ghostRow && ghostRow.seal === 'reverted' && evLines.filter((x) => x.signature === ghost).length === 1,
  `seal=${ghostRow ? ghostRow.seal : 'MISSING'} occurrences=${evLines.filter((x) => x.signature === ghost).length}`);

// --- C. HONESTY: an unreachable endpoint is not a verdict ----------------
const c3 = await cli(['reorg', 'check', '--db', DB, '--limit', '500', '--rpc', 'http://127.0.0.1:9']);
const c3j = json(c3.out);
console.log(`REORG CHECK#3 (dead endpoint) exit=${c3.code} complete=${c3j.complete} incomplete=${c3j.incomplete} reverted=${c3j.reverted}`);
check('an_unreachable_endpoint_never_records_a_revert',
  c3.code !== 0 && c3j.complete === false && c3j.reverted === 0,
  `exit=${c3.code} complete=${c3j.complete} reverted=${c3j.reverted}`);

// The reorg count must still be exactly the one real detection.
const st = await cli(['status', '--db', DB]);
const stj = json(st.out);
console.log(`STATUS reverted=${stj.reverted} events=${stj.events}`);
check('status_reports_exactly_one_revert', stj.reverted === 1, `reverted=${stj.reverted}`);

const verdict = results.every(Boolean);
console.log(verdict ? 'REORG E2E: ALL PASS' : 'REORG E2E: FAILURE');
process.exit(verdict ? 0 : 1);
