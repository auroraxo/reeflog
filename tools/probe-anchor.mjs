// Probe real RPC anchor semantics for the reeflog verify walk.
// Question: which `before` / `until` boundaries are usable, and how do
// phantom (forked-out) signatures behave?
const RPC = 'https://api.mainnet-beta.solana.com';
const PROGRAM = '675kPX9MHTjS2zt1qfr1NYHuzeLXfQM9H24wFSUt1Mp8';

const PHANTOM = 'WzDZdoPvsjECDAr6bZKR97sMR8e3ZPGHLoCMHtp8DJeBYeyaxyED9TQ3fmh1NSnDTKFjSsny4VWVsufLQvTYxQx'; // slot 462337273, confirmed-then-forked-out
const VALID = '2jpD7qCrGW8DFEyXQ43QZB8qEff9cDePoanCbwtze1gnYRLQ3V6ms4xYXq8LZvMVc5MECuhihbCTwju7s9uAu2Mg';     // slot 462337273
const OLDEST = '4EL2SxpfWAQCo7FrSqCsyzpkDWRtNxvVNEmwHBKYLUjgCCWELZAgfrV5dtqW8pSGRw6sxyjHgozioe5vbjHy3DY2'; // slot 462334353, oldest held

async function rpc(method, params) {
  const r = await fetch(RPC, {
    method: 'POST',
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify({ jsonrpc: '2.0', id: 1, method, params }),
  });
  const j = await r.json();
  return j.error ? { error: j.error } : { result: j.result };
}

function summarize(label, out) {
  if (out.error) {
    console.log(`${label}: ERROR ${out.error.code} ${out.error.message}`);
    return;
  }
  const rows = out.result;
  if (!Array.isArray(rows)) { console.log(`${label}: ${JSON.stringify(rows).slice(0, 200)}`); return; }
  const first = rows[0], last = rows[rows.length - 1];
  console.log(`${label}: n=${rows.length} newest=slot ${first?.slot} oldest=slot ${last?.slot}`);
}

(async () => {
  console.log('--- A: before=<phantom> ---');
  summarize('A', await rpc('getSignaturesForAddress', [PROGRAM, { limit: 10, before: PHANTOM, commitment: 'finalized' }]));

  console.log('--- B: until=<phantom> ---');
  summarize('B', await rpc('getSignaturesForAddress', [PROGRAM, { limit: 10, until: PHANTOM, commitment: 'finalized' }]));

  console.log('--- C: before=<valid> ---');
  summarize('C', await rpc('getSignaturesForAddress', [PROGRAM, { limit: 10, before: VALID, commitment: 'finalized' }]));

  console.log('--- D: until=<valid> ---');
  summarize('D', await rpc('getSignaturesForAddress', [PROGRAM, { limit: 10, until: VALID, commitment: 'finalized' }]));

  console.log('--- E: before=<oldest held> ---');
  summarize('E', await rpc('getSignaturesForAddress', [PROGRAM, { limit: 10, before: OLDEST, commitment: 'finalized' }]));

  console.log('--- F: until=<oldest held> (limit 1000) ---');
  const f = await rpc('getSignaturesForAddress', [PROGRAM, { limit: 1000, until: OLDEST, commitment: 'finalized' }]);
  summarize('F', f);
  if (Array.isArray(f.result)) {
    // The last element is the OLDEST signature newer than OLDEST => the exact
    // `before` boundary that covers the journal's whole window.
    const last = f.result[f.result.length - 1];
    console.log('   F last (candidate before-boundary):', last.slot, last.signature);
    const probe = await rpc('getSignaturesForAddress', [PROGRAM, { limit: 10, before: last.signature, commitment: 'finalized' }]);
    summarize('   F-follow', probe);
  }

  console.log('--- G: getSignatureStatuses on phantom + valid ---');
  const st = await rpc('getSignatureStatuses', [[PHANTOM, VALID], { searchTransactionHistory: true }]);
  console.log(JSON.stringify(st.result ?? st.error));

  console.log('--- H: tip page (no boundary) ---');
  summarize('H', await rpc('getSignaturesForAddress', [PROGRAM, { limit: 10, commitment: 'finalized' }]));
})();
