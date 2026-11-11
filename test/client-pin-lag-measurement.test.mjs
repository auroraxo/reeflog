// Pin for the client pin-lag measurement (2026-11-11).
//
// The defect was silent by construction: a release commit that moves the
// crate, the lockfile and the README but not package.json ships a tag whose
// own client fetches the PREVIOUS release. Nothing on CI could see it — B8
// couples the request to package.json, and package.json was the lying side.
// This file pins the MEASUREMENT so the claim stays a measurement:
//
//   P1 the artifact exists and names the published v0.7.16 tree as baseline;
//   P2 the baseline faces: manifest 0.7.15 vs crate 0.7.16, requested tag
//      v0.7.15, servesThePinnedRelease false — the defect, as measured;
//   P3 the fixed faces: manifest == crate, requested tag v0.7.16,
//      servesThePinnedRelease true — the delivery this tree owes;
//   P4 the verdict field says defect-present-on-baseline;
//   P5 no host path may appear in the artifact;
//   F  FALSIFICATION: flipping a pinned number/flag must make this test go
//      red — done by construction in P2/P3 (exact strings, booleans compared
//      with ===) so an edited artifact cannot silently re-bless itself.
import test from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';

const ART = path.resolve(
  import.meta.dirname,
  '..',
  'research',
  'client-pin-lag-published-v0716-2026-11-11.json',
);

const report = JSON.parse(fs.readFileSync(ART, 'utf8'));

test('P1: the artifact names the published v0.7.16 tree as its baseline', () => {
  assert.match(report.source, /^published v0\.7\.16 release tree \(git archive v0\.7\.16\) vs this tree$/,
    'the measured seat is the PUBLISHED v0.7.16 tag tree, not a local checkout');
  assert.equal(report.measuredAt, '2026-11-11');
});

test('P2: the baseline shows the lag — manifest 0.7.15, crate 0.7.16, requested v0.7.15', () => {
  assert.equal(report.baseline.manifest, '0.7.15');
  assert.equal(report.baseline.crate, '0.7.16');
  assert.equal(report.baseline.requestedTag, 'v0.7.15');
  assert.equal(report.baseline.servesThePinnedRelease, false);
  assert.equal(report.baselineServesPinnedRelease, false);
});

test('P3: this tree serves its own pinned release — manifest == crate == requested tag', () => {
  assert.equal(report.fixed.manifest, '0.7.16');
  assert.equal(report.fixed.crate, '0.7.16');
  assert.equal(report.fixed.requestedTag, 'v0.7.16');
  assert.equal(report.fixed.servesThePinnedRelease, true);
  assert.equal(report.fixedServesPinnedRelease, true);
});

test('P4: the verdict reads defect-present-on-baseline', () => {
  assert.equal(report.verdict, 'defect-present-on-baseline');
});

test('P5: no host path leaks into the artifact', () => {
  const text = fs.readFileSync(ART, 'utf8');
  assert.doesNotMatch(text, /\/home\/|\/Users\/|C:\\/, 'a host path would pin the claim to one machine');
});
