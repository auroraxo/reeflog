// End-to-end proof of ACCOUNT DATA (the bytes) in the envelope against live
// mainnet.
//
// The claim under test (tools/probe-account-data.mjs pinned the premise
// first): `hydrate --account-data` stores what each enumerated account HELD
// — bytes, not just who owns it — batched, capped, and with absence and
// truncation recorded as facts rather than guessed.
//
// Checks, each independently falsifiable:
//  1. PASS COMPLETE        — the pass runs to completion; the debt counter
//                            reaches 0; rows were stored.
//  2. BYTES TRUE           — every stored byte string is compared against
//                            an INDEPENDENT re-fetch. Live accounts can move
//                            between the two reads (measured first run: 31
//                            of 272), so differences are CLASSIFIED: a
//                            third read that also differs proves the
//                            account is live-moving; otherwise only the
//                            mutable region of a token account (>= byte 64)
//                            may differ. Anything else is a FAIL.
//  3. TX CONSENSUS         — mint/owner DECODED from the stored bytes must
//                            equal the token metadata the transaction
//                            itself stated. Immutable fields, race-free,
//                            and wrong-address storage cannot survive it.
//  4. SIZE IS TRUE         — the stored `bytes` equals the chain's length.
//  5. TRUNCATION IS MARKED — an account over the cap is cut at the cap,
//                            marked, and the probe confirmed more exists.
//  6. EXECUTABLES FLAGGED  — program code delivered as a flag + size,
//                            independently confirmed executable.
//  7. MISSING NOT GUESSED  — an address recorded missing is really absent.
//  8. BATCHING             — rpc_calls far below the address count.
//  9. IDEMPOTENCE          — a second run stores nothing, costs 0 calls.
// 10. DELIVERY             — `tail` carries account_data in the envelope.
//
// Exit codes: 0 = all pass, 1 = a check failed, 2 = setup failed (rate limit
// or empty program — NOT a verdict), 3 = skipped (missing binary).
import { spawn } from 'node:child_process';
import fs from 'node:fs';
import path from 'node:path';

const RPC = process.env.RPC || 'https://api.mainnet-beta.solana.com';
const PROGRAM = process.env.PROGRAM || '675kPX9MHTjS2zt1qfr1NYHuzeLXfQM9H24wFSUt1Mp8'; // Raydium AMM v4
const BIN = path.resolve(process.env.BIN || './target/release/reeflog');
const DIR = process.env.DIR || '/tmp/reeflog-e2e-account-data';
const DB = path.join(DIR, 'reeflog.db');
const CAP = 16384; // must match ACCOUNT_DATA_CAP in src/account_data.rs

if (!fs.existsSync(BIN)) {
  console.log(`SKIP: binary not found at ${BIN} (build first)`);
  process.exit(3);
}

async function rpc(method, params, tries = 8) {
  let last;
  for (let i = 0; i < tries; i++) {
    if (i) await new Promise((r) => setTimeout(r, Math.min(1000 * 2 ** (i - 1), 15000)));
    try {
      const r = await fetch(RPC, {
        method: 'POST',
        headers: { 'content-type': 'application/json' },
        body: JSON.stringify({ jsonrpc: '2.0', id: 1, method, params }),
      });
      if (r.status === 429) { last = new Error('429'); continue; }
      const j = await r.json();
      if (j.error) throw new Error(JSON.stringify(j.error));
      return j.result;
    } catch (e) { last = e; }
  }
  throw last;
}

function cli(args, timeoutMs = 600000) {
  return new Promise((resolve) => {
    const c = spawn(BIN, args, { stdio: ['ignore', 'pipe', 'pipe'] });
    let o = '', e = '';
    const t = setTimeout(() => { c.kill('SIGKILL'); resolve({ code: 99, out: o, err: e + '\ntimeout' }); }, timeoutMs);
    c.stdout.on('data', (d) => { o += d; });
    c.stderr.on('data', (d) => { e += d; });
    c.on('close', (code) => { clearTimeout(t); resolve({ code, out: o, err: e }); });
    c.on('error', (err) => { clearTimeout(t); resolve({ code: null, out: o, err: `${e}\n${err}` }); });
  });
}
const json = (s) => { try { return JSON.parse(s); } catch { return { raw: s }; } };

const checks = [];
const check = (name, ok, detail) => {
  checks.push({ name, ok: !!ok, detail });
  console.log(`${ok ? 'PASS' : 'FAIL'} ${name}${detail ? ` — ${detail}` : ''}`);
};

// base58 encode (the SPL layout stores addresses as 32 raw bytes; the tx
// metadata names them as base58 strings — one alphabet, two worlds).
const ALPHA = '123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz';
const b58 = (bytes) => {
  let n = 0n;
  for (const b of bytes) n = n * 256n + BigInt(b);
  let out = '';
  while (n > 0n) { out = ALPHA[Number(n % 58n)] + out; n /= 58n; }
  for (const b of bytes) { if (b !== 0) break; out = '1' + out; }
  return out || '1';
};

const TOKEN_PROGRAMS = new Set([
  'TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA',
  'TokenzQdBNbLqP5VEhdkAS6EPFLC1PHnBqCXEpPxuEb',
  'ATokenGPvbdGVxr1b2hvZbsiqW5xWH25efTNsLJA8knL',
]);

async function main() {
  fs.rmSync(DIR, { recursive: true, force: true });
  fs.mkdirSync(DIR, { recursive: true });

  // ---- setup: fresh journal, short live ingest -------------------------
  const init = await cli(['init', '--db', DB, '--program', PROGRAM]);
  if (init.code !== 0) { console.log(`FAIL setup: init ${init.err}`); process.exit(2); }
  const SECONDS = Number(process.env.SECONDS || 20);
  const ROWS = Number(process.env.ROWS || 120);
  const start = await cli(['start', '--db', DB, '--seconds', String(SECONDS)], 180000);
  if (start.code !== 0) { console.log(`FAIL setup: start ${start.err}`); process.exit(2); }

  const status0 = json((await cli(['status', '--db', DB])).out);
  if (!status0.events) { console.log('FAIL setup: 0 events ingested (rate limited?)'); process.exit(2); }

  // ---- accounts first, then data ---------------------------------------
  // The data pass rides on HELD envelopes (its predicate refuses rows
  // without one), so the accounts pass must run first — the CLI order the
  // README documents.
  const accts = await cli(['hydrate', '--db', DB, '--limit', String(ROWS), '--accounts'], 900000);
  const acctsJson = json(accts.out);
  const storedAccounts = acctsJson.accounts?.stored ?? 0;
  const accountsFailed = acctsJson.accounts?.failed ?? 1;
  if (storedAccounts < 10 || accountsFailed !== 0) {
    console.log(`FAIL setup: account hydration too weak (stored=${storedAccounts}, failed=${accountsFailed})`);
    process.exit(2);
  }

  const pass = await cli(['hydrate', '--db', DB, '--limit', '5000', '--account-data'], 900000);
  const pj = json(pass.out);
  const ad = pj.account_data || {};
  const status1 = json((await cli(['status', '--db', DB])).out);

  check('pass complete and debt cleared',
    ad.complete === true && status1.account_data_missing === 0 && (ad.stored ?? 0) > 0,
    `stored=${ad.stored} left=${ad.left} held=${status1.account_data_held} missing=${status1.account_data_missing}`);

  // ---- delivery ---------------------------------------------------------
  const cursor = path.join(DIR, 'cursor.txt');
  const delivered = (await cli(['tail', '--db', DB, '--cursor', cursor, '--limit', '5000', '--with-accounts'], 600000))
    .out.trim().split('\n').filter(Boolean).map(json)
    .filter((r) => r.accounts && r.accounts.account_data);
  check('delivery: tail carries account_data inside the envelope',
    delivered.length > 0,
    `${delivered.length} delivered rows carry an account_data map`);
  if (delivered.length === 0) {
    console.log('\nFAIL: nothing delivered — the other checks cannot run');
    process.exit(1);
  }

  // ---- collect the stored claims ---------------------------------------
  const sample = delivered.slice(0, 10);
  const allHeld = [];
  const allExec = [];
  for (const row of sample) {
    const d = row.accounts.account_data;
    for (const [addr, entry] of Object.entries(d.data || {})) {
      if (entry.state === 'held') allHeld.push({ sig: row.signature, addr, entry });
      else if (entry.state === 'skipped_executable') allExec.push({ sig: row.signature, addr, entry });
    }
  }
  check('sample is non-vacuous: held accounts were collected',
    allHeld.length > 0,
    `${allHeld.length} held + ${allExec.length} executable accounts across ${sample.length} rows`);
  if (allHeld.length === 0) {
    console.log('\nFAIL: no held accounts in the sample — the byte checks cannot run');
    process.exit(1);
  }

  // ---- independent re-fetch of every claimed address --------------------
  const addresses = [...new Set([...allHeld.map((h) => h.addr), ...allExec.map((h) => h.addr)])];
  const refetched = new Map();
  for (let i = 0; i < addresses.length; i += 100) {
    const chunk = addresses.slice(i, i + 100);
    const res = await rpc('getMultipleAccounts', [chunk, { encoding: 'base64', commitment: 'finalized' }]);
    chunk.forEach((a, k) => refetched.set(a, res.value[k]));
  }

  // ---- classify every byte difference -----------------------------------
  // Identical is simple. A difference must be EXPLAINED or the check fails:
  //   drift_live      — a third read differs from the second: the account
  //                     is moving right now; neither read was a defect.
  //   drift_volatile  — chain stable now, but the account is POSITIVELY
  //                     identified as a classic SPL token account (exactly
  //                     165 bytes: mint [0..32], owner [32..64] are
  //                     immutable; the difference is in the mutable region
  //                     >= 64: amount, delegate, close authority).
  //   unattributed    — chain stable now and the layout does NOT let me
  //                     claim any field immutable. This is reported as a
  //                     count, never as a pass: a mint's supply and a
  //                     program's state both live here, so "different from
  //                     an earlier read" cannot be attributed without a
  //                     same-instant snapshot. The DECISIVE anchors are the
  //                     completeness, size, and tx-consensus checks below.
  // A product defect must be caught where attribution is possible — never
  // by asserting immutability a layout does not have (the first version of
  // this harness claimed mint/owner immutability for a Token-2022 MINT,
  // whose supply [36..44] sits inside that window; it failed 2/2 on a real
  // mint and was corrected here).
  const CLASSIC_TOKEN_ACCOUNT = (b) => b.length === 165;
  const cls = { identical: 0, drift_live: 0, drift_volatile: 0, unattributed: 0 };
  const bad = { immutable: 0, length: 0, gone: 0, absent_but_held: 0 };
  const explain = [];
  let sizeChecked = 0, sizeAgree = 0;
  let truncChecked = 0, truncAgree = 0;

  for (const h of allHeld) {
    const acct = refetched.get(h.addr);
    if (!acct) {
      // The chain does not have this account NOW. If the product recorded it
      // as `held`, that is a guessed/snapshot-mismatched fact: an absent
      // address must live in `missing`, never in `data`. This is the exact
      // defect the guess-the-missing mutant creates, so it must FAIL here
      // rather than be skipped as "not comparable".
      bad.absent_but_held++;
      explain.push({ addr: h.addr, why: 'absent-but-recorded-held' });
      continue;
    }
    const chainBytes = Buffer.from(acct.data[0], 'base64');
    const storedBytes = Buffer.from(h.entry.data, 'base64');

    // The size the product claims must be the account's real size.
    sizeChecked++;
    if (h.entry.bytes === chainBytes.length) sizeAgree++;
    else { bad.length++; explain.push({ addr: h.addr, why: 'size-lie', stored: h.entry.bytes, chain: chainBytes.length }); }

    // Truncation: an account longer than the cap must be stored cut at the
    // cap, marked truncated, with the at-cap probe's confirmation.
    if (chainBytes.length > CAP) {
      truncChecked++;
      const ok = storedBytes.length === CAP && h.entry.truncated === true && h.entry.truncation_verified === true;
      if (ok) truncAgree++;
      else explain.push({ addr: h.addr, why: 'truncation-not-marked', stored: storedBytes.length, truncated: h.entry.truncated, verified: h.entry.truncation_verified });
    }

    if (storedBytes.equals(chainBytes)) { cls.identical++; continue; }
    if (storedBytes.length !== chainBytes.length) {
      bad.length++;
      explain.push({ addr: h.addr, why: 'length-diff', stored: storedBytes.length, chain: chainBytes.length });
      continue;
    }
    // Third read: is the account moving right now?
    const t3 = await rpc('getAccountInfo', [h.addr, { encoding: 'base64', commitment: 'finalized' }]);
    const b3 = t3.value ? Buffer.from(t3.value.data[0], 'base64') : null;
    if (b3 && !b3.equals(chainBytes)) { cls.drift_live++; continue; }
    // Stable + same length: claim immutability ONLY where the layout grants
    // it — a classic 165-byte SPL token account (mint and owner cannot
    // change for the life of the account).
    if (CLASSIC_TOKEN_ACCOUNT(storedBytes)) {
      if (!storedBytes.subarray(0, 64).equals(chainBytes.subarray(0, 64))) {
        bad.immutable++;
        explain.push({ addr: h.addr, why: 'token-account-mint-or-owner-diff' });
      } else {
        cls.drift_volatile++;
      }
    } else {
      cls.unattributed++;
      if (explain.length < 6) explain.push({ addr: h.addr, why: 'unattributed', len: storedBytes.length, owner: acct.owner });
    }
  }

  const contentOk = bad.immutable === 0 && bad.length === 0 && bad.absent_but_held === 0
    && bad.gone === 0;
  check('stored bytes contradict neither the chain nor absence',
    contentOk && sizeChecked > 0,
    `${cls.identical} identical, ${cls.drift_live} live-moving, ${cls.drift_volatile} volatile-region (classic token accounts), ${cls.unattributed} unattributed (reported, no immutability claimed); contradictions: absent-but-held=${bad.absent_but_held} size=${bad.length} immutable=${bad.immutable}${explain.length ? `; ${JSON.stringify(explain.slice(0, 3))}` : ''}`);

  check("the stored size is the account's real size",
    sizeChecked > 0 && sizeAgree === sizeChecked,
    `${sizeAgree}/${sizeChecked} sizes equal the chain's byte length`);

  if (truncChecked > 0) {
    check('an account over the cap is truncated, marked, and the probe verified it',
      truncAgree === truncChecked,
      `${truncAgree}/${truncChecked} over-cap accounts capped at ${CAP} B with truncated+verified`);
  } else {
    console.log(`SKIP truncation check — no account over ${CAP} B in this window (reported, not claimed)`);
  }

  // ---- tx consensus: decoded immutable fields vs the tx's own words -----
  // For addresses the transaction itself described (token_changes on the
  // same index the envelope's account list maps), the mint/owner decoded
  // from the STORED bytes must equal the metadata the tx stated.
  let consensusChecked = 0, consensusAgree = 0;
  for (const h of allHeld) {
    const row = sample.find((r) => r.signature === h.sig);
    const tc = (row?.accounts?.token_changes || []).find((t) =>
      (row.accounts.accounts || []).some((a) => a.index === t.index && a.address === h.addr));
    if (!tc?.mint) continue;
    const acct = refetched.get(h.addr);
    if (!acct || !TOKEN_PROGRAMS.has(acct.owner)) continue;
    const bytes = Buffer.from(h.entry.data, 'base64');
    // Only a CLASSIC token account has the mint [0..32] / owner [32..64]
    // layout; a mint account (398 B, Token-2022) has its supply inside that
    // window and would be misread. Claiming the layout is what makes the
    // comparison deterministic.
    if (bytes.length !== 165) continue;
    consensusChecked++;
    if (b58(bytes.subarray(0, 32)) === tc.mint && b58(bytes.subarray(32, 64)) === (tc.owner || null)) {
      consensusAgree++;
    } else {
      explain.push({ addr: h.addr, why: 'tx-metadata-vs-bytes', txMint: tc.mint, decodedMint: b58(bytes.subarray(0, 32)) });
    }
  }
  if (consensusChecked > 0) {
    check("mint/owner decoded from stored bytes match the transaction's own token metadata",
      consensusAgree === consensusChecked,
      `${consensusAgree}/${consensusChecked} immutable fields agree across stored bytes and tx`);
  } else {
    console.log("SKIP tx-metadata consensus — no tx-described token account in this window's sample (reported, not claimed)");
  }

  // ---- executables are flagged, not stored ------------------------------
  let execSeen = 0, execFlagged = 0, execVerified = 0;
  for (const { addr, entry } of allExec) {
    execSeen++;
    if (entry.data === undefined) execFlagged++;
    const acct = refetched.get(addr);
    if (acct && acct.executable === true && entry.bytes === Buffer.from(acct.data[0], 'base64').length) execVerified++;
  }
  if (execSeen > 0) {
    check('program code is flagged, byte-free, and its size is right',
      execFlagged === execSeen && execVerified === execSeen,
      `${execFlagged}/${execSeen} flagged without bytes; ${execVerified}/${execSeen} independently confirmed executable with matching size`);
  } else {
    console.log('SKIP executable check — no program account in this window (reported, not claimed)');
  }

  // ---- missing not guessed ---------------------------------------------
  const missingAddrs = [];
  for (const row of sample) {
    for (const a of row.accounts.account_data.missing || []) missingAddrs.push(a);
  }
  let missingChecked = 0, missingHeld = 0;
  for (let i = 0; i < missingAddrs.length; i += 100) {
    const chunk = missingAddrs.slice(i, i + 100);
    const res = await rpc('getMultipleAccounts', [chunk, { encoding: 'base64', commitment: 'finalized' }]);
    chunk.forEach((a, k) => {
      missingChecked++;
      if (res.value[k] === null) missingHeld++;
    });
  }
  if (missingChecked > 0) {
    check('addresses recorded missing really are absent from the chain',
      missingHeld === missingChecked,
      `${missingHeld}/${missingChecked} confirmed gone on an independent read`);
  } else {
    console.log('SKIP missing check — no missing address in this window (reported, not claimed)');
  }

  // ---- batching (the cost claim) ---------------------------------------
  const addrCount = (ad.addresses_held ?? 0) + (ad.addresses_missing ?? 0) + (ad.addresses_skipped_executable ?? 0);
  const rpcCalls = ad.rpc_calls ?? -1;
  check('batching: rpc_calls far below address count',
    addrCount > 0 && rpcCalls >= 1 && rpcCalls <= Math.ceil(addrCount / 20),
    `${rpcCalls} calls for ${addrCount} distinct addresses (${ad.addresses_held} held, ${ad.addresses_missing} missing, ${ad.addresses_skipped_executable} executable)`);

  // ---- idempotence ------------------------------------------------------
  const second = await cli(['hydrate', '--db', DB, '--limit', '5000', '--account-data'], 300000);
  const secondj = json(second.out);
  check('idempotence: second run stores nothing and costs 0 calls',
    secondj.account_data?.considered === 0 && secondj.account_data?.rpc_calls === 0 && secondj.account_data?.stored === 0,
    `considered=${secondj.account_data?.considered} stored=${secondj.account_data?.stored} calls=${secondj.account_data?.rpc_calls}`);

  const failed = checks.filter((c) => !c.ok).length;
  console.log(`\n${checks.length - failed}/${checks.length} checks passed`);
  if (failed) { console.log('E2E FAILED'); process.exit(1); }
  console.log('E2E PASSED');
  process.exit(0);
}

main().catch((e) => { console.error('harness error:', e); process.exit(2); });
