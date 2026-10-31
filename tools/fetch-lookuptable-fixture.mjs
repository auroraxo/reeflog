// Fetch ONE real mainnet transaction that uses an address-lookup table, in
// both encodings, and pin it as a fixture:
//   raw  = encoding json         (what reeflog's hydrate pays for)
//   auth = encoding jsonParsed   (the RPC's own signer/writable flags)
//
// Purpose: the account-list path for LOADED accounts has no authority in the
// encoding reeflog stores. This fixture lets the test assert, offline and on
// every run, that reeflog's derived list equals jsonParsed's stated flags.
import fs from 'node:fs';

const RPC = process.env.RPC || 'https://api.mainnet-beta.solana.com';
const PROGRAMS = ['JUP6LkbZbjS1jKKwapdHNy74zcZ3tLUZoi5QNyVTaV4', '675kPX9MHTjS2zt1qfr1NYHuzeLXfQM9H24wFSUt1Mp8'];

let backoff = 500;
async function rpc(method, params, tries = 30) {
  let last;
  for (let i = 0; i < tries; i++) {
    if (i) await new Promise((r) => setTimeout(r, backoff + Math.floor(Math.random() * 500)));
    try {
      const r = await fetch(RPC, {
        method: 'POST', headers: { 'content-type': 'application/json' },
        body: JSON.stringify({ jsonrpc: '2.0', id: 1, method, params }),
      });
      if (r.status === 429) { last = new Error('429'); backoff = Math.min(backoff * 2, 9000); continue; }
      const j = await r.json();
      if (j.error) {
        const s = JSON.stringify(j.error);
        if (s.includes('-32005') || s.includes('Too many')) { last = new Error('throttle'); backoff = Math.min(backoff * 2, 9000); continue; }
        throw new Error(s);
      }
      backoff = Math.max(300, Math.floor(backoff / 2));
      return j.result;
    } catch (e) { last = e; }
  }
  throw last;
}

for (const program of PROGRAMS) {
  const sigs = await rpc('getSignaturesForAddress', [program, { limit: 15 }]);
  for (const s of sigs) {
    const raw = await rpc('getTransaction', [s.signature, { encoding: 'json', maxSupportedTransactionVersion: 1, commitment: 'finalized' }]);
    if (!raw || !raw.meta) continue;
    const lw = raw.meta.loadedAddresses?.writable?.length ?? 0;
    const lr = raw.meta.loadedAddresses?.readonly?.length ?? 0;
    if (lw + lr < 4) continue; // want a real lookup-table use, not a token one
    const auth = await rpc('getTransaction', [s.signature, { encoding: 'jsonParsed', maxSupportedTransactionVersion: 1, commitment: 'finalized' }]);
    if (!auth) continue;
    const fixture = {
      signature: s.signature,
      program,
      fetchedAt: new Date().toISOString(),
      note: 'raw = encoding json (what reeflog stores); auth = encoding jsonParsed (the RPC signer/writable flags)',
      loadedWritable: lw, loadedReadonly: lr,
      raw,
      auth: auth.transaction.message.accountKeys,
    };
    const out = process.argv[2] || 'test/fixtures/account-list-lookuptable.json';
    fs.writeFileSync(out, JSON.stringify(fixture, null, 1));
    console.log(JSON.stringify({ written: out, signature: s.signature, lw, lr, nStatic: raw.transaction.message.accountKeys.length, nAuth: fixture.auth.length }, null, 1));
    process.exit(0);
  }
}
console.error('no lookup-table transaction found');
process.exit(1);
