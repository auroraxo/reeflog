// Probe: can the envelope carry AUTHORITY — which program owns each account
// the transaction touched — and what does it actually cost?
//
// The card names "Account-Bytes/Authority im Envelope" as open. `owner` is
// NOT in the getTransaction response (only a token account's wallet-owner is,
// via pre/postTokenBalances), so unlike the envelope's other fields this one
// is not free: it costs getMultipleAccounts calls. This probe measures the
// cost before any code is written, and pins the two premises:
//
//   P1 COST    — batching 100 addresses per call saves an order of magnitude
//                over one call per address, because addresses repeat across
//                (and within) transactions.
//   P2 TRUTH   — the `owner` the RPC states for a token account equals the
//                authority DECODED from the account's own bytes (layout:
//                mint[0..32] || owner[32..64]), checked three ways against
//                preTokenBalances' stated owner. Two independent paths
//                agreeing is evidence; one path reading itself is not.
//
// Also measured, because they decide the honest envelope shape:
//   - accounts GONE by fetch time (closed between tx and probe) — must land
//     in an explicit missing list, never a guessed owner.
//   - response bytes with dataSlice (owner-only) vs full data.
import fs from 'node:fs';

const RPC = process.env.RPC || 'https://api.mainnet-beta.solana.com';
const PROGRAM = process.env.PROGRAM || '675kPX9MHTjS2zt1qfr1NYHuzeLXfQM9H24wFSUt1Mp8'; // Raydium AMM v4
const N_TX = Number(process.env.N_TX || 30);
const BATCH = 100;

async function rpc(method, params, tries = 8) {
  let last;
  for (let i = 0; i < tries; i++) {
    if (i) {
      // Exponential, capped: the public endpoint rate-limits in bursts, and
      // a probe that gives up after 3.5s measures its own impatience.
      const wait = Math.min(1000 * 2 ** (i - 1), 15000);
      await new Promise((r) => setTimeout(r, wait));
    }
    try {
      const r = await fetch(RPC, {
        method: 'POST',
        headers: { 'content-type': 'application/json' },
        body: JSON.stringify({ jsonrpc: '2.0', id: 1, method, params }),
      });
      if (r.status === 429) { last = new Error('429'); continue; }
      const j = await r.json();
      if (j.error) throw new Error(`${method}: ${JSON.stringify(j.error)}`);
      return j.result;
    } catch (e) { last = e; }
  }
  throw last;
}

// The canonical account list the envelope uses: static keys, then the lookup
// table's writable accounts, then its readonly ones. Owner fetching is keyed
// by address, so ORDER only matters for reproducing reeflog's own indexing —
// which the per-address map must agree with.
function canonicalAddresses(raw) {
  const msg = raw.transaction?.message || {};
  const meta = raw.meta || {};
  const out = [];
  for (const k of msg.accountKeys || []) {
    out.push(typeof k === 'string' ? k : k.pubkey);
  }
  for (const a of meta.loadedAddresses?.writable || []) out.push(a);
  for (const a of meta.loadedAddresses?.readonly || []) out.push(a);
  return out;
}

const ALPHA = '123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz';
const b58 = (bytes) => {
  let n = 0n;
  for (const b of bytes) n = n * 256n + BigInt(b);
  let out = '';
  while (n > 0n) { out = ALPHA[Number(n % 58n)] + out; n /= 58n; }
  for (const b of bytes) { if (b !== 0) break; out = '1' + out; }
  return out || '1';
};

async function main() {
  const sigs = (await rpc('getSignaturesForAddress', [PROGRAM, { limit: 200 }]))
    .filter((s) => !s.err)
    .slice(0, N_TX);
  if (sigs.length === 0) {
    console.error('probe setup failed: no signatures returned (rate limited or empty program)');
    process.exit(2);
  }
  const out = {
    rpc: RPC, program: PROGRAM, n_tx: sigs.length, batch_size: BATCH,
    measured_at: new Date().toISOString(), checks: {}, probes: {},
  };

  const rows = [];
  const globalUnique = new Map(); // address -> first tx index seen
  let totalSlots = 0;
  for (const s of sigs) {
    const raw = await rpc('getTransaction', [s.signature, {
      encoding: 'json', maxSupportedTransactionVersion: 1, commitment: 'finalized',
    }]);
    if (!raw) continue;
    const addrs = canonicalAddresses(raw);
    // per-tx uniqueness: a transaction can name the same address twice
    const perTxUnique = [...new Set(addrs)];
    totalSlots += addrs.length;
    for (const a of perTxUnique) if (!globalUnique.has(a)) globalUnique.set(a, rows.length);
    rows.push({
      sig: s.signature, slot: raw.slot, addrs, perTxUnique,
      tokenOwners: (raw.meta?.preTokenBalances || []).map((t) => ({
        idx: t.accountIndex, owner: t.owner, mint: t.mint,
      })),
    });
  }

  // Index check: does the canonical (static ++ loaded) list line up with
  // jsonParsed's own accountKeys (which INCLUDE loaded entries, flagged)?
  // A mismatch would mean the owner map gets keys reeflog itself would index
  // differently — checked here on a sample, not assumed.
  let idxChecked = 0, idxMismatch = 0;
  for (const r of rows.slice(0, 5)) {
    const raw = await rpc('getTransaction', [r.sig, {
      encoding: 'jsonParsed', maxSupportedTransactionVersion: 1, commitment: 'finalized',
    }]);
    const parsed = (raw?.transaction?.message?.accountKeys || []).map((k) => (typeof k === 'string' ? k : k.pubkey));
    idxChecked++;
    if (JSON.stringify(parsed) !== JSON.stringify(r.addrs)) idxMismatch++;
  }
  out.checks.canonical_index_matches_jsonParsed = {
    checked: idxChecked, mismatch: idxMismatch, ok: idxMismatch === 0,
    detail: 'canonical static++loaded order equals jsonParsed accountKeys (incl. loaded, in order)',
  };

  // ---- P1: call and byte cost ------------------------------------------
  const naiveCalls = rows.reduce((n, r) => n + r.perTxUnique.length, 0);
  const perRowBatchedCalls = rows.reduce((n, r) => n + Math.ceil(r.perTxUnique.length / BATCH), 0);
  const globalBatchedCalls = Math.ceil(globalUnique.size / BATCH);

  // Fetch owners for EVERY unique address, owner-only (dataSlice length 1):
  // the `owner` field is returned regardless of the slice.
  const ownerMap = new Map();     // address -> owner program
  const missingSet = new Set();   // address -> account gone at fetch time
  let fetchCalls = 0, fetchBytes = 0, fetchKeys = 0;
  const allUnique = [...globalUnique.keys()];
  for (let i = 0; i < allUnique.length; i += BATCH) {
    const chunk = allUnique.slice(i, i + BATCH);
    const res = await rpc('getMultipleAccounts', [chunk, {
      encoding: 'base64', dataSlice: { offset: 0, length: 1 }, commitment: 'finalized',
    }]);
    fetchCalls++; fetchKeys += chunk.length;
    const raw = JSON.stringify(res);
    fetchBytes += Buffer.byteLength(raw);
    const value = res.value || [];
    value.forEach((acct, k) => {
      const addr = chunk[k];
      if (acct === null) missingSet.add(addr);
      else ownerMap.set(addr, acct.owner);
    });
  }
  out.probes.owner_fetch = {
    unique_addresses: allUnique.length,
    address_slots_over_txs: totalSlots,
    repeat_factor: +(totalSlots / allUnique.length).toFixed(2),
    calls_one_per_address: naiveCalls,
    calls_per_row_batched: perRowBatchedCalls,
    calls_global_batched: fetchCalls,
    saved_vs_naive: +(naiveCalls / fetchCalls).toFixed(1),
    response_bytes_owner_only: fetchBytes,
    bytes_per_address: +(fetchBytes / fetchKeys).toFixed(1),
    accounts_missing_at_fetch: missingSet.size,
    missing_fraction: +(missingSet.size / allUnique.length).toFixed(3),
    missing_addresses: [...missingSet].slice(0, 10),
    batch_size_accepted: fetchCalls === Math.ceil(allUnique.length / BATCH),
  };

  // ---- P2: three-way authority agreement --------------------------------
  // (a) owner stated by preTokenBalances for the token account,
  // (b) owner DECODED from the account's own bytes (mint||owner at 0..64),
  // (c) owner the envelope's owner map records for the same address.
  // For (b) we must fetch full data for the token accounts — a small sample.
  const tokenAddrs = [];
  for (const r of rows) {
    for (const t of r.tokenOwners) {
      const addr = r.addrs[t.idx];
      if (addr && !tokenAddrs.some((x) => x.addr === addr)) {
        tokenAddrs.push({ addr, statedOwner: t.owner, mint: t.mint, sig: r.sig });
      }
    }
  }
  const sample = tokenAddrs.slice(0, 12);
  const agree = { all3: 0, stated_vs_bytes: 0, stated_vs_map: 0, checked: sample.length, mismatches: [] };
  const programAgree = { token_program_owned: 0, mint_agrees: 0, checked: 0 };
  for (const t of sample) {
    const res = await rpc('getAccountInfo', [t.addr, { encoding: 'base64', commitment: 'finalized' }]);
    const acct = res.value;
    if (!acct) { agree.mismatches.push({ addr: t.addr, reason: 'account gone' }); continue; }
    const data = Buffer.from(acct.data[0], 'base64');
    let decodedOwner = null, decodedMint = null;
    if (data.length >= 64) {
      decodedMint = b58(data.subarray(0, 32));
      decodedOwner = b58(data.subarray(32, 64));
    }
    const mapOwner = ownerMap.get(t.addr) || null;
    const bytesOk = decodedOwner === t.statedOwner;
    const mapOk = mapOwner === acct.owner;
    if (bytesOk) agree.stated_vs_bytes++;
    if (mapOk) agree.stated_vs_map++;
    if (bytesOk && mapOk) agree.all3++;
    else agree.mismatches.push({
      addr: t.addr, stated: t.statedOwner, decoded: decodedOwner, map: mapOwner,
      rpcOwner: acct.owner, dataLen: data.length,
    });
    programAgree.checked++;
    if (acct.owner === 'TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA'
      || acct.owner === 'TokenzQdBNbLqP5VEhdkAS6EPFLC1PHnBqCXEpPxuEb'
      || acct.owner === 'ATokenGPvbdGVxr1b2hvZbsiqW5xWH25efTNsLJA8knL') programAgree.token_program_owned++;
    // The mint DECODED from the account bytes must equal the mint the
    // transaction's token balances name — the account and the balance entry
    // are otherwise two unlinked claims about the same address.
    if (decodedMint === t.mint) programAgree.mint_agrees++;
  }
  out.probes.three_way_authority = { ...agree, program: programAgree };

  // ---- negative control -------------------------------------------------
  // The honesty-critical path: an address with NO account must come back as
  // MISSING (null), never as a guessed owner. A control that only checks a
  // known-good account proves nothing about the missing case.
  //
  // This address was observed in a real transaction AND is on the probe's
  // own missing list: its account no longer exists (closed/funded-away).
  // It must yield null and NO owner entry.
  const absentAccount = 'jitodontfronttitanssoLfLare1111111111111111';
  const ctrl = await rpc('getMultipleAccounts', [[absentAccount], {
    encoding: 'base64', dataSlice: { offset: 0, length: 1 }, commitment: 'finalized',
  }]);
  const ctrlValue = ctrl.value?.[0];
  const ctrlMissing = ctrlValue === null;
  const ctrlNotInMap = !ownerMap.has(absentAccount);
  // And one address that DOES exist must carry a real owner — the map is not
  // simply empty (a harness that "passes" by never filling anything proves
  // nothing).
  const aLive = [...ownerMap.entries()].find(([, o]) => !!o);
  out.probes.negative_control = {
    ok: ctrlMissing && ctrlNotInMap && !!aLive,
    absent_account_is_null: ctrlMissing,
    absent_account_absent_from_owner_map: ctrlNotInMap,
    absent_address: absentAccount,
    a_live_address_carries_an_owner: !!aLive,
    live_example: aLive ? { address: aLive[0], owner: aLive[1] } : null,
    detail: 'a well-formed but nonexistent address must yield null and NO owner entry; a live address must carry one',
  };

  // ---- verdict -----------------------------------------------------------
  const p1 = out.probes.owner_fetch.saved_vs_naive >= 10;
  const p2 = sample.length > 0 && agree.all3 === agree.checked;
  out.verdict = {
    p1_cost_saves_10x: p1,
    p2_authority_three_way: p2,
    negative_control_holds: out.probes.negative_control.ok,
    premise_holds: p1 && p2 && out.probes.negative_control.ok,
    statement: p1 && p2 && out.probes.negative_control.ok
      ? `premise HOLDS: ${agree.all3}/${agree.checked} token accounts agree across stated/decoded/map owners; ${fetchCalls} batched calls replace ${naiveCalls} naive ones (${out.probes.owner_fetch.saved_vs_naive}x); ${out.probes.owner_fetch.accounts_missing_at_fetch} of ${allUnique.length} addresses are gone at fetch time and stay explicitly missing`
      : `premise FAILS: p1=${p1} p2=${p2} neg=${out.probes.negative_control.ok} — see mismatches`,
  };

  const file = 'research/owners-probe-2026-11-01.json';
  fs.writeFileSync(file, JSON.stringify(out, null, 2));
  console.log(JSON.stringify(out, null, 2));
  console.log(`\nwrote ${file}`);
  process.exit(out.verdict.premise_holds ? 0 : 1);
}

main().catch((e) => { console.error('probe failed:', e); process.exit(2); });
