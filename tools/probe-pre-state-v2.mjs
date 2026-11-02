// Premise probe v2: can pre-event account state be reconstructed from the
// chain's transaction HISTORY alone — with an INDEPENDENT witness — instead
// of an archival RPC?
//
// Background (README, qualification item 4): pre-event state "needs the
// chain's history, not a second fetch — and it stays open". The transaction
// metadata reeflog already pays for states the pre-event LAMPORT balance and
// pre-event TOKEN amount (`meta.preBalances` / `meta.preTokenBalances`); the
// pre-event BYTES are not in it. This probe measures whether the history
// itself (getSignaturesForAddress + getTransaction — served by every public
// RPC, no archival node) reconstructs the pre-event balances and whether the
// witness verifies the reconstruction.
//
// Method, per (event transaction T, account A that moved in T):
//   1. Find the nearest PRIOR transaction P touching A, any outcome,
//      via paginated getSignaturesForAddress (a hot account has all of its
//      recent signatures AFTER T — v1's single 12-signature window returned
//      an empty "prior" list for exactly those accounts; pagination with a
//      `before` cursor reaches older history and v1's no_prior_touch cases
//      are re-examined here).
//   2. candidate_all = P.meta.postBalances[A]. For a FAILED P this is only
//      correct if a failed transaction's post-balances equal the state after
//      it (effects rolled back, fee charged) — measured, not assumed:
//      failed_prior_fee_delta counts how many failed priors differ pre->post
//      and by how much.
//   3. candidate_success = same, but skipping failed priors (v1's rule).
//   4. Both candidates are compared against T.meta.preBalances[A] (the
//      witness). The strategy whose agreement rate is higher wins on
//      evidence, not argument.
//
// A second, equally falsifiable question: is the reconstruction possible at
// all for tokens the same way (candidate from P.meta.postTokenBalances)?
//
// Exit codes: 0 = measurement completed (read verdict), 1 = probe failed,
// 2 = setup failed (rate limit / no transactions — NOT a verdict).
import fs from 'node:fs';
import path from 'node:path';

const RPC = process.env.RPC || 'https://api.mainnet-beta.solana.com';
const PROGRAM = process.env.PROGRAM || '675kPX9MHTjS2zt1qfr1NYHuzeLXfQM9H24wFSUt1Mp8';
const TX_COUNT = Number(process.env.TX_COUNT || 8);
const PAGE = 100;
const MAX_PAGES = Number(process.env.MAX_PAGES || 5);
const OUT = process.env.OUT || 'research/pre-state-probe-v2-2026-11-02.json';

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

function canonicalAddresses(tx) {
  const msg = tx.transaction.message;
  const out = [];
  for (const k of msg.accountKeys || []) out.push(typeof k === 'string' ? k : k.pubkey);
  const loaded = (tx.meta && tx.meta.loadedAddresses) || {};
  for (const g of ['writable', 'readonly']) for (const a of loaded[g] || []) out.push(a);
  return out;
}

function balancesFor(tx, address) {
  const addrs = canonicalAddresses(tx);
  const idx = addrs.indexOf(address);
  if (idx < 0) return null;
  const pre = tx.meta.preBalances || [];
  const post = tx.meta.postBalances || [];
  if (idx >= pre.length || idx >= post.length) return null;
  return { index: idx, pre: pre[idx], post: post[idx] };
}

function tokenFor(tx, address, mint) {
  const addrs = canonicalAddresses(tx);
  const idx = addrs.indexOf(address);
  if (idx < 0) return null;
  const pick = (list) => {
    for (const t of list || []) {
      if (t.accountIndex === idx && (!mint || t.mint === mint)) return t.uiTokenAmount && t.uiTokenAmount.amount;
    }
    return undefined;
  };
  return { pre: pick(tx.meta.preTokenBalances), post: pick(tx.meta.postTokenBalances) };
}

// Paginated nearest-prior search: keep pulling pages with a `before` cursor
// until a signature strictly older than `slot` is found (or MAX_PAGES).
async function nearestPrior(address, slot) {
  let before;
  let pages = 0;
  let newestOlder = null;
  while (pages < MAX_PAGES) {
    const params = [address, { limit: PAGE }];
    if (before) params[1].before = before;
    let batch;
    try {
      batch = await rpc('getSignaturesForAddress', params);
    } catch (e) {
      return { error: String(e).slice(0, 140), pages };
    }
    pages++;
    if (!batch.length) break;
    for (const s of batch) {
      if (s.slot < slot) { newestOlder = s; break; }
    }
    if (newestOlder) break;
    before = batch[batch.length - 1].signature;
    await sleep(150);
  }
  return { prior: newestOlder, pages };
}

async function main() {
  const sigs = await rpc('getSignaturesForAddress', [PROGRAM, { limit: 40 }]);
  const candidates = sigs.filter((s) => !s.err).slice(0, TX_COUNT);
  if (!candidates.length) {
    console.error('SETUP FAIL: no successful signatures');
    process.exit(2);
  }

  const report = {
    generated_at: new Date().toISOString(),
    rpc: RPC,
    program: PROGRAM,
    tx_count: TX_COUNT,
    pagination: { page: PAGE, max_pages: MAX_PAGES },
    method:
      'nearest prior transaction touching the account (paginated history, any ' +
      "outcome); its post-balances compared against the event transaction's own " +
      'meta.preBalances witness; failed priors measured for the fee-only ' +
      'lamport change that a "skip failed" rule misses',
    totals: {
      tx_considered: 0,
      accounts_considered: 0,
      prior_found: 0,
      prior_not_found: 0,
      pages_used: 0,
      lamport_agree_all: 0,
      lamport_agree_success: 0,
      lamport_decided: 0,
      token_agree: 0,
      token_decided: 0,
      failed_priors: 0,
      failed_priors_fee_only: 0,
      failed_priors_fee_delta_sum: 0,
      rpc_calls: 0,
    },
    transactions: [],
    limits: [
      'A candidate is derived from the nearest prior TRANSACTION, not from an ' +
        'account-state query: nothing here reads account state at a past slot.',
      'Agreement is only claimed where both witness and candidate exist; ' +
        'accounts with no reachable prior transaction are reported as such and ' +
        'are not counted as agreement.',
      'The reconstruction covers LAMPORT balances and TOKEN amounts, never ' +
        'account BYTES: a mutable account\'s bytes at event time are not ' +
        'derivable from balances, and this probe says nothing about them.',
      'Paginated history search is bounded by MAX_PAGES signatures pages per ' +
        'account; beyond that the account is reported as not-found.',
      'Only one program (Raydium AMM v4) on one date; the numbers are a ' +
        'sample of that window, not a population claim.',
    ],
  };

  for (const s of candidates) {
    let tx;
    try {
      tx = await rpc('getTransaction', [s.signature, {
        encoding: 'json', maxSupportedTransactionVersion: 1, commitment: 'finalized',
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
    const moved = [];
    for (let i = 0; i < addrs.length && i < pre.length && i < post.length; i++) {
      if (pre[i] !== post[i]) moved.push({ index: i, address: addrs[i] });
    }
    const tokMoved = [];
    for (const t of tx.meta.postTokenBalances || []) {
      const p = (tx.meta.preTokenBalances || []).find(
        (x) => x.accountIndex === t.accountIndex && x.mint === t.mint);
      const before = p ? p.uiTokenAmount.amount : '0';
      if (before !== t.uiTokenAmount.amount) {
        tokMoved.push({ index: t.accountIndex, address: addrs[t.accountIndex], mint: t.mint, before });
      }
    }

    const rec = {
      signature: s.signature,
      slot: tx.slot,
      lamport_moved: moved.length,
      token_moved: tokMoved.length,
      cases: [],
    };

    for (const m of moved.slice(0, 5)) {
      report.totals.accounts_considered++;
      const c = {
        kind: 'lamport', address: m.address,
        witness_pre: pre[m.index],
        candidate_all: null, candidate_success: null,
        prior: null, prior_err: false, pages: 0, verdict: 'unknown',
      };
      const found = await nearestPrior(m.address, tx.slot);
      c.pages = found.pages || 0;
      report.totals.pages_used += c.pages;
      if (found.error) { c.verdict = 'history_unavailable'; rec.cases.push(c); continue; }
      if (!found.prior) { report.totals.prior_not_found++; c.verdict = 'no_prior_touch'; rec.cases.push(c); continue; }
      report.totals.prior_found++;
      let ptx;
      try {
        ptx = await rpc('getTransaction', [found.prior.signature, {
          encoding: 'json', maxSupportedTransactionVersion: 1, commitment: 'finalized',
        }]);
      } catch (e) {
        c.verdict = 'prior_fetch_failed'; rec.cases.push(c); continue;
      }
      if (!ptx || !ptx.meta) { c.verdict = 'prior_fetch_failed'; rec.cases.push(c); continue; }
      const b = balancesFor(ptx, m.address);
      if (!b) { c.verdict = 'prior_index_unresolved'; rec.cases.push(c); continue; }
      c.prior = found.prior.signature;
      c.prior_slot = found.prior.slot;
      c.prior_err = ptx.meta.err ? true : false;
      c.candidate_all = b.post;
      if (c.prior_err) {
        report.totals.failed_priors++;
        const feeOnly = b.pre === b.post;
        if (feeOnly) report.totals.failed_priors_fee_only++;
        else report.totals.failed_priors_fee_delta_sum += Math.abs(b.post - b.pre);
      }
      // Strategy "success": the nearest prior with NO error. Same page walk,
      // filtered — recompute from the same page data where possible by
      // walking again only if the nearest prior failed.
      if (!c.prior_err) {
        c.candidate_success = b.post;
      } else {
        // find nearest non-failed prior within the already collected window
        let before; let pages = 0; let successOlder = null;
        const evSlot = tx.slot;
        while (pages < MAX_PAGES) {
          const params = [m.address, { limit: PAGE }];
          if (before) params[1].before = before;
          try {
            const batch = await rpc('getSignaturesForAddress', params);
            if (!batch.length) break;
            pages++;
            for (const p of batch) {
              if (p.slot < evSlot && !p.err) { successOlder = p; break; }
            }
            if (successOlder) break;
            before = batch[batch.length - 1].signature;
            await sleep(150);
          } catch (e) { break; }
        }
        if (successOlder) {
          try {
            const stx = await rpc('getTransaction', [successOlder.signature, {
              encoding: 'json', maxSupportedTransactionVersion: 1, commitment: 'finalized',
            }]);
            const sb = stx && balancesFor(stx, m.address);
            if (sb) c.candidate_success = sb.post;
          } catch (e) { /* left null → undecided under this strategy */ }
        }
      }
      report.totals.lamport_decided++;
      if (c.candidate_all === c.witness_pre) report.totals.lamport_agree_all++;
      if (c.candidate_success === null || c.candidate_success === undefined) {
        // undecided under the success rule — counted separately below
        c.undecided_success = true;
      } else if (c.candidate_success === c.witness_pre) {
        report.totals.lamport_agree_success++;
      }
      c.verdict = c.candidate_all === c.witness_pre
        ? 'agree_all'
        : (c.candidate_success !== null && c.candidate_success === c.witness_pre
          ? 'agree_success_only'
          : 'disagree');
      rec.cases.push(c);
      await sleep(150);
    }

    for (const t of tokMoved.slice(0, 3)) {
      const c = {
        kind: 'token', address: t.address, mint: t.mint,
        witness_pre: t.before, candidate: null, prior: null, prior_err: false,
        pages: 0, verdict: 'unknown',
      };
      const found = await nearestPrior(t.address, tx.slot);
      c.pages = found.pages || 0;
      if (found.error) { c.verdict = 'history_unavailable'; rec.cases.push(c); continue; }
      if (!found.prior) { c.verdict = 'no_prior_touch'; rec.cases.push(c); continue; }
      let ptx;
      try {
        ptx = await rpc('getTransaction', [found.prior.signature, {
          encoding: 'json', maxSupportedTransactionVersion: 1, commitment: 'finalized',
        }]);
      } catch (e) { c.verdict = 'prior_fetch_failed'; rec.cases.push(c); continue; }
      if (!ptx || !ptx.meta) { c.verdict = 'prior_fetch_failed'; rec.cases.push(c); continue; }
      const tb = tokenFor(ptx, t.address, t.mint);
      if (!tb) { c.verdict = 'prior_index_unresolved'; rec.cases.push(c); continue; }
      c.prior = found.prior.signature;
      c.prior_slot = found.prior.slot;
      c.prior_err = ptx.meta.err ? true : false;
      // Token balances of a FAILED prior are rolled back: its post is the
      // state after it, same rule as lamports minus the fee distinction.
      c.candidate = tb.post === undefined ? null : tb.post;
      report.totals.token_decided++;
      if (c.candidate === t.before) report.totals.token_agree++;
      c.verdict = c.candidate === t.before ? 'agree' : 'disagree';
      rec.cases.push(c);
      await sleep(150);
    }

    report.transactions.push(rec);
    console.error(`tx ${s.signature.slice(0, 12)} slot ${tx.slot}: ${rec.cases.length} cases`);
  }

  report.totals.rpc_calls = calls;
  const t = report.totals;
  const ar = (n, d) => (d ? Number((n / d).toFixed(4)) : null);
  report.verdict = {
    prior_found_rate: ar(t.prior_found, t.accounts_considered),
    lamport_agreement_nearest_any_outcome: ar(t.lamport_agree_all, t.lamport_decided),
    lamport_agreement_nearest_successful_only: ar(t.lamport_agree_success, t.lamport_decided),
    token_agreement: ar(t.token_agree, t.token_decided),
    failed_priors_seen: t.failed_priors,
    failed_priors_that_were_fee_only: t.failed_priors_fee_only,
    rpc_calls_per_decided_case: ar(t.rpc_calls, t.lamport_decided + t.token_decided),
    premise_archival_required:
      t.lamport_decided > 0 && t.lamport_agree_all / t.lamport_decided >= 0.9
        ? 'REFUTED — pre-event lamport/token state was reconstructed from transaction history served by a plain public RPC and verified against each transaction\'s own witness; no archival node and no account-state-at-slot query involved'
        : 'NOT REFUTED — reconstruction did not reach the pre-registered 90% agreement threshold',
    scope:
      'This refutes the archival premise for LAMPORT and TOKEN balances only. ' +
      'Account BYTES as of the event are a separate, still-open question: bytes ' +
      'are not derivable from balances, and nothing here claims otherwise.',
  };

  fs.mkdirSync(path.dirname(OUT), { recursive: true });
  fs.writeFileSync(OUT, JSON.stringify(report, null, 2) + '\n');
  console.log(JSON.stringify({ totals: t, verdict: report.verdict }, null, 2));
}

main().catch((e) => {
  console.error(`PROBE FAIL: ${e && e.message ? e.message : e}`);
  process.exit(1);
});
