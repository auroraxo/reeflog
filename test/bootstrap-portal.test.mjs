// PULS V — a captive portal (or a rate limiter, or a proxy) that answers the
// release API with 200 + the wrong body must produce a VERDICT IN THE
// PRODUCT'S VOICE, not an engine crash and not a refusal that blames the
// wrong cause.
//
// Why this exists (2026-11-11): measured against the PUBLISHED v0.7.18 tree
// (research/captive-portal-2026-11-11.json):
//
//   T1 api-portal    — 200 + HTML -> JSON.parse throws SyntaxError out of
//                      bootstrap(): a consumer catching ReeflogError never
//                      sees it, and the message names no URL, no cause and
//                      no repair (raw-crash);
//   T2 api-ratelimit — 200 + {"message":"API rate limit exceeded…"} (valid
//                      JSON, no release shape) -> refused as "release v0.7.18
//                      has no asset named …", which sends the stranger to a
//                      release page that is fine; the API's own words were
//                      discarded (misverdict);
//   G1 asset HTML    — 200 + HTML on the ASSET leg must STILL refuse on the
//                      digest (fail closed — this guard passed pre-fix);
//   G2 empty assets  — a well-formed release object that genuinely lacks
//                      THIS asset keeps its honest message;
//   G3 healthy       — a served release still installs, and a second call is
//                      served from disk (guards).
//
// The fix class is narrow: a body that is not a release object at the
// release-API leg becomes a named ReeflogError that carries what the
// endpoint actually answered.
import test from 'node:test';
import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import fs from 'node:fs';
import http from 'node:http';
import os from 'node:os';
import path from 'node:path';
import { createHash } from 'node:crypto';
import { bootstrap, ReeflogError } from '../index.js';

const VERSION = 'v9.9.9';

const PORTAL_HTML = '<!DOCTYPE html>\n<html><head><title>Just a moment...</title></head><body>Checking your browser</body></html>';
const RATE_LIMIT = JSON.stringify({
  message: 'API rate limit exceeded for 203.0.113.7.',
  documentation_url: 'https://docs.github.com/rest/overview/resources-for-the-rest-api#rate-limiting',
});

function makeArchive(script) {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'reeflog-portal-archive-'));
  const bin = path.join(dir, 'reeflog');
  fs.writeFileSync(bin, script, { mode: 0o755 });
  const tarball = path.join(dir, 'reeflog.tar.gz');
  execFileSync('tar', ['-czf', tarball, '-C', dir, 'reeflog']);
  const bytes = fs.readFileSync(tarball);
  fs.rmSync(dir, { recursive: true, force: true });
  return bytes;
}

const sha256 = (buf) => createHash('sha256').update(buf).digest('hex');

/**
 * A stand-in for the release API + the asset CDN.
 * `api` may be a string or a function of the server port (the release JSON
 * needs the port to point at the CDN).
 */
function serve({ archive, api, apiHeaders = {}, asset = null, assetHeaders = {} }) {
  const server = http.createServer((req, res) => {
    if (req.url.includes('/releases/tags/')) {
      const port = server.address().port;
      const body = typeof api === 'function' ? api(port) : api;
      res.writeHead(200, { 'content-type': 'application/json', ...apiHeaders });
      res.end(body);
      return;
    }
    if (req.url.startsWith('/dl/')) {
      res.writeHead(200, assetHeaders);
      res.end(asset ?? archive);
      return;
    }
    res.statusCode = 404;
    res.end('not found');
  });
  return new Promise((resolve) => {
    server.listen(0, '127.0.0.1', () => {
      const port = server.address().port;
      resolve({
        apiBase: `http://127.0.0.1:${port}`,
        downloadBase: `http://127.0.0.1:${port}/dl`,
        close: () => new Promise((r) => server.close(r)),
      });
    });
  });
}

const releaseJson = (archive, port) => JSON.stringify({
  tag_name: VERSION,
  assets: [{
    name: 'reeflog-linux-amd64.tar.gz',
    digest: `sha256:${sha256(archive)}`,
    browser_download_url: `http://127.0.0.1:${port}/dl/reeflog-linux-amd64.tar.gz`,
  }],
});

const tmpDest = () => fs.mkdtempSync(path.join(os.tmpdir(), 'reeflog-portal-'));

test('T1: HTML on the release-API leg is a named refusal, not a SyntaxError', async () => {
  const archive = makeArchive('#!/bin/sh\necho ok\n');
  const srv = await serve({
    archive,
    api: PORTAL_HTML,
    apiHeaders: { 'content-type': 'text/html; charset=utf-8' },
  });
  const dest = tmpDest();
  try {
    await assert.rejects(
      () => bootstrap({ version: VERSION, apiBase: srv.apiBase, downloadBase: srv.downloadBase, dest }),
      (e) => {
        assert.ok(e instanceof ReeflogError,
          `expected a ReeflogError, got ${e.name}: ${e.message}`);
        // The verdict must NAME THE CLASS — echoing the engine's own
        // "is not valid JSON" from a swallowed SyntaxError would hide a
        // captive portal behind a generic parse failure.
        assert.match(e.message, /answered with HTML/i,
          `the verdict must name the HTML-not-JSON cause: ${e.message}`);
        assert.match(e.message, /releases\/tags/,
          `the verdict must name the URL that answered: ${e.message}`);
        return true;
      },
      'portal HTML on the API leg must reject with a ReeflogError',
    );
    assert.ok(!fs.existsSync(path.join(dest, 'reeflog')), 'no binary may be left behind');
  } finally {
    await srv.close();
    fs.rmSync(dest, { recursive: true, force: true });
  }
});

test('T2: a rate-limit JSON refuses with the API\'s OWN words, not "the release has no asset"', async () => {
  const archive = makeArchive('#!/bin/sh\necho ok\n');
  const srv = await serve({ archive, api: RATE_LIMIT });
  const dest = tmpDest();
  try {
    await assert.rejects(
      () => bootstrap({ version: VERSION, apiBase: srv.apiBase, downloadBase: srv.downloadBase, dest }),
      (e) => {
        assert.ok(e instanceof ReeflogError, `expected a ReeflogError, got ${e.name}`);
        assert.match(e.message, /rate limit exceeded/i,
          `the verdict must carry what the API actually answered: ${e.message}`);
        assert.doesNotMatch(e.message, /has no asset named/,
          `the release page is not the cause; blaming it misdirects the stranger: ${e.message}`);
        return true;
      },
      'a non-release JSON body must refuse with the endpoint\'s own message',
    );
    assert.ok(!fs.existsSync(path.join(dest, 'reeflog')), 'no binary may be left behind');
  } finally {
    await srv.close();
    fs.rmSync(dest, { recursive: true, force: true });
  }
});

test('G1: HTML on the ASSET leg still refuses on the digest (fail closed)', async () => {
  const archive = makeArchive('#!/bin/sh\necho ok\n');
  const srv = await serve({
    archive,
    api: (port) => releaseJson(archive, port),
    asset: PORTAL_HTML,
    assetHeaders: { 'content-type': 'text/html' },
  });
  const dest = tmpDest();
  try {
    await assert.rejects(
      () => bootstrap({ version: VERSION, apiBase: srv.apiBase, downloadBase: srv.downloadBase, dest }),
      (e) => {
        assert.ok(e instanceof ReeflogError, `expected a ReeflogError, got ${e.name}`);
        assert.match(e.message, /sha256 mismatch|refus/i, `must refuse: ${e.message}`);
        return true;
      },
      'portal HTML on the asset leg must refuse',
    );
    assert.ok(!fs.existsSync(path.join(dest, 'reeflog')), 'no binary may be left behind');
  } finally {
    await srv.close();
    fs.rmSync(dest, { recursive: true, force: true });
  }
});

test('G2: a well-formed release that lacks THIS asset keeps its honest message', async () => {
  const archive = makeArchive('#!/bin/sh\necho ok\n');
  const srv = await serve({ archive, api: JSON.stringify({ tag_name: VERSION, assets: [] }) });
  const dest = tmpDest();
  try {
    await assert.rejects(
      () => bootstrap({ version: VERSION, apiBase: srv.apiBase, downloadBase: srv.downloadBase, dest }),
      (e) => {
        assert.ok(e instanceof ReeflogError);
        assert.match(e.message, /has no asset named/,
          'a genuine release object without the asset keeps this message');
        return true;
      },
    );
  } finally {
    await srv.close();
    fs.rmSync(dest, { recursive: true, force: true });
  }
});

test('G3: a healthy release still installs, and a second call is served from disk', async () => {
  const archive = makeArchive('#!/bin/sh\necho "reeflog 9.9.9"\n');
  const srv = await serve({ archive, api: (port) => releaseJson(archive, port) });
  const dest = tmpDest();
  try {
    const out = await bootstrap({ version: VERSION, apiBase: srv.apiBase, downloadBase: srv.downloadBase, dest });
    assert.equal(out.cached, false);
    assert.equal(out.digest, sha256(archive));
    const ran = execFileSync(out.bin, [], { encoding: 'utf8' });
    assert.match(ran, /reeflog 9\.9\.9/);
    const again = await bootstrap({ version: VERSION, apiBase: srv.apiBase, downloadBase: srv.downloadBase, dest });
    assert.equal(again.cached, true, 'the second call must not touch the network');
  } finally {
    await srv.close();
    fs.rmSync(dest, { recursive: true, force: true });
  }
});
