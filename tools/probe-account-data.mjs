// Premise probe for `hydrate --account-data`: how big ARE the accounts a
// transaction enumerates, on live mainnet?
//
// The design question the numbers must answer (measured, not argued):
//   - What byte cap (if any) does a full-data batch fetch need? Fetching
//     full bytes for every address of every row without a bound could move
//     megabytes per row through a public endpoint; a cap that covers almost
//     everything while bounding the worst case is only defensible if we know
//     the real distribution.
//   - How many accounts sit at or above the candidate cap (16384 B)? Those
//     are the ones that would need the at-cap probe (offset=cap, length=1)
//     to classify `full` vs `truncated` — that is the probe's cost model.
//   - How many addresses no longer exist (the `missing` rule the owners pass
//     already established must carry over)?
//
// Method: recent Raydium AMM v4 transactions → canonical account list
// (static ++ loaded writable ++ loaded readonly, the same order as the
// envelope) → ONE batched getMultipleAccounts per 100 addresses with NO
// dataSlice (full bytes) → exact per-address length from the returned base64.
//
// Exit codes: 0 = measurements complete, 1 = harness defect, 2 = setup failed
// (rate limit / empty window — NOT a verdict).
const RPC = process.env.RPC || 'https://api.mainnet-beta.solana.com';
const PROGRAM = process.env.PROGRAM || '675kPX9MHTjS2zt1qfr1NYHuzeLXfQM9H24wFSUt1Mp8'; // Raydium AMM v4
const TXS = Number(process.env.TXS || 30);
const CAP = Number(process.env.CAP || 16384);

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

function canonicalAddresses(tx) {
  const msg = tx.transaction.message;
  const out = [];
  for (const k of msg.accountKeys) out.push(typeof k === 'string' ? k : k.pubkey);
  for (const a of tx.meta?.loadedAddresses?.writable || []) out.push(a);
  for (const a of tx.meta?.loadedAddresses?.readonly || []) out.push(a);
  return out;
}

const sigs = await rpc('getSignaturesForAddress', [PROGRAM, { limit: TXS * 2 }]);
if (!sigs?.length) { console.error('FAIL setup: no signatures'); process.exit(2); }

// Distinct transactions, newest first, skipping failed txs (their envelope
// still enumerates accounts, but a mix would muddy the size story).
const txs = [];
for (const s of sigs) {
  if (txs.length >= TXS) break;
  const tx = await rpc('getTransaction', [s.signature, {
    encoding: 'json', maxSupportedTransactionVersion: 1, commitment: 'finalized',
  }]);
  if (tx?.meta && tx.meta.err === null) txs.push(tx);
}
if (txs.length < 10) { console.error(`FAIL setup: only ${txs.length} usable txs`); process.exit(2); }

const all = new Set();
for (const tx of txs) for (const a of canonicalAddresses(tx)) all.add(a);
const addresses = [...all];
console.log(`measuring ${txs.length} transactions, ${addresses.length} distinct addresses`);

// Full bytes, batched, no dataSlice: the cost model of the proposed pass.
// Also records WHICH program owns the biggest accounts — if the tail is
// program executables (owner == BPFLoader), the storage story must say so.
const sizes = new Map();   // address -> byte length (exists)
const ownerOf = new Map(); // address -> owner program
const execOf = new Map();  // address -> executable flag
const gone = [];
let fetchCalls = 0;
for (let i = 0; i < addresses.length; i += 100) {
  const chunk = addresses.slice(i, i + 100);
  const res = await rpc('getMultipleAccounts', [chunk, { encoding: 'base64', commitment: 'finalized' }]);
  fetchCalls++;
  chunk.forEach((a, k) => {
    const e = res.value[k];
    if (!e) { gone.push(a); return; }
    sizes.set(a, Buffer.from(e.data[0], 'base64').length);
    ownerOf.set(a, e.owner || null);
    execOf.set(a, !!e.executable);
  });
}

const lens = [...sizes.values()].sort((a, b) => a - b);
const q = (p) => lens[Math.min(lens.length - 1, Math.floor(p * lens.length))];
const over = lens.filter((n) => n > CAP).length;
const atCap = lens.filter((n) => n === CAP).length;
const totalBytes = lens.reduce((a, b) => a + b, 0);
// Who owns the oversized accounts? Owner identity decides the storage rule:
// program bytes (owner BPFLoaderUpgradeable/211111...) are environment, not
// event data; storing them in a journal would be wrong in a different way
// than truncating data would be wrong for state accounts.
const bpfOwner = [...ownerOf.values()].find((o) => o && o.startsWith('BPFLoader'));
const bigOwners = [...sizes.entries()]
  .sort((a, b) => b[1] - a[1])
  .slice(0, 6)
  .map(([a, n]) => ({ bytes: n, owner: ownerOf.get(a), executable: execOf.get(a), address: a }));
// Bytes if program executables are NOT stored as event data (the candidate
// rule): total bytes over state (non-executable) accounts only.
const stateTotal = [...sizes.entries()].filter(([a]) => !execOf.get(a)).reduce((s, [, n]) => s + n, 0);
const execTotal = [...sizes.entries()].filter(([a]) => execOf.get(a)).reduce((s, [, n]) => s + n, 0);
const perRowFullBytes = txs.map((tx) => {
  const seen = new Set();
  let sum = 0;
  for (const a of canonicalAddresses(tx)) {
    if (seen.has(a)) continue;
    seen.add(a);
    sum += sizes.get(a) ?? 0;
  }
  return sum;
}).sort((a, b) => a - b);
// The DB-growth number: what one row would actually STORE under the
// candidate rule (non-executable state only, min(size, cap) per address).
const perRowCappedBytes = txs.map((tx) => {
  const seen = new Set();
  let sum = 0;
  for (const a of canonicalAddresses(tx)) {
    if (seen.has(a)) continue;
    seen.add(a);
    if (execOf.get(a)) continue;
    const n = sizes.get(a);
    if (n != null) sum += Math.min(n, CAP);
  }
  return sum;
}).sort((a, b) => a - b);

const report = {
  probe: 'account-data-size-2026-11-02',
  txs: txs.length,
  distinct_addresses: addresses.length,
  missing_addresses: gone.length,
  fetch_calls_full: fetchCalls,
  bytes: {
    min: lens[0], p50: q(0.5), p90: q(0.9), p95: q(0.95), p99: q(0.99), max: lens[lens.length - 1],
    total: totalBytes,
  },
  cap_candidate: CAP,
  over_cap: over,
  at_cap_exact: atCap,
  needs_probe: over + atCap,
  biggest_account_owners: bigOwners,
  bpf_loader_owner_present: !!bpfOwner,
  state_bytes_total: stateTotal,
  executable_bytes_total: execTotal,
  per_row_full_bytes_p50: perRowFullBytes[Math.floor(perRowFullBytes.length / 2)],
  per_row_full_bytes_max: perRowFullBytes[perRowFullBytes.length - 1],
  per_row_stored_bytes_p50: perRowCappedBytes[Math.floor(perRowCappedBytes.length / 2)],
  per_row_stored_bytes_max: perRowCappedBytes[perRowCappedBytes.length - 1],
  zero_byte_accounts: lens.filter((n) => n === 0).length,
};
console.log(JSON.stringify(report, null, 2));
process.exit(0);
