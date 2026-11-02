// Premise probe: does pre-event account state really need an ARCHIVAL RPC,
// or is the chain's own transaction history enough?
//
// The README's open qualification item (item 4, "historical state") says:
//
//   "What remains open is the half that is genuinely different: account bytes
//    AS OF the event (historical state), not bytes at fetch time. That is a
//    different problem — it needs the chain's history, not a second fetch —
//    and it stays open."
//
// That sentence contains two claims that have never been measured:
//   (P1) historical state is NOT reconstructible from what a normal RPC
//        serves (i.e. an archival RPC is required), and
//   (P2) the chain's transaction HISTORY would not be enough anyway.
//
// This probe measures them against live mainnet.
//
// The chain already states the pre-event state, for free, inside the
// transaction's own metadata: `meta.preBalances` (lamports before) and
// `meta.preTokenBalances` (token amount before, plus the owning token
// program). What it does NOT state is the account's BYTES before the event.
//
// Reconstruction candidate, no archival node required:
//   For account A touched by transaction T at slot S:
//     find the most recent transaction P (slot < S) that also touched A,
//     P's post-balances for A are A's state after P,
//     and if no other transaction touched A between P and T, then A's state
//     after P IS A's state before T.
//
// The chain supplies an INDEPENDENT WITNESS for that reconstruction: T's own
// `preBalances` says what A held before T. So the reconstruction is
// falsifiable without any assumption:
//     candidate(A) == T.meta.preBalances[A]  =>  verified
//     candidate(A) != T.meta.preBalances[A]  =>  refuted (or A was touched in
//                                                between, which is exactly
//                                                the gap the verdict reports)
//
// A high agreement rate would REFUTE P1/P2: pre-event state is
// reconstructible from the chain's transaction history, which every RPC
// serves, and the reconstruction is self-verifying against the witness.
//
// What this probe measures (no verdict assumed):
//   1. PRIOR-TOUCH FOUND  — how often a prior touching transaction exists at
//      all (the "no history" case, e.g. a brand-new account).
//   2. LAMPORT AGREEMENT  — candidate vs witness, per (tx, account).
//   3. TOKEN AGREEMENT    — same for token amounts.
//   4. COST               — RPC calls per reconstructed account.
//
// Exit codes: 0 = measurement completed (read the report for the verdict),
// 1 = a check failed, 2 = setup failed (rate limit / empty program — NOT a
// verdict), 3 = skipped (missing prerequisites).
import fs from 'node:fs';
import path from 'node:path';

const RPC = process.env.RPC || 'https://api.mainnet-beta.solana.com';
const PROGRAM = process.env.PROGRAM || '675kPX9MHTjS2zt1qfr1NYHuzeLXfQM9H24wFSUt1Mp8'; // Raydium AMM v4
const TX_COUNT = Number(process.env.TX_COUNT || 6);
const PRIOR_LIMIT = Number(process.env.PRIOR_LIMIT || 12);
const OUT = process.env.OUT || 'research/pre-state-probe-2026-11-02.json';

let calls = 0;
async function rpc(method, params, tries = 8) {
  for (let i = 0; i < tries; i++) {
    if (i) await new Promise((r) => setTimeout(r, Math.min(1000 * 2 ** (i - 1), 12000)));
    try {
      calls++;
      const r = await fetch(RPC, {
        method: 'POST',
        headers: { 'content-type': 'application/json' },
        body: JSON.stringify({ jsonrpc: '2.0', id: 1, method, params }),
      });
      if (r.status === 429) continue;
      const j = await r.json();
      if (j.error) throw new Error(JSON.stringify(j.error));
      return j.result;
    } catch (e) {
      if (i === tries - 1) throw e;
    }
  }
  throw new Error('exhausted');
}
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

// The account list, in the order the balance arrays use: static keys, then the
// lookup table's writable, then its readonly. Same rule reeflog's
// src/accounts.rs applies — indexing balances by `message.accountKeys` alone
// maps every balance to the WRONG address on a lookup-table transaction.
function canonicalAddresses(tx) {
  const msg = tx.transaction.message;
  const out = [];
  for (const k of msg.accountKeys || []) {
    out.push(typeof k === 'string' ? k : k.pubkey);
  }
  const loaded = (tx.meta && tx.meta.loadedAddresses) || {};
  for (const group of ['writable', 'readonly']) {
    for (const a of loaded[group] || []) out.push(a);
  }
  return out;
}

function balancesAt(tx, address) {
  const addrs = canonicalAddresses(tx);
  const idx = addrs.indexOf(address);
  if (idx < 0) return null;
  const pre = tx.meta && tx.meta.preBalances;
  const post = tx.meta && tx.meta.postBalances;
  if (!pre || !post || idx >= pre.length || idx >= post.length) return null;
  return { index: idx, pre: pre[idx], post: post[idx] };
}

function tokenBalanceAt(tx, address, mint) {
  const addrs = canonicalAddresses(tx);
  const idx = addrs.indexOf(address);
  if (idx < 0) return null;
  const pick = (list) => {
    for (const t of list || []) {
      if (t.accountIndex !== idx) continue;
      if (mint && t.mint !== mint) continue;
      return { mint: t.mint, owner: t.owner, amount: t.uiTokenAmount && t.uiTokenAmount.amount };
    }
    return null;
  };
  return {
    pre: pick(tx.meta && tx.meta.preTokenBalances),
    post: pick(tx.meta && tx.meta.postTokenBalances),
  };
}

const report = {
  generated_at: new Date().toISOString(),
  rpc: RPC,
  program: PROGRAM,
  tx_count: TX_COUNT,
  method:
    'pre-event state reconstructed from the chain transaction history: the most ' +
    'recent PRIOR transaction touching the account gives its post-state, ' +
    "verified against the event transaction's own meta.preBalances witness",
  transactions: [],
  totals: {
    tx_considered: 0,
    accounts_considered: 0,
    prior_found: 0,
    prior_missing: 0,
    lamport_agree: 0,
    lamport_disagree: 0,
    token_agree: 0,
    token_disagree: 0,
    token_cases: 0,
    rpc_calls: 0,
  },
  limits: [
    'A prior touching transaction is found via getSignaturesForAddress; a tx ' +
      'that was rolled back (meta.err) contributes its PRE balances as the ' +
      'state after it.',
    'The reconstruction only holds if no OTHER transaction touched the ' +
      'account between the prior tx and the event tx; the witness ' +
      '(meta.preBalances) detects exactly that case and reports disagreement.',
    'Only lamport and token balances are compared. Account BYTES as of the ' +
      'event are not reconstructed here — bytes of a mutable account cannot ' +
      'be derived from balances alone.',
    'Prior-history search is bounded by PRIOR_LIMIT signatures per account, ' +
      'so an account touched long ago may be reported as prior_missing.',
  ],
};

async function main() {
  const sigs = await rpc('getSignaturesForAddress', [PROGRAM, { limit: 40 }]);
  const candidates = sigs.filter((s) => !s.err).slice(0, TX_COUNT);
  if (!candidates.length) {
    console.error('SETUP FAIL: no successful signatures for program');
    process.exit(2);
  }

  for (const s of candidates) {
    let tx;
    try {
      tx = await rpc('getTransaction', [s.signature, {
        encoding: 'json',
        maxSupportedTransactionVersion: 1,
        commitment: 'finalized',
      }]);
    } catch (e) {
      console.error(`SETUP FAIL: getTransaction ${s.signature}: ${e}`);
      process.exit(2);
    }
    if (!tx || !tx.meta) continue;
    report.totals.tx_considered++;

    const addrs = canonicalAddresses(tx);
    const pre = tx.meta.preBalances || [];
    const post = tx.meta.postBalances || [];
    // Accounts whose lamports MOVED in this transaction: these are the ones
    // whose pre-event state a consumer actually needs.
    const moved = [];
    for (let i = 0; i < addrs.length; i++) {
      if (i >= pre.length || i >= post.length) continue;
      if (pre[i] !== post[i]) moved.push({ index: i, address: addrs[i] });
    }
    // Token movements too.
    const tokMoved = [];
    for (const t of tx.meta.postTokenBalances || []) {
      const preT = (tx.meta.preTokenBalances || []).find((x) => x.accountIndex === t.accountIndex && x.mint === t.mint);
      const before = preT ? preT.uiTokenAmount.amount : '0';
      if (before !== t.uiTokenAmount.amount) {
        tokMoved.push({ index: t.accountIndex, address: addrs[t.accountIndex], mint: t.mint, before });
      }
    }

    const txRec = {
      signature: s.signature,
      slot: tx.slot,
      accounts_total: addrs.length,
      lamport_moved: moved.length,
      token_moved: tokMoved.length,
      accounts: [],
    };

    for (const m of moved.slice(0, 6)) {
      report.totals.accounts_considered++;
      const rec = {
        address: m.address,
        witness_pre_lamports: pre[m.index],
        candidate: null,
        verdict: 'unknown',
      };
      let priorSigs;
      try {
        priorSigs = await rpc('getSignaturesForAddress', [m.address, { limit: PRIOR_LIMIT }]);
      } catch (e) {
        rec.verdict = 'history_unavailable';
        txRec.accounts.push(rec);
        continue;
      }
      const older = priorSigs.filter((p) => !p.err && p.slot < tx.slot);
      if (!older.length) {
        report.totals.prior_missing++;
        rec.verdict = 'no_prior_touch';
        txRec.accounts.push(rec);
        continue;
      }
      report.totals.prior_found++;
      // Most recent prior transaction that touched the account.
      const prior = older[0];
      let ptx;
      try {
        ptx = await rpc('getTransaction', [prior.signature, {
          encoding: 'json',
          maxSupportedTransactionVersion: 1,
          commitment: 'finalized',
        }]);
      } catch (e) {
        rec.verdict = 'prior_fetch_failed';
        txRec.accounts.push(rec);
        continue;
      }
      if (!ptx || !ptx.meta) {
        rec.verdict = 'prior_fetch_failed';
        txRec.accounts.push(rec);
        continue;
      }
      const b = balancesAt(ptx, m.address);
      if (!b) {
        rec.verdict = 'prior_index_unresolved';
        txRec.accounts.push(rec);
        continue;
      }
      // A rolled-back prior transaction left the account in its PRE state.
      const candidate = ptx.meta.err ? b.pre : b.post;
      rec.candidate = candidate;
      rec.prior_signature = prior.signature;
      rec.prior_slot = prior.slot;
      rec.prior_err = ptx.meta.err ? true : false;
      if (candidate === rec.witness_pre_lamports) {
        rec.verdict = 'agree';
        report.totals.lamport_agree++;
      } else {
        rec.verdict = 'disagree';
        report.totals.lamport_disagree++;
      }
      // The decided cases carry the evidence — a report whose only rows are
      // the undecidable ones proves nothing about the decision.
      txRec.accounts.push(rec);
      await sleep(200);
    }

    // Token case: same reconstruction, compared against the token witness.
    for (const t of tokMoved.slice(0, 4)) {
      report.totals.token_cases++;
      const rec = { address: t.address, mint: t.mint, witness_pre_amount: t.before, candidate: null, verdict: 'unknown' };
      try {
        const priorSigs = await rpc('getSignaturesForAddress', [t.address, { limit: PRIOR_LIMIT }]);
        const older = priorSigs.filter((p) => !p.err && p.slot < tx.slot);
        if (!older.length) { rec.verdict = 'no_prior_touch'; txRec.accounts.push(rec); continue; }
        const prior = older[0];
        const ptx = await rpc('getTransaction', [prior.signature, {
          encoding: 'json', maxSupportedTransactionVersion: 1, commitment: 'finalized',
        }]);
        if (!ptx || !ptx.meta) { rec.verdict = 'prior_fetch_failed'; txRec.accounts.push(rec); continue; }
        const tb = tokenBalanceAt(ptx, t.address, t.mint);
        if (!tb) { rec.verdict = 'prior_index_unresolved'; txRec.accounts.push(rec); continue; }
        const candidate = ptx.meta.err ? (tb.pre && tb.pre.amount) : (tb.post && tb.post.amount);
        rec.candidate = candidate === undefined ? null : candidate;
        rec.prior_signature = prior.signature;
        if (rec.candidate === t.before) { rec.verdict = 'agree'; report.totals.token_agree++; }
        else { rec.verdict = 'disagree'; report.totals.token_disagree++; }
      } catch (e) {
        rec.verdict = 'history_unavailable';
      }
      txRec.accounts.push(rec);
      await sleep(200);
    }

    report.transactions.push(txRec);
    console.error(`tx ${s.signature.slice(0, 12)} slot ${tx.slot}: ${txRec.accounts.length} account cases`);
  }

  report.totals.rpc_calls = calls;
  const t = report.totals;
  const decided = t.lamport_agree + t.lamport_disagree;
  report.verdict = {
    prior_found_rate: t.accounts_considered ? Number((t.prior_found / t.accounts_considered).toFixed(4)) : null,
    lamport_agreement_rate: decided ? Number((t.lamport_agree / decided).toFixed(4)) : null,
    token_agreement_rate: (t.token_agree + t.token_disagree) ? Number((t.token_agree / (t.token_agree + t.token_disagree)).toFixed(4)) : null,
    rpc_calls_per_decided_account: decided ? Number((t.rpc_calls / decided).toFixed(2)) : null,
    premise_archival_required:
      decided > 0 && t.lamport_agree / decided >= 0.9
        ? 'REFUTED — pre-event balance state was reconstructed from chain transaction history and verified against the transaction\'s own witness, with no archival RPC involved'
        : 'NOT REFUTED — reconstruction did not reach the pre-registered 90% agreement threshold',
    note:
      'A refutation of the archival premise is about BALANCES. Account BYTES as ' +
      'of the event remain a separate question: a mutable account\'s bytes are ' +
      'not derivable from its balances, and this probe says nothing about them.',
  };

  fs.mkdirSync(path.dirname(OUT), { recursive: true });
  fs.writeFileSync(OUT, JSON.stringify(report, null, 2) + '\n');
  console.log(JSON.stringify({ totals: t, verdict: report.verdict }, null, 2));
  console.log(`report written: ${OUT}`);
}

main().catch((e) => {
  console.error(`PROBE FAIL: ${e && e.message ? e.message : e}`);
  process.exit(1);
});
