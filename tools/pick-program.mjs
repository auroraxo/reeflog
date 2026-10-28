// pick-program.mjs — measure recent signature throughput for candidate
// programs so the kill-test can use a program whose ground truth is cheap to
// fetch (public RPC rate-limits after ~10 pages of 500).
//
// usage: node tools/pick-program.mjs <programId> [<programId>...]

const RPC = process.env.SOLANA_RPC || 'https://api.mainnet-beta.solana.com';

async function rpc(method, params) {
  const r = await fetch(RPC, {
    method: 'POST',
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify({ jsonrpc: '2.0', id: 1, method, params }),
  });
  const j = await r.json();
  if (j.error) throw new Error(`${method}: ${JSON.stringify(j.error)}`);
  return j.result;
}

async function rate(program) {
  // Two pages 90 seconds apart in chain terms: take the tip signature now,
  // then walk back to the signature that was ~90s older.
  const now = await rpc('getSlot', [{ commitment: 'finalized' }]);
  const first = await rpc('getSignaturesForAddress', [
    program,
    { limit: 1000, commitment: 'finalized' },
  ]);
  if (!first.length) return { program, sigs: 0 };
  // Walk back one page to estimate rate per page.
  const second = await rpc('getSignaturesForAddress', [
    program,
    { limit: 1000, commitment: 'finalized', before: first[first.length - 1].signature },
  ]);
  const spanSlots = first[0].slot - (second.length ? second[second.length - 1].slot : first[0].slot);
  return {
    program,
    page1: first.length,
    page2: second.length,
    tipSlot: first[0].slot,
    spanSlots,
    approxPerMin: spanSlots ? Math.round((first.length + second.length) / (spanSlots / 60)) : null,
    nowSlot: now,
  };
}

const ids = process.argv.slice(2);
for (const id of ids) {
  try {
    console.log(JSON.stringify(await rate(id)));
  } catch (e) {
    console.log(JSON.stringify({ program: id, error: String(e) }));
  }
  await new Promise((r) => setTimeout(r, 400));
}
