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
//   V3 the live user-zero artifact: 12/12 on the published 0.7.21, a real
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
const live = load('registry-install-live-v0721-2026-11-13.json');

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

test('V3: the published 0.7.21 passed the live registry-install user-zero 12/12', () => {
  assert.equal(live.package, 'reeflog-client@0.7.21');
  assert.equal(live.installedVersion, '0.7.21');
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

test('V4: no host path leaks into the artifacts', () => {
  for (const f of ['registry-channel-v0720-2026-11-13.json',
    'registry-channel-v0721-2026-11-13.json',
    'registry-install-live-v0721-2026-11-13.json']) {
    const text = fs.readFileSync(path.join(dir, f), 'utf8');
    assert.doesNotMatch(text, /\/home\/|\/Users\/|C:\\/, `${f} must not pin the claim to one machine`);
  }
});
