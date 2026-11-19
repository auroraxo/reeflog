#!/usr/bin/env node
// measure-webhook-redirect.mjs — measure ONE redirecting webhook endpoint
// against the delivery pass (baseline binary vs fixed binary, one live
// two-sink probe).
//
// THE QUESTION: does `webhook push` deliver to the URL the config names —
// and when the endpoint answers 3xx, what does the journal and the report
// say about it?
//
// The baseline is measured against the RELEASE binary (the bytes a stranger
// installs); the fixed side against this tree's build.
//
// Faces (aligned with tests/webhook_redirect_domain.rs):
//   D1 the configured endpoint received exactly ONE request (its refusal) —
//      and the event was NOT recorded delivered (defect: delivered 1, exit 0,
//      the hop's 200 recorded as the verdict, cursor advanced past the seq)
//   D2 the redirect destination received ZERO requests, not even a bodyless
//      GET carrying the signature header (defect: 1 GET with the signature)
//   D3 the report holds the exit code and names the redirect mechanism
//      (defect: healthy report, exit 0, no mention of redirect)
//   D4 the attempt records the endpoint's OWN answer (3xx), not a 2xx
//      (defect: 200)
//   D5 the cursor stays behind the undelivered event (defect: advanced)
//
// Guards (green on BOTH sides):
//   G1 a plain 200 sink delivers normally (delivered, cursor, exit 0)
//   G2 a 404 sink is refused with the flat 4xx wording (unchanged)
//   G3 a 500 sink is transient with its own wording (unchanged)
//
//   node tools/measure-webhook-redirect.mjs --baseline <bin> --fixed <bin> [--out f.json]
import { spawnSync, spawn } from 'node:child_process';
import crypto from 'node:crypto';
import fs from 'node:fs';
import net from 'node:net';
import os from 'node:os';
import path from 'node:path';

const argv = process.argv.slice(2);
function opt(name, dflt) {
  const i = argv.indexOf(name);
  return i >= 0 && argv[i + 1] !== undefined ? argv[i + 1] : dflt;
}
const BASELINE = opt('--baseline', null);
const FIXED = opt('--fixed', null);
const OUT = opt('--out', `research/webhook-redirect-${new Date().toISOString().slice(0, 10)}.json`);
const LABEL_BASE = opt('--label-baseline', 'baseline');
const LABEL_FIXED = opt('--label-fixed', 'fixed');

const PROGRAM = 'whirLbMiicVdio4qvUfM5KAg6Ct8VwpYzGff3uctyCc';
const die = (m) => { console.error(m); process.exit(2); };
if (!BASELINE || !FIXED) die('usage: measure-webhook-redirect.mjs --baseline <bin> --fixed <bin> [--out f.json]');
// Children run with cwd = the probe's own temp dir: the binaries must be
// absolute or the spawn fails with ENOENT (measured 2026-11-19).
const BASELINE_BIN = path.resolve(BASELINE);
const FIXED_BIN = path.resolve(FIXED);

const versionOf = (bin) => (spawnSync(bin, ['--version'], { encoding: 'utf8' }).stdout || '').trim();
const sha256 = (bin) => crypto.createHash('sha256').update(fs.readFileSync(bin)).digest('hex');

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

// A recorder that answers a SCRIPTED status for every request and records
// every request it sees: line, body bytes, and which signature headers
// survived. Records BEFORE answering, so a request answered badly is still
// a fact.
function recorder(answer, location) {
  const hits = [];
  const state = { answer, location };
  const server = net.createServer((s) => {
    let acc = Buffer.alloc(0);
    s.on('data', (d) => {
      acc = Buffer.concat([acc, d]);
      const p = acc.indexOf('\r\n\r\n');
      if (p < 0) return;
      const head = acc.subarray(0, p).toString();
      const cl = Number((head.match(/content-length:\s*(\d+)/i) || [])[1] || 0);
      if (acc.length < p + 4 + cl) return;
      const body = acc.subarray(p + 4, p + 4 + cl);
      const line = head.split('\r\n')[0];
      hits.push({
        request_line: line,
        method: line.split(' ')[0],
        body_bytes: body.length,
        signature_header: /x-reeflog-signature:/i.test(head),
      });
      const code = state.answer;
      let resp;
      if (code === 200) {
        resp = 'HTTP/1.1 200 OK\r\ncontent-length: 2\r\nconnection: close\r\n\r\nok';
      } else if (code >= 300 && code < 400 && state.location) {
        resp = `HTTP/1.1 ${code} Moved\r\nlocation: ${state.location}\r\ncontent-length: 0\r\nconnection: close\r\n\r\n`;
      } else {
        resp = `HTTP/1.1 ${code} Gone\r\ncontent-length: 0\r\nconnection: close\r\n\r\n`;
      }
      s.write(resp);
      s.end();
    });
    s.on('error', () => {});
  });
  return new Promise((resolve) => {
    server.listen(0, '127.0.0.1', () => {
      resolve({
        server,
        port: server.address().port,
        url: `http://127.0.0.1:${server.address().port}/hook`,
        hits: () => hits.slice(),
        setAnswer: (c) => { state.answer = c; },
        setLocation: (l) => { state.location = l; },
        postCount: () => hits.filter((h) => h.method === 'POST').length,
      });
    });
  });
}

function seed(db) {
  const r = spawnSync('node', ['-e', `
const { DatabaseSync } = require('node:sqlite');
const d = new DatabaseSync(process.argv[1]);
const nonce = process.argv[2];
d.exec(\`INSERT INTO events(signature, slot, err, logs, source, first_seen, seal_status, payload_state)
        VALUES ('M\${nonce}_1\${'x'.repeat(40)}', 44000001, NULL, '[]', 'ws', 1700000000000, 'sealed', 'held')\`);
`, db, String(Date.now())], { encoding: 'utf8' });
  if (r.status !== 0) die(`seed failed: ${r.stderr}`);
}

// The sinks live in THIS process, so the binary must run ASYNC: a
// spawnSync here would freeze the event loop and with it the sinks, and
// every delivery would read as a timeout (measured 2026-11-19 — the first
// version of this tool did exactly that).
const run = (bin, args, cwd) =>
  new Promise((resolve) => {
    const child = spawn(bin, args, { cwd, encoding: 'utf8', timeout: 30000 });
    let stdout = '';
    let stderr = '';
    child.stdout.on('data', (c) => { stdout += c; });
    child.stderr.on('data', (c) => { stderr += c; });
    child.on('error', (e) => resolve({ status: null, stdout, stderr: String(e), error: e }));
    child.on('close', (status) => resolve({ status, stdout, stderr }));
  });

const dbOf = (db, sql) => {
  const r = spawnSync('node', ['-e', `
const { DatabaseSync } = require('node:sqlite');
const d = new DatabaseSync(process.argv[1]);
const rows = d.prepare(process.argv[2]).all();
console.log(JSON.stringify(rows));
`, db, sql], { encoding: 'utf8' });
  return JSON.parse(r.stdout || '[]');
};

const sleep2 = sleep;

async function measureSide(bin, label) {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), `reeflog-redirect-${label}-`));
  const db = path.join(dir, 'j.db');
  const r = await run(bin, ['init', '--db', db, '--program', PROGRAM], dir);
  if (r.status !== 0) die(`init failed on ${label}: ${r.stderr}`);
  seed(db);

  // One recorder per question: the redirect target is its own sink so a
  // request that follows the redirect is a fact about that sink.
  const primary = await recorder(200, null);
  const hop = await recorder(200, null);
  primary.setAnswer(302);
  hop.setAnswer(200);
  primary.setLocation(hop.url);

  const cfg = path.join(dir, 'cfg.json');
  fs.writeFileSync(cfg, JSON.stringify({ targets: [{ id: 't1', url: primary.url, secret: 's-t1', timeout_ms: 3000, dead_letter_after: 0 }] }));

  const push = await run(bin, ['webhook', 'push', '--db', db, '--config', cfg], dir);
  const status = await run(bin, ['webhook', 'status', '--db', db, '--config', cfg], dir);
  const state = dbOf(db, 'SELECT next_seq FROM webhook_state');
  const receipts = dbOf(db, 'SELECT COUNT(*) AS n FROM webhook_receipts');
  const attempts = dbOf(db, 'SELECT status, error FROM webhook_attempts ORDER BY id');

  let rep = {};
  try { rep = JSON.parse(push.stdout); } catch {}
  let srep = {};
  try { srep = JSON.parse(status.stdout); } catch {}

  primary.server.close();
  hop.server.close();

  return {
    label,
    binVersion: versionOf(bin),
    binSha256: sha256(bin),
    exitCode: push.status,
    reportDelivered: rep.targets?.[0]?.delivered ?? null,
    reportError: rep.targets?.[0]?.error ?? null,
    reportBlocked: rep.targets?.[0]?.blocked ?? null,
    statusLastError: srep.targets?.[0]?.last_error ?? null,
    cursor: state[0]?.next_seq ?? null,
    receipts: receipts[0]?.n ?? null,
    attemptStatus: attempts[0]?.status ?? null,
    attemptError: attempts[0]?.error ?? null,
    primaryHits: primary.hits(),
    hopHits: hop.hits(),
  };
}

// ---- G2/G3 are measured once per side on their own sinks, same probe. ----
async function measureGuards(bin, label) {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), `reeflog-redirect-guard-${label}-`));
  const db = path.join(dir, 'j.db');
  await run(bin, ['init', '--db', db, '--program', PROGRAM], dir);
  seed(db);

  const sink404 = await recorder(404, null);
  const sink500 = await recorder(500, null);
  const sink200 = await recorder(200, null);

  const mk = async (name, sink) => {
    const cfg = path.join(dir, `${name}.json`);
    fs.writeFileSync(cfg, JSON.stringify({ targets: [{ id: `t-${name}`, url: sink.url, secret: `s-${name}`, timeout_ms: 3000, dead_letter_after: 0 }] }));
    return cfg;
  };
  const cfg200 = await mk('g200', sink200);
  const cfg404 = await mk('g404', sink404);
  const cfg500 = await mk('g500', sink500);

  const p200 = await run(bin, ['webhook', 'push', '--db', db, '--config', cfg200], dir);
  const p404 = await run(bin, ['webhook', 'push', '--db', db, '--config', cfg404], dir);
  const p500 = await run(bin, ['webhook', 'push', '--db', db, '--config', cfg500], dir);
  const err = (p) => { try { return JSON.parse(p.stdout).targets?.[0]?.error ?? null; } catch { return null; } };

  sink200.server.close();
  sink404.server.close();
  sink500.server.close();

  return {
    label,
    guard200: { exit: p200.status, error: err(p200) },
    guard404: { exit: p404.status, error: err(p404) },
    guard500: { exit: p500.status, error: err(p500) },
  };
}

const base = await measureSide(BASELINE_BIN, LABEL_BASE);
const fixed = await measureSide(FIXED_BIN, LABEL_FIXED);
const baseGuard = await measureGuards(BASELINE_BIN, LABEL_BASE);
const fixedGuard = await measureGuards(FIXED_BIN, LABEL_FIXED);

const faces = [];
// Every face is a PREDICATE over one side's measurement. Recording both
// sides separately keeps the report honest: the baseline must FAIL the
// face (that is the defect) and the fixed side must PASS it (that is the
// fix) — one pass/fail field per side, no verdict by assertion.
const face = (name, basePred, fixedPred, detail) => {
  const b = !!basePred;
  const f = !!fixedPred;
  faces.push({ name, baseline: b, fixed: f, detail: detail ?? null });
};

// D1: the configured endpoint received exactly ONE request, and the event
// was NOT recorded delivered.
face('D1 the configured endpoint received ONE request and the event was not recorded delivered',
  base.primaryHits.length === 1 && base.reportDelivered === 0,
  fixed.primaryHits.length === 1 && fixed.reportDelivered === 0,
  { base: { primaryHits: base.primaryHits.length, reportDelivered: base.reportDelivered, exit: base.exitCode },
    fixed: { primaryHits: fixed.primaryHits.length, reportDelivered: fixed.reportDelivered, exit: fixed.exitCode } });

// D2: the redirect destination received nothing.
face('D2 the redirect destination received ZERO requests',
  base.hopHits.length === 0,
  fixed.hopHits.length === 0,
  { base: base.hopHits, fixed: fixed.hopHits });

// D3: the report holds the exit code and names the redirect mechanism.
face('D3 the report holds the exit code and names the redirect mechanism',
  base.exitCode === 3 && (base.reportError || '').includes('redirect'),
  fixed.exitCode === 3 && (fixed.reportError || '').includes('redirect'),
  { base: { exit: base.exitCode, reportError: base.reportError, statusLastError: base.statusLastError },
    fixed: { exit: fixed.exitCode, reportError: fixed.reportError, statusLastError: fixed.statusLastError } });

// D4: the attempt records the endpoint's own answer.
face('D4 the attempt records the endpoint answer (3xx), not a 2xx',
  base.attemptStatus === 302,
  fixed.attemptStatus === 302,
  { base: { attemptStatus: base.attemptStatus }, fixed: { attemptStatus: fixed.attemptStatus } });

// D5: the cursor stays behind the event.
face('D5 the cursor stays behind the undelivered event',
  base.cursor === 0,
  fixed.cursor === 0,
  { base: { cursor: base.cursor }, fixed: { cursor: fixed.cursor } });

// Guards: the normal cases must be IDENTICAL on both sides (the fix must
// not move them), and each must hold the pre-existing wording.
face('G1 a plain 200 sink delivers normally on both sides',
  baseGuard.guard200.exit === 0,
  fixedGuard.guard200.exit === 0,
  { base: baseGuard.guard200, fixed: fixedGuard.guard200 });
face('G2 a 404 sink is refused identically on both sides (unchanged wording)',
  baseGuard.guard404.exit === 3 && baseGuard.guard404.error === 'refused with http 404',
  fixedGuard.guard404.exit === 3 && fixedGuard.guard404.error === 'refused with http 404',
  { base: baseGuard.guard404, fixed: fixedGuard.guard404 });
face('G3 a 500 sink is transient identically on both sides (unchanged wording)',
  baseGuard.guard500.exit === 3 && baseGuard.guard500.error === 'http 500',
  fixedGuard.guard500.exit === 3 && fixedGuard.guard500.error === 'http 500',
  { base: baseGuard.guard500, fixed: fixedGuard.guard500 });

const defectFaces = faces.filter((f) => !f.baseline);
const fixedClean = faces.filter((f) => !f.fixed);
const verdict =
  defectFaces.length === 5 && fixedClean.length === 0
    ? 'webhook-redirect-refused-at-the-configured-endpoint'
    : 'webhook-redirect-followed-and-recorded-delivered';

const report = {
  verdict,
  baselineDefectFaces: defectFaces.length,
  fixedDefectFaces: fixedClean.length,
  baseline: base,
  fixed,
  baselineGuard: baseGuard,
  fixedGuard: fixedGuard,
  faces,
  at: new Date().toISOString(),
  dir: '<tmp>/',
};
fs.mkdirSync(path.dirname(OUT), { recursive: true });
fs.writeFileSync(OUT, JSON.stringify(report, null, 2));
for (const f of faces) {
  const tag = f.baseline && f.fixed ? 'BOTH-HOLD' : f.baseline ? 'BASELINE-ONLY' : f.fixed ? 'FIXED-ONLY' : 'NEITHER';
  console.log(`${tag} ${f.name}${f.detail !== null && f.detail !== undefined ? ` — ${JSON.stringify(f.detail)}` : ''}`);
}
console.log(`verdict ${verdict} (baseline ${defectFaces.length} defect faces, fixed ${fixedClean.length}) -> ${OUT}`);
process.exit(0);
