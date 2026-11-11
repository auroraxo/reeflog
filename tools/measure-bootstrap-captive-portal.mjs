// PULS V measurement — the CAPTIVE-PORTAL face of bootstrap().
//
// The client's own comments name the captive-portal class ("a portal that
// answers every request with a login page", index.js httpGet). This harness
// asks whether that promise HOLDS on the tree under test, on the surfaces of
// ONE defect: an HTML page (or an HTML-shaped answer) where JSON is expected.
//
//   F1 api-portal    — portal HTML answers the releases/tags/<v> request;
//   F2 api-ratelimit — GitHub's own overload JSON (valid JSON, no `assets`
//                      array — what a proxy/portal/secondary API answers);
//   F3 asset-portal  — portal HTML answers the asset download;
//   F4 cached        — a completed install consults NO network (guard).
//
// Each face runs in its OWN child process (PULS-U lesson), imports the
// tree's own index.js, points bootstrap() at a stand-in server, and records:
//
//   no-verdict — neither resolved nor a named ReeflogError inside VERDICT_MS;
//   raw-crash  — rejected with a non-ReeflogError (e.g. SyntaxError from
//                JSON.parse of HTML): no verdict in the product's voice;
//   refused    — a ReeflogError (named verdict).
//
// The harness FAILS if the BASE tree shows no defect — a measurement that
// cannot distinguish the trees proves nothing.
import { spawn } from 'node:child_process';
import http from 'node:http';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import zlib from 'node:zlib';
import { createHash, randomBytes } from 'node:crypto';

const PORT = 18099 + (Number(process.env.PORT_OFFSET) || 0);
const BASE = 'http://127.0.0.1:' + PORT;
const VERDICT_MS = Number(process.env.VERDICT_MS || 12000);
const VERSION = 'v0.7.18';

const PORTAL_HTML = `<!DOCTYPE html>
<html><head><title>Just a moment...</title></head>
<body><div id="cf-chl-widget">Checking your browser before accessing api.github.com</div>
<script>setTimeout(function(){location.reload(1)},5000);</script></body></html>`;

const RATE_LIMIT_BODY = JSON.stringify({
  message: 'API rate limit exceeded for 127.0.0.1.',
  documentation_url: 'https://docs.github.com/rest/overview/resources-for-the-rest-api#rate-limiting',
});

// --- a REAL one-entry release archive: tar.gz whose only entry is `reeflog` ---
function tarEntry(name, data) {
  const header = Buffer.alloc(512);
  header.write(name, 0, 100, 'utf8');
  header.write('0000644\0', 100, 8);            // mode
  header.write('0000000\0', 108, 8);            // uid
  header.write('0000000\0', 116, 8);            // gid
  header.write(data.length.toString(8).padStart(11, '0') + '\0', 124, 12);
  header.write('00000000000\0', 136, 12);       // mtime
  header.write('        ', 148, 8);             // checksum placeholder
  header.write('0', 156, 1);                    // type: regular file
  header.write('ustar\0', 257, 6);
  header.write('00', 263, 2);
  let sum = 0;
  for (const b of header) sum += b;
  header.write(sum.toString(8).padStart(6, '0') + '\0 ', 148, 8);
  const pad = Buffer.alloc((512 - (data.length % 512)) % 512);
  const end = Buffer.alloc(1024);
  return Buffer.concat([header, data, pad, end]);
}
const fakeBinary = Buffer.from('#!/bin/sh\necho reeflog 0.7.18\n');
const assetBytes = zlib.gzipSync(tarEntry('reeflog', fakeBinary));
const assetSha = createHash('sha256').update(assetBytes).digest('hex');

const RELEASE = {
  tag_name: VERSION,
  assets: [
    {
      name: 'reeflog-linux-amd64.tar.gz',
      browser_download_url: `${BASE}/assets/reeflog-linux-amd64.tar.gz`,
      digest: `sha256:${assetSha}`,
    },
  ],
};

function makeServer(mode) {
  return http.createServer((req, res) => {
    const u = req.url || '';
    if (mode === 'api-portal' && u.startsWith('/repos/')) {
      res.writeHead(200, { 'content-type': 'text/html' });
      res.end(PORTAL_HTML);
      return;
    }
    if (mode === 'api-ratelimit' && u.startsWith('/repos/')) {
      res.writeHead(200, { 'content-type': 'application/json' });
      res.end(RATE_LIMIT_BODY);
      return;
    }
    if (mode === 'asset-portal' && u.startsWith('/assets/')) {
      res.writeHead(200, { 'content-type': 'text/html' });
      res.end(PORTAL_HTML);
      return;
    }
    if (u.startsWith('/repos/')) {
      res.writeHead(200, { 'content-type': 'application/json' });
      res.end(JSON.stringify(RELEASE));
      return;
    }
    if (u.startsWith('/assets/')) {
      res.writeHead(200, { 'content-type': 'application/x-gzip' });
      res.end(assetBytes);
      return;
    }
    res.writeHead(404);
    res.end('{}');
  });
}

// The child code is built PER FACE, after the temp dest exists: capturing
// process.env.DEST at module load yielded `undefined` and every child fell
// back to the DEFAULT ~/.reeflog — all faces then resolved from a shared
// cache without touching the stand-in server (a false all-green that would
// have certified nothing).
function childCodeFor(dest) {
  return `
import { bootstrap } from ${JSON.stringify(path.resolve(process.env.TREE, 'index.js'))};
const dest = ${JSON.stringify(dest)};
try {
  const r = await bootstrap({
    apiBase: ${JSON.stringify(BASE)},
    downloadBase: ${JSON.stringify(BASE + '/assets')},
    dest,
    timeoutMs: ${JSON.stringify(Number(process.env.CHILD_TIMEOUT_MS || 5000))},
  });
  console.log(JSON.stringify({ kind: 'resolved', cached: r.cached }));
} catch (e) {
  console.log(JSON.stringify({
    kind: 'refused',
    name: e.name,
    isReeflogError: e.name === 'ReeflogError',
    message: String(e.message || '').slice(0, 300),
  }));
}
`;
}

function oneFace(mode, tree, label, serverMode = mode, sharedDest = null) {
  return new Promise((resolve) => {
    const dest = sharedDest ?? fs.mkdtempSync(path.join(os.tmpdir(), 'reeflog-portal-face-'));
    const server = makeServer(serverMode);
    let child = null;
    let out = '';
    let done = false;
    const finish = (result) => {
      if (done) return;
      done = true;
      clearTimeout(kill);
      server.close();
      try { child?.kill('SIGKILL'); } catch { /* already gone */ }
      resolve({ mode: label, ...result });
    };
    const kill = setTimeout(() => finish({ outcome: 'no-verdict', reason: `no verdict within ${VERDICT_MS} ms` }), VERDICT_MS);
    // The server MUST be listening before the child makes its first request:
    // bootstrap() does not retry, so a not-yet-bound port would measure the
    // wrong thing (connection refused instead of the portal's answer).
    server.listen(PORT, '127.0.0.1', () => {
      child = spawn(process.execPath, ['--input-type=module', '-e', childCodeFor(dest)], {
        env: {
          ...process.env,
          TREE: tree,
          DEST: dest,
          CHILD_TIMEOUT_MS: String(Number(process.env.CHILD_TIMEOUT_MS || 5000)),
        },
        stdio: ['ignore', 'pipe', 'pipe'],
      });
      child.stdout.on('data', (d) => { out += d; });
      child.on('close', () => {
        const line = out.split('\n').find((l) => l.trim().startsWith('{'));
        if (!line) return finish({ outcome: 'no-verdict', reason: 'child exited without a verdict line' });
        try {
          const v = JSON.parse(line);
          if (v.kind === 'resolved') return finish({ outcome: 'resolved', cached: v.cached });
          if (!v.isReeflogError) return finish({ outcome: 'raw-crash', name: v.name, message: v.message });
          return finish({ outcome: 'refused', message: v.message });
        } catch (e) {
          return finish({ outcome: 'no-verdict', parseError: String(e) });
        }
      });
      child.on('error', (e) => finish({ outcome: 'no-verdict', spawnError: String(e) }));
    });
  });
}

const tree = process.env.TREE;
if (!tree) { console.error('TREE is required'); process.exit(2); }

const results = [];
results.push(await oneFace('api-portal', tree, 'F1 api-portal'));
results.push(await oneFace('api-ratelimit', tree, 'F2 api-ratelimit'));
results.push(await oneFace('asset-portal', tree, 'F3 asset-portal'));

// F4 guard: prime a healthy install into ONE dest, then let the portal take
// over and ask again against the SAME dest — the cached leg must not consult
// the network at all, so the portal cannot break a completed install.
{
  const shared = fs.mkdtempSync(path.join(os.tmpdir(), 'reeflog-portal-face-'));
  const prime = await oneFace('healthy', tree, 'F4 prime', 'healthy', shared);
  const cached = await oneFace('api-portal', tree, 'F4 cached', 'api-portal', shared);
  results.push({ ...cached, primed: prime.outcome === 'resolved', primeOutcome: prime.outcome });
}

const report = {
  measurement: 'bootstrap-captive-portal',
  tree,
  tagLabel: process.env.TAG_LABEL || null,
  version: VERSION,
  faces: results,
};
const outPath = process.env.OUT || 'portal-report.json';
fs.writeFileSync(outPath, JSON.stringify(report, null, 2));
const counts = {};
for (const r of results) counts[r.outcome] = (counts[r.outcome] || 0) + 1;
console.log(JSON.stringify({ outPath, counts, faces: results.map((r) => `${r.mode}=${r.outcome}`) }));
