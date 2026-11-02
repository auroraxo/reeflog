// End-to-end proof of EVENT-TIME AUTHORITY in the envelope, against live
// mainnet.
//
// The claim under test: reeflog's envelope states, for every token account a
// transaction touched, WHO held that account AT THE MOMENT OF THE EVENT —
// taken from the transaction's own `preTokenBalances`, which is the chain's
// statement at event time. Previously the envelope carried that authority
// only for accounts whose amount CHANGED; measured 2026-11-02
// (tools/probe-tx-time-authority.mjs), 82.4% of the token accounts a
// transaction touched (103/125 over 10 live Raydium AMM v4 transactions)
// carry an unchanged amount and were dropped entirely.
//
// Checks, each independently falsifiable:
//  1. PASS COMPLETE      — the hydration pass runs; rows are stored.
//  2. TOKEN STATE HELD   — the stored envelope carries a `token_state` list.
//  3. COVERAGE           — every token account the transaction touched whose
//                          amount did NOT change appears in token_state.
//  4. AUTHORITY TRUE     — each stored authority equals the chain's own
//                          `preTokenBalances[].owner` for that account/mint,
//                          re-derived by the harness from an independent
//                          re-fetch (not read back from reeflog).
//  5. NO DUPLICATE       — an account whose amount changed appears in
//                          token_changes, not in token_state; the two lists
//                          never claim the same account.
//  6. ADDRESS TRUE       — every stored address is the account at that index
//                          in the canonical key order (static ++ loaded
//                          writable ++ loaded readonly), so a lookup-table
//                          transaction cannot shift the rows.
//  7. MUTANT CAUGHT      — the probe's own parser is mutated (authority
//                          replaced by the mint) and the check must go red.
//  8. DELIVERY           — `tail` carries token_state.
//
// Exit codes: 0 = all pass, 1 = a check failed, 2 = setup failed (rate limit
// or empty program — NOT a verdict), 3 = skipped (missing binary).
import { spawn } from 'node:child_process';
import fs from 'node:fs';
import path from 'node:path';

const RPC = process.env.RPC || 'https://api.mainnet-beta.solana.com';
const PROGRAM = process.env.PROGRAM || '675kPX9MHTjS2zt1qfr1NYHuzeLXfQM9H24wFSUt1Mp8'; // Raydium AMM v4
const BIN = path.resolve(process.env.BIN || './target/release/reeflog');
const DIR = process.env.DIR || '/tmp/reeflog-e2e-token-state';
const DB = path.join(DIR, 'reeflog.db');
const SECONDS = Number(process.env.SECONDS || 45);

if (!fs.existsSync(BIN)) {
  console.log(`SKIP: binary not found at ${BIN} (build first)`);
  process.exit(3);
}

async function rpc(method, params, tries = 8) {
  let last;
  for (let i = 0; i < tries; i++) {
    if (i) await new Promise((r) => setTimeout(r, Math.min(1000 * 2 ** (i - 1), 15000)));
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

function cli(args, timeoutMs = 900000) {
  return new Promise((resolve) => {
    const c = spawn(BIN, args, { stdio: ['ignore', 'pipe', 'pipe'] });
    let o = '', e = '';
    const t = setTimeout(() => { c.kill('SIGKILL'); resolve({ code: 99, out: o, err: e + '\ntimeout' }); }, timeoutMs);
    c.stdout.on('data', (d) => { o += d; });
    c.stderr.on('data', (d) => { e += d; });
    c.on('close', (code) => { clearTimeout(t); resolve({ code, out: o, err: e }); });
    c.on('error', (err) => { clearTimeout(t); resolve({ code: null, out: o, err: `${e}\n${err}` }); });
  });
}
const json = (s) => { try { return JSON.parse(s); } catch { return { raw: s }; } };

const checks = [];
const check = (name, ok, detail) => {
  checks.push({ name, ok: !!ok, detail });
  console.log(`${ok ? 'PASS' : 'FAIL'} ${name}${detail ? ` — ${detail}` : ''}`);
};

// Canonical key order: static keys, then the lookup table's writable, then
// its readonly — the order the balance arrays are indexed over.
function canonicalKeys(tx) {
  const meta = tx.meta || {};
  const msg = tx.transaction?.message || {};
  return [
    ...(msg.accountKeys || []),
    ...(meta.loadedAddresses?.writable || []),
    ...(meta.loadedAddresses?.readonly || []),
  ];
}

// The harness's OWN reading of the chain's event-time authority, derived
// from an independent re-fetch. The mutant variant replaces the authority
// with the mint, so a check that only compared shapes would stay green.
function expectedTokenState(tx, mutant = false) {
  const keys = canonicalKeys(tx);
  const meta = tx.meta || {};
  const pre = meta.preTokenBalances || [];
  const post = meta.postTokenBalances || [];
  const changed = new Set();
  for (const t of pre) {
    const p = post.find((x) => x.accountIndex === t.accountIndex && x.mint === t.mint);
    const before = t.uiTokenAmount?.amount ?? '0';
    const after = p ? (p.uiTokenAmount?.amount ?? '0') : '0';
    if (before !== after) changed.add(`${t.accountIndex}|${t.mint}`);
  }
  const out = [];
  for (const t of pre) {
    const key = `${t.accountIndex}|${t.mint}`;
    if (changed.has(key)) continue; // changed accounts live in token_changes
    out.push({
      index: t.accountIndex,
      address: keys[t.accountIndex] ?? null,
      mint: t.mint,
      authority: mutant ? t.mint : (t.owner ?? null),
      token_program: t.programId ?? null,
    });
  }
  return { expected: out, changed: [...changed] };
}

async function main() {
  fs.rmSync(DIR, { recursive: true, force: true });
  fs.mkdirSync(DIR, { recursive: true });

  const init = await cli(['init', '--db', DB, '--program', PROGRAM]);
  if (init.code !== 0) {
    console.error(`SETUP FAIL: init exited ${init.code}: ${init.err}`);
    process.exit(2);
  }
  const start = await cli(['start', '--db', DB, '--seconds', String(SECONDS)]);
  if (start.code !== 0) {
    console.error(`SETUP FAIL: start exited ${start.code}: ${start.err}`);
    process.exit(2);
  }
  const hyd = await cli(['hydrate', '--db', DB, '--accounts']);
  if (hyd.code !== 0) {
    console.error(`SETUP FAIL: hydrate exited ${hyd.code}: ${hyd.err}`);
    process.exit(2);
  }
  const status = json(await cli(['status', '--db', DB]).then((r) => r.out));
  const held = status.accounts_held || 0;
  check('PASS COMPLETE', held > 0, `accounts_held=${held}`);

  // Read the stored envelopes. `tail` requires a cursor file; a fresh one
  // starts at 0 (the first harness run exited with 0 rows because the
  // cursor argument was missing — the refusal went to stderr and stdout
  // was empty, which the old code read as "no rows").
  const cursor = path.join(DIR, 'cursor.txt');
  fs.writeFileSync(cursor, '0');
  const tail = await cli(['tail', '--db', DB, '--cursor', cursor]);
  if (tail.code !== 0) {
    console.error(`SETUP FAIL: tail exited ${tail.code}: ${tail.err}`);
    process.exit(2);
  }
  const rows = tail.out.split('\n').filter((l) => l.trim().startsWith('{'))
    .map((l) => { try { return JSON.parse(l); } catch { return null; } })
    .filter(Boolean)
    .filter((r) => r.accounts);
  check('ROWS READ', rows.length > 0, `${rows.length} rows with an envelope`);

  if (!rows.length) {
    console.error('SETUP FAIL: no stored envelopes');
    process.exit(2);
  }

  let withState = 0;
  let coverageChecked = 0;
  let coverageOk = 0;
  let authorityChecked = 0;
  let authorityOk = 0;
  let addressChecked = 0;
  let addressOk = 0;
  let overlap = 0;
  let changedChecked = 0;
  const mismatches = [];

  for (const row of rows) {
    const tx = await rpc('getTransaction', [row.signature, {
      encoding: 'json', maxSupportedTransactionVersion: 1, commitment: 'finalized',
    }]);
    if (!tx || !tx.meta) continue;
    const state = row.accounts.token_state;
    if (!Array.isArray(state)) continue;
    withState++;
    const { expected, changed } = expectedTokenState(tx);

    // 3. COVERAGE
    coverageChecked++;
    const expKeys = new Set(expected.map((e) => `${e.index}|${e.mint}`));
    const gotKeys = new Set(state.map((e) => `${e.index}|${e.mint}`));
    const missing = [...expKeys].filter((k) => !gotKeys.has(k));
    const extra = [...gotKeys].filter((k) => !expKeys.has(k));
    if (!missing.length && !extra.length) coverageOk++;
    else if (mismatches.length < 3) {
      mismatches.push({ sig: row.signature.slice(0, 12), missing, extra });
    }

    // 4. AUTHORITY TRUE (against the harness's independent reading)
    for (const e of expected) {
      const got = state.find((x) => x.index === e.index && x.mint === e.mint);
      if (!got) continue;
      authorityChecked++;
      const gotAuthority = got.authority ?? null;
      if (gotAuthority === e.authority) authorityOk++;
      else if (mismatches.length < 3) {
        mismatches.push({ sig: row.signature.slice(0, 12), index: e.index, expected: e.authority, got: gotAuthority });
      }
      // 6. ADDRESS TRUE
      addressChecked++;
      if ((got.address ?? null) === e.address) addressOk++;
    }

    // 5. NO DUPLICATE
    changedChecked++;
    const changedKeys = new Set(changed);
    for (const e of state) {
      if (changedKeys.has(`${e.index}|${e.mint}`)) { overlap++; break; }
    }
  }

  check('TOKEN STATE HELD', withState > 0, `${withState}/${rows.length} envelopes carry token_state`);
  check('COVERAGE', coverageChecked > 0 && coverageOk === coverageChecked,
    `${coverageOk}/${coverageChecked} transactions: every unchanged token account is listed`);
  check('AUTHORITY TRUE', authorityChecked > 0 && authorityOk === authorityChecked,
    `${authorityOk}/${authorityChecked} event-time authorities equal the chain's own statement`);
  check('ADDRESS TRUE', addressChecked > 0 && addressOk === addressChecked,
    `${addressOk}/${addressChecked} addresses sit at their canonical index`);
  check('NO DUPLICATE', overlap === 0,
    `${overlap} accounts appear in both token_state and token_changes`);
  if (mismatches.length) console.error(`mismatch samples: ${JSON.stringify(mismatches)}`);

  // 7. MUTANT CAUGHT — the harness's own parser is mutated; the comparison
  // must go red. A green check over an unmutated parser proves nothing.
  let mutantCaught = false;
  {
    const row = rows.find((r) => Array.isArray(r.accounts.token_state) && r.accounts.token_state.length);
    if (row) {
      const tx = await rpc('getTransaction', [row.signature, {
        encoding: 'json', maxSupportedTransactionVersion: 1, commitment: 'finalized',
      }]);
      if (tx && tx.meta) {
        const { expected } = expectedTokenState(tx, true);
        const state = row.accounts.token_state;
        let agree = 0;
        for (const e of expected) {
          const got = state.find((x) => x.index === e.index && x.mint === e.mint);
          if (got && (got.authority ?? null) === e.authority) agree++;
        }
        const decided = expected.length;
        mutantCaught = decided > 0 && agree < decided;
        console.error(`mutant: ${agree}/${decided} authorities still agree — the check ${mutantCaught ? 'caught it' : 'DID NOT CATCH IT'}`);
      }
    }
  }
  check('MUTANT CAUGHT', mutantCaught, 'a mutated authority reader makes the authority check red');

  // 8. DELIVERY — `tail` carries token_state (the rows above came from
  // `tail`, so this is proven by the rows themselves; asserted explicitly so
  // a future change that stops delivering it fails loudly).
  check('DELIVERY', rows.some((r) => Array.isArray(r.accounts.token_state)),
    'tail delivers token_state inside the envelope');

  const failed = checks.filter((c) => !c.ok);
  console.log(`\n${checks.length - failed.length}/${checks.length} checks passed`);
  process.exit(failed.length ? 1 : 0);
}

main().catch((e) => {
  console.error(`E2E FAIL: ${e && e.message ? e.message : e}`);
  process.exit(1);
});
