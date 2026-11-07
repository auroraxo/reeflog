// The cached path must not claim a verification it never performed.
//
// Why this exists (2026-11-07, measured against the PUBLISHED v0.7.0
// client from an empty app): `bootstrap()` returns from disk when
// install.json names the same version and asset — and returns
// `{ cached: true, digest: <archive sha256> }` WITHOUT ever hashing the
// binary that is actually on disk. Observed live:
//
//   first  -> { digest: "f6b36c53…", cached: false }   (archive verified)
//   tamper: append bytes to the installed binary
//   second -> { digest: "f6b36c53…", cached: true  }   <- unchanged claim
//   sha256 of the bytes on disk = 6b9fd8b7…
//
// A replaced, truncated or corrupted binary is handed back as "verified"
// with a digest that does not describe it. Two separate lies in one
// return value: the cache hit was never checked, and `digest` belongs to
// the ARCHIVE, not to `bin`.
//
// The fix, pinned here:
//   - install.json records `binarySha256` — the hash of the EXTRACTED
//     executable, next to the archive hash;
//   - a cache hit RE-HASHES the file on disk and only reports
//     `cached: true` when it matches;
//   - a sidecar without `binarySha256` (every install made before this
//     fix) is not trusted as verified: bootstrap re-fetches ONCE and
//     upgrades the sidecar, then caches again;
//   - the return value carries `binarySha256` so a caller can verify
//     `bin` itself, not just the archive it came from.
//
// C2 is the false-positive guard: an untouched install must still cache
// with ZERO network. It is the one test here expected to pass on the
// pre-fix tree.
import test from 'node:test';
import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import fs from 'node:fs';
import http from 'node:http';
import os from 'node:os';
import path from 'node:path';
import { createHash } from 'node:crypto';
import { bootstrap } from '../index.js';

const VERSION = 'v9.9.9';
const sha256 = (buf) => createHash('sha256').update(buf).digest('hex');

function makeArchive(script) {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'reeflog-cache-'));
  const bin = path.join(dir, 'reeflog');
  fs.writeFileSync(bin, script, { mode: 0o755 });
  const tarball = path.join(dir, 'reeflog.tar.gz');
  execFileSync('tar', ['-czf', tarball, '-C', dir, 'reeflog']);
  const bytes = fs.readFileSync(tarball);
  fs.rmSync(dir, { recursive: true, force: true });
  return bytes;
}

function serve({ archive, count = { n: 0 } }) {
  const server = http.createServer((req, res) => {
    if (req.url.includes('/releases/tags/')) {
      res.setHeader('content-type', 'application/json');
      res.end(JSON.stringify({
        tag_name: VERSION,
        assets: [{
          name: 'reeflog-linux-amd64.tar.gz',
          digest: `sha256:${sha256(archive)}`,
          browser_download_url: `http://127.0.0.1:${server.address().port}/dl`,
        }],
      }));
      return;
    }
    if (req.url.startsWith('/dl')) {
      count.n += 1;
      res.setHeader('content-type', 'application/gzip');
      res.end(archive);
      return;
    }
    res.statusCode = 404;
    res.end('nope');
  });
  return new Promise((resolve) => {
    server.listen(0, '127.0.0.1', () => {
      const port = server.address().port;
      resolve({
        apiBase: `http://127.0.0.1:${port}`,
        downloadBase: `http://127.0.0.1:${port}/dl`,
        count,
        close: () => new Promise((r) => server.close(r)),
      });
    });
  });
}

const sidecar = (dest) => JSON.parse(fs.readFileSync(path.join(dest, 'install.json'), 'utf8'));

test('C1: a BINARY on disk that no longer matches the sidecar is not served as verified', async () => {
  const archive = makeArchive('#!/bin/sh\necho "reeflog 9.9.9"\n');
  const srv = await serve({ archive });
  const dest = fs.mkdtempSync(path.join(os.tmpdir(), 'reeflog-cache-'));
  try {
    const first = await bootstrap({ version: VERSION, apiBase: srv.apiBase, downloadBase: srv.downloadBase, dest });
    assert.equal(first.binarySha256, sha256(fs.readFileSync(first.bin)),
      'the install must record the hash of the EXTRACTED binary');

    // Someone (corruption, a bad write, an attacker with local write)
    // changes the installed binary. The sidecar still names this version.
    fs.appendFileSync(first.bin, '\n// tampered\n');

    const downloadsBefore = srv.count.n;
    const second = await bootstrap({ version: VERSION, apiBase: srv.apiBase, downloadBase: srv.downloadBase, dest });

    // The tampered bytes must never come back as a verified cache hit.
    assert.equal(second.cached, false,
      'a cache hit on bytes that do not match the sidecar is a false verification');
    assert.ok(srv.count.n > downloadsBefore, 'it must re-fetch to re-establish trust');
    assert.equal(second.binarySha256, sha256(fs.readFileSync(second.bin)),
      'the reported binary hash must describe the bytes actually on disk');
    assert.equal(second.digest, sha256(archive), 'digest stays the archive hash (documented contract)');

    // …and the re-fetched binary is the real one.
    const ran = execFileSync(second.bin, [], { encoding: 'utf8' });
    assert.match(ran, /reeflog 9\.9\.9/, 'the re-install must restore the working binary');
    assert.equal(sidecar(dest).binarySha256, second.binarySha256);
  } finally {
    await srv.close();
    fs.rmSync(dest, { recursive: true, force: true });
  }
});

test('C2 (guard): an untouched install still caches with ZERO network', async () => {
  const archive = makeArchive('#!/bin/sh\necho "reeflog 9.9.9"\n');
  const srv = await serve({ archive });
  const dest = fs.mkdtempSync(path.join(os.tmpdir(), 'reeflog-cache-'));
  try {
    await bootstrap({ version: VERSION, apiBase: srv.apiBase, downloadBase: srv.downloadBase, dest });
    const before = srv.count.n;
    const second = await bootstrap({ version: VERSION, apiBase: srv.apiBase, downloadBase: srv.downloadBase, dest });
    assert.equal(second.cached, true, 'a clean install must still be served from disk');
    assert.equal(srv.count.n, before, 'a clean cache hit must not download');
    assert.equal(second.digest, sha256(archive));
  } finally {
    await srv.close();
    fs.rmSync(dest, { recursive: true, force: true });
  }
});

test('C3: a sidecar from BEFORE this fix (no binarySha256) is upgraded, never trusted blind', async () => {
  const archive = makeArchive('#!/bin/sh\necho "reeflog 9.9.9"\n');
  const srv = await serve({ archive });
  const dest = fs.mkdtempSync(path.join(os.tmpdir(), 'reeflog-cache-'));
  try {
    const first = await bootstrap({ version: VERSION, apiBase: srv.apiBase, downloadBase: srv.downloadBase, dest });

    // Replay the real pre-fix sidecar: version + asset + ARCHIVE hash only.
    const legacy = sidecar(dest);
    delete legacy.binarySha256;
    fs.writeFileSync(path.join(dest, 'install.json'), `${JSON.stringify(legacy, null, 2)}\n`);

    const downloadsBefore = srv.count.n;
    const second = await bootstrap({ version: VERSION, apiBase: srv.apiBase, downloadBase: srv.downloadBase, dest });
    assert.equal(second.cached, false,
      'a sidecar that records no binary hash proves nothing about the bytes on disk');
    assert.ok(srv.count.n > downloadsBefore, 'one re-fetch upgrades the sidecar');
    assert.equal(sidecar(dest).binarySha256, sha256(fs.readFileSync(first.bin)),
      'after the one-time upgrade the sidecar carries the binary hash again');

    // …and from then on it caches again (the upgrade must not disable caching).
    const downloadsAfterUpgrade = srv.count.n;
    const third = await bootstrap({ version: VERSION, apiBase: srv.apiBase, downloadBase: srv.downloadBase, dest });
    assert.equal(third.cached, true, 'the upgraded sidecar must restore the cache');
    assert.equal(srv.count.n, downloadsAfterUpgrade, 'restored cache: zero network');
  } finally {
    await srv.close();
    fs.rmSync(dest, { recursive: true, force: true });
  }
});

test('C4: a sidecar claiming a binary hash for bytes that differ is re-fetched, and the claim is replaced', async () => {
  const archive = makeArchive('#!/bin/sh\necho "reeflog 9.9.9"\n');
  const srv = await serve({ archive });
  const dest = fs.mkdtempSync(path.join(os.tmpdir(), 'reeflog-cache-'));
  try {
    const first = await bootstrap({ version: VERSION, apiBase: srv.apiBase, downloadBase: srv.downloadBase, dest });
    // Replace the binary AND leave a sidecar that claims the old hash.
    fs.writeFileSync(first.bin, '#!/bin/sh\necho impostor\n', { mode: 0o755 });
    const meta = sidecar(dest);
    assert.notEqual(meta.binarySha256, sha256(fs.readFileSync(first.bin)));

    const second = await bootstrap({ version: VERSION, apiBase: srv.apiBase, downloadBase: srv.downloadBase, dest });
    assert.equal(second.cached, false, 'a lying sidecar must not produce a cache hit');
    assert.equal(second.binarySha256, sha256(fs.readFileSync(second.bin)),
      'the claim must be re-established from the re-fetched bytes');
    const ran = execFileSync(second.bin, [], { encoding: 'utf8' });
    assert.match(ran, /reeflog 9\.9\.9/, 'the impostor must be replaced by the real binary');
  } finally {
    await srv.close();
    fs.rmSync(dest, { recursive: true, force: true });
  }
});
