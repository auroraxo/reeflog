// Probe: what does the getTransaction response reeflog ALREADY pays for
// actually contain, and what would it cost to keep the account part?
//
// reeflog's hydration pass already fetches every missing row with
// `getTransaction` and stores `meta.logMessages` — nothing else. The rest of
// the response (account keys, loaded addresses, balance deltas) is thrown
// away. This probe measures what is in it, so the account feature is designed
// against measurements instead of against the Solana docs.
import fs from 'node:fs';

const RPC = process.env.RPC || 'https://api.mainnet-beta.solana.com';
const PROGRAM = process.env.PROGRAM || '675kPX9MHTjS2zt1qfr1NYHuzeLXfQM9H24wFSUt1Mp8'; // Raydium AMM v4

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

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

async function main() {
  const sigs = await rpc('getSignaturesForAddress', [PROGRAM, { limit: 12 }]);
  const out = { rpc: RPC, program: PROGRAM, probes: [] };
  let i = 0;
  for (const s of sigs) {
    if (s.err) continue;
    if (i >= 4) break;
    i++;
    const raw = await rpc('getTransaction', [s.signature, {
      encoding: 'json', maxSupportedTransactionVersion: 1, commitment: 'finalized',
    }]);
    if (!raw) { out.probes.push({ sig: s.signature, note: 'chain does not know it' }); continue; }
    const bytesJson = Buffer.byteLength(JSON.stringify(raw));
    const meta = raw.meta || {};
    const msg = raw.transaction?.message || {};
    const staticKeys = msg.accountKeys || [];
    const loaded = meta.loadedAddresses || {};
    const loadedW = loaded.writable || [];
    const loadedR = loaded.readonly || [];
    const pre = meta.preBalances || [];
    const post = meta.postBalances || [];
    const preTok = meta.preTokenBalances || [];
    const postTok = meta.postTokenBalances || [];

    // lamport deltas
    let lamportDelta = 0;
    let nonZero = 0;
    for (let k = 0; k < pre.length; k++) {
      const d = (post[k] ?? 0) - (pre[k] ?? 0);
      lamportDelta += d;
      if (d !== 0) nonZero++;
    }
    // token deltas keyed by account+token
    const tokMap = new Map();
    for (const t of preTok) tokMap.set(`${t.accountIndex}:${t.mint}:${t.owner}`, { pre: t.uiTokenAmount?.amount ?? null, idx: t.accountIndex });
    for (const t of postTok) {
      const k = `${t.accountIndex}:${t.mint}:${t.owner}`;
      const e = tokMap.get(k) || { pre: null, idx: t.accountIndex };
      e.post = t.uiTokenAmount?.amount ?? null;
      tokMap.set(k, e);
    }
    const tokChanged = [...tokMap.values()].filter((e) => e.pre !== e.post).length;

    // how many accounts does jsonParsed see that json does not?
    let innerOnly = null;
    let parsedAccountKeys = null;
    try {
      const parsed = await rpc('getTransaction', [s.signature, {
        encoding: 'jsonParsed', maxSupportedTransactionVersion: 1, commitment: 'finalized',
      }]);
      if (parsed) {
        const seen = new Set();
        let innerAccounts = 0;
        const walk = (ixs) => {
          for (const ix of ixs || []) {
            if (ix.accounts) { innerAccounts += ix.accounts.length; for (const a of ix.accounts) seen.add(a); }
            if (ix.parsed && ix.parsed.info && Array.isArray(ix.parsed.info.accounts)) {
              for (const a of ix.parsed.info.accounts) { innerAccounts++; seen.add(a); }
            }
          }
        };
        walk(parsed.transaction?.message?.instructions);
        for (const inner of parsed.meta?.innerInstructions || []) walk(inner.instructions);
        parsedAccountKeys = seen.size;
        const jsonSet = new Set([...staticKeys, ...loadedW, ...loadedR]);
        innerOnly = [...seen].filter((a) => !jsonSet.has(a));
        out.probes.push({
          sig: s.signature,
          slot: raw.slot,
          version: msg.version ?? 'legacy',
          bytes_json_response: bytesJson,
          static_accounts: staticKeys.length,
          loaded_writable: loadedW.length,
          loaded_readonly: loadedR.length,
          total_accounts_json: staticKeys.length + loadedW.length + loadedR.length,
          lamport_balance_slots: pre.length,
          accounts_with_lamport_delta: nonZero,
          net_lamport_delta: lamportDelta,
          token_balance_rows: tokMap.size,
          token_rows_changed: tokChanged,
          jsonParsed_distinct_accounts: parsedAccountKeys,
          accounts_only_in_jsonParsed: innerOnly ? innerOnly.length : null,
          sample_inner_only: innerOnly ? innerOnly.slice(0, 3) : null,
        });
      }
    } catch (e) {
      out.probes.push({ sig: s.signature, error: String(e.message || e) });
    }
    await sleep(1200);
  }
  const p = out.probes.filter((x) => x.total_accounts_json != null);
  if (p.length) {
    out.summary = {
      probes: p.length,
      avg_static: +(p.reduce((a, x) => a + x.static_accounts, 0) / p.length).toFixed(1),
      avg_total_accounts: +(p.reduce((a, x) => a + x.total_accounts_json, 0) / p.length).toFixed(1),
      avg_bytes_response: Math.round(p.reduce((a, x) => a + x.bytes_json_response, 0) / p.length),
      avg_accounts_only_in_jsonParsed: +(p.reduce((a, x) => a + (x.accounts_only_in_jsonParsed || 0), 0) / p.length).toFixed(1),
      total_lamport_delta_accounts: p.reduce((a, x) => a + x.accounts_with_lamport_delta, 0),
      total_token_rows_changed: p.reduce((a, x) => a + x.token_rows_changed, 0),
    };
  }
  fs.writeFileSync(process.env.OUT || '/tmp/reeflog-account-probe.json', JSON.stringify(out, null, 2));
  console.log(JSON.stringify(out.summary || out, null, 2));
}
main().catch((e) => { console.error('FAILED:', e.message); process.exit(1); });
