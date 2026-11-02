// Probe: is the OWNER / AUTHORITY of an account at EVENT TIME available for
// free, without an archival RPC?
//
// reeflog's README states the limit plainly (qualification item 4):
//   "Owner = Abrufzeitpunkt, nicht Ereigniszeit" — the owner map says who owns
//   an account NOW, not at the moment of the event.
//   "account bytes AS OF the event ... needs the chain's history ... stays open"
//
// But the transaction's own metadata states, for every TOKEN account the
// transaction touched, two event-time facts the fetch-time pass cannot know:
//   meta.preTokenBalances[].owner      — the authority that owned the token
//                                        account BEFORE the transaction
//   meta.preTokenBalances[].programId  — the SPL token program that owned it
//                                        BEFORE the transaction
// Both are the chain's statement, at event time, already inside the response
// reeflog pays for. reeflog currently stores them ONLY for accounts whose
// balance CHANGED (token_changes) and DROPS them for untouched token
// accounts.
//
// This probe measures the size of that gap, on live mainnet:
//   A. COVERAGE   — how many token accounts appear in preTokenBalances but
//                   carry no entry in reeflog's token_changes.
//   B. SKEW       — how often the event-time authority differs from the
//                   authority a fetch-time read would report NOW (the number
//                   of cases where "fetch time != event time" is a real,
//                   observable error and not a theoretical caveat).
//   C. OWNER-PROGRAM SKEW — same for the owning program.
//   D. CLOSED ACCOUNTS — token accounts present pre-transaction and absent
//                   post-transaction (closed): their event-time authority is
//                   stated, but a fetch-time read finds NOTHING (the account
//                   is gone), so the fetch-time pass answers "missing" for
//                   exactly the accounts whose event-time owner is known.
//
// Exit codes: 0 = measurement completed (read verdict), 1 = probe failed,
// 2 = setup failed (rate limit / no transactions — NOT a verdict).
import fs from 'node:fs';
import path from 'node:path';

const RPC = process.env.RPC || 'https://api.mainnet-beta.solana.com';
const PROGRAM = process.env.PROGRAM || '675kPX9MHTjS2zt1qfr1NYHuzeLXfQM9H24wFSUt1Mp8';
const TX_COUNT = Number(process.env.TX_COUNT || 12);
const OUT = process.env.OUT || 'research/tx-time-authority-probe-2026-11-02.json';

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
  const out = [];
  for (const k of tx.transaction.message.accountKeys || []) {
    out.push(typeof k === 'string' ? k : k.pubkey);
  }
  const la = (tx.meta && tx.meta.loadedAddresses) || {};
  for (const g of ['writable', 'readonly']) for (const a of la[g] || []) out.push(a);
  return out;
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
    method:
      "the transaction's own preTokenBalances state the authority and the " +
      'owning token program AT EVENT TIME for every token account the ' +
      'transaction touched; a fetch-time read is compared against them',
    totals: {
      tx_considered: 0,
      pre_token_entries: 0,
      token_accounts_touched: 0,
      entries_with_authority: 0,
      entries_with_program: 0,
      entries_reported_by_reeflog: 0,
      entries_dropped_by_reeflog: 0,
      dropped_with_authority: 0,
      dropped_with_program: 0,
      closed_accounts: 0,
      closed_with_authority: 0,
      authority_fetch_now_present: 0,
      authority_fetch_now_absent: 0,
      authority_known_event_time_but_absent_now: 0,
      authority_not_parsable_now: 0,
      authority_skew_measured: 0,
      authority_skew_differs: 0,
      program_skew_measured: 0,
      program_skew_differs: 0,
      rpc_calls: 0,
    },
    transactions: [],
    limits: [
      'Only TOKEN accounts are covered: preTokenBalances names the authority ' +
        'and the owning token program. A general account\'s owner program is ' +
        'not in the transaction metadata and stays a fetch-time fact.',
      'The fetch-time comparison is a LIVE read taken after the event, so a ' +
        'difference is a real skew (the account changed afterwards) — not a ' +
        'rounding or ordering artefact.',
      'One program (Raydium AMM v4) on one date; a sample of that window.',
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
    const preTok = tx.meta.preTokenBalances || [];
    const postTok = tx.meta.postTokenBalances || [];
    const postIdxMint = new Set(postTok.map((t) => `${t.accountIndex}:${t.mint}`));

    // What reeflog's envelope currently reports: token accounts whose amount
    // changed (src/accounts.rs: entries are skipped when before == after).
    const reported = new Set();
    for (const t of preTok) {
      const p = postTok.find((x) => x.accountIndex === t.accountIndex && x.mint === t.mint);
      const before = t.uiTokenAmount.amount;
      const after = p ? p.uiTokenAmount.amount : '0';
      if (before !== after) reported.add(`${t.accountIndex}:${t.mint}`);
    }

    const rec = {
      signature: s.signature,
      slot: tx.slot,
      pre_token_entries: preTok.length,
      reported_by_reeflog: reported.size,
      dropped: 0,
      closed: 0,
      entries: [],
    };

    for (const t of preTok) {
      report.totals.pre_token_entries++;
      const address = addrs[t.accountIndex];
      const key = `${t.accountIndex}:${t.mint}`;
      const authority = t.owner;
      const program = t.programId;
      const closed = !postIdxMint.has(key);
      if (authority) report.totals.entries_with_authority++;
      if (program) report.totals.entries_with_program++;
      const isReported = reported.has(key);
      if (isReported) report.totals.entries_reported_by_reeflog++;
      else {
        report.totals.entries_dropped_by_reeflog++;
        rec.dropped++;
        if (authority) report.totals.dropped_with_authority++;
        if (program) report.totals.dropped_with_program++;
      }
      if (closed) {
        report.totals.closed_accounts++;
        rec.closed++;
        if (authority) report.totals.closed_with_authority++;
      }
      // Live fetch-time comparison.
      const e = {
        address, mint: t.mint,
        event_time_authority: authority || null,
        event_time_program: program || null,
        reported_by_reeflog: isReported,
        closed,
        fetch_now: null,
        authority_same_now: null,
        program_same_now: null,
      };
      if (address) {
        try {
          const now = await rpc('getAccountInfo', [address, {
            encoding: 'jsonParsed', commitment: 'finalized',
          }]);
          const v = now.value;
          if (v === null) {
            e.fetch_now = 'absent';
            if (authority) {
              report.totals.authority_fetch_now_absent++;
              report.totals.authority_known_event_time_but_absent_now++;
            }
          } else {
            e.fetch_now = 'present';
            // The top-level `owner` of an account is the OWNING PROGRAM. The
            // token AUTHORITY lives in the parsed SPL layout
            // (data.parsed.info.owner). Comparing the event-time authority
            // against the program here was this probe's first decoder bug:
            // it reported 75/75 skew on the very first run — impossible, and
            // the program check (0/75) exposed which side was wrong.
            const parsed = v.data && v.data.parsed && v.data.parsed.info;
            const nowAuthority = parsed && typeof parsed.owner === 'string' ? parsed.owner : null;
            const nowProgram = v.owner;
            if (authority) {
              report.totals.authority_fetch_now_present++;
              if (nowAuthority === null) {
                e.authority_same_now = null; // not parsed — no claim
                report.totals.authority_not_parsable_now++;
              } else {
                report.totals.authority_skew_measured++;
                if (nowAuthority === authority) e.authority_same_now = true;
                else { e.authority_same_now = false; report.totals.authority_skew_differs++; }
              }
            }
            if (program) {
              report.totals.program_skew_measured++;
              if (nowProgram === program) e.program_same_now = true;
              else { e.program_same_now = false; report.totals.program_skew_differs++; }
            }
          }
        } catch (err) {
          e.fetch_now = 'unavailable';
        }
        await sleep(120);
      }
      rec.entries.push(e);
    }
    report.transactions.push(rec);
    console.error(`tx ${s.signature.slice(0, 12)} slot ${tx.slot}: pre=${rec.pre_token_entries} reported=${rec.reported_by_reeflog} dropped=${rec.dropped} closed=${rec.closed}`);
  }

  report.totals.rpc_calls = calls;
  const t = report.totals;
  const rate = (n, d) => (d ? Number((n / d).toFixed(4)) : null);
  report.verdict = {
    token_accounts_touched: t.token_accounts_touched || t.pre_token_entries,
    reeflog_reports_share: rate(t.entries_reported_by_reeflog, t.pre_token_entries),
    reeflog_drops_share: rate(t.entries_dropped_by_reeflog, t.pre_token_entries),
    dropped_entries_that_carry_event_time_authority: t.dropped_with_authority,
    dropped_entries_that_carry_event_time_owner_program: t.dropped_with_program,
    closed_accounts: t.closed_accounts,
    closed_accounts_with_event_time_authority: t.closed_with_authority,
    event_time_authority_but_no_fetch_time_answer: t.authority_known_event_time_but_absent_now,
    authority_skew_rate_where_measurable: rate(t.authority_skew_differs, t.authority_skew_measured),
    program_skew_rate_where_measurable: rate(t.program_skew_differs, t.program_skew_measured),
    event_time_authority_free:
      t.dropped_with_authority > 0
        ? 'YES — the transaction metadata states the authority and the owning token program at event time for token accounts reeflog currently drops them for; the fetch-time pass cannot recover them (closed accounts are simply absent now)'
        : 'NO EVIDENCE — no dropped entry carried an event-time authority in this window',
  };
  fs.mkdirSync(path.dirname(OUT), { recursive: true });
  fs.writeFileSync(OUT, JSON.stringify(report, null, 2) + '\n');
  console.log(JSON.stringify({ totals: t, verdict: report.verdict }, null, 2));
}

main().catch((e) => {
  console.error(`PROBE FAIL: ${e && e.message ? e.message : e}`);
  process.exit(1);
});
