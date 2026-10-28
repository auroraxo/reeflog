// Batch search for a REAL forked-out (reverted) transaction.
//
// Why batch: getSignatureStatuses accepts up to 256 signatures per call and,
// with searchTransactionHistory, answers null for a signature the canonical
// chain does not know — including one whose block was forked out. That turns
// a 15k-signature sweep (one call each, 429-heavy) into ~60 calls.
//
// Every null candidate is then confirmed independently with getTransaction
// (the same two-source rule the detector must apply): only a signature that
// BOTH sources refuse is a real fork-out, and only a signature the journal
// actually delivered via WebSocket qualifies as a revert fixture.
import { readFileSync, writeFileSync } from 'node:fs';
import { execSync } from 'node:child_process';

const RPC = 'https://api.mainnet-beta.solana.com';
let calls = 0, rateLimited = 0;

async function rpc(method, params) {
  calls++;
  for (let attempt = 0; attempt < 8; attempt++) {
    const res = await fetch(RPC, {
      method: 'POST',
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify({ jsonrpc: '2.0', id: 1, method, params }),
    });
    if (res.status === 429) { rateLimited++; await new Promise(r => setTimeout(r, 700 * (attempt + 1))); continue; }
    if (res.status >= 500) { await new Promise(r => setTimeout(r, 500 * (attempt + 1))); continue; }
    const body = await res.json();
    if (body.error) return { error: body.error };
    return { value: body.result };
  }
  return { error: { message: 'retries exhausted' } };
}

const dbs = execSync(
  `python3 - <<'PY'
import sqlite3, json
out = []
for db in ['/tmp/reeflog-killtest-full/reeflog.db', '/tmp/reeflog-killtest-sync/reeflog.db',
           '/tmp/reeflog-killtest-confirm/reeflog.db', '/tmp/reeflog-killtest-compat/reeflog.db',
           '/tmp/reeflog-killtest-worker/reeflog.db']:
    try:
        c = sqlite3.connect(db)
        for sig, slot in c.execute("select signature, slot from events where source='ws' order by seq"):
            out.append([sig, slot])
    except Exception:
        pass
print(json.dumps(out))
PY`,
  { maxBuffer: 1 << 26 }
).toString();

const rows = JSON.parse(dbs);
console.log(JSON.stringify({ phase: 'loaded', wsSignatures: rows.length }));

const candidates = [];
for (let i = 0; i < rows.length; i += 200) {
  const chunk = rows.slice(i, i + 200);
  const res = await rpc('getSignatureStatuses', [chunk.map(r => r[0]), { searchTransactionHistory: true }]);
  if (res.error) {
    console.log(JSON.stringify({ phase: 'chunk-error', at: i, error: String(res.error.message || '').slice(0, 80) }));
    continue;
  }
  const vals = res.value && res.value.value;
  if (!Array.isArray(vals)) { console.log(JSON.stringify({ phase: 'bad-shape', at: i })); continue; }
  vals.forEach((st, k) => {
    if (st === null || st === undefined) candidates.push(chunk[k]);
  });
  if ((i / 200) % 10 === 0) console.log(JSON.stringify({ phase: 'sweep', scanned: i + chunk.length, nullCandidates: candidates.length, calls, rateLimited }));
}

console.log(JSON.stringify({ phase: 'sweep-done', scanned: rows.length, nullCandidates: candidates.length, calls, rateLimited }));

// Second, independent source for each candidate.
const confirmed = [];
for (const [signature, slot] of candidates) {
  const tx = await rpc('getTransaction', [signature, { encoding: 'json', maxSupportedTransactionVersion: 1, commitment: 'finalized' }]);
  const statusKnown = false;
  const txKnown = !(tx.error) && tx.value !== null;
  confirmed.push({ signature, slot, txKnown, statusKnown, txError: tx.error ? String(tx.error.message || '').slice(0, 60) : null });
}

const realForkOuts = confirmed.filter(c => !c.txKnown && !c.txError);
const inconclusive = confirmed.filter(c => c.txError);
writeFileSync('/tmp/forkout-candidates.json', JSON.stringify({ candidates: confirmed }, null, 1));
console.log(JSON.stringify({
  phase: 'done',
  scanned: rows.length,
  statusNull: candidates.length,
  bothSourcesRefuse: realForkOuts.length,
  rpcIndeterminate: inconclusive.length,
  real: realForkOuts.slice(0, 5),
  calls, rateLimited,
}));
