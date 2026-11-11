#!/usr/bin/env node
// The client pin lag (2026-11-11) — MEASURED, not inferred.
//
// The defect: release commit 6b51d2b (v0.7.16) moved Cargo.toml, Cargo.lock
// and the README install pins, and forgot package.json. The published tag
// v0.7.16 therefore shipped `"version": "0.7.15"` in the npm manifest.
//
// Why that matters beyond cosmetics: `bootstrap()` derives the release tag it
// asks the GitHub release API for from package.json (that coupling is B8).
// So a stranger who follows the README — `npm install
// github:auroraxo/reeflog#v0.7.16` — installed a client that asked for
// v0.7.15 and downloaded the v0.7.15 archive: the binary that STILL carries
// the shared-secret defect v0.7.16 fixed (one secret, two endpoints). The
// tag the README pins and the binary a stranger got were different releases.
//
// What this tool measures, on BOTH trees:
//   D1 the tag the client REQUESTED from the release API (observed: a local
//      stand-in for api.github.com records the request path);
//   D2 the crate version of the tree (Cargo.toml);
//   D3 the manifest version of the tree (package.json);
//   D4 the verdict: requested tag == crate tag?
//
// The tool FAILS (exit 1) when the baseline tree shows no defect — a
// measurement that cannot see the defect proves nothing.
//
//   node tools/measure-client-pin-lag.mjs
//   BASELINE_TAG=v0.7.16 OUT=research/x.json node tools/measure-client-pin-lag.mjs
import { execFileSync } from 'node:child_process';
import fs from 'node:fs';
import http from 'node:http';
import os from 'node:os';
import path from 'node:path';
import { createHash } from 'node:crypto';

const BASELINE_TAG = process.env.BASELINE_TAG || 'v0.7.16';
const OUT = process.env.OUT || null;

/** Build a real .tar.gz carrying an executable named `reeflog`, the way the
 *  release workflow does (`tar -czf … -C dir reeflog`). */
function makeArchive() {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'reeflog-pinlag-archive-'));
  const bin = path.join(dir, 'reeflog');
  fs.writeFileSync(bin, '#!/bin/sh\necho reeflog\n', { mode: 0o755 });
  const tarball = path.join(dir, 'reeflog.tar.gz');
  execFileSync('tar', ['-czf', tarball, '-C', dir, 'reeflog']);
  const bytes = fs.readFileSync(tarball);
  fs.rmSync(dir, { recursive: true, force: true });
  return bytes;
}

/** A stand-in for the GitHub release API + the asset CDN. It records which
 *  release tag the client asks for — the observation this measurement is
 *  built on. */
function serve(archive) {
  const seen = [];
  const digest = `sha256:${createHash('sha256').update(archive).digest('hex')}`;
  const server = http.createServer((req, res) => {
    if (req.url.includes('/releases/tags/')) {
      seen.push(req.url);
      res.setHeader('content-type', 'application/json');
      res.end(JSON.stringify({
        tag_name: 'v0.0.0',
        assets: [{
          name: 'reeflog-linux-amd64.tar.gz',
          digest,
          browser_download_url: `http://127.0.0.1:${server.address().port}/dl/reeflog-linux-amd64.tar.gz`,
        }],
      }));
      return;
    }
    if (req.url.startsWith('/dl/')) {
      res.setHeader('content-type', 'application/gzip');
      res.end(archive);
      return;
    }
    res.statusCode = 404;
    res.end('not found');
  });
  return new Promise((resolve) => {
    server.listen(0, '127.0.0.1', () => resolve({
      apiBase: `http://127.0.0.1:${server.address().port}`,
      downloadBase: `http://127.0.0.1:${server.address().port}/dl`,
      seen,
      close: () => new Promise((r) => server.close(r)),
    }));
  });
}

/** The tree's own numbers, read from the tree — not from this repo. */
function readTree(root) {
  const pkg = JSON.parse(fs.readFileSync(path.join(root, 'package.json'), 'utf8'));
  const cargo = fs.readFileSync(path.join(root, 'Cargo.toml'), 'utf8');
  const crate = /^version\s*=\s*"([^"]+)"/m.exec(cargo)?.[1] ?? null;
  return { manifest: pkg.version, crate };
}

/** Run the tree's own client against the stand-in and record the release tag
 *  it asked for. This is the OBSERVED behaviour — not a re-implementation
 *  of what the client ought to do. */
async function observeRequestedTag(root, archive) {
  const srv = await serve(archive);
  const dest = fs.mkdtempSync(path.join(os.tmpdir(), 'reeflog-pinlag-dest-'));
  let requested = null;
  let error = null;
  try {
    const mod = await import(path.join(root, 'index.js'));
    await mod.bootstrap({ apiBase: srv.apiBase, downloadBase: srv.downloadBase, dest });
    const hit = srv.seen.find((u) => u.includes('/releases/tags/'));
    requested = hit ? decodeURIComponent(hit.split('/releases/tags/')[1].split('?')[0]) : null;
  } catch (err) {
    error = String(err && err.message ? err.message : err);
  } finally {
    await srv.close();
    fs.rmSync(dest, { recursive: true, force: true });
  }
  return { requested, error };
}

/** Unpack a git tag into a scratch directory (the PUBLISHED tree). */
function unpackTag(tag) {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), `reeflog-${tag}-`));
  const archive = execFileSync('git', ['archive', tag], { cwd: repoRoot(), maxBuffer: 64 * 1024 * 1024 });
  execFileSync('tar', ['-x', '-C', dir], { input: archive });
  return dir;
}

function repoRoot() {
  return path.resolve(import.meta.dirname, '..');
}

const archive = makeArchive();
const baselineDir = unpackTag(BASELINE_TAG);
const fixedDir = repoRoot();

const baselineTree = readTree(baselineDir);
const fixedTree = readTree(fixedDir);
const baselineObserved = await observeRequestedTag(baselineDir, archive);
const fixedObserved = await observeRequestedTag(fixedDir, archive);

const face = (tree, observed) => ({
  manifest: tree.manifest,
  crate: tree.crate,
  requestedTag: observed.requested,
  error: observed.error,
  // The defect face: the client asks for a release that is NOT the release
  // this tree is. The stranger gets the old binary.
  servesThePinnedRelease: observed.requested === `v${tree.crate}`,
});
const baseline = face(baselineTree, baselineObserved);
const fixed = face(fixedTree, fixedObserved);

const report = {
  measuredAt: new Date().toISOString().slice(0, 10),
  source: `published ${BASELINE_TAG} release tree (git archive ${BASELINE_TAG}) vs this tree`,
  defect: `published ${BASELINE_TAG}: the npm manifest named ${baseline.manifest} while the crate named `
    + `${baseline.crate}; bootstrap() asked the release API for ${baseline.requestedTag}, so a stranger who `
    + `installed the pinned tag got the previous binary — one release behind the one the README names`,
  baseline,
  fixed,
  verdict: baseline.servesThePinnedRelease ? 'NO-DEFECT-ON-BASELINE' : 'defect-present-on-baseline',
  baselineServesPinnedRelease: baseline.servesThePinnedRelease,
  fixedServesPinnedRelease: fixed.servesThePinnedRelease,
};

fs.rmSync(baselineDir, { recursive: true, force: true });

if (OUT) {
  fs.mkdirSync(path.dirname(path.resolve(OUT)), { recursive: true });
  fs.writeFileSync(path.resolve(OUT), `${JSON.stringify(report, null, 2)}\n`);
}

console.log(JSON.stringify(report, null, 2));

// A measurement that cannot see the defect proves nothing: if the baseline
// tree served the pinned release anyway, this tool has to fail.
if (baseline.servesThePinnedRelease) {
  console.error('NO VERDICT: the baseline tree serves the pinned release — this measurement cannot see the defect');
  process.exit(1);
}
if (!fixed.servesThePinnedRelease) {
  console.error('NO VERDICT: this tree still does not serve its own pinned release');
  process.exit(1);
}
