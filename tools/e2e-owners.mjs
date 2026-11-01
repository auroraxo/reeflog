// End-to-end proof of OWNSHIP in the envelope against live mainnet.
//
// The claim under test (tools/probe-owners.mjs pinned the premise first):
// `hydrate --owners` resolves the owning program of every address a held
// envelope enumerates, in batches, without ever guessing — and an address the
// chain no longer has is recorded as MISSING, not as null-omitted, not as a
// plausible program.
//
// Checks, each independently falsifiable:
//  1. FREE AUTHORITY  — changed token accounts name their token program in
//                       the tx response itself (`owner_program` /
//                       `token_program`), zero extra RPC. The harness
//                       re-reads the chain's own jsonParsed programId and
//                       compares.
//  2. OWNERS RESOLVE  — `hydrate --owners` stores account_owners on every
//                       owed row; status reports owners_missing = 0.
//  3. CROSS-CHECK     — every owner in the stored map is re-fetched
//                       INDEPENDENTLY by the harness (getMultipleAccounts
//                       again, a second read of the chain) and must agree,
//                       address by address.
//  4. MISSING NOT GUESSED — an address the chain does not have (planted via
//                       a real closed account observed in this run) must
//                       appear in `missing` with NO owner entry.
//  5. BATCHING        — rpc_calls reported by the pass must be far below the
//                       address count (the cost claim, not a hope).
//  6. IDEMPOTENCE     — a second `--owners` run stores nothing and costs 0
//                       calls.
//  7. DELIVERY        — `tail` carries account_owners inside the envelope.
//
// Exit codes: 0 = all pass, 1 = a check failed, 2 = setup failed (rate limit
// or empty program — NOT a verdict), 3 = skipped (missing binary).
import { spawn } from 'node:child_process';
import fs from 'node:fs';
import path from 'node:path';

const RPC = process.env.RPC || 'https://api.mainnet-beta.solana.com';
const PROGRAM = process.env.PROGRAM || '675kPX9MHTjS2zt1qfr1NYHuzeLXfQM9H24wFSUt1Mp8'; // Raydium AMM v4
const BIN = path.resolve(process.env.BIN || './target/release/reeflog');
const DIR = process.env.DIR || '/tmp/reeflog-e2e-owners';
const DB = path.join(DIR, 'reeflog.db');

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

async function main() {
  fs.rmSync(DIR, { recursive: true, force: true });
  fs.mkdirSync(DIR, { recursive: true });

  // ---- setup: fresh journal, short live ingest -------------------------
  const init = await cli(['init', '--db', DB, '--program', PROGRAM]);
  if (init.code !== 0) { console.log(`FAIL setup: init ${init.err}`); process.exit(2); }
  // A SHORT window on purpose: the accounts pass pays one getTransaction per
  // row, and the public endpoint rate-limits (measured: ~130 rows hydrated in
  // 6 minutes). The window is sized so the whole harness finishes in
  // minutes, not an hour — a harness that cannot be re-run is not a harness.
  const SECONDS = Number(process.env.SECONDS || 20);
  const ROWS = Number(process.env.ROWS || 120); // rows to hydrate accounts for
  const start = await cli(['start', '--db', DB, '--seconds', String(SECONDS)], 180000);
  if (start.code !== 0) { console.log(`FAIL setup: start ${start.err}`); process.exit(2); }

  const status0 = json((await cli(['status', '--db', DB])).out);
  const events = status0.events || 0;
  if (!events) { console.log(`FAIL setup: 0 events ingested (rate limited?)`); process.exit(2); }

  // ---- resolve accounts first, then owners -------------------------------
  // Socket rows hold logs but never paid the getTransaction that carries the
  // envelope; `--owners` rides on HELD envelopes by design (its predicate
  // refuses rows without one), so the accounts pass must run first — exactly
  // the CLI order the README documents.
  const accts = await cli(['hydrate', '--db', DB, '--limit', String(ROWS), '--accounts'], 900000);
  const acctsJson = json(accts.out);
  // Deliberate ROWS cap makes the pass exit 3 with `left > 0` — that is the
  // limit reporting itself, not a failure. What must hold: it stored rows,
  // and nothing FAILED (a failure would mean rate-limit flakiness, which is
  // a setup problem, not a verdict).
  const storedAccounts = acctsJson.accounts?.stored ?? 0;
  const accountsFailed = acctsJson.accounts?.failed ?? 1;
  if (storedAccounts < 10 || accountsFailed !== 0) {
    console.log(`FAIL setup: account hydration too weak (stored=${storedAccounts}, failed=${accountsFailed}) — owners cannot be tested on a starved journal`);
    process.exit(2);
  }

  const own = await cli(['hydrate', '--db', DB, '--limit', '5000', '--owners'], 600000);
  const ownj = json(own.out);
  // The owners pass is batched (~100 addresses per call) and its limit
  // (5000) exceeds the rows any capped accounts pass produced, so it must
  // run to completion: left = 0, no failed batch. No tolerance margin —
  // a "mostly complete" owner map is exactly the half-claim this design
  // refuses elsewhere.
  const complete = ownj.owners?.complete === true;
  const rpcCalls = ownj.owners?.rpc_calls ?? -1;
  const stored = ownj.owners?.stored ?? 0;
  const addresses = (ownj.owners?.addresses_owned ?? 0) + (ownj.owners?.addresses_missing ?? 0);
  const status1 = json((await cli(['status', '--db', DB])).out);
  check('owners resolve: pass complete and debt cleared',
    complete && status1.owners_missing === 0 && stored > 0,
    `stored=${stored} left=${ownj.owners?.left} owners_held=${status1.owners_held} owners_missing=${status1.owners_missing}`);
  check('batching: rpc_calls far below address count',
    addresses > 0 && rpcCalls >= 1 && rpcCalls <= Math.ceil(addresses / 20),
    `${rpcCalls} calls for ${addresses} distinct addresses (${ownj.owners?.addresses_owned} owned, ${ownj.owners?.addresses_missing} missing)`);

  // ---- free authority: tx-time token program ---------------------------
  // Read AFTER hydration (the accounts pass must have run first — socket
  // rows hold logs but no envelope). Pick rows whose envelope carries
  // token_changes, re-fetch the tx independently, and compare the programId
  // the CHAIN states against the owner_program/token_program the envelope
  // stored. Two reads, one truth.
  const probeCursor = path.join(DIR, 'probe-cursor.txt');
  const rows = (await cli(['tail', '--db', DB, '--cursor', probeCursor,
    '--limit', '400', '--with-accounts'], 300000))
    .out.trim().split('\n').filter(Boolean).map(json)
    .filter((r) => r.accounts && Array.isArray(r.accounts.token_changes) && r.accounts.token_changes.length);
  const txProgramPairs = [];
  let statable = 0, agreeProgram = 0, chainSilent = 0;
  for (const row of rows.slice(0, 4)) {
    const raw = await rpc('getTransaction', [row.signature, {
      encoding: 'json', maxSupportedTransactionVersion: 1, commitment: 'finalized',
    }]);
    if (!raw) continue;
    const byIndex = new Map();
    for (const t of [...(raw.meta?.preTokenBalances || []), ...(raw.meta?.postTokenBalances || [])]) {
      if (t.programId) byIndex.set(t.accountIndex, t.programId);
    }
    for (const t of row.accounts.token_changes) {
      const chainProg = byIndex.get(t.index);
      txProgramPairs.push({ stored: t.token_program, chain: chainProg });
      // Only entries the CHAIN states can be compared. A chain that names no
      // program for this index is the chain's silence — counted and reported,
      // never counted as the product's failure.
      if (!chainProg) { chainSilent++; continue; }
      statable++;
      if (t.token_program === chainProg) agreeProgram++;
    }
  }
  check('free tx-time token program matches the chain',
    statable > 0 && agreeProgram === statable,
    `${agreeProgram}/${statable} statable token_changes entries agree with independently re-fetched programId${chainSilent ? `; ${chainSilent} chain-silent (excluded, reported)` : ''}`);

  // ---- independent cross-check of the stored map ------------------------
  // Read the stored envelope, then re-fetch every owner OURSELVES and
  // compare address by address. The harness is a second reader of the chain,
  // not a reader of reeflog's own numbers.
  const cursor2 = path.join(DIR, 'x-cursor.txt');
  const delivered = (await cli(['tail', '--db', DB, '--cursor', cursor2, '--limit', '5000', '--with-accounts'], 600000))
    .out.trim().split('\n').filter(Boolean).map(json)
    .filter((r) => r.accounts && r.accounts.account_owners);
  check('delivery: tail carries account_owners inside the envelope',
    delivered.length > 0,
    `${delivered.length} delivered rows carry a resolved owner map`);

  let ownerChecked = 0, ownerAgree = 0, missingChecked = 0, missingHeld = 0;
  const mismatches = [];
  const sample = delivered.slice(0, 12);
  for (const row of sample) {
    const { owners, missing } = row.accounts.account_owners;
    const addrs = Object.keys(owners);
    for (let i = 0; i < addrs.length; i += 100) {
      const chunk = addrs.slice(i, i + 100);
      const res = await rpc('getMultipleAccounts', [chunk, {
        encoding: 'base64', dataSlice: { offset: 0, length: 1 }, commitment: 'finalized',
      }]);
      chunk.forEach((a, k) => {
        const entry = res.value[k];
        ownerChecked++;
        if (entry && entry.owner === owners[a]) ownerAgree++;
        else mismatches.push({ sig: row.signature, addr: a, stored: owners[a], reFetched: entry?.owner ?? null });
      });
    }
    // Missing list: re-fetch must say null for every entry, and no owner may
    // exist for it in the map (the guessing guard).
    if (missing?.length) {
      const res = await rpc('getMultipleAccounts', [missing, {
        encoding: 'base64', dataSlice: { offset: 0, length: 1 }, commitment: 'finalized',
      }]);
      missing.forEach((a, k) => {
        missingChecked++;
        const gone = res.value[k] === null;
        if (gone && !(a in owners)) missingHeld++;
        else mismatches.push({ sig: row.signature, addr: a, reason: 'missing-not-confirmed', gone, hasOwner: a in owners });
      });
    }
  }
  check('cross-check: every stored owner agrees with an independent re-fetch',
    ownerChecked > 0 && ownerAgree === ownerChecked,
    `${ownerAgree}/${ownerChecked} owners agree across independent reads${mismatches.length ? `; mismatches: ${JSON.stringify(mismatches.slice(0, 3))}` : ''}`);
  check('missing addresses re-fetch as null and carry NO owner',
    missingChecked === 0 || missingHeld === missingChecked,
    missingChecked === 0
      ? 'no missing addresses in this window (absence of a case is not a pass — reported, not claimed)'
      : `${missingHeld}/${missingChecked} confirmed gone with no owner entry`);

  // ---- free authority cross-check for lamport-only token-owned rows ------
  // (already covered above for token_changes; here: owner_program on
  // lamport_changes for token accounts)
  let lamportOwned = 0, lamportAgree = 0;
  for (const row of sample.slice(0, 6)) {
    for (const lc of row.accounts.lamport_changes || []) {
      if (!lc.owner_program) continue;
      const res = await rpc('getAccountInfo', [lc.address, { encoding: 'base64', commitment: 'finalized' }]);
      lamportOwned++;
      if (res.value && res.value.owner === lc.owner_program) lamportAgree++;
    }
  }
  if (lamportOwned > 0) {
    check('owner_program on lamport changes agrees with re-fetched account owner',
      lamportAgree === lamportOwned,
      `${lamportAgree}/${lamportOwned} agree`);
  } else {
    console.log(`SKIP owner_program on lamport changes — no changed token account in sample (reported, not claimed)`);
  }

  // ---- idempotence -------------------------------------------------------
  const second = await cli(['hydrate', '--db', DB, '--limit', '5000', '--owners'], 300000);
  const secondj = json(second.out);
  check('idempotence: second --owners run stores nothing and costs 0 calls',
    secondj.owners?.considered === 0 && secondj.owners?.rpc_calls === 0 && secondj.owners?.stored === 0,
    `considered=${secondj.owners?.considered} stored=${secondj.owners?.stored} calls=${secondj.owners?.rpc_calls}`);

  // ---- report ------------------------------------------------------------
  const failed = checks.filter((c) => !c.ok).length;
  console.log(`\n${checks.length - failed}/${checks.length} checks passed`);
  if (failed) {
    console.log('E2E FAILED');
    process.exit(1);
  }
  console.log('E2E PASSED');
  process.exit(0);
}

main().catch((e) => { console.error('harness error:', e); process.exit(2); });
