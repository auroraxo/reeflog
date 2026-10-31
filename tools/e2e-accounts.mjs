// End-to-end proof of the account envelope against live mainnet.
//
// The claim under test: reeflog already pays for the whole `getTransaction`
// response when it hydrates a row's logs, and that response carries what the
// transaction did to ACCOUNTS — lamport balance changes and SPL token balance
// changes, keyed to addresses. Until this feature the account half was thrown
// away. `tail` now delivers it, and `hydrate --accounts` fills it in for rows
// the socket delivered (the socket notification carries no balances).
//
// Checks, each independently falsifiable:
//  1. FREE PATH   — a row with no logs gains BOTH logs and an account
//                   envelope from ONE hydrate pass.
//  2. PAID PATH   — a planted socket row (logs held, no envelope) gains an
//                   envelope from `hydrate --accounts`.
//  3. DELIVERY    — `tail` emits `accounts` (parsed, non-null) and
//                   `account_state`, and a row WITHOUT an envelope emits
//                   `account_state: null` (still owed, not claimed empty).
//  4. CROSS-CHECK — every stored envelope is re-fetched independently by the
//                   harness from the chain and compared FIELD BY FIELD:
//                   lamport deltas and token deltas must agree exactly.
//                   The harness does its own parsing; it is not reading
//                   reeflog's own numbers back.
//  5. IDEMPOTENCE — a second `hydrate --accounts` stores nothing and (for
//                   enriched rows) costs no RPC.
//  6. HONESTY     — `status` counts accounts_held / accounts_missing / and
//                   never claims coverage the journal does not have.
//
// Run against the PRE-FIX binary (commit 95bf34f) this fails at check 1:
// the old binary has no `--accounts` flag and no `accounts` field in tail.
import { spawn } from 'node:child_process';
import fs from 'node:fs';
import path from 'node:path';

const RPC = process.env.RPC || 'https://api.mainnet-beta.solana.com';
const PROGRAM = process.env.PROGRAM || '675kPX9MHTjS2zt1qfr1NYHuzeLXfQM9H24wFSUt1Mp8'; // Raydium AMM v4
const BIN = path.resolve(process.env.BIN || './target/release/reeflog');
const DIR = '/tmp/reeflog-e2e-accounts';
const DB = path.join(DIR, 'reeflog.db');

async function rpc(method, params, tries = 6) {
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
    // 'close', not 'exit': stdout may still be unread at 'exit'.
    c.on('close', (code) => resolve({ code, out: o, err: e }));
    c.on('error', (err) => resolve({ code: null, out: o, err: `${e}\n${err}` }));
  });
}
const json = (s) => { try { return JSON.parse(s); } catch { return { raw: s }; } };

const checks = [];
const check = (name, ok, detail) => {
  checks.push({ name, ok: !!ok, detail });
  console.log(`${ok ? 'PASS' : 'FAIL'} ${name}${detail ? ` — ${detail}` : ''}`);
};

// The harness's OWN independent parse of a getTransaction response. Kept
// deliberately separate from reeflog's extractor: it walks the balances and
// derives the delta set directly, so agreement is two implementations
// agreeing on the chain, not one implementation agreeing with itself.
//
// Comparison is on canonical (key-sorted) JSON: reeflog stores envelopes
// through serde_json's sorted map, this harness builds objects in insertion
// order — a plain stringify would compare key order, not content.
const canonical = (v) => {
  if (Array.isArray(v)) return `[${v.map(canonical).join(',')}]`;
  if (v && typeof v === 'object') {
    return `{${Object.keys(v).sort().map((k) => `${JSON.stringify(k)}:${canonical(v[k])}`).join(',')}}`;
  }
  return JSON.stringify(v);
};
function parseIndependent(tx) {
  const meta = tx.meta || {};
  const msg = tx.transaction?.message || {};
  const keys = [...(msg.accountKeys || []), ...(meta.loadedAddresses?.writable || []), ...(meta.loadedAddresses?.readonly || [])];
  const lamports = [];
  const pre = meta.preBalances || [], post = meta.postBalances || [];
  for (let i = 0; i < Math.max(pre.length, post.length); i++) {
    const b = pre[i] ?? 0, a = post[i] ?? 0;
    if (b !== a && i < keys.length) lamports.push({ index: i, address: keys[i], before: b, after: a, delta: a - b });
  }
  const tok = new Map();
  const put = (rows, which) => {
    for (const t of rows || []) {
      const k = `${t.accountIndex}|${t.mint}|${t.owner}`;
      const e = tok.get(k) || { index: t.accountIndex, mint: t.mint, owner: t.owner };
      e[which] = BigInt(t.uiTokenAmount?.amount ?? '0');
      e[`${which}Raw`] = t.uiTokenAmount?.amount ?? null;
      tok.set(k, e);
    }
  };
  put(meta.preTokenBalances, 'b');
  put(meta.postTokenBalances, 'a');
  const tokens = [];
  for (const [, e] of tok) {
    const before = e.b ?? 0n, after = e.a ?? 0n;
    if (before !== after && e.index < keys.length) {
      // Absent from one side = zero balance on that side (an account missing
      // from preTokenBalances did not exist as a token account yet; missing
      // from postTokenBalances was closed). The same domain rule reeflog's
      // extractor applies — here derived from the raw rows, not read back.
      tokens.push({
        index: e.index, address: keys[e.index], mint: e.mint, owner: e.owner,
        before: e.bRaw ?? '0', after: e.aRaw ?? '0', delta: (after - before).toString(),
      });
    }
  }
  // The instruction half of the envelope, parsed independently: program ids
  // resolved against the harness's own key list, inner instructions tagged
  // with their parent index and their position inside it. Same order the
  // response presents them: outer instructions first, then inner groups.
  const resolve = (ix) => {
    const programId = Number.isSafeInteger(ix?.programIdIndex)
      ? (keys[ix.programIdIndex] ?? null)
      : (typeof ix?.programId === 'string' ? ix.programId : null);
    const accounts = Array.isArray(ix?.accounts)
      ? ix.accounts.map((i) => (Number.isSafeInteger(i) && i >= 0 && i < keys.length ? keys[i] : null))
      : [];
    return {
      inner: false,
      outer: 0,
      program_id: programId,
      accounts,
      data: typeof ix?.data === 'string' ? ix.data : null,
    };
  };
  const instructions = [];
  for (const [i, ix] of (msg.instructions ?? []).entries()) {
    const entry = resolve(ix);
    entry.outer = i;
    instructions.push(entry);
  }
  for (const group of meta.innerInstructions ?? []) {
    const outer = Number.isSafeInteger(group?.index) ? group.index : 0;
    for (const [position, ix] of (group?.instructions ?? []).entries()) {
      const entry = resolve(ix);
      entry.outer = outer;
      entry.inner = true;
      entry.inner_index = position;
      instructions.push(entry);
    }
  }
  const err = meta.err === undefined ? null : meta.err;

  return { lamports, tokens, instructions, err };
}

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

// ---------------------------------------------------------------------------
// 0. Set up a fresh journal with real rows in three distinct states.
// ---------------------------------------------------------------------------
fs.rmSync(DIR, { recursive: true, force: true });
fs.mkdirSync(DIR, { recursive: true });

const sigs = (await rpc('getSignaturesForAddress', [PROGRAM, { limit: 8, commitment: 'finalized' }]))
  .filter((s) => !s.err);
const chosen = sigs.slice(0, 5).map((s) => s.signature);
if (chosen.length < 3) { console.error('FAILED: not enough chain signatures'); process.exit(2); }

let r = await cli(['init', '--db', DB, '--program', PROGRAM]);
if (r.code !== 0) { console.error('FAILED: init', r); process.exit(2); }

const { DatabaseSync } = await import('node:sqlite');
const db = new DatabaseSync(DB);
const now = Date.now();
// Row A+B: recovered rows (no logs, no accounts) — the FREE path via hydrate.
const stmtA = db.prepare('INSERT INTO events(signature, slot, first_seen, source, seal_status, seal_seen) VALUES(?,?,?,?,?,?)');
stmtA.run(chosen[0], 1, now, 'repair', 'finalized', now);
stmtA.run(chosen[1], 2, now, 'repair', 'finalized', now);
// Row C+D: planted SOCKET rows — logs held from the notification, no envelope
// (the WS logsNotification carries no balances). The PAID path must fill them.
const stmtC = db.prepare('INSERT INTO events(signature, slot, first_seen, source, logs, payload_state, seal_status, seal_seen) VALUES(?,?,?,?,?,?,?,?)');
stmtC.run(chosen[2], 3, now, 'ws', '["Program log: planted socket row"]', 'held', 'finalized', now);
stmtC.run(chosen[3], 4, now, 'ws', '["Program log: planted socket row 2"]', 'held', 'finalized', now);
// Row E: a row the chain does not know (forked out long ago) — known outcome,
// must never become a permanent "owed" row.
const stmtE = db.prepare('INSERT INTO events(signature, slot, first_seen, source, seal_status, seal_seen) VALUES(?,?,?,?,?,?)');
stmtE.run('4b1SoLuSXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXX', 5, now, 'repair', 'finalized', now);
db.close();
console.log(`planted: 2 recovered rows (free path), 2 socket rows (paid path), 1 unknown sig`);

// ---------------------------------------------------------------------------
// 1. FREE PATH: hydrate fills logs AND the envelope from the same call.
// ---------------------------------------------------------------------------
const h1 = await cli(['hydrate', '--db', DB, '--limit', '10', '--rpc', RPC]);
const h1j = json(h1.out);
check('free path: hydrate complete (exit 0)', h1.code === 0, `exit=${h1.code} out=${(h1.out || '').slice(0, 120).replace(/\n/g, ' ')}`);
check('free path: the unknown signature is recorded, not pending',
  h1j.complete === true,
  `complete=${h1j.complete} unavailable=${h1j.unavailable} invalid=${h1j.invalid} pending_left=${h1j.pending_left}`);
check('free path: envelope stored with the logs from one paid response',
  (h1j.accounts_stored_free ?? 0) >= 1, `accounts_stored_free=${h1j.accounts_stored_free}`);

// The OLD binary silently ignores --accounts; the NEW one must run the paid pass.
const h2 = await cli(['hydrate', '--db', DB, '--limit', '10', '--rpc', RPC, '--accounts']);
const h2j = json(h2.out);
const acct = h2j.accounts || {};
check('paid path: --accounts flag exists and reports (old binary has no such block)',
  acct.requested !== false && typeof acct.stored === 'number',
  `accounts block: ${JSON.stringify(acct).slice(0, 200)}`);
check('paid path: both planted socket rows enriched', acct.stored >= 2,
  `stored=${acct.stored} left=${acct.left} unavailable=${acct.unavailable}`);

// Idempotence: run again — nothing new to store, no debt for rows with an
// envelope or a recorded outcome.
const h3 = await cli(['hydrate', '--db', DB, '--limit', '10', '--rpc', RPC, '--accounts']);
const h3j = json(h3.out);
check('idempotence: second pass stores nothing new and completes',
  (h3j.accounts?.stored ?? -1) === 0 && h3j.accounts?.complete === true,
  `stored=${h3j.accounts?.stored} complete=${h3j.accounts?.complete} left=${h3j.accounts?.left}`);

// ---------------------------------------------------------------------------
// 2. DELIVERY: tail emits the envelope; owed rows say so honestly.
// ---------------------------------------------------------------------------
const cursor = path.join(DIR, 'cursor.txt');
const t1 = await cli(['tail', '--db', DB, '--cursor', cursor]);
const rows = t1.out.split('\n').filter(Boolean).map((l) => json(l));
check('delivery: all rows emitted', rows.length >= 5, `rows=${rows.length}`);
check('delivery: the accounts and account_state fields exist on every row',
  rows.every((x) => 'accounts' in x && 'account_state' in x),
  `field coverage on ${rows.length} rows`);
const withEnv = rows.filter((x) => x.accounts && typeof x.accounts === 'object');
// A row WITHOUT an envelope must never be presented as an unknowing blank:
// either the journal owes it (a socket row, still owed — counted in status),
// or it states a reason. The planted bogus signature never even got an event
// payload: its reason lives in `payload_state` (invalid/unavailable), which
// is what tail delivers for it.
const noEnvelope = rows.filter((x) => !(x.accounts && typeof x.accounts === 'object'));
const explained = noEnvelope.filter((x) =>
  x.account_state != null || (x.payload_state != null && x.payload_state !== 'held'));
const owedSocketRows = noEnvelope.filter((x) => x.payload_state === 'held' && x.account_state == null);
check('delivery: every row without an envelope either states a reason or is a counted socket-row debt',
  explained.length + owedSocketRows.length === noEnvelope.length,
  `no-envelope rows=${noEnvelope.length}: explained=${explained.length} owed=${owedSocketRows.length}`);
check('delivery: the chain-refused row states invalid/unavailable, not a blank pending',
  rows.some((x) => x.payload_state === 'invalid' || x.account_state === 'unavailable'),
  `states=${rows.filter((x) => x.payload_state && x.payload_state !== 'held').map((x) => x.payload_state).join(',') || '(none)'}`);

// Cursor: a second read delivers nothing.
const t2 = await cli(['tail', '--db', DB, '--cursor', cursor]);
check('cursor: second read delivers nothing', t2.out.trim() === '',
  `second read lines=${t2.out.split('\n').filter(Boolean).length}`);

// --with-accounts: rows without an envelope are skipped AND reported.
const cursor2 = path.join(DIR, 'cursor2.txt');
const t3 = await cli(['tail', '--db', DB, '--cursor', cursor2, '--with-accounts']);
const t3rows = t3.out.split('\n').filter(Boolean).map((l) => json(l));
check('--with-accounts: delivers only rows carrying an envelope',
  t3rows.length === withEnv.length && t3rows.every((x) => x.accounts),
  `delivered=${t3rows.length}, expected=${withEnv.length}`);
check('--with-accounts: skips are reported on stderr, not swallowed',
  /skipped/.test(t3.err) || withEnv.length === rows.length,
  `stderr=${t3.err.trim().split('\n').pop()}`);

// ---------------------------------------------------------------------------
// 2b. DETERMINISTIC: a CLOSED token account, served by a local RPC stub.
//
// Live traffic only closes a token account in some transactions; a check
// that depends on the sample is a check that is sometimes blind. This canned
// response contains the exact shape the first live cross-check caught — an
// account present pre, absent post — and the local stub answers the binary's
// getTransaction calls directly, so the closed-account path runs every time.
// The stub also rewrites the signature the binary asks about, so the row
// planted for it does not need to exist on chain.
// ---------------------------------------------------------------------------
const CLOSED_SIG = 'Loca1StubClosedAccountSignatureAAAAAAAAAAAAAAAAAAAAAAA';
const FAIL_SIG = 'Loca1StubFailedTransactionSignatureAAAAAAAAAAAAAAAAAAAA';
const stubTx = {
  slot: 42,
  transaction: {
    message: {
      accountKeys: ['StubPayer11111111111111111111111111111111111', 'StubAcct22222222222222222222222222222222222222'],
      instructions: [
        { programIdIndex: 1, accounts: [0, 1], data: 'stub-outer' },
        { programIdIndex: 0, accounts: [1], data: 'stub-outer2' },
      ],
    },
  },
  meta: {
    logMessages: ['Program log: stub'],
    preBalances: [1000, 500],
    postBalances: [1000, 600],
    innerInstructions: [
      { index: 0, instructions: [
        { programIdIndex: 0, accounts: [1], data: 'stub-cpi-a' },
        { programIdIndex: 1, accounts: [0], data: 'stub-cpi-b' },
      ] },
    ],
    preTokenBalances: [
      { accountIndex: 1, mint: 'StubMint111111111111111111111111111111111111', owner: 'StubOwnr111111111111111111111111111111111111', uiTokenAmount: { amount: '1331874357100' } },
    ],
    postTokenBalances: [],
    err: null,
  },
};
// A transaction that FAILED. Every live transaction the cross-check samples
// on a healthy run succeeds, so `err: null === null` would compare green
// even if the envelope dropped the field entirely — this canned failure is
// what pins "err is carried verbatim", independent of live traffic.
const failTx = {
  slot: 43,
  transaction: {
    message: {
      accountKeys: ['StubPayer11111111111111111111111111111111111'],
      instructions: [{ programIdIndex: 0, accounts: [0], data: 'failing-ix' }],
    },
  },
  meta: {
    logMessages: ['Program log: about to fail'],
    preBalances: [1000],
    postBalances: [1000],
    err: { InstructionError: [0, { Custom: 42 }] },
  },
};
import http from 'node:http';
const stubServer = http.createServer((req, res) => {
  let body = '';
  req.on('data', (d) => { body += d; });
  req.on('end', () => {
    const j = JSON.parse(body);
    const sig = j.params?.[0];
    res.writeHead(200, { 'content-type': 'application/json' });
    if (sig === CLOSED_SIG) {
      res.end(JSON.stringify({ jsonrpc: '2.0', id: j.id, result: stubTx }));
    } else if (sig === FAIL_SIG) {
      res.end(JSON.stringify({ jsonrpc: '2.0', id: j.id, result: failTx }));
    } else {
      res.end(JSON.stringify({ jsonrpc: '2.0', id: j.id, result: null }));
    }
  });
});
await new Promise((res) => stubServer.listen(0, '127.0.0.1', res));
const stubUrl = `http://127.0.0.1:${stubServer.address().port}`;

// Plant a held-log row for the closed-account signature; only --accounts
// will ever fetch it (hydrate's payload pass sees logs already held).
const db2 = new DatabaseSync(DB);
db2.prepare("INSERT INTO events(signature, slot, first_seen, source, logs, payload_state, seal_status, seal_seen) VALUES(?,?,?,?,?,?,?,?)")
  .run(CLOSED_SIG, 6, Date.now(), 'ws', '["Program log: closed account row"]', 'held', 'finalized', Date.now());
db2.prepare("INSERT INTO events(signature, slot, first_seen, source, logs, payload_state, seal_status, seal_seen) VALUES(?,?,?,?,?,?,?,?)")
  .run(FAIL_SIG, 7, Date.now(), 'ws', '["Program log: about to fail"]', 'held', 'finalized', Date.now());
db2.close();

const h4 = await cli(['hydrate', '--db', DB, '--limit', '10', '--rpc', stubUrl, '--accounts']);
const h4j = json(h4.out);
const closedRow = (await cli(['tail', '--db', DB, '--cursor', path.join(DIR, 'cursor3.txt')]))
  .out.split('\n').filter(Boolean).map((l) => json(l))
  .find((x) => x.signature === CLOSED_SIG);
const closedTok = closedRow?.accounts?.token_changes?.[0];
check('closed account: the stub-served row gets its envelope',
  !!closedTok, `token_changes=${JSON.stringify(closedRow?.accounts?.token_changes)}`);
check('closed account: absence from post means delta = -before, never an empty string',
  closedTok?.before === '1331874357100' && closedTok?.after === '0' && closedTok?.delta === '-1331874357100',
  `before=${closedTok?.before} after=${closedTok?.after} delta=${closedTok?.delta}`);

// The CPI shape and the failed-transaction outcome, both from the local stub:
// neither depends on live traffic, and both are invisible to a sample of
// successful transactions.
const failRow = (await cli(['tail', '--db', DB, '--cursor', path.join(DIR, 'cursor5.txt')]))
  .out.split('\n').filter(Boolean).map((l) => json(l))
  .find((x) => x.signature === FAIL_SIG);
check('failed transaction: the outcome is carried verbatim, not flattened to null',
  JSON.stringify(failRow?.accounts?.err) === JSON.stringify({ InstructionError: [0, { Custom: 42 }] }),
  `err=${JSON.stringify(failRow?.accounts?.err)}`);
const stubIxs = closedRow?.accounts?.instructions ?? [];
const stubInner = stubIxs.filter((i) => i.inner);
check('instructions: the CPI is tagged with its parent and its own position',
  stubInner.length === 2 && stubInner[0].outer === 0 && stubInner[0].inner_index === 0
    && stubInner[1].outer === 0 && stubInner[1].inner_index === 1
    && stubInner[0].data === 'stub-cpi-a' && stubInner[1].data === 'stub-cpi-b',
  `inner=${JSON.stringify(stubInner).slice(0, 240)}`);
check('instructions: the outer instructions come first, in execution order',
  stubIxs.length === 4 && stubIxs[0].inner === false && stubIxs[0].outer === 0
    && stubIxs[0].data === 'stub-outer' && stubIxs[1].outer === 1 && stubIxs[1].data === 'stub-outer2',
  `outer=${JSON.stringify(stubIxs.slice(0, 2)).slice(0, 200)}`);
stubServer.close();

// ---------------------------------------------------------------------------
// 3. CROSS-CHECK: reeflog's stored envelope vs the harness's own re-fetch.
// ---------------------------------------------------------------------------
const store = rows.filter((x) => x.accounts && x.accounts.lamport_changes);
let crossChecked = 0, crossMismatch = 0;
const mismatchSamples = [];
for (const row of store) {
  const tx = await rpc('getTransaction', [row.signature, {
    encoding: 'json', maxSupportedTransactionVersion: 1, commitment: 'finalized',
  }]);
  if (!tx) continue;
  const indep = parseIndependent(tx);
  const mine = row.accounts;
  const sortL = (a) => [...a].sort((x, y) => String(x.address).localeCompare(String(y.address)));
  const same = canonical(sortL(indep.lamports)) === canonical(sortL(mine.lamport_changes || []));
  const tokSame = canonical(sortL(indep.tokens)) === canonical(sortL(mine.token_changes || []));
  // The instruction half: every outer and inner instruction, program id,
  // resolved accounts, verbatim base58 data, parent and CPI position, plus
  // the transaction outcome — compared the same way, re-parsed from the
  // chain response rather than read back from reeflog's own numbers.
  const ixSame = canonical(indep.instructions) === canonical(mine.instructions || []);
  const errSame = canonical(indep.err) === canonical(mine.err ?? null);
  if (!same || !tokSame || !ixSame || !errSame) {
    crossMismatch++;
    if (mismatchSamples.length < 2) {
      mismatchSamples.push({
        sig: row.signature.slice(0, 12),
        lamportsEqual: same, tokensEqual: tokSame, instructionsEqual: ixSame, errEqual: errSame,
        indepLamports: indep.lamports.length, myLamports: (mine.lamport_changes || []).length,
        indepTokens: indep.tokens.length, myTokens: (mine.token_changes || []).length,
        indepInstructions: indep.instructions.length, myInstructions: (mine.instructions || []).length,
      });
    }
  } else {
    crossChecked++;
  }
  await sleep(300);
}
check('cross-check: stored envelopes (balances, instructions, outcome) equal an independent re-fetch field by field',
  crossMismatch === 0 && crossChecked >= 2,
  `matched=${crossChecked} mismatched=${crossMismatch}${mismatchSamples.length ? ` samples=${JSON.stringify(mismatchSamples)}` : ''}`);

// ---------------------------------------------------------------------------
// 4. STATUS reports coverage honestly.
// ---------------------------------------------------------------------------
const s = await cli(['status', '--db', DB]);
const sj = json(s.out);
// Count the envelopes in the JOURNAL, not in an earlier tail snapshot: the
// stub-served closed-account row was enriched after that snapshot.
const t4 = await cli(['tail', '--db', DB, '--cursor', path.join(DIR, 'cursor4.txt')]);
const t4rows = t4.out.split('\n').filter(Boolean).map((l) => json(l));
const envNow = t4rows.filter((x) => x.accounts && typeof x.accounts === 'object').length;
// The instruction half must be present on the enriched rows — an envelope
// with zero instructions would still satisfy the balance cross-check, so the
// claim "the journal carries the instruction bytes" needs its own check.
const ixRows = rows.filter((x) => Array.isArray(x.accounts?.instructions) && x.accounts.instructions.length);
check('instructions: enriched rows carry the instruction bytes with their data',
  ixRows.length >= 2 && ixRows.every((x) => x.accounts.instructions.every((i) => typeof i.data === 'string' && i.data.length)),
  `rows with instructions=${ixRows.length}, sample=${JSON.stringify(ixRows[0]?.accounts?.instructions?.[0] ?? null).slice(0, 200)}`);
check('instructions: the outer/inner distinction and the parent index are recorded',
  ixRows.length >= 2 && ixRows.every((x) => x.accounts.instructions.every((i) => typeof i.inner === 'boolean' && Number.isSafeInteger(i.outer))),
  `sample=${JSON.stringify((ixRows[0]?.accounts?.instructions ?? []).slice(0, 2)).slice(0, 220)}`);

check('status: accounts_held covers exactly the rows with envelopes',
  sj.accounts_held === envNow && typeof sj.accounts_missing === 'number',
  `accounts_held=${sj.accounts_held}, envelopes_in_journal=${envNow}, accounts_missing=${sj.accounts_missing}, unreachable=${sj.accounts_unreachable}`);

const failed = checks.filter((c) => !c.ok);
console.log(`\n${checks.length - failed.length}/${checks.length} checks passed`);
if (failed.length) {
  console.log('FAILED:', failed.map((f) => f.name).join(' | '));
  process.exit(1);
}
console.log('ACCOUNT E2E PASSED');
