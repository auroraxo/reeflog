#!/usr/bin/env node
// THE REGISTRY CHANNEL, MEASURED: does the PUBLISHED PACKAGE tell the
// truth to the person who installs it FROM the registry?
//
// WHY (measured 2026-11-13, this repo): `npm view reeflog-client` answers
// 200 — the registry serves reeflog-client@0.7.20 (published 2026-11-12
// 16:57Z) — and `npm install reeflog-client` into an empty app followed by
// bootstrap() installs `reeflog 0.7.20` and passes the full stranger-path
// live check (8/8 against a real 4835-event mainnet journal). But the
// README those exact bytes carry mentions the registry command NOWHERE:
// every install line says `npm install github:auroraxo/reeflog#…`. The
// stranger standing on the npm page is pointed off the channel they are
// on. This is the signerd v0.2.18 class (published docs vs measured
// registry state), in its INVISIBILITY form: not a denial, a silence.
//
// WHAT THIS MEASURES (all faces judged on the PUBLISHED tarball bytes,
// fetched from registry.npmjs.org — never from this working tree):
//   G0  the registry serves the package at the manifest's own version (200).
//   D1  the published README carries the registry install command
//       (`npm install reeflog-client`) — FAILS on the v0.7.20 bytes.
//   D2  the published README's first install paragraph names the registry
//       channel (package page first, not buried) — FAILS on v0.7.20.
//   G3  the git-tag install path STAYS documented (a fix that deletes the
//       old channel is the false-positive guard) — holds on both sides.
//   G4  the published manifest does not deny npm publication (no
//       "not published to npm" sentence) — holds on both sides.
//   O1  the name on the registry is OURS (maintainer account matches the
//       publish identity `npm whoami` reports here) — the signerd O-class.
//
// Verdict: registry-invisible (D1 or D2 violated) | registry-consistent |
// no-verdict when G0 fails (the channel itself is the premise).
//
//   OUT=research/registry-channel-v0720-2026-11-13.json \
//     node tools/measure-registry-channel.mjs
// Exit: 0 registry-consistent, 1 a defect face violated, 2 no verdict.
import { execFileSync } from 'node:child_process';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';

const PKG = 'reeflog-client';
const OUT = process.env.OUT || null;

function repoRoot() {
  return path.resolve(import.meta.dirname, '..');
}

const manifest = JSON.parse(fs.readFileSync(path.join(repoRoot(), 'package.json'), 'utf8'));
const wantVersion = manifest.version;

const faces = [];
const record = (name, ok, detail) => { faces.push({ name, ok, detail }); };

// G0 — the registry answers for the exact version this tree's manifest names.
let registryDoc;
try {
  registryDoc = JSON.parse(execFileSync('npm', ['view', `${PKG}@${wantVersion}`, '--json'],
    { encoding: 'utf8', maxBuffer: 32 * 1024 * 1024 }));
} catch {
  console.error(`no verdict: registry does not serve ${PKG}@${wantVersion}`);
  process.exit(2);
}
record('G0 registry serves manifest version', Boolean(registryDoc?.dist?.tarball),
  `${PKG}@${wantVersion} tarball ${registryDoc.dist.tarball ? 'present' : 'absent'}`);

// Fetch the PUBLISHED tarball bytes — the exact bytes a stranger receives.
const tmp = fs.mkdtempSync(path.join(os.tmpdir(), 'regchan-'));
let readme, pubManifest;
try {
  execFileSync('curl', ['-sS', '--fail', '-o', path.join(tmp, 'p.tgz'), registryDoc.dist.tarball]);
  execFileSync('tar', ['-xzf', path.join(tmp, 'p.tgz'), '-C', tmp]);
  readme = fs.readFileSync(path.join(tmp, 'package', 'README.md'), 'utf8');
  pubManifest = JSON.parse(fs.readFileSync(path.join(tmp, 'package', 'package.json'), 'utf8'));
} catch (e) {
  console.error(`no verdict: could not fetch/unpack the published tarball: ${e.message}`);
  process.exit(2);
} finally {
  fs.rmSync(tmp, { recursive: true, force: true });
}

const REGISTRY_CMD = new RegExp(`npm (install|i) ${PKG}\\b`);
const GIT_TAG_CMD = /npm (install|i) github:auroraxo\/reeflog#v\d+\.\d+\.\d+/;

// D1 — the registry command exists somewhere in the published README.
record('D1 published README carries registry install command',
  REGISTRY_CMD.test(readme),
  REGISTRY_CMD.test(readme) ? 'found' : 'the package page never says npm install ' + PKG);

// D2 — the FIRST install paragraph (the status line a stranger reads
// first) names the registry command, not only the git-tag path.
const firstPara = readme.split('\n').find((l) => l.startsWith('**Status:')) || '';
record('D2 first install paragraph names the registry channel',
  REGISTRY_CMD.test(firstPara),
  REGISTRY_CMD.test(firstPara) ? 'named' : 'first paragraph offers only the git-tag path');

// G3 — the git-tag path stays (false-positive guard: the fix must ADD a
// channel, not quietly delete the documented one).
record('G3 git-tag install path still documented', GIT_TAG_CMD.test(readme),
  GIT_TAG_CMD.test(readme) ? 'present' : 'MISSING — a fix must keep both channels');

// G4 — no publication denial anywhere (the signerd R1/R2 class).
const DENIAL = /not (yet )?published to npm|no package is published|publishing (is|remains) credential-blocked/i;
record('G4 published README does not deny npm publication', !DENIAL.test(readme),
  DENIAL.test(readme) ? 'denial sentence present' : 'no denial');

// O1 — ownership: the registry maintainer is this machine's publish identity.
let whoami = null;
try { whoami = execFileSync('npm', ['whoami'], { encoding: 'utf8' }).trim(); } catch { /* offline */ }
const maint = String(registryDoc.maintainers?.[0] || '');
const ownerName = maint.split(/[^A-Za-z0-9._-]/)[0];
const ours = whoami === null ? null : ownerName === whoami || maint.includes(whoami);
record('O1 registry name is ours', ours !== false,
  `maintainer "${maint}" vs npm whoami ${whoami === null ? '(offline, unchecked)' : `"${whoami}"`}`);

const defects = faces.filter((f) => f.name.startsWith('D') && !f.ok).length;
// A violated GUARD (G*/O*) means the measurement itself cannot testify:
// the premise is broken, not the subject. Defect faces (D*) are the verdict.
const guardsBroken = faces.filter((f) => !f.name.startsWith('D') && !f.ok).length;
const verdict = guardsBroken > 0 ? 'no-verdict'
  : defects > 0 ? 'registry-invisible'
  : 'registry-consistent';

const report = {
  source: `published ${PKG}@${pubManifest.version} tarball from registry.npmjs.org`,
  measuredAt: new Date().toISOString().slice(0, 10),
  manifestVersion: pubManifest.version,
  distTags: registryDoc['dist-tags'],
  faces,
  defects,
  verdict,
};
const text = JSON.stringify(report, null, 2);
if (OUT) fs.writeFileSync(OUT, text);
console.log(text);
process.exit(guardsBroken > 0 ? 2 : (defects > 0 ? 1 : 0));
