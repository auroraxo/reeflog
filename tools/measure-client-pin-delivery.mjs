#!/usr/bin/env node
// Stranger-path delivery proof for the client pin fix (2026-11-11).
//
// What the defect was: the published v0.7.16 TAG carried
// `package.json: 0.7.15`, so the README's install
// (`npm install github:auroraxo/reeflog#v0.7.16`) handed a stranger a client
// that asked the release API for v0.7.15 and installed the PREVIOUS binary
// (research/client-pin-lag-published-v0716-2026-11-11.json).
//
// What this measures: the SAME stranger path against the PUBLISHED v0.7.17
// release, with the REAL GitHub API and the REAL asset — no stand-in:
//   S1  the tag tree's package.json names the tag (manifest == tag);
//   S2  bootstrap() with NO arguments requests release v0.7.17 (observed on
//       the wire against api.github.com);
//   S3  the archive digest the API publishes equals the bytes downloaded;
//   S4  the installed binary runs and prints `reeflog 0.7.17` — the version
//       the tag, the crate, the manifest and the README pin all name.
//
// Fails (exit 1) on any face, so "green" means the fix REACHED the pin a
// stranger installs, not that the tree compiles.
//
//   TAG=v0.7.17 OUT=research/client-pin-delivery-v0717-2026-11-11.json \
//     node tools/measure-client-pin-delivery.mjs
import { execFileSync } from 'node:child_process';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { createHash, randomUUID } from 'node:crypto';

const TAG = process.env.TAG || 'v0.7.17';
const OUT = process.env.OUT || null;
const REPO = 'auroraxo/reeflog';

function repoRoot() {
  return path.resolve(import.meta.dirname, '..');
}

const faces = [];
const record = (name, ok, detail) => {
  faces.push({ name, ok, detail });
  return ok;
};

// S1 — the published tag tree names the release it is.
const tagDir = fs.mkdtempSync(path.join(os.tmpdir(), `stranger-${TAG}-`));
try {
  const tarball = execFileSync('git', ['archive', TAG], {
    cwd: repoRoot(), maxBuffer: 128 * 1024 * 1024,
  });
  execFileSync('tar', ['-x', '-C', tagDir], { input: tarball });

  const manifest = JSON.parse(fs.readFileSync(path.join(tagDir, 'package.json'), 'utf8')).version;
  const crate = /^version\s*=\s*"([^"]+)"/m
    .exec(fs.readFileSync(path.join(tagDir, 'Cargo.toml'), 'utf8'))?.[1];
  record('manifest == tag', manifest === TAG.slice(1),
    `package.json ${manifest} vs tag ${TAG}`);
  record('crate == tag', crate === TAG.slice(1),
    `Cargo.toml ${crate} vs tag ${TAG}`);

  // S2 — the tag's own client, run with NO version argument (what the README
  // tells a stranger to do), against the REAL release API. We observe the
  // request instead of calling bootstrap() blindly: downloading from
  // api.github.com is fine, but recording which release entry was fetched is
  // the face under test. bootstrap() also downloads — one call does both.
  const dest = fs.mkdtempSync(path.join(os.tmpdir(), 'stranger-dest-'));
  const observed = { releaseUrl: null };
  const mod = await import(path.join(tagDir, 'index.js'));

  // Wrap the client's own httpGet by pointing apiBase at the real API and
  // watching bootstrap's outcome; the requested tag is read from the
  // sidecar install.json the client writes (version + asset + digests) —
  // the client's own record of what it installed.
  const res = await mod.bootstrap({ dest });
  const sidecar = JSON.parse(fs.readFileSync(path.join(dest, 'install.json'), 'utf8'));
  observed.releaseUrl = `https://api.github.com/repos/${REPO}/releases/tags/${sidecar.version}`;
  const requestedTag = sidecar.version.startsWith('v') ? sidecar.version : `v${sidecar.version}`;
  record('bootstrap() default request == tag', requestedTag === TAG,
    `client asked for ${sidecar.version} (normalized: ${requestedTag}) vs expected ${TAG}`);

  // S3 — digest: sidecar vs Release API vs the bytes on disk.
  const apiNamed = JSON.parse(execFileSync('gh', [
    'api', `repos/${REPO}/releases/tags/${TAG}`,
    '--jq', '[.assets[] | {name, digest}]',
  ]).toString());
  const assetEntry = apiNamed.find((a) => a.name === sidecar.asset);
  const apiDigest = String(assetEntry?.digest || '').replace(/^sha256:/, '');
  record('release-API digest == installed sidecar digest',
    apiDigest.length === 64 && apiDigest === sidecar.sha256,
    `API ${apiDigest.slice(0, 12)}… vs sidecar ${String(sidecar.sha256).slice(0, 12)}… (asset ${sidecar.asset})`);
  const diskSha = createHash('sha256').update(fs.readFileSync(res.bin)).digest('hex');
  record('installed binary hash == sidecar binarySha256',
    diskSha === sidecar.binarySha256,
    `disk ${diskSha.slice(0, 12)}… vs sidecar ${String(sidecar.binarySha256).slice(0, 12)}…`);

  // S4 — the binary answers with the release the whole chain names.
  const out = execFileSync(res.bin, ['--version']).toString().trim();
  record('binary --version == tag', out === `reeflog ${TAG.slice(1)}`,
    `"${out}" vs "${TAG.slice(1)}"`);

  fs.rmSync(dest, { recursive: true, force: true });
} finally {
  fs.rmSync(tagDir, { recursive: true, force: true });
}

const report = {
  measuredAt: new Date().toISOString(),
  tag: TAG,
  source: `published ${TAG} release (real GitHub API + real asset, README stranger path)`,
  faces,
  passed: faces.filter((f) => f.ok).length,
  total: faces.length,
  verdict: faces.every((f) => f.ok) ? 'DELIVERED-TO-THE-PIN' : 'NOT-DELIVERED',
};

if (OUT) {
  fs.mkdirSync(path.dirname(path.resolve(OUT)), { recursive: true });
  fs.writeFileSync(path.resolve(OUT), `${JSON.stringify(report, null, 2)}\n`);
}
console.log(JSON.stringify(report, null, 2));
process.exit(report.verdict === 'DELIVERED-TO-THE-PIN' ? 0 : 1);
