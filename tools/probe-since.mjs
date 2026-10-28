// Probe: is `since` (a SLOT) usable to anchor the verify walk?
// If yes, no journal-held signature is ever needed as an RPC boundary —
// which removes the -32020 phantom failure mode entirely.
const RPC = 'https://api.mainnet-beta.solana.com';
const PROGRAM = '675kPX9MHTjS2zt1qfr1NYHuzeLXfQM9H24wFSUt1Mp8';
const FLOOR = 462334353; // journal's oldest held slot
const CEIL = 462337273;  // journal's newest held slot

async function rpc(method, params) {
  const r = await fetch(RPC, {
    method: 'POST',
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify({ jsonrpc: '2.0', id: 1, method, params }),
  });
  const j = await r.json();
  return j.error ? { error: j.error } : { result: j.result };
}

function show(label, out) {
  if (out.error) { console.log(`${label}: ERROR ${out.error.code} ${out.error.message}`); return null; }
  const rows = out.result;
  if (!Array.isArray(rows)) { console.log(`${label}: ${JSON.stringify(rows).slice(0, 160)}`); return null; }
  const first = rows[0], last = rows[rows.length - 1];
  console.log(`${label}: n=${rows.length} newest=slot ${first?.slot} oldest=slot ${last?.slot}`);
  if (rows.length) console.log(`   first sig ${first.signature}`);
  if (rows.length > 1) console.log(`   last  sig ${last.signature}`);
  return rows;
}

(async () => {
  console.log('--- S1: since=FLOOR ---');
  const s1 = show('S1', await rpc('getSignaturesForAddress', [PROGRAM, { limit: 10, since: FLOOR, commitment: 'finalized' }]));

  console.log('--- S2: since=FLOOR, limit 1000 (is the range bounded below by since?) ---');
  const s2 = show('S2', await rpc('getSignaturesForAddress', [PROGRAM, { limit: 1000, since: FLOOR, commitment: 'finalized' }]));

  console.log('--- S3: does before-page still respect since? ---');
  if (s2 && s2.length) {
    const last = s2[s2.length - 1];
    const s3 = await rpc('getSignaturesForAddress', [PROGRAM, { limit: 1000, since: FLOOR, before: last.signature, commitment: 'finalized' }]);
    show('S3', s3);
  }

  console.log('--- S4: since=CEIL+1 (empty range expected) ---');
  show('S4', await rpc('getSignaturesForAddress', [PROGRAM, { limit: 10, since: CEIL + 1, commitment: 'finalized' }]));

  console.log('--- S5: since=CEIL (newest in-window, then page down) ---');
  const s5 = show('S5', await rpc('getSignaturesForAddress', [PROGRAM, { limit: 5, since: CEIL, commitment: 'finalized' }]));

  console.log('--- S6: before+until together ---');
  const OLDEST = '4EL2SxpfWAQCo7FrSqCsyzpkDWRtNxvVNEmwHBKYLUjgCCWELZAgfrV5dtqW8pSGRw6sxyjHgozioe5vbjHy3DY2';
  const t = await rpc('getSignaturesForAddress', [PROGRAM, { limit: 1000, before: OLDEST, until: OLDEST, commitment: 'finalized' }]);
  show('S6', t);

  console.log('--- S7: unsupported param check (typo) ---');
  show('S7', await rpc('getSignaturesForAddress', [PROGRAM, { limit: 10, sinces: FLOOR, commitment: 'finalized' }]));
})();
