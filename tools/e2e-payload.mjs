// End-to-end probe of the PAYLOAD contract on live mainnet.
//
// The signature contract (detect -> repair -> prove) is checked by
// e2e-repair.mjs. It deliberately writes rows the way `repair` writes them —
// signature, slot, error, NO logs — because that is the shape the chain's
// getSignaturesForAddress hands over. This harness asks the question that
// one does not: once the holes are filled, can a consumer actually read
// what happened?
//
// Checks:
//  1. Rows in the repair shape carry no payload and `tail` says so
//     (this is the defect the payload work fixes: 0 of 2095 rows delivered
//      payload before the fix).
//  2. `hydrate` fills exactly those rows, complete, exit 0.
//  3. The stored payload is byte-identical to an independent, freshly fetched
//     getTransaction for the same signature — not just "non-null".
//  4. `tail` delivers the payload to the consumer.
//  5. A signature the chain does not know is recorded `unavailable`, never
//     given an empty log array (an empty array claims "no logs", which is a
//     different statement from "no transaction").
//  6. An unfinished hydration pass must not exit 0 (rate limit / limit hit).
import { spawn } from 'node:child_process';
import fs from 'node:fs';
import path from 'node:path';

const RPC = 'https://api.mainnet-beta.solana.com';
const PROGRAM = '675kPX9MHTjS2zt1qfr1NYHuzeLXfQM9H24wFSUt1Mp8'; // Raydium AMM v4
const BIN = path.resolve('./target/release/reeflog');
const DIR = '/tmp/reeflog-payload-e2e';
const DB = path.join(DIR, 'reeflog.db');
const CURSOR = path.join(DIR, 'cursor.txt');
const FORGED = 'FoRgedSiGnatureThaTtHeChAiNnEvErKnOws1111111111111111111111111111111111111';
// A well-formed 64-byte signature that is not on chain: the forked-out shape
// (measured: answers `result: null`, no error — distinct from a malformed key,
// which the endpoint refuses with -32602).
const REAL = '5xqWc919Y199HowdFX7u9LdD2S6Ve9rM8SArRuofN6tVQGFnMPaykGx8FWXsPsPHAe16LVTYmc2ym48vHffxLJP2';
const ABSENT = REAL.slice(0, 10) + (REAL[10] === 'A' ? 'B' : 'A') + REAL.slice(11);

async function rpc(method, params, tries = 8) {
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
const json = (s) => { try { return JSON.parse(s); } catch { return { raw: s }; } };

const checks = [];
const push = (name, pass, detail) => checks.push({ name, pass, detail });

// --- setup: a journal with ONLY repair-shaped rows -------------------------
const rows = await rpc('getSignaturesForAddress', [PROGRAM, { limit: 40, commitment: 'finalized' }]);
console.log(`chain page: ${rows.length} rows, slots ${rows[rows.length - 1].slot}..${rows[0].slot}`);

fs.rmSync(DIR, { recursive: true, force: true });
fs.mkdirSync(DIR, { recursive: true });

let r = await cli(['init', '--db', DB, '--program', PROGRAM]);
if (r.code !== 0) { console.error('INIT FAILED', r); process.exit(2); }

const { DatabaseSync } = await import('node:sqlite');
{
  const db = new DatabaseSync(DB);
  const ins = db.prepare(
    'INSERT OR IGNORE INTO events(signature,slot,err,logs,source,seal_status,first_seen) VALUES(?,?,?,?,?,?,?)',
  );
  for (const row of rows) {
    const err = row.err == null ? null : JSON.stringify(row.err);
    // logs = NULL: exactly what `repair` and `backfill` write (signature walk).
    ins.run(row.signature, row.slot, err, null, 'repair', 'pending', Date.now());
  }
  // A row the chain has never heard of: malformed key (endpoint refuses it).
  ins.run(FORGED, rows[0].slot, null, null, 'repair', 'pending', Date.now());
  // A well-formed signature that is not on chain: the forked-out case.
  ins.run(ABSENT, rows[0].slot, null, null, 'repair', 'pending', Date.now());
  db.close();
}
console.log(`planted ${rows.length} repair-shaped rows + 1 malformed key + 1 chain-unknown`);

// --- 1. the defect as shipped: no payload, and tail says so ----------------
{
  const { DatabaseSync } = await import('node:sqlite');
  const db = new DatabaseSync(DB, { readOnly: true });
  const held = db.prepare('SELECT COUNT(*) n FROM events WHERE logs IS NOT NULL').get().n;
  const pending = db.prepare(
    'SELECT COUNT(*) n FROM events WHERE logs IS NULL AND payload_state IS NULL',
  ).get().n;
  db.close();
  push(
    'repair_shape_rows_arrive_without_payload (the defect being fixed)',
    held === 0 && pending === rows.length + 2,
    `held=${held} pending=${pending} expected held=0 pending=${rows.length + 2}`,
  );
}

// --- 2. hydrate fills exactly those rows ----------------------------------
const t0 = Date.now();
let h1 = await cli(['hydrate', '--db', DB, '--limit', '200']);
const h1j = json(h1.out);
console.log(`HYDRATE exit=${h1.code} complete=${h1j.complete} hydrated=${h1j.hydrated} unavailable=${h1j.unavailable} failed=${h1j.failed} pending=${h1j.pending_left} in ${Date.now() - t0}ms`);
push(
  'hydrate_fills_all_known_rows_and_is_complete',
  h1.code === 0 && h1j.complete === true && h1j.hydrated === rows.length && h1j.pending_left === 0,
  `hydrated=${h1j.hydrated} expected=${rows.length}, complete=${h1j.complete}, exit=${h1.code}`,
);
push(
  'hydrate_records_a_forked_out_signature_as_unavailable',
  h1j.unavailable === 1 && h1j.failed === 0,
  `unavailable=${h1j.unavailable} expected 1 (well-formed, not on chain), failed=${h1j.failed}`,
);
push(
  'hydrate_records_a_malformed_key_as_invalid_not_as_a_failure',
  h1j.invalid === 1 && h1j.failed === 0,
  `invalid=${h1j.invalid} expected 1 (endpoint refused the key), failed=${h1j.failed}`,
);

// --- 3. independent byte-for-byte control ---------------------------------
{
  const { DatabaseSync } = await import('node:sqlite');
  const db = new DatabaseSync(DB, { readOnly: true });
  const stored = db.prepare('SELECT signature, logs FROM events WHERE payload_state = ?1').all('held');
  const unknown = db.prepare(
    'SELECT signature, logs, payload_state FROM events WHERE payload_state IN (?1, ?2)',
  ).all('unavailable', 'invalid');
  db.close();

  let identical = 0, mismatched = 0;
  for (const s of stored) {
    let tx;
    for (let a = 0; a < 8; a++) {
      try {
        const res = await fetch(RPC, {
          method: 'POST',
          headers: { 'content-type': 'application/json' },
          body: JSON.stringify({
            jsonrpc: '2.0', id: 1, method: 'getTransaction',
            params: [s.signature, { encoding: 'json', maxSupportedTransactionVersion: 1, commitment: 'finalized' }],
          }),
        });
        if (res.status === 429) { await new Promise((x) => setTimeout(x, 400 * (a * a + 1))); continue; }
        const j = await res.json();
        if (j.error) throw new Error(JSON.stringify(j.error));
        tx = j.result;
        break;
      } catch (e) { await new Promise((x) => setTimeout(x, 400 * (a * a + 1))); }
    }
    if (JSON.stringify(tx?.meta?.logMessages ?? null) === s.logs) identical++;
    else { mismatched++; console.log('  MISMATCH', s.signature.slice(0, 20)); }
  }
  push(
    'stored_payload_is_byte_identical_to_a_fresh_independent_getTransaction',
    mismatched === 0 && identical === stored.length && stored.length > 0,
    `${identical}/${stored.length} identical, ${mismatched} mismatched`,
  );
  push(
    'chain_unknown_row_keeps_null_logs_and_says_why',
    unknown.some((u) => u.signature === ABSENT && u.logs === null && u.payload_state === 'unavailable'),
    `${unknown.length} non-held row(s); absent-sig state=${unknown.find((u) => u.signature === ABSENT)?.payload_state} logs=${JSON.stringify(unknown.find((u) => u.signature === ABSENT)?.logs)}`,
  );
  push(
    'malformed_key_is_kept_separate_from_a_fork_out',
    unknown.some((u) => u.signature === FORGED && u.payload_state === 'invalid') &&
      !unknown.some((u) => u.payload_state === 'unavailable' && u.signature === FORGED),
    `malformed key state=${unknown.find((u) => u.signature === FORGED)?.payload_state} (want invalid, not unavailable)`,
  );
}

// --- 4. tail delivers the payload to the consumer --------------------------
{
  const t = await cli(['tail', '--db', DB, '--cursor', CURSOR, '--limit', '1000']);
  const lines = t.out.split('\n').filter(Boolean).map((l) => json(l));
  const withPayload = lines.filter((x) => Array.isArray(x.logs) && x.logs.length > 0).length;
  const unavailableRow = lines.find((x) => x.signature === FORGED);
  push(
    'tail_delivers_event_content_not_just_a_signature',
    withPayload === rows.length,
    `${withPayload}/${rows.length} delivered rows carry a non-empty log array`,
  );
  push(
    'tail_marks_the_unreadable_row_instead_of_silently_omitting_it',
    unavailableRow && unavailableRow.logs === null && unavailableRow.payload_state === 'invalid',
    `payload_state=${unavailableRow?.payload_state} logs=${JSON.stringify(unavailableRow?.logs)}`,
  );
  const absentRow = lines.find((x) => x.signature === ABSENT);
  push(
    'tail_marks_the_forked_out_row_as_unavailable',
    absentRow && absentRow.logs === null && absentRow.payload_state === 'unavailable',
    `payload_state=${absentRow?.payload_state} logs=${JSON.stringify(absentRow?.logs)}`,
  );
  // no-replay: second pass over the same cursor must be empty
  const t2 = await cli(['tail', '--db', DB, '--cursor', CURSOR, '--limit', '1000']);
  push(
    'cursor_resume_still_never_replays',
    t2.out.trim() === '',
    `second pass delivered ${t2.out.split('\n').filter(Boolean).length} line(s)`,
  );
  // --no-logs keeps the compact shape for callers that only need identities
  const t3 = await cli(['tail', '--db', DB, '--cursor', path.join(DIR, 'c2.txt'), '--limit', '10', '--no-logs' ]);
  const compact = t3.out.split('\n').filter(Boolean).map((l) => json(l));
  push(
    'no_logs_flag_keeps_identity_only_shape',
    compact.length > 0 && compact.every((x) => !('logs' in x)),
    `${compact.length} rows, all without payload field=${compact.every((x) => !('logs' in x))}`,
  );
}

// --- 5. an unfinished hydration must not exit 0 ----------------------------
{
  const { DatabaseSync } = await import('node:sqlite');
  const db = new DatabaseSync(DB);
  const ins = db.prepare(
    'INSERT OR IGNORE INTO events(signature,slot,err,logs,source,seal_status,first_seen) VALUES(?,?,?,?,?,?,?)',
  );
  // Two new pending rows, limit 1: one gets an outcome, one is left behind.
  ins.run(rows[1].signature + 'x', rows[1].slot, null, null, 'repair', 'pending', Date.now());
  ins.run(rows[2].signature + 'y', rows[2].slot, null, null, 'repair', 'pending', Date.now());
  db.close();
  const h2 = await cli(['hydrate', '--db', DB, '--limit', '1']); // leaves 1 pending
  const h2j = json(h2.out);
  push(
    'a_pass_cut_short_by_its_limit_never_exits_zero',
    h2.code !== 0 && h2j.complete === false && h2j.pending_left > 0,
    `exit=${h2.code} complete=${h2j.complete} pending_left=${h2j.pending_left}`,
  );
}

const verdict = checks.every((c) => c.pass);
for (const c of checks) console.log(`${c.pass ? 'PASS' : 'FAIL'}  ${c.name}: ${c.detail}`);
console.log(verdict ? 'PAYLOAD E2E: ALL PASS' : 'PAYLOAD E2E: FAILURE');
process.exit(verdict ? 0 : 1);
