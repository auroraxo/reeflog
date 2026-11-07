#!/usr/bin/env node
// The cache claim, measured against the REAL published release.
//
// The offline suite (test/bootstrap-cache-integrity.test.mjs) proves the
// contract against a local stand-in for GitHub. This run asks the same
// question against the live release API and the live asset CDN:
//
//  1. FRESH       — bootstrap() installs from the published release and the
//                   sidecar records BOTH hashes (archive + extracted binary).
//  2. CACHED      — an untouched second call is served from disk, zero
//                   downloads, and its claim still matches the bytes.
//  3. TAMPER      — append bytes to the installed binary; the next call must
//                   NOT report a cache hit, must re-fetch, and must end with
//                   a `binarySha256` that describes what is on disk.
//  4. LEGACY      — a sidecar from before this fix (no binarySha256) is
//                   upgraded by one re-fetch, then caches again.
//  5. CROSS-ASSET — the OTHER platform's asset is hashed too, so the claim
//                   is not limited to the platform this host runs.
//
// Exit: 0 all pass, 1 a check failed, 2 setup failed.
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { createHash } from 'node:crypto';
import { bootstrap, RELEASE_ASSETS } from '../index.js';

const pkg = JSON.parse(fs.readFileSync(new URL('../package.json', import.meta.url), 'utf8'));
const VERSION = process.env.VERSION || `v${pkg.version}`;
const OUT = process.env.OUT || path.resolve('research/bootstrap-cache-2026-11-07.json');
const OTHER = process.env.OTHER || Object.keys(RELEASE_ASSETS).find(
  (k) => k !== `${process.platform}-${process.arch}`) || 'darwin-arm64';

const checks = [];
const record = (name, ok, detail) => {
  checks.push({ name, ok, detail });
  console.log(`${ok ? 'PASS' : 'FAIL'} ${name} — ${detail}`);
};
const sha = (p) => createHash('sha256').update(fs.readFileSync(p)).digest('hex');
const meta = (dest) => JSON.parse(fs.readFileSync(path.join(dest, 'install.json'), 'utf8'));

const dest = fs.mkdtempSync(path.join(os.tmpdir(), 'reeflog-cache-live-'));
const result = { generatedAt: new Date().toISOString(), version: VERSION, checks: [] };

try {
  // 1. fresh
  const t0 = Date.now();
  const first = await bootstrap({ version: VERSION, dest });
  const msFresh = Date.now() - t0;
  const m = meta(dest);
  record('fresh_install_records_both_hashes',
    /^[0-9a-f]{64}$/.test(m.sha256 || '') && m.binarySha256 === sha(first.bin) && first.binarySha256 === sha(first.bin),
    `archive=${(m.sha256 || '').slice(0, 12)}… binary=${(m.binarySha256 || '').slice(0, 12)}… ms=${msFresh}`);
  result.fresh = { archive: m.sha256, binary: m.binarySha256, ms: msFresh };

  // 2. untouched cache hit — zero network (a fresh dest that already has the
  //    sidecar: point a second call at it and require cached + matching bytes)
  const t1 = Date.now();
  const second = await bootstrap({ version: VERSION, dest });
  const msCached = Date.now() - t1;
  record('untouched_install_caches_and_the_claim_still_matches',
    second.cached === true && second.binarySha256 === sha(second.bin),
    `cached=${second.cached} binary=${second.binarySha256.slice(0, 12)}… ms=${msCached}`);
  result.cached = { cached: second.cached, ms: msCached };

  // 3. tamper with the INSTALLED binary
  const before = fs.statSync(first.bin).size;
  fs.appendFileSync(first.bin, '\n// tampered by the cache-integrity probe\n');
  const t2 = Date.now();
  const third = await bootstrap({ version: VERSION, dest });
  const msTamper = Date.now() - t2;
  const disk = sha(third.bin);
  record('tampered_binary_is_never_served_as_a_cache_hit',
    third.cached === false && third.binarySha256 === disk && third.digest === second.digest,
    `cached=${third.cached} reported=${third.binarySha256.slice(0, 12)}… disk=${disk.slice(0, 12)}… size ${before}->${fs.statSync(third.bin).size} ms=${msTamper}`);
  result.tampered = { cached: third.cached, reported: third.binarySha256, onDisk: disk, ms: msTamper };

  // 4. legacy sidecar (what every install made before this fix looks like)
  const legacy = meta(dest);
  delete legacy.binarySha256;
  fs.writeFileSync(path.join(dest, 'install.json'), `${JSON.stringify(legacy, null, 2)}\n`);
  const fourth = await bootstrap({ version: VERSION, dest });
  const upgraded = meta(dest);
  const fifth = await bootstrap({ version: VERSION, dest });
  record('pre_fix_sidecar_is_upgraded_by_one_fetch_then_caches',
    fourth.cached === false && /^[0-9a-f]{64}$/.test(upgraded.binarySha256 || '') && fifth.cached === true,
    `first call cached=${fourth.cached} upgraded=${Boolean(upgraded.binarySha256)} next cached=${fifth.cached}`);
  result.legacy = { firstCallCached: fourth.cached, upgraded: Boolean(upgraded.binarySha256), nextCached: fifth.cached };

  // 5. the OTHER platform's asset: bootstrap cannot run it here, but the
  //    release must still carry it with a digest (a Mac stranger's half).
  const releaseUrl = `https://api.github.com/repos/auroraxo/reeflog/releases/tags/${VERSION}`;
  const res = await fetch(releaseUrl, { headers: { 'user-agent': 'reeflog-cache-probe' } });
  const release = await res.json();
  const otherAsset = (release.assets || []).find((a) => a.name === RELEASE_ASSETS[OTHER]);
  record('other_platform_asset_present_with_a_sha256_digest',
    Boolean(otherAsset) && /^sha256:[0-9a-f]{64}$/.test(otherAsset?.digest || ''),
    `${OTHER}: ${otherAsset ? otherAsset.name : 'MISSING'} digest=${(otherAsset?.digest || 'none').slice(0, 20)}…`);
  result.otherPlatform = {
    platform: OTHER,
    present: Boolean(otherAsset),
    digest: otherAsset?.digest || null,
    note: 'present and digest-bearing; not extracted or executed on this host',
  };
} catch (e) {
  record('setup', false, String(e && e.message ? e.message : e));
}

result.checks = checks;
result.ok = checks.every((c) => c.ok);
result.limits = [
  'one host, one network, one platform executed (linux-x64); the darwin-arm64 asset is verified present with a digest but was not extracted or executed here',
  'the tamper is a local append by this probe — an attacker who can write the install dir can also re-run bootstrap() afterwards; this check defends against a stale or corrupted cache being reported as verified, not against a hostile local user',
  'the trust anchor for the DOWNLOAD is unchanged: github.com/auroraxo/reeflog over TLS (the release API digest); this fix makes the CACHE claim true, it does not strengthen the download anchor',
  'no third party has used this client yet',
];

fs.mkdirSync(path.dirname(OUT), { recursive: true });
fs.writeFileSync(OUT, `${JSON.stringify(result, null, 2)}\n`);
fs.rmSync(dest, { recursive: true, force: true });
console.log(`\n${result.ok ? 'ALL CHECKS PASSED' : 'CHECKS FAILED'} → ${OUT}`);
process.exit(result.ok ? 0 : 1);
