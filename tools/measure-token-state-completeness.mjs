#!/usr/bin/env node
// The TOKEN-STATE completeness measurement (PULS AY, 2026-11-20) — MEASURED
// with the REAL binaries, not inferred from the source.
//
// The defect (published v0.7.40): `token_state` (the event-time token
// authority, commit 3ed0536, 2026-11-02) is written by the extractor but
// is NOT part of any envelope completeness predicate. An envelope from an
// extractor BEFORE that commit carries instructions + account list + flag
// provenance and NO `token_state` — and every count-based query calls it
// WHOLE. The same "half envelope presented as a whole one" class the
// instruction-bytes defect already paid for, one field further along.
//
// The faces (each a predicate for BOTH trees):
//   D1 `status` counts a `token_state`-less envelope as PARTIAL;
//   D2 the accounts debt COUNTER counts it (queue's twin);
//   D3 `tail --with-accounts` HOLDS it instead of delivering+consuming;
//   D4 the WRITE guard refuses a `token_state`-less envelope over a good
//      one (extractor-regression door closed);
//   D5 a half envelope over a RECORDED outcome leaves the debt queue
//      (no infinite refetch, no permanent exit 3);
//   G1 a COMPLETE envelope (three markers) is delivered and consumed;
//   G2 plain `tail` is never held by the accounts queue;
//   G3 a recorded outcome with no envelope is skipped, not held.
//
//   node tools/measure-token-state-completeness.mjs
//   BIN=/path/to/reeflog OUT=/tmp/x.json LABEL=v0.7.40 node tools/measure-token-state-completeness.mjs
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import crypto from 'node:crypto';
import { execFileSync, spawn, spawnSync } from 'node:child_process';

const BIN = process.env.BIN || path.resolve(import.meta.dirname, '..', 'target/release/reeflog');
const OUT = process.env.OUT || null;
const LABEL = process.env.LABEL || 'this-tree';
const PROGRAM = 'whirLbMiicVdio4qvUfM5KAg6Ct8VwpYzGff3uctyCc';

if (!fs.existsSync(BIN)) {
  console.error(`no reeflog binary at ${BIN} — refusing to measure without one`);
  process.exit(2);
}

const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'reeflog-tokstate-'));
const db = path.join(dir, 'j.db');
const cursor = path.join(dir, 'c.txt');

function run(args) {
  const r = spawnSync(BIN, args, { cwd: dir, encoding: 'utf8' });
  return {
    code: r.status,
    out: (r.stdout || '').split('\n').filter(Boolean),
    err: (r.stderr || '').trim(),
  };
}

// Like run(), but ASYNC — required whenever a local stub server must answer
// the child while it runs: spawnSync blocks this process's event loop, so an
// in-process HTTP server can never serve a spawnSync'd child (the socket is
// accepted by the backlog and then times out — which looks exactly like a
// dead RPC endpoint).
function runAsync(args) {
  return new Promise((resolve) => {
    const c = spawn(BIN, args, { cwd: dir });
    let stdout = '';
    let stderr = '';
    c.stdout.on('data', (d) => { stdout += d; });
    c.stderr.on('data', (d) => { stderr += d; });
    c.on('close', (code) => resolve({
      code,
      out: stdout.split('\n').filter(Boolean),
      err: stderr.trim(),
    }));
  });
}

function init() {
  for (const f of [db, db + '-wal', db + '-shm', cursor]) fs.rmSync(f, { force: true });
  execFileSync(BIN, ['init', '--db', db, '--program', PROGRAM], { cwd: dir, stdio: 'ignore' });
}

const { DatabaseSync } = await import('node:sqlite');
function plant(rows) {
  const conn = new DatabaseSync(db);
  const now = Date.now();
  for (const r of rows) {
    conn
      .prepare(
        `INSERT INTO events(signature, slot, err, logs, source, first_seen, payload_state, accounts, account_state)
         VALUES (?, ?, NULL, ?, 'ws', ?, 'held', ?, ?)`,
      )
      .run(r.sig, r.slot, r.logs, now, r.accounts, r.state);
  }
  conn.close();
}

function envelopeOf(sig) {
  const conn = new DatabaseSync(db);
  const row = conn.prepare('SELECT accounts, account_state FROM events WHERE signature = ?').get(sig);
  conn.close();
  return row ?? null;
}

function statusReport() {
  const r = run(['status', '--db', db]);
  const text = r.out.join('\n');
  const start = text.indexOf('{');
  return { code: r.code, report: start >= 0 ? JSON.parse(text.slice(start)) : null };
}

const WHOLE = JSON.stringify({
  version: 'legacy', slot: 10, program_id: 'P', err: null,
  accounts_total: 1, accounts_static: 1, accounts_loaded: 0,
  accounts_flags_source: 'header',
  accounts: [{ index: 0, address: 'A', signer: true, writable: true, source: 'static' }],
  instructions: [], lamport_changes: [], token_changes: [],
  token_state: [{ index: 0, address: 'A', mint: 'M', source: 'tx-time' }],
});
// The envelope an extractor BEFORE the event-time authority wrote:
// instructions + account list + flag provenance, NO token_state.
const HALF = JSON.stringify({
  version: 'legacy', slot: 10, program_id: 'P', err: null,
  accounts_total: 1, accounts_static: 1, accounts_loaded: 0,
  accounts_flags_source: 'header',
  accounts: [{ index: 0, address: 'A', signer: true, writable: true, source: 'static' }],
  instructions: [], lamport_changes: [], token_changes: [],
});

const SIG_WHOLE = 'SigWhole111111111111111111111111111111111111111111111';
const SIG_HALF = 'SigHalf11111111111111111111111111111111111111111111111';
const LOG = '["Program log: hi"]';

const faces = [];
function face(id, kind, detail) {
  faces.push({ id, kind, ...detail });
}

// --- D1 + D2: the health report and the debt counter over ONE half row ---
init();
plant([{ sig: SIG_HALF, slot: 1, logs: LOG, accounts: HALF, state: 'held' }]);
{
  const { code, report } = statusReport();
  face('D1', 'defect-guard', {
    exitCode: code,
    accountsPartial: report?.accounts_partial,
    accountsHeld: report?.accounts_held,
  });
  face('D2', 'defect-guard', {
    accountsMissing: report?.accounts_missing,
  });
}

// --- D3: `tail --with-accounts` over whole + half rows ---
init();
plant([
  { sig: SIG_WHOLE, slot: 1, logs: LOG, accounts: WHOLE, state: 'held' },
  { sig: SIG_HALF, slot: 2, logs: LOG, accounts: HALF, state: 'held' },
]);
{
  const r = run(['tail', '--db', db, '--cursor', cursor, '--with-accounts']);
  const sigs = r.out
    .filter((l) => l.trim().startsWith('{'))
    .map((l) => { try { return JSON.parse(l).signature; } catch { return null; } })
    .filter(Boolean);
  let cursorPos = 0;
  if (fs.existsSync(cursor)) {
    const raw = fs.readFileSync(cursor, 'utf8').trim();
    const m = raw.startsWith('{') ? raw.match(/"value":\s*(\d+)/) : null;
    cursorPos = m ? Number(m[1]) : Number(raw) || 0;
  }
  face('D3', 'defect-guard', {
    exitCode: r.code,
    delivered: sigs,
    cursorPos,
    namedHydrate: r.err.includes('hydrate'),
    err: r.err,
  });
}

// --- D4: the accounts pass must SELECT a stored half row and REFILL it ---
// The defect tree's queue never notices the missing marker: `hydrate
// --accounts` considers 0 rows (no RPC is ever paid) and the journal
// stands half forever while calling itself complete. The fixed tree
// refetches and replaces it. The RPC is a one-shot local endpoint that
// counts the calls, so "was the debt ever noticed" is a measured fact.
init();
plant([{ sig: SIG_HALF, slot: 1, logs: LOG, accounts: HALF, state: 'held' }]);
{
  const { createServer } = await import('node:http');
  let rpcCalls = 0;
  const srv = createServer((req, res) => {
    let body = '';
    req.on('data', (c) => { body += c; });
    req.on('end', () => {
      rpcCalls += 1;
      res.writeHead(200, { 'content-type': 'application/json' });
      res.end(JSON.stringify({
        jsonrpc: '2.0', id: 1,
        result: {
          slot: 10,
          transaction: { message: { accountKeys: ['A'], instructions: [] } },
          meta: { logMessages: ['x'], preBalances: [1], postBalances: [1], preTokenBalances: [], postTokenBalances: [] },
        },
      }));
    });
  });
  await new Promise((ok) => srv.listen(0, '127.0.0.1', ok));
  const port = srv.address().port;
  try {
    const r = await runAsync(['hydrate', '--db', db, '--accounts', '--rpc', `http://127.0.0.1:${port}`, '--limit', '5']);
    const after = envelopeOf(SIG_HALF);
    const storedEnvelope = after?.accounts ? JSON.parse(after.accounts) : null;
    face('D4', 'defect-guard', {
      exitCode: r.code,
      err: r.err,
      rpcCalls,
      accountState: after?.account_state ?? null,
      storedHasTokenState: storedEnvelope ? Object.prototype.hasOwnProperty.call(storedEnvelope, 'token_state') : null,
    });
  } finally {
    srv.close();
  }
}

// --- D5: a half envelope over a recorded outcome leaves the debt queue ---
init();
plant([{ sig: SIG_HALF, slot: 1, logs: LOG, accounts: HALF, state: 'unavailable' }]);
{
  const { code, report } = statusReport();
  face('D5', 'defect-guard', {
    exitCode: code,
    accountsMissing: report?.accounts_missing,
    accountsPartial: report?.accounts_partial,
  });
}

// --- G1: a complete envelope is delivered and consumed ---
init();
plant([{ sig: SIG_WHOLE, slot: 1, logs: LOG, accounts: WHOLE, state: 'held' }]);
{
  const r = run(['tail', '--db', db, '--cursor', cursor, '--with-accounts']);
  const sigs = r.out
    .filter((l) => l.trim().startsWith('{'))
    .map((l) => { try { return JSON.parse(l).signature; } catch { return null; } })
    .filter(Boolean);
  let cursorPos = 0;
  if (fs.existsSync(cursor)) {
    const raw = fs.readFileSync(cursor, 'utf8').trim();
    const m = raw.startsWith('{') ? raw.match(/"value":\s*(\d+)/) : null;
    cursorPos = m ? Number(m[1]) : Number(raw) || 0;
  }
  face('G1', 'guard', { exitCode: r.code, delivered: sigs, cursorPos });
}

// --- G2: plain tail is never held by the accounts queue ---
init();
plant([{ sig: SIG_HALF, slot: 1, logs: LOG, accounts: HALF, state: 'held' }]);
{
  const r = run(['tail', '--db', db, '--cursor', cursor]);
  const sigs = r.out
    .filter((l) => l.trim().startsWith('{'))
    .map((l) => { try { return JSON.parse(l).signature; } catch { return null; } })
    .filter(Boolean);
  face('G2', 'guard', { exitCode: r.code, delivered: sigs });
}

// --- G3: recorded outcome with no envelope is skipped, not held ---
init();
plant([{ sig: 'SigFinal111111111111111111111111111111111111111111111', slot: 1, logs: LOG, accounts: null, state: 'unavailable' }]);
{
  const r = run(['tail', '--db', db, '--cursor', cursor, '--with-accounts']);
  let cursorPos = 0;
  if (fs.existsSync(cursor)) {
    const raw = fs.readFileSync(cursor, 'utf8').trim();
    const m = raw.startsWith('{') ? raw.match(/"value":\s*(\d+)/) : null;
    cursorPos = m ? Number(m[1]) : Number(raw) || 0;
  }
  face('G3', 'guard', { exitCode: r.code, cursorPos });
}

// --- verdicts ---
const byId = Object.fromEntries(faces.map((f) => [f.id, f]));
// DEFECT (what the broken tree does):
//   D1 accounts_partial stays 0 over a half envelope
//   D2 accounts_missing stays 0 (the status counter never counted
//      half-but-held rows — a THIRD copy of the predicate)
//   D3 the half row is DELIVERED and CONSUMED (cursor past it, exit 0)
//   D4 the accounts pass never notices the debt: 0 RPCs, the stored
//      envelope still lacks the marker — the journal can never grow
//      into a whole one
//   D5 the final row's halfness is invisible (partial stays 0)
const DEFECT = (f) =>
  f.id === 'D1' ? f.accountsPartial === 0
  : f.id === 'D2' ? f.accountsMissing === 0
  : f.id === 'D3' ? f.exitCode === 0 && f.delivered.includes(SIG_HALF) && f.cursorPos === 2
  : f.id === 'D4' ? f.rpcCalls === 0 && f.storedHasTokenState === false
  : f.id === 'D5' ? f.accountsPartial === 0 && f.accountsMissing === 0
  : false;
// FIXED (what the honest tree does):
const FIXED = (f) =>
  f.id === 'D1' ? f.accountsPartial === 1
  : f.id === 'D2' ? f.accountsMissing === 1
  : f.id === 'D3' ? f.exitCode === 3 && f.delivered.length === 1 && f.delivered[0] === SIG_WHOLE && f.cursorPos === 1 && f.namedHydrate
  : f.id === 'D4' ? f.rpcCalls === 1 && f.storedHasTokenState === true && f.exitCode === 0
  : f.id === 'D5' ? f.accountsMissing === 0 && f.accountsPartial === 1
  : false;
// GUARDS (green on BOTH trees):
const GUARD = (f) =>
  f.id === 'G1' ? f.exitCode === 0 && f.delivered.length === 1 && f.delivered[0] === SIG_WHOLE && f.cursorPos === 1
  : f.id === 'G2' ? f.exitCode === 0 && f.delivered.length === 1 && f.delivered[0] === SIG_HALF
  : f.id === 'G3' ? f.exitCode === 0 && f.cursorPos === 1
  : false;

// The four faces the BASELINE can carry. D5 is fix-side only: the defect
// tree's queue never knew the field existed, so it cannot misqueue a final
// row — what it does is keep the halfness invisible, which D1 already
// covers for the held variant.
const defectFaces = ['D1', 'D2', 'D3', 'D4'];
const allDFaces = ['D1', 'D2', 'D3', 'D4', 'D5'];
const defectVisible = defectFaces.every((id) => DEFECT(byId[id]));
const defectGone = defectFaces.every((id) => !DEFECT(byId[id]));
const fixedHolds = allDFaces.every((id) => FIXED(byId[id]));
const guardsGreen = ['G1', 'G2', 'G3'].every((id) => GUARD(byId[id]));

let verdict;
if (defectVisible && !fixedHolds) verdict = 'token-state-completeness-blind';
else if (!defectVisible && fixedHolds && guardsGreen) verdict = 'token-state-completeness-held';
else if (!defectVisible && fixedHolds && !guardsGreen) verdict = 'token-state-completeness-fixed-guard-broken';
else verdict = 'token-state-completeness-inconclusive';

const report = {
  tool: 'measure-token-state-completeness',
  measuredAt: new Date().toISOString(),
  tree: LABEL,
  binary: path.basename(BIN),
  binarySha256: crypto.createHash('sha256').update(fs.readFileSync(BIN)).digest('hex'),
  binaryVersion: execFileSync(BIN, ['--version'], { encoding: 'utf8' }).trim(),
  verdict,
  faces,
  summary: { defectVisible, defectGone, fixedHolds, guardsGreen },
};
const clean = JSON.stringify(report, null, 2).replaceAll(dir, '<tmpdir>');
if (OUT) {
  fs.mkdirSync(path.dirname(OUT), { recursive: true });
  fs.writeFileSync(OUT, clean + '\n');
}
for (const f of faces) console.log(`${f.id}  ${f.kind}: ${JSON.stringify(f)}`);
console.log(`VERDICT: ${verdict}`);
fs.rmSync(dir, { recursive: true, force: true });
process.exit(verdict === 'token-state-completeness-held' || verdict === 'token-state-completeness-blind' ? 0 : 1);
