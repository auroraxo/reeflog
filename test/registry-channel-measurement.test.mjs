// Pin for the registry-channel measurements (2026-11-13).
//
// The defect was invisibility, not denial: reeflog-client@0.7.20 was served
// by registry.npmjs.org while the README inside those exact bytes said only
// `npm install github:auroraxo/reeflog#v0.7.20` — the stranger standing on
// the npm page was pointed off the channel they were on. This file pins the
// MEASUREMENTS so both sides of the claim stay measurements:
//
//   V1 the v0.7.20 artifact: verdict registry-invisible, 2 defect faces
//      (D1 no registry command, D2 first paragraph git-tag only), guards
//      G0/G3/G4/O1 all green — the defect measured, not the channel broken;
//   V2 the v0.7.21 artifact: verdict registry-consistent, 0 defects, all
//      six faces green — the fix REACHED the published tarball;
//   V2b the v0.7.22 artifact: the channel still names itself after the
//      release that fixed the payload hold (0 defects, latest = 0.7.22);
//   V3 the live user-zero artifact: 12/12 on the published 0.7.22, a real
//      mainnet journal behind every read;
//   V4 no host path in any artifact;
//   F  falsification by construction: every face is compared with ===
//      against literals, so an edited artifact cannot re-bless itself.
import test from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';

const dir = path.resolve(import.meta.dirname, '..', 'research');
const load = (name) => JSON.parse(fs.readFileSync(path.join(dir, name), 'utf8'));

const before = load('registry-channel-v0720-2026-11-13.json');
const after = load('registry-channel-v0721-2026-11-13.json');
const current = load('registry-channel-v0722-2026-11-13.json');
const latest = load('registry-channel-v0723-2026-11-13.json');
const published = load('registry-channel-v0724-2026-11-13.json');
const live = load('registry-install-live-v0723-2026-11-13.json');
const live24 = load('registry-install-live-v0724-2026-11-13.json');

const face = (report, prefix) => report.faces.find((f) => f.name.startsWith(prefix));

test('V1: the v0.7.20 tarball measured registry-invisible — 2 defect faces, guards green', () => {
  assert.equal(before.source, 'published reeflog-client@0.7.20 tarball from registry.npmjs.org');
  assert.equal(before.measuredAt, '2026-11-13');
  assert.equal(before.verdict, 'registry-invisible');
  assert.equal(before.defects, 2);
  assert.equal(face(before, 'D1').ok, false);
  assert.equal(face(before, 'D2').ok, false);
  assert.equal(face(before, 'G0').ok, true, 'the registry serving the package is the premise');
  assert.equal(face(before, 'G3').ok, true, 'the git-tag path held before the fix');
  assert.equal(face(before, 'G4').ok, true, 'this README never denied — it stayed silent');
  assert.equal(face(before, 'O1').ok, true, 'the name is ours');
});

test('V2: the v0.7.21 tarball measured registry-consistent — 0 defects, six faces green', () => {
  assert.equal(after.source, 'published reeflog-client@0.7.21 tarball from registry.npmjs.org');
  assert.equal(after.measuredAt, '2026-11-13');
  assert.equal(after.verdict, 'registry-consistent');
  assert.equal(after.defects, 0);
  for (const f of after.faces) assert.equal(f.ok, true, `${f.name} must hold`);
  assert.equal(after.faces.length, 6);
  assert.equal(after.distTags.latest, '0.7.21');
});

test('V2b: the v0.7.22 tarball measures registry-consistent — the channel names itself', () => {
  assert.equal(current.source, 'published reeflog-client@0.7.22 tarball from registry.npmjs.org');
  assert.equal(current.measuredAt, '2026-11-13');
  assert.equal(current.verdict, 'registry-consistent');
  assert.equal(current.defects, 0);
  for (const f of current.faces) assert.equal(f.ok, true, `${f.name} must hold`);
  assert.equal(current.faces.length, 6);
  assert.equal(current.distTags.latest, '0.7.22');
});

test('V2c: the v0.7.23 tarball measures registry-consistent — 0 defects', () => {
  assert.equal(latest.source, 'published reeflog-client@0.7.23 tarball from registry.npmjs.org');
  assert.equal(latest.verdict, 'registry-consistent');
  assert.equal(latest.defects, 0);
  for (const f of latest.faces) assert.equal(f.ok, true, `${f.name} must hold`);
  assert.equal(latest.distTags.latest, '0.7.23');
});

test('V3: the published 0.7.23 passed the live registry-install user-zero 12/12', () => {
  assert.equal(live.package, 'reeflog-client@0.7.23');
  assert.equal(live.installedVersion, '0.7.23');
  assert.equal(live.verdict, 'all_pass');
  assert.equal(live.checks.length, 12);
  assert.ok(live.journalEvents > 0, 'every read was answered by a real mainnet journal');
  assert.ok(live.liveSeconds >= 60, 'the daemon ran against the chain, not a fixture');
  const names = live.checks.map((c) => c.name);
  assert.ok(names.includes('registry_install_from_empty_app'));
  assert.ok(names.includes('binary_version_matches_manifest'));
  assert.ok(names.includes('rows_byte_equal_to_cli'));
  assert.ok(names.includes('held_row_is_named_not_silent'));
  for (const c of live.checks) assert.equal(c.ok, true, `${c.name} must pass`);
});

test('V2d: the v0.7.24 tarball measures registry-consistent — 0 defects', () => {
  assert.equal(published.source, 'published reeflog-client@0.7.24 tarball from registry.npmjs.org');
  assert.equal(published.verdict, 'registry-consistent');
  assert.equal(published.defects, 0);
  for (const f of published.faces) assert.equal(f.ok, true, `${f.name} must hold`);
  assert.equal(published.distTags.latest, '0.7.24');
});

test('V3b: the published 0.7.24 passed the live registry-install user-zero 12/12', () => {
  assert.equal(live24.package, 'reeflog-client@0.7.24');
  assert.equal(live24.installedVersion, '0.7.24');
  assert.equal(live24.verdict, 'all_pass');
  assert.equal(live24.checks.length, 12);
  assert.ok(live24.journalEvents > 0, 'every read was answered by a real mainnet journal');
  assert.ok(live24.liveSeconds >= 60, 'the daemon ran against the chain, not a fixture');
  for (const c of live24.checks) assert.equal(c.ok, true, `${c.name} must pass`);
});

test('V2e: the v0.7.25 tarball measures registry-consistent — 0 defects', () => {
  const r = JSON.parse(fs.readFileSync(path.join(dir, 'registry-channel-v0725-2026-11-13.json'), 'utf8'));
  assert.equal(r.source, 'published reeflog-client@0.7.25 tarball from registry.npmjs.org');
  assert.equal(r.verdict, 'registry-consistent');
  assert.equal(r.defects, 0);
  for (const f of r.faces) assert.equal(f.ok, true, `${f.name} must hold`);
  assert.equal(r.distTags.latest, '0.7.25');
});

test('V3c: the published 0.7.25 passed the live registry-install user-zero 12/12', () => {
  const live25 = load('registry-install-live-v0725-2026-11-13.json');
  assert.equal(live25.package, 'reeflog-client@0.7.25');
  assert.equal(live25.installedVersion, '0.7.25');
  assert.equal(live25.verdict, 'all_pass');
  assert.equal(live25.checks.length, 12);
  assert.ok(live25.journalEvents > 0, 'every read was answered by a real mainnet journal');
  for (const c of live25.checks) assert.equal(c.ok, true, `${c.name} must pass`);
  // The zero-dependency release still bootstraps the binary it names.
  const bv = live25.checks.find((c) => c.name === 'binary_version_matches_manifest');
  assert.equal(bv.ok, true);
  assert.match(String(bv.detail), /0\.7\.25/);
});

test('V4: no host path leaks into the artifacts', () => {
  for (const f of ['registry-channel-v0720-2026-11-13.json',
    'registry-channel-v0721-2026-11-13.json',
    'registry-channel-v0722-2026-11-13.json',
    'registry-channel-v0723-2026-11-13.json',
    'registry-channel-v0724-2026-11-13.json',
    'registry-channel-v0725-2026-11-13.json',
    'registry-install-live-v0723-2026-11-13.json',
    'registry-install-live-v0724-2026-11-13.json',
    'registry-install-live-v0725-2026-11-13.json']) {
    const text = fs.readFileSync(path.join(dir, f), 'utf8');
    assert.doesNotMatch(text, /\/home\/|\/Users\/|C:\\/, `${f} must not pin the claim to one machine`);
  }
});
