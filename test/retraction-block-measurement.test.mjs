// Pin for the retraction-block measurement (2026-11-17).
//
// The defect was invisible by construction: a refused retraction was
// reported on the EVENT channel's line, at a position that did not exist
// (`at seq None`), with Rust debug formatting in the same line, and the
// retraction's reason arrived on the event channel's `blocked_error`
// field. Nothing on CI could see it — the report had one error field for
// two queues and the stderr line named the wrong one. This file pins the
// MEASUREMENT so the claim stays a measurement:
//
//   P1 the two-tree artifact names the PUBLISHED v0.7.32 asset as its
//      baseline and the fixed tree as its fixed side, by sha256;
//   P2 the baseline faces: D1-D3 red with the exact observed pre-fix
//      values, D4 green (it is the must-not-move face) — the defect, as
//      measured;
//   P3 the guards green on BOTH sides — the defect faces are red because
//      of the reporting, not because the fixture was broken;
//   P4 the PUBLISHED v0.7.33 artifact holds the rule: every face and
//      guard green, verdict retraction-block-named;
//   P5 no host path may appear in either artifact;
//   F  FALSIFICATION: flipping a pinned value must make this test go red —
//      done by construction (exact strings, booleans compared with ===)
//      so an edited artifact cannot silently re-bless itself.
import test from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';

const RESEARCH = path.resolve(import.meta.dirname, '..', 'research');
const TWO_TREE = path.join(RESEARCH, 'retraction-block-blind-2026-11-17.json');
const PUBLISHED_FIXED = path.join(
  RESEARCH,
  'retraction-block-published-v0733-2026-11-17.json',
);

const two = JSON.parse(fs.readFileSync(TWO_TREE, 'utf8'));
const pubFix = JSON.parse(fs.readFileSync(PUBLISHED_FIXED, 'utf8'));

const face = (v, id) => v.faces.find((f) => (f.face ?? f.id) === id);
const guard = (v, id) => v.guards.find((g) => (g.guard ?? g.id) === id);

test('P1: the two-tree artifact names the published v0.7.32 asset as its baseline', () => {
  assert.equal(two.artifact, 'retraction-block-blind');
  assert.equal(two.baseline.tree, 'published v0.7.32 release asset');
  assert.equal(two.baseline.binary.sha256, '534c2ec75cfcae1d0c70b5302e70bab1eb9405603f72c0753cf7e355e6053d97');
  assert.equal(two.baseline.binary.version, 'reeflog 0.7.32');
  assert.equal(two.baseline.verdict, 'retraction-block-blind');
  assert.equal(two.fixed.binary.version, 'reeflog 0.7.33');
  assert.equal(two.fixed.verdict, 'retraction-block-named');
});

test('P2: the baseline faces carry the defect exactly as measured', () => {
  const d1 = face(two, 'D1');
  assert.equal(d1.pre.ok, false, 'D1 must be red on v0.7.32');
  assert.equal(d1.pre.observed.namesRetraction, false);
  assert.equal(d1.pre.observed.namesReorgId, false);
  assert.equal(d1.pre.observed.noPhantomPosition, false);
  assert.match(d1.pre.observed.stderr, /at seq None/);

  const d2 = face(two, 'D2');
  assert.equal(d2.pre.ok, false, 'D2 must be red on v0.7.32');
  assert.equal(d2.pre.observed.noDebugFormatting, false);

  const d3 = face(two, 'D3');
  assert.equal(d3.pre.ok, false, 'D3 must be red on v0.7.32');
  assert.equal(d3.pre.observed.reorgError, null, 'v0.7.32 has no reorg_error field');

  // D4 is the must-not-move face: green on both sides.
  assert.equal(face(two, 'D4').pre.ok, true, 'D4 must be green on v0.7.32 (facts unchanged)');
});

test('P3: the guards are green on BOTH sides', () => {
  for (const id of ['G1', 'G2', 'G3']) {
    assert.equal(guard(two, id).pre.ok, true, `guard ${id} must be green on v0.7.32`);
    assert.equal(guard(two, id).post.ok, true, `guard ${id} must be green after the fix`);
  }
  // The design rule the fix must not overturn: a refused retraction is
  // never quarantined.
  assert.deepEqual(guard(two, 'G2').pre.observed.deadLetters, []);
  assert.deepEqual(guard(two, 'G2').post.observed.deadLetters, []);
});

test('P4: the PUBLISHED v0.7.33 asset holds the rule — the fix is in the bytes a stranger installs', () => {
  assert.equal(pubFix.binary.sha256, 'e660b9ecb37d8ba6774ee6ef6d65e08d11b2696d9d603d7edf39c85ae3197d9b');
  assert.equal(pubFix.binary.version, 'reeflog 0.7.33');
  assert.equal(pubFix.label, 'published-v0.7.33');
  assert.equal(pubFix.verdict, 'retraction-block-named');
  for (const id of ['D1', 'D2', 'D3', 'D4']) {
    assert.equal(face(pubFix, id).ok, true, `face ${id} must be green on the published v0.7.33 asset`);
  }
  for (const id of ['G1', 'G2', 'G3']) {
    assert.equal(guard(pubFix, id).ok, true, `guard ${id} must be green on the published v0.7.33 asset`);
  }
  assert.equal(face(pubFix, 'D3').observed.reorgError, 'refused with http 404');
  assert.equal(face(pubFix, 'D3').observed.eventError, null);
});

test('P5: neither artifact carries a host path', () => {
  for (const file of [TWO_TREE, PUBLISHED_FIXED]) {
    const text = fs.readFileSync(file, 'utf8');
    assert.ok(!text.includes('/home/aurora'), `${path.basename(file)} must not carry a host path`);
    assert.ok(!text.includes('/tmp/reeflog'), `${path.basename(file)} must not carry a temp path`);
  }
});
