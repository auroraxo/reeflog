// Measure the public endpoint's tolerance for sequential
// getSignaturesForAddress calls at limit=1000 — the walk primitive reeflog
// depends on. How many pages can it take before it answers 429?
const RPC = 'https://api.mainnet-beta.solana.com';
const PROGRAM = '675kPX9MHTjS2zt1qfr1NYHuzeLXfQM9H24wFSUt1Mp8';

async function rpc(method, params) {
  const r = await fetch(RPC, {
    method: 'POST',
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify({ jsonrpc: '2.0', id: 1, method, params }),
  });
  if (r.status === 429) return { code: 429 };
  const j = await r.json();
  if (j.error) return { code: 'rpc', error: j.error };
  return { code: 200, result: j.result };
}

(async () => {
  const LIMIT = Number(process.argv[2] || 1000);
  const N = Number(process.argv[3] || 40);
  let before = null;
  let ok = 0, throttled = 0, errs = 0;
  const t0 = Date.now();
  for (let i = 0; i < N; i++) {
    const params = [PROGRAM, { limit: LIMIT, commitment: 'finalized' }];
    if (before) params[1].before = before;
    const r = await rpc('getSignaturesForAddress', params);
    if (r.code === 429) { throttled++; await new Promise((s) => setTimeout(s, 1500)); continue; }
    if (r.code !== 200) { errs++; console.log('  err', JSON.stringify(r.error).slice(0, 120)); await new Promise((s) => setTimeout(s, 1000)); continue; }
    ok++;
    const rows = r.result;
    if (!rows.length) break;
    before = rows[rows.length - 1].signature;
    if (i % 5 === 0) process.stdout.write(`  page ${i} slot ${rows[0].slot}\n`);
  }
  const secs = (Date.now() - t0) / 1000;
  console.log(`limit=${LIMIT} calls=${N} ok=${ok} throttled=${throttled} errs=${errs} in ${secs.toFixed(1)}s (${(ok / secs).toFixed(2)} pages/s)`);
})();
