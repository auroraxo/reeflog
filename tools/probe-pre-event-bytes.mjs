// Premise probe: PRE-EVENT BYTES — what can the chain's own free history
// certify about an account's BYTES at event time, without any archival RPC?
//
// The card's open item: "Pre-Event-BYTES (nicht Balances) — unveraendert
// offen, derzeit als Rekursion ueber Vorgaenger-Transaktionen denkbar, aber
// als Feature NICHT gebaut und ohne Beleg."
//
// Two candidate routes, both measured here, neither assumed:
//
//   (R) RECURSION over predecessor transactions: fetch the nearest prior
//       transaction P touching account A and look for A's account bytes
//       inside P's response. We enumerate P's data-bearing fields and count
//       where A's STATE payload would live. (Instruction `data` is the
//       instruction's bytes, not the account's — counted separately.)
//
//   (H) HISTORICAL READ: getAccountInfo with minContextSlot = the event's
//       slot. If the response is anchored at the CURRENT context slot, the
//       caller silently received today's bytes for a past-slot question —
//       a footgun, not a method. Measured via response.context.slot vs the
//       requested slot (and against accounts we KNOW moved since).
//
//   (C) CERTIFICATION via signature history — the constructive half:
//       if the event's signature is the account A's LAST signature
//       (getSignaturesForAddress from the head returns nothing newer than
//       the event), then nothing touched A after the event, so the bytes
//       fetchable NOW are the post-event bytes, and the chain itself
//       certifies the interval. The event's own meta.postBalances /
//       postTokenBalances are an INDEPENDENT WITNESS for that claim:
//       certified  =>  fetch-time lamports == event.postBalances
//                      fetch-time token bytes (mint/owner/amount) decode to
//                      event.postTokenBalances. A single mismatch refutes
//       the boundary logic.
//
// Controls (house style):
//   wrong-side guard: for accounts the event MOVED (pre != post), the same
//   witness compared against the PRE side MUST fail — if it agrees there
//   too, the witness is vacuous. Expected RED by construction, reported.
//   off-list guard: classification refuses any address not in the event's
//   own account list (an unrelated address has "no signatures after E" and
//   would otherwise certify as if it were the event's state).
//
// Exit codes: 0 = measurement completed (read the report for the verdict),
// 1 = a check failed, 2 = setup failed (rate limit / empty program — NOT a
// verdict), 3 = skipped (missing prerequisites).
import fs from 'node:fs';
import path from 'node:path';

const RPC = process.env.RPC || 'https://api.mainnet-beta.solana.com';
const PROGRAM = process.env.PROGRAM || '675kPX9MHTjS2zt1qfr1NYHuzeLXfQM9H24wFSUt1Mp8'; // Raydium AMM v4
const TX_COUNT = Number(process.env.TX_COUNT || 6);
const ACCOUNT_CAP = Number(process.env.ACCOUNT_CAP || 40);
const OUT = process.env.OUT || 'research/pre-event-bytes-probe-2026-11-02.json';

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

const TOKEN_PROGRAMS = new Set([
  'TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA',
  'TokenzQdBNbLqP5VEhdkAS6EPFLC1PHnBqCXEpPxuEb',
]);

// Canonical account list with permissions: jsonParsed names signer/writable
// per key and marks loaded keys with source — the flags are the chain's own,
// previously cross-checked against reeflog's header-derived list 346/346.
function accountRows(tx) {
  const out = [];
  for (const k of tx.transaction.message.accountKeys) {
    out.push({ address: k.pubkey, signer: !!k.signer, writable: !!k.writable, source: k.source });
  }
  return out;
}

function balanceAt(tx, address) {
  const rows = accountRows(tx);
  const idx = rows.findIndex((r) => r.address === address);
  if (idx < 0) return null;
  return { index: idx, pre: tx.meta.preBalances[idx], post: tx.meta.postBalances[idx] };
}

function tokenAt(tx, address, side) {
  const rows = accountRows(tx);
  const idx = rows.findIndex((r) => r.address === address);
  if (idx < 0) return null;
  const list = side === 'pre' ? tx.meta.preTokenBalances : tx.meta.postTokenBalances;
  for (const t of list || []) {
    if (t.accountIndex !== idx) continue;
    return {
      mint: t.mint,
      owner: t.owner,
      amount: BigInt(t.uiTokenAmount.amount),
      programId: t.programId,
    };
  }
  return null;
}

// SPL Token layout decoded by the CALLER below: mint(32) owner(32)
// amount(8) at fixed offsets; Token-2022 appends extensions after 165.
const B58 = '123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz';
function b58encode(bytes) {
  let n = 0n;
  for (const b of bytes) n = (n << 8n) | BigInt(b);
  let out = '';
  while (n > 0n) { out = B58[Number(n % 58n)] + out; n /= 58n; }
  for (const b of bytes) { if (b === 0) out = '1' + out; else break; }
  return out;
}

function b58decode(s) {
  let n = 0n;
  for (const c of s) n = n * 58n + BigInt(B58.indexOf(c));
  const b = [];
  while (n > 0n) { b.unshift(Number(n & 255n)); n >>= 8n; }
  for (const c of s) { if (c === '1') b.unshift(0); else break; }
  return Buffer.from(b);
}

function classify(historyRows, eventSig) {
  const newer = (historyRows || []).filter((s) => s.signature !== eventSig);
  return newer.length === 0 ? 'certified_no_touch_after' : 'touched_after';
}

const report = {
  generated_at: new Date().toISOString(),
  rpc: RPC,
  program: PROGRAM,
  tx_count: TX_COUNT,
  account_cap: ACCOUNT_CAP,
  method:
    'boundary certification in three phases: (1) for each account the event ' +
    'touched, ask the signature history (head-first, until=event) whether ' +
    'anything touched it AFTER the event; (2) fetch every account; (3) ' +
    'RE-classify the certified set after the fetch — a certification can ' +
    'expire in the classify->fetch gap on a live chain, and expired rows are ' +
    'counted, never witnessed. Only rows still certified at fetch time are ' +
    "witnessed against the event's own postBalances/postTokenBalances",
  events: [],
  totals: {
    accounts_considered: 0,
    classified_certified: 0,
    classified_touched_after: 0,
    classify_errors: 0,
    witness_post_lamport_agreement: 0,
    witness_post_lamport_disagreement: 0,
    expired_between_phases: 0, // certified at classify time, touched by the re-check after the fetch: certification is a property of a moment
    witness_skipped_fetch_null: 0, // certified, but the account is gone at fetch time: unwitnessed, never claimed
    witness_pre_must_fail_hit: 0, // wrong-side guard: agreement here = vacuous witness
    witness_pre_must_fail_agreed: 0,
    token_decode_cases: 0,
    token_decode_agreement: 0,
    token_decode_disagreement: 0,
    historical_read_requests: 0,
    historical_read_anchored_current: 0,
    historical_read_anchored_requested: 0,
    predecessor_scans: 0,
    predecessor_account_state_payloads: 0,
    predecessor_instruction_data_fields: 0,
  },
  offlist_guard: null, // HARNESS invariant, not a chain measurement
  limits: [],
  verdict: null,
};

// --- off-list guard: classification must refuse foreign addresses ----------
function classifyOrThrow(rows, eventSig, address, ownList) {
  if (!ownList.has(address)) throw new Error('offlist');
  return classify(rows, eventSig);
}
try {
  classifyOrThrow([], 'x', 'ForeignAddress1111111111111111111111111111', new Set(['Listed']));
  report.offlist_guard = { refused: false, note: 'HARNESS INVARIANT BROKEN: classification accepted a foreign address' };
} catch {
  report.offlist_guard = { refused: true, note: "HARNESS INVARIANT (not a chain measurement): the classifier refuses an address that is not in the event's own account list — without it an unrelated address would certify as if it were the event's state" };
}

const sigs = await rpc('getSignaturesForAddress', [PROGRAM, { limit: 200, commitment: 'finalized' }]);
const chosen = [];
for (const s of sigs) {
  if (chosen.length >= TX_COUNT) break;
  try {
    const tx = await rpc('getTransaction', [s.signature, { encoding: 'jsonParsed', maxSupportedTransactionVersion: 1, commitment: 'finalized' }]);
    if (!tx || !tx.meta || tx.meta.err) continue;
    if (!tx.meta.preTokenBalances || !tx.meta.postTokenBalances) continue;
    chosen.push(tx);
  } catch { /* skip */ }
}
if (chosen.length === 0) {
  console.error('SETUP: no suitable transactions');
  process.exit(2);
}
report.events = chosen.map((tx) => ({
  signature: tx.transaction.signatures[0],
  slot: tx.slot,
  writable_accounts: accountRows(tx).filter((r) => r.writable).length,
}));

// Gather accounts: writable ones the event touched, capped.
const picks = [];
const seen = new Set();
for (const tx of chosen) {
  const eventSig = tx.transaction.signatures[0];
  const own = accountRows(tx);
  for (const r of own) {
    if (!r.writable) continue;
    if (seen.has(r.address)) continue;
    seen.add(r.address);
    picks.push({ tx, eventSig, address: r.address, own: new Set(own.map((o) => o.address)) });
    if (picks.length >= ACCOUNT_CAP) break;
  }
  if (picks.length >= ACCOUNT_CAP) break;
}

const classified = [];
for (const p of picks) {
  report.totals.accounts_considered++;
  try {
    const rows = await rpc('getSignaturesForAddress', [
      p.address,
      { until: p.eventSig, limit: 100, commitment: 'finalized' },
    ]);
    const verdict = classifyOrThrow(rows, p.eventSig, p.address, p.own);
    classified.push({ ...p, verdict, newer: (rows || []).filter((s) => s.signature !== p.eventSig).length });
    if (verdict === 'certified_no_touch_after') report.totals.classified_certified++;
    else report.totals.classified_touched_after++;
  } catch (e) {
    report.totals.classify_errors++;
    console.error(`classify failed for ${p.address}: ${e.message}`);
  }
}

// Fetch-time read for every classified account (one batch).
const addresses = classified.map((c) => c.address);
const fetchNow = new Map();
for (let i = 0; i < addresses.length; i += 100) {
  const batch = addresses.slice(i, i + 100);
  const res = await rpc('getMultipleAccounts', [batch, { encoding: 'base64', commitment: 'finalized' }]);
  batch.forEach((a, j) => fetchNow.set(a, (res.value || [])[j]));
}

// Phase 3: re-classify every account certified in phase 1 AFTER the fetch.
// The classify->fetch gap is real time: an account touched in between shows
// post-touch state while its certification has already expired. Only an
// account that is STILL certified at fetch time witnesses anything; its
// disagreement would be a genuine refutation.
for (const c of classified) {
  if (c.verdict !== 'certified_no_touch_after') continue;
  try {
    const rows2 = await rpc('getSignaturesForAddress', [
      c.address,
      { until: c.eventSig, limit: 100, commitment: 'finalized' },
    ]);
    const v2 = classifyOrThrow(rows2, c.eventSig, c.address, c.own);
    if (v2 !== 'certified_no_touch_after') {
      c.expired = true;
      report.totals.expired_between_phases++;
    }
  } catch (e) {
    c.expired = true;
    report.totals.expired_between_phases++; // unverifiable = never witnessed
    console.error(`re-classify failed for ${c.address}: ${e.message}`);
  }
}

// Witness pass.
for (const c of classified) {
  if (c.expired) continue;
  const now = fetchNow.get(c.address);
  const bal = balanceAt(c.tx, c.address);
  if (c.verdict === 'certified_no_touch_after' && (!now || !bal)) {
    report.totals.witness_skipped_fetch_null++;
  }
  if (!now || !bal) continue;
  const lamportsNow = BigInt(now.lamports);

  if (c.verdict === 'certified_no_touch_after') {
    if (lamportsNow === BigInt(bal.post)) report.totals.witness_post_lamport_agreement++;
    else report.totals.witness_post_lamport_disagreement++;
    // token bytes vs event's chain-stated post facts
    const postTok = tokenAt(c.tx, c.address, 'post');
    if (postTok && TOKEN_PROGRAMS.has(now.owner) && now.data && now.data[0]) {
      const bytes = Buffer.from(now.data[0], 'base64');
      if (bytes.length >= 72) {
        report.totals.token_decode_cases++;
        const mintHex = Buffer.from(bytes.slice(0, 32)).toString('hex');
        const ownerHex = Buffer.from(bytes.slice(32, 64)).toString('hex');
        const amount = bytes.readBigUInt64LE(64);
        // the chain names mint/owner as base58 strings; decode each to its
        // 32 raw bytes and compare hex, amount as raw u64 LE
        const ok =
          Buffer.from(b58decode(postTok.mint)).toString('hex') === mintHex &&
          Buffer.from(b58decode(postTok.owner)).toString('hex') === ownerHex &&
          amount === postTok.amount;
        if (ok) report.totals.token_decode_agreement++;
        else report.totals.token_decode_disagreement++;
      }
    }
  }

  // wrong-side guard: on accounts the event MOVED, the PRE side must NOT
  // agree with fetch-time — if it does, the witness proves nothing.
  if (c.verdict === 'certified_no_touch_after' && BigInt(bal.pre) !== BigInt(bal.post)) {
    report.totals.witness_pre_must_fail_hit++;
    if (lamportsNow === BigInt(bal.pre)) report.totals.witness_pre_must_fail_agreed++;
  }
}

// Historical-read test: ask for the account at the event's slot and record
// WHERE the answer is anchored — the parameter question, independent of the
// account's content.

const sampleEvent = chosen[0];
const sampleEventSlot = sampleEvent.slot;
try {
  const addr = classified[0] && classified[0].address;
  if (addr) {
    report.totals.historical_read_requests++;
    const r = await rpc('getAccountInfo', [addr, { encoding: 'base64', minContextSlot: sampleEventSlot }]);
    const anchor = r && r.context && r.context.slot;
    if (anchor === sampleEventSlot) report.totals.historical_read_anchored_requested++;
    else report.totals.historical_read_anchored_current++;
    report.historical_read = {
      requested_slot: sampleEventSlot,
      response_context_slot: anchor,
      interpretation:
        anchor === sampleEventSlot
          ? 'context matched the requested slot (verify content against a known move)'
          : 'context anchored elsewhere: the parameter is a FLOOR, not a time machine — past-slot question answered with current state and no error',
    };
  }
} catch (e) {
  report.historical_read = { error: e.message };
}

// Predecessor recursion: for touched_after accounts, fetch the newest NEWER
// signature's transaction... no — the idea is the PRIOR touching tx. Fetch
// signatures BEFORE the event, take the head, fetch that tx, and inventory
// every data-bearing field for an account-state payload of our address.
for (const c of classified.filter((x) => x.verdict === 'touched_after').slice(0, 6)) {
  try {
    const prior = await rpc('getSignaturesForAddress', [
      c.address,
      { before: c.eventSig, limit: 1, commitment: 'finalized' },
    ]);
    if (!prior || prior.length === 0) continue;
    const ptx = await rpc('getTransaction', [
      prior[0].signature,
      { encoding: 'jsonParsed', maxSupportedTransactionVersion: 1, commitment: 'finalized' },
    ]);
    if (!ptx) continue;
    report.totals.predecessor_scans++;
    // Inventory: walk the response; any `data` key carries instruction bytes
    // (base58) — count them; any account-state payload would be a per-account
    // data array (base64 account bytes) — count them.
    const walk = (node) => {
      if (!node || typeof node !== 'object') return;
      if (Array.isArray(node)) { node.forEach(walk); return; }
      for (const [k, v] of Object.entries(node)) {
        if (k === 'data' && typeof v === 'string') report.totals.predecessor_instruction_data_fields++;
        else if (k === 'data' && Array.isArray(v) && typeof v[0] === 'string' && v[1] === 'base64') {
          report.totals.predecessor_account_state_payloads++;
        }
        walk(v);
      }
    };
    walk(ptx);
  } catch (e) {
    console.error(`predecessor scan failed: ${e.message}`);
  }
}

// --- verdict ---------------------------------------------------------------
const t = report.totals;
const witnessed = t.witness_post_lamport_agreement + t.witness_post_lamport_disagreement;
const wrongSideDiscriminates =
  t.witness_pre_must_fail_hit === 0 ||
  t.witness_pre_must_fail_agreed < t.witness_pre_must_fail_hit;

report.limits = [
  'certification covers the interval AFTER the event only: fetch-time bytes equal the POST-event state, never the pre-event state — the pre-event half of the item stays open by construction and is not claimed',
  'certification is a property of a moment: rows whose newer-touch re-check fires between phase 1 and phase 3 land in expired_between_phases and are excluded from every witness denominator (counted, not hidden)',
  'a certified account fetchable at witness time only; accounts that vanished by fetch time cannot be witnessed (count them via fetch nulls, not claimed here)',
  'getSignaturesForAddress head-first with limit 100: an account with >100 newer signatures classifies conservatively as touched_after',
  'one endpoint, one program window, ACCOUNT_CAP-bounded — a measurement window, not a population census',
  'no audit, no external consumer, User Zero only',
];

if (!report.offlist_guard.refused) {
  report.verdict = { code: 'CHECK_FAILED', text: 'off-list guard accepted a foreign address — harness cannot distinguish the event from an unrelated address' };
} else if (t.witness_post_lamport_disagreement > 0 || t.token_decode_disagreement > 0) {
  report.verdict = { code: 'REFUTED', text: 'certified accounts disagreed with the event post-state witness — boundary logic or fetch is wrong' };
} else if (t.classified_certified === 0) {
  report.verdict = { code: 'NO_CERTIFIED_CASES', text: 'no account in the window was event-final — certification untested this window' };
} else if (witnessed === 0) {
  report.verdict = {
    code: 'NO_WITNESSED_CASES',
    text: `${t.classified_certified} accounts certified at classify time, but ${t.expired_between_phases} expired before the fetch and ${t.witness_skipped_fetch_null} are already gone — zero accounts were still-certified-and-fetchable; this window witnesses nothing and refutes nothing — the boundary logic stayed UNTESTED here, not validated`,
  };
} else if (!wrongSideDiscriminates) {
  report.verdict = { code: 'CHECK_FAILED', text: 'wrong-side guard agreed on the pre side — the witness cannot distinguish pre from post state and is vacuous' };
} else {
  report.verdict = {
    code: 'CERTIFIED',
    text:
      `${t.classified_certified}/${t.accounts_considered} touched accounts have no touch after the event; ` +
      `fetch-time lamports agree with the event's own post-state in ${t.witness_post_lamport_agreement} of ${t.witness_post_lamport_agreement + t.witness_post_lamport_disagreement} certified accounts that still exist and were still certified at fetch time (${t.expired_between_phases} certifications expired between classify and fetch and ${t.witness_skipped_fetch_null} certified accounts are gone already — none of them are witnessed) ` +
      `(token bytes ${t.token_decode_agreement}/${t.token_decode_cases}; wrong-side guard: pre-side agreed only ${t.witness_pre_must_fail_agreed}/${t.witness_pre_must_fail_hit}); ` +
      `historical read anchored at current slot in ${t.historical_read_anchored_current}/${t.historical_read_requests}; ` +
      `predecessor responses carried ${t.predecessor_account_state_payloads} account-state payloads across ${t.predecessor_scans} scans`,
  };
}

fs.mkdirSync(path.dirname(OUT), { recursive: true });
fs.writeFileSync(OUT, JSON.stringify(report, null, 2) + '\n');
console.log(JSON.stringify(report.verdict, null, 2));
console.log(`accounts=${t.accounts_considered} certified=${t.classified_certified} touched_after=${t.classified_touched_after} calls=${calls}`);
console.log(`post-agree=${t.witness_post_lamport_agreement} post-disagree=${t.witness_post_lamport_disagreement} token=${t.token_decode_agreement}/${t.token_decode_cases} wrong-side-agreed=${t.witness_pre_must_fail_agreed}/${t.witness_pre_must_fail_hit}`);
process.exit(report.verdict.code === 'CHECK_FAILED' || report.verdict.code === 'REFUTED' ? 1 : 0);
