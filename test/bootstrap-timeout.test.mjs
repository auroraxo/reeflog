// bootstrap() must always answer: every download gets a verdict, not silence.
//
// Why this exists (measured 2026-11-11 on the PUBLISHED v0.7.17 tree,
// research/bootstrap-hang-published-v0717-2026-11-11.json): httpGet() set no
// timeout and followed redirects with no bound. Against a server that accepts
// the connection and never answers, `await bootstrap()` hung with NO verdict
// (watchdog at 45 s fired; the process was still waiting). Against a 302 whose
// `location` points back at itself, it re-requested 14,937 times in 12 s and
// still had no verdict. The README's stranger path is
// `npm install github:auroraxo/reeflog#vX && bootstrap()` — a stranger whose
// network path hiccups through a captive portal or a broken CDN mirror gets a
// silently frozen install, and the same no-answer class the daemon-side
// webhook timeout (MAX_TIMEOUT_MS, src/webhook.rs) exists to refuse was
// re-opened on the client side.
//
//   T1 connect-then-stall on the release API — bootstrap REJECTS with a named
//      timeout inside the budget, nothing is written;
//   T2 headers arrive, the archive body stops mid-stream — the refusal names
//      the BYTE stall specifically ("no bytes for …"), not a headers stall;
//   T3 a permanent redirect loop — bootstrap REJECTS naming the redirect
//      bound, with a BOUNDED number of requests (not unbounded);
//   T4 the DEFAULT budget (no timeoutMs passed, the stranger's real path) is
//      finite — silence must never need an option to become an answer;
//   T5 after the verdict the socket is DESTROYED — a stalled request must not
//      keep the install process alive past its own answer;
//   G1 (guard) a healthy API + archive still installs — passes on BOTH trees;
//   G2 (guard) a finite redirect chain (2 hops, then the answer) still
//      installs — redirects stay legal, only loops are refused;
//   G3 (guard) a slow-but-steady trickle under the inactivity bound installs
//      — the fix must not over-tighten into rejecting real downloads.
import test from 'node:test';
import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import fs from 'node:fs';
import http from 'node:http';
import os from 'node:os';
import path from 'node:path';
import { createHash } from 'node:crypto';
import { bootstrap, ReeflogError, DEFAULT_GET_TIMEOUT_MS, MAX_REDIRECTS } from '../index.js';

const VERSION = 'v9.9.9';

function makeArchive(script) {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'reeflog-to-archive-'));
  const bin = path.join(dir, 'reeflog');
  fs.writeFileSync(bin, script, { mode: 0o755 });
  const tarball = path.join(dir, 'reeflog.tar.gz');
  execFileSync('tar', ['-czf', tarball, '-C', dir, 'reeflog']);
  const bytes = fs.readFileSync(tarball);
  fs.rmSync(dir, { recursive: true, force: true });
  return bytes;
}

const sha256 = (buf) => createHash('sha256').update(buf).digest('hex');

function listen(server) {
  return new Promise((resolve) => {
    server.listen(0, '127.0.0.1', () => resolve(server));
  });
}
const close = (server) => new Promise((r) => server.close(r));

// A server that accepts the TCP connection and then says NOTHING — not
// headers, not body, not reset. This is a captive portal / half-open proxy.
async function serveStall() {
  const server = http.createServer(() => { /* never answer */ });
  await listen(server);
  return { base: `http://127.0.0.1:${server.address().port}`, close: () => close(server) };
}

// A release API that answers, and a /dl/ that sends the gzip HEADER and then
// stalls before any body byte.
async function serveBodyStall() {
  const server = http.createServer((req, res) => {
    if (req.url.includes('/releases/tags/')) {
      res.setHeader('content-type', 'application/json');
      res.end(JSON.stringify({
        tag_name: VERSION,
        assets: [{
          name: 'reeflog-linux-amd64.tar.gz',
          digest: 'sha256:0000000000000000000000000000000000000000000000000000000000000000',
          browser_download_url: `http://127.0.0.1:${server.address().port}/dl/reeflog-linux-amd64.tar.gz`,
        }],
      }));
      return;
    }
    // headers arrive, body never does. writeHead alone does NOT flush to the
    // socket — flushHeaders() makes this face a true byte-stall instead of
    // silently degrading into the headers-stall of T1.
    res.writeHead(200, { 'content-type': 'application/gzip' });
    res.flushHeaders();
    res.write(Buffer.from([0x1f, 0x8b])); // first gzip bytes, then silence
  });
  await listen(server);
  return {
    apiBase: `http://127.0.0.1:${server.address().port}`,
    downloadBase: `http://127.0.0.1:${server.address().port}/dl`,
    close: () => close(server),
  };
}

// A /self that always 302s back to itself, mounted in front of a working
// API+download so the ONLY failing leg is the loop.
async function serveRedirectLoop() {
  const hits = { loop: 0 };
  const archive = makeArchive('#!/bin/sh\necho "reeflog 9.9.9"\n');
  const digest = `sha256:${sha256(archive)}`;
  const server = http.createServer((req, res) => {
    const port = server.address().port;
    if (req.url.startsWith('/self')) {
      hits.loop += 1;
      res.writeHead(302, { location: `http://127.0.0.1:${port}/self` });
      res.end();
      return;
    }
    if (req.url.includes('/releases/tags/')) {
      // Point the release entry at the loop — this is what a tampering or
      // misconfigured mirror looks like from the client's seat.
      res.setHeader('content-type', 'application/json');
      res.end(JSON.stringify({
        tag_name: VERSION,
        assets: [{
          name: 'reeflog-linux-amd64.tar.gz',
          digest,
          browser_download_url: `http://127.0.0.1:${port}/self`,
        }],
      }));
      return;
    }
    res.statusCode = 404;
    res.end('nope');
  });
  await listen(server);
  return {
    apiBase: `http://127.0.0.1:${portOf(server)}`,
    downloadBase: null,
    hits,
    close: () => close(server),
  };
}
const portOf = (server) => server.address().port;

const TEST_TIMEOUT_MS = 1500;
const watchdog = (label, ms = 8000) => {
  const t = setTimeout(() => {
    assert.fail(`${label}: bootstrap() still had NO verdict after ${ms} ms — silence is not an answer`);
  }, ms);
  t.unref?.();
  return () => clearTimeout(t);
};

test('T1: an endpoint that accepts and never answers is a NAMED timeout, not a hang', async () => {
  const srv = await serveStall();
  const dest = fs.mkdtempSync(path.join(os.tmpdir(), 'reeflog-t1-'));
  const cancel = watchdog('T1');
  try {
    await assert.rejects(
      bootstrap({ version: VERSION, apiBase: srv.base, dest, timeoutMs: TEST_TIMEOUT_MS }),
      (e) => {
        assert.ok(e instanceof ReeflogError, 'must be the client error type');
        assert.match(e.message, /timeout|no answer|stall/i, `refusal must name the timeout: ${e.message}`);
        return true;
      },
    );
    assert.ok(!fs.existsSync(path.join(dest, 'reeflog')), 'a timed-out install writes no binary');
  } finally {
    cancel();
    await srv.close();
    fs.rmSync(dest, { recursive: true, force: true });
  }
});

test('T2: headers then a dead body is a NAMED timeout too', async () => {
  const srv = await serveBodyStall();
  const dest = fs.mkdtempSync(path.join(os.tmpdir(), 'reeflog-t2-'));
  const cancel = watchdog('T2');
  try {
    await assert.rejects(
      bootstrap({ version: VERSION, apiBase: srv.apiBase, dest, timeoutMs: TEST_TIMEOUT_MS }),
      (e) => {
        assert.ok(e instanceof ReeflogError);
        // The face is headers-then-dead-body: a correct fix must name the
        // BYTE stall ("no bytes for …"), not misreport it as a headers stall.
        assert.match(e.message, /no bytes for/, `refusal must name the byte stall: ${e.message}`);
        return true;
      },
    );
    assert.ok(!fs.existsSync(path.join(dest, 'reeflog')));
  } finally {
    cancel();
    await srv.close();
    fs.rmSync(dest, { recursive: true, force: true });
  }
});

test('T3: a redirect loop is refused by name after a BOUNDED number of requests', async () => {
  const srv = await serveRedirectLoop();
  const dest = fs.mkdtempSync(path.join(os.tmpdir(), 'reeflog-t3-'));
  const cancel = watchdog('T3');
  try {
    await assert.rejects(
      bootstrap({ version: VERSION, apiBase: srv.apiBase, dest, timeoutMs: TEST_TIMEOUT_MS }),
      (e) => {
        assert.ok(e instanceof ReeflogError);
        assert.match(e.message, /redirect/i, `refusal must name redirects: ${e.message}`);
        return true;
      },
    );
    // bounded: the loop leg must be attempted a small fixed number of times,
    // never "as long as the process lives".
    assert.ok(srv.hits.loop <= 10, `redirect loop attempted ${srv.hits.loop} times — must be bounded (<= 10)`);
  } finally {
    cancel();
    await srv.close();
    fs.rmSync(dest, { recursive: true, force: true });
  }
});

test('T4: the DEFAULT budget is finite — a stranger without options still gets a verdict', async () => {
  // Contract pin: the default budget is exported and finite. This is what a
  // stranger's `await bootstrap()` runs on; if it is gone, zero, or Infinity
  // the no-options path is silent again, and that is a RED before any
  // behavioral face has to burn the whole budget proving it.
  assert.equal(typeof DEFAULT_GET_TIMEOUT_MS, 'number');
  assert.ok(Number.isFinite(DEFAULT_GET_TIMEOUT_MS) && DEFAULT_GET_TIMEOUT_MS > 0,
    `default budget must be finite and > 0, got ${DEFAULT_GET_TIMEOUT_MS}`);
  assert.ok(DEFAULT_GET_TIMEOUT_MS >= 1000 && DEFAULT_GET_TIMEOUT_MS <= 120000,
    `default budget must stay in the usable band (1s..120s), got ${DEFAULT_GET_TIMEOUT_MS}`);
  assert.equal(typeof MAX_REDIRECTS, 'number');
  assert.ok(Number.isInteger(MAX_REDIRECTS) && MAX_REDIRECTS >= 1 && MAX_REDIRECTS <= 10,
    `redirect bound must be a small finite hop count, got ${MAX_REDIRECTS}`);

  const srv = await serveStall();
  const dest = fs.mkdtempSync(path.join(os.tmpdir(), 'reeflog-t4-'));
  // The default budget is 30 s: give the watchdog room to prove a hang
  // instead of racing the fix.
  const cancel = watchdog('T4', 35000);
  try {
    await assert.rejects(
      // NO timeoutMs: this is exactly the documented stranger path,
      // `bootstrap()` with no options at all.
      bootstrap({ version: VERSION, apiBase: srv.base, dest }),
      (e) => {
        assert.ok(e instanceof ReeflogError);
        assert.match(e.message, /timeout|no answer|stall/i);
        return true;
      },
    );
    assert.ok(!fs.existsSync(path.join(dest, 'reeflog')));
  } finally {
    cancel();
    await srv.close();
    fs.rmSync(dest, { recursive: true, force: true });
  }
});

test('T5: the verdict destroys the socket — no request outlives its own answer', async () => {
  // Contract pin on the DESTROY, so the socket-leak mutation is caught by a
  // fast source check before the behavioral face has to run: the verdict
  // paths must destroy their socket, not abandon it.
  const src = fs.readFileSync(new URL('../index.js', import.meta.url), 'utf8');
  assert.match(src, /req\.setTimeout\(0\);/,
    'the headers-phase idle timer must be disarmed once headers arrive, or it races the byte timer');
  assert.match(src, /res\.destroy\(\);/,
    'the byte-stall verdict must destroy the response socket');
  assert.match(src, /req\.destroy\(\);/,
    'the headers-stall verdict must destroy the request socket');

  const srv = await serveStall();
  const srvPort = Number(new URL(srv.base).port);
  const dest = fs.mkdtempSync(path.join(os.tmpdir(), 'reeflog-t5-'));
  const cancel = watchdog('T5', 35000);
  const net = await import('node:net');
  try {
    await assert.rejects(
      bootstrap({ version: VERSION, apiBase: srv.base, dest }),
      /timeout|no answer|stall/i,
    );
    // The refusal alone is not the contract: an abandoned-but-open socket
    // pins the event loop, so an install script would never exit even with
    // the verdict in hand. The stalled connection must be GONE.
    const leaked = await new Promise((resolve) => {
      const t = setTimeout(() => {
        clearInterval(poll);
        resolve(process._getActiveHandles?.().some(
          (h) => h instanceof net.Socket && !h.destroyed && h.remotePort === srvPort,
        ) ?? false);
      }, 2500);
      t.unref?.();
      const poll = setInterval(() => { /* keep the loop observable */ }, 250);
      poll.unref?.();
    });
    assert.ok(!leaked, 'a stalled socket survived the verdict — the install process would never exit');
  } finally {
    cancel();
    await srv.close();
    fs.rmSync(dest, { recursive: true, force: true });
  }
});

// --- guards: these are expected to pass on the PRE-FIX tree too; they are
// the false-positive protection against an over-tight fix.

async function serveHealthy({ trickleMs = 0 } = {}) {
  const archive = makeArchive('#!/bin/sh\necho "reeflog 9.9.9"\n');
  const digest = `sha256:${sha256(archive)}`;
  const server = http.createServer((req, res) => {
    const port = server.address().port;
    if (req.url.startsWith('/api2/')) {
      res.setHeader('content-type', 'application/json');
      res.end(JSON.stringify({
        tag_name: VERSION,
        assets: [{
          name: 'reeflog-linux-amd64.tar.gz',
          digest,
          browser_download_url: `http://127.0.0.1:${port}/drip/reeflog-linux-amd64.tar.gz`,
        }],
      }));
      return;
    }
    if (req.url.includes('/releases/tags/')) {
      if (trickleMs) {
        // one hop through a redirect, THEN the answer — G2 exercises the
        // redirect path itself, not just its bound.
        res.writeHead(302, { location: `http://127.0.0.1:${port}/api2/releases/tags/${VERSION}` });
        res.end();
        return;
      }
      res.setHeader('content-type', 'application/json');
      res.end(JSON.stringify({
        tag_name: VERSION,
        assets: [{
          name: 'reeflog-linux-amd64.tar.gz',
          digest,
          browser_download_url: `http://127.0.0.1:${port}/dl/reeflog-linux-amd64.tar.gz`,
        }],
      }));
      return;
    }
    if (req.url.startsWith('/dl/')) {
      res.setHeader('content-type', 'application/gzip');
      res.end(archive);
      return;
    }
    if (req.url.startsWith('/drip/') && trickleMs) {
      // G3: a slow connection that MAKES PROGRESS — 4 chunks, trickleMs apart.
      res.writeHead(200, { 'content-type': 'application/gzip' });
      const size = Math.ceil(archive.length / 4);
      let i = 0;
      const timer = setInterval(() => {
        const piece = archive.subarray(i * size, (i + 1) * size);
        if (piece.length) res.write(piece);
        i += 1;
        if (i >= 4) { clearInterval(timer); res.end(); }
      }, trickleMs);
      return;
    }
    res.statusCode = 404;
    res.end('nope');
  });
  await listen(server);
  return {
    apiBase: `http://127.0.0.1:${portOf(server)}`,
    archive,
    close: () => close(server),
  };
}

test('G1: a healthy release API installs exactly as before (no over-tightening)', async () => {
  const srv = await serveHealthy();
  const dest = fs.mkdtempSync(path.join(os.tmpdir(), 'reeflog-g1-'));
  try {
    const out = await bootstrap({ version: VERSION, apiBase: srv.apiBase, dest, timeoutMs: 4000 });
    assert.equal(out.cached, false);
    assert.ok(fs.existsSync(out.bin));
    const ran = execFileSync(out.bin, [], { encoding: 'utf8' });
    assert.match(ran, /reeflog 9\.9\.9/);
  } finally {
    await srv.close();
    fs.rmSync(dest, { recursive: true, force: true });
  }
});

test('G3: a slow-but-steady trickle still installs — progress defers the verdict', async () => {
  // Chunks every 1000 ms with a 1500 ms budget: a naive TOTAL deadline would
  // kill this ~4 s download; only BETWEEN-BYTES inactivity may judge it.
  const srv = await serveHealthy({ trickleMs: 1000 });
  const dest = fs.mkdtempSync(path.join(os.tmpdir(), 'reeflog-g3-'));
  try {
    const out = await bootstrap({ version: VERSION, apiBase: srv.apiBase, dest, timeoutMs: 1500 });
    assert.ok(fs.existsSync(out.bin), 'a download that keeps making progress must complete');
    assert.equal(out.digest, sha256(srv.archive));
  } finally {
    await srv.close();
    fs.rmSync(dest, { recursive: true, force: true });
  }
});

test('G2: a finite redirect chain (2 hops, then the answer) still installs', async () => {
  const srv = await serveHealthy({ trickleMs: 60 });
  const dest = fs.mkdtempSync(path.join(os.tmpdir(), 'reeflog-g2-'));
  try {
    const out = await bootstrap({ version: VERSION, apiBase: srv.apiBase, dest, timeoutMs: 4000 });
    assert.ok(fs.existsSync(out.bin), 'redirected answers are still answers');
    assert.equal(out.digest, sha256(srv.archive));
  } finally {
    await srv.close();
    fs.rmSync(dest, { recursive: true, force: true });
  }
});
