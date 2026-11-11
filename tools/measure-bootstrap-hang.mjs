#!/usr/bin/env node
// Two-tree measurement of the bootstrap() hang defect (2026-11-11).
//
// The defect: httpGet() in the published v0.7.17 client set NO timeout and
// followed redirects with NO bound. Measured on the PUBLISHED v0.7.17 tree:
//
//   T1  an endpoint that accepts the connection and never answers —
//       `await bootstrap()` produced NO verdict (45 s watchdog fired; the
//       process was still waiting; the test runner could not even exit);
//   T2  headers arrive, the archive body stops mid-stream — same, no verdict;
//   T3  a self-referential 302 — 14,937 re-requests in 12 s and still no
//       verdict (the request count was UNBOUNDED, not merely slow).
//
// This tool runs the SAME faces against two trees — the published v0.7.17
// (git archive) and the working tree — and fails (exit 1) if the baseline
// shows no defect or if the fixed tree still shows one. It never reports
// "green" for a tree that merely failed to hang.
//
//   OUT=research/bootstrap-hang-2026-11-11.json \
//     node tools/measure-bootstrap-hang.mjs
import { execFileSync, spawn } from 'node:child_process';
import fs from 'node:fs';
import http from 'node:http';
import os from 'node:os';
import path from 'node:path';

const OUT = process.env.OUT || null;
const BASELINE_TAG = process.env.BASELINE_TAG || 'v0.7.17';
const ROOT = path.resolve(import.meta.dirname, '..');
const FACE_TIMEOUT_MS = Number(process.env.FACE_TIMEOUT_MS || 2000);
const VERDICT_WAIT_MS = Number(process.env.VERDICT_WAIT_MS || 8000);

// ---------------------------------------------------------------- servers ---

function serveStall() {
  const server = http.createServer(() => { /* accepts, never answers */ });
  return new Promise((resolve) => server.listen(0, '127.0.0.1', () => resolve({
    base: `http://127.0.0.1:${server.address().port}`,
    close: () => new Promise((r) => server.close(r)),
  })));
}

function serveBodyStall() {
  const server = http.createServer((req, res) => {
    if (req.url.includes('/releases/tags/')) {
      res.setHeader('content-type', 'application/json');
      res.end(JSON.stringify({
        tag_name: 'v0.7.17',
        assets: [{
          name: 'reeflog-linux-amd64.tar.gz',
          digest: 'sha256:0000000000000000000000000000000000000000000000000000000000000000',
          browser_download_url: `http://127.0.0.1:${server.address().port}/dl/reeflog-linux-amd64.tar.gz`,
        }],
      }));
      return;
    }
    res.writeHead(200, { 'content-type': 'application/gzip' });
    res.write(Buffer.from([0x1f, 0x8b])); // headers + first bytes arrive…
    // …then the body never completes: byte-stall face, distinct from api-stall
  });
  return new Promise((resolve) => server.listen(0, '127.0.0.1', () => resolve({
    apiBase: `http://127.0.0.1:${server.address().port}`,
    close: () => new Promise((r) => server.close(r)),
  })));
}

function serveRedirectLoop() {
  const hits = { loop: 0 };
  const server = http.createServer((req, res) => {
    const port = server.address().port;
    if (req.url.startsWith('/self')) {
      hits.loop += 1;
      res.writeHead(302, { location: `http://127.0.0.1:${port}/self` });
      res.end();
      return;
    }
    if (req.url.includes('/releases/tags/')) {
      res.setHeader('content-type', 'application/json');
      res.end(JSON.stringify({
        tag_name: 'v0.7.17',
        assets: [{
          name: 'reeflog-linux-amd64.tar.gz',
          digest: 'sha256:1111111111111111111111111111111111111111111111111111111111111111',
          browser_download_url: `http://127.0.0.1:${port}/self`,
        }],
      }));
      return;
    }
    res.statusCode = 404;
    res.end('nope');
  });
  return new Promise((resolve) => server.listen(0, '127.0.0.1', () => resolve({
    apiBase: `http://127.0.0.1:${server.address().port}`,
    hits,
    close: () => new Promise((r) => server.close(r)),
  })));
}

// ------------------------------------------------------------------ faces ---

/** Run one face in a DEDICATED child process — ASYNCHRONOUSLY. Two traps,
 *  both hit and both fixed here: (1) the baseline's unbounded redirect
 *  recursion floods the event loop of an in-process runner, so an in-process
 *  verdict timer never fires — the defect must not eat its own instrument;
 *  (2) `execFileSync` BLOCKS the parent's event loop, and the stand-in
 *  GitHub server lives in the parent — every face would time out against a
 *  deaf server and the measurement would pass for the wrong reason. spawn +
 *  promise keeps the parent's server answering while the child runs. The
 *  child prints one JSON line: the face's outcome from INSIDE. A child still
 *  alive at VERDICT_WAIT_MS is killed and reported as no-verdict — the
 *  defect, measured, not guessed. */
const FACE_RUNNER = `
import { pathToFileURL } from 'node:url';
const mod = await import(pathToFileURL(process.argv[1]).href);
const opts = JSON.parse(process.argv[2]);
const t0 = Date.now();
try {
  await mod.bootstrap({ dest: opts.__dest, version: 'v0.7.17', ...opts });
  console.log(JSON.stringify({ kind: 'resolved', ms: Date.now() - t0 }));
} catch (e) {
  console.log(JSON.stringify({ kind: 'rejected', message: String(e.message || e), ms: Date.now() - t0 }));
}
`;

async function face(clientIndex, label, makeServer, opts = {}) {
  const srv = await makeServer();
  const dest = fs.mkdtempSync(path.join(os.tmpdir(), `reeflog-hang-${label}-`));
  const started = Date.now();
  const childOpts = { dest, apiBase: srv.apiBase ?? srv.base, downloadBase: srv.downloadBase ?? null, timeoutMs: FACE_TIMEOUT_MS, ...opts, __dest: dest };
  let inner = null;
  try {
    inner = await new Promise((resolve) => {
      const child = spawn(process.execPath, [
        '--input-type=module', '-e', FACE_RUNNER, clientIndex, JSON.stringify(childOpts),
      ], { stdio: ['ignore', 'pipe', 'pipe'] });
      let out = '';
      child.stdout.on('data', (d) => { out += d; });
      const killTimer = setTimeout(() => {
        child.kill('SIGKILL');
        resolve({ kind: 'no-verdict', message: `child killed at ${VERDICT_WAIT_MS} ms with no verdict` });
      }, VERDICT_WAIT_MS);
      child.on('close', () => {
        clearTimeout(killTimer);
        try {
          resolve(JSON.parse(out.trim().split('\n').pop()));
        } catch {
          resolve({ kind: 'crashed-no-verdict', message: out.slice(0, 160) });
        }
      });
    });
  } finally {
    await srv.close();
    fs.rmSync(dest, { recursive: true, force: true });
  }
  const outcome = { label, ...inner, wallMs: Date.now() - started };
  if (srv.hits) outcome.redirectAttempts = srv.hits.loop;
  return outcome;
}

async function runFaces(clientIndex, treeName) {
  const faces = [];
  faces.push(await face(clientIndex, 'api-stall', serveStall));
  faces.push(await face(clientIndex, 'body-stall', serveBodyStall));
  faces.push(await face(clientIndex, 'redirect-loop', serveRedirectLoop));
  const hangs = faces.filter((f) => f.kind === 'no-verdict').length;
  const boundedRedirects = faces.every(
    (f) => f.label !== 'redirect-loop' || (f.redirectAttempts ?? 0) <= 10,
  );
  return {
    tree: treeName,
    faces,
    hangs,
    boundedRedirects,
    verdict: hangs === 0 ? 'every-leg-answers' : 'no-verdict',
  };
}

// ------------------------------------------------------------------ trees ---

const baselineDir = fs.mkdtempSync(path.join(os.tmpdir(), 'reeflog-baseline-'));
const tarball = execFileSync('git', ['archive', BASELINE_TAG], { cwd: ROOT, maxBuffer: 128 * 1024 * 1024 });
fs.writeFileSync(path.join(baselineDir, 'baseline.tar'), tarball);
execFileSync('tar', ['-xf', path.join(baselineDir, 'baseline.tar'), '-C', baselineDir]);
fs.rmSync(path.join(baselineDir, 'baseline.tar'));

const baseline = await runFaces(path.join(baselineDir, 'index.js'), `published ${BASELINE_TAG}`);
const fixed = await runFaces(path.join(ROOT, 'index.js'), 'working tree');

fs.rmSync(baselineDir, { recursive: true, force: true });

// The measurement FAILS when the baseline shows no defect (a green baseline
// would make every "after" number meaningless) or when the fixed tree still
// hangs.
const baselineShowsDefect = baseline.hangs > 0;
const fixedClean = fixed.hangs === 0 && fixed.boundedRedirects;
const report = {
  measuredAt: new Date().toISOString(),
  defect: 'bootstrap() had no timeout and no redirect bound: a stalling or looping endpoint left the install with no verdict',
  baseline: { ...baseline, tag: BASELINE_TAG },
  fixed,
  baselineShowsDefect,
  fixedClean,
  verdict: baselineShowsDefect && fixedClean ? 'contract-holds' : 'measurement-invalid',
};

if (OUT) {
  const outPath = path.resolve(OUT);
  fs.mkdirSync(path.dirname(outPath), { recursive: true });
  fs.writeFileSync(outPath, `${JSON.stringify(report, null, 2)}\n`);
}
console.log(JSON.stringify(report, null, 2));
process.exit(report.verdict === 'contract-holds' ? 0 : 1);
