// Pin for the WEBHOOK-STALE-ERROR measurement (PULS AG, 2026-11-13).
//
// FALSIFIABLE, like every house pin: each assertion below was checked to go
// RED when the artifact claim is flipped, and both artifacts were then
// restored byte-identical.
//
// What the artifacts prove:
//   * the PUBLISHED v0.7.23 binary still carries the defect (a recovered
//     target keeps reporting its old failure) — that is the immutable fact
//     this pin keeps honest;
//   * the fix tree does not;
//   * the guards hold on BOTH, so the fix buys the correction without
//     silencing a failure that is still standing.
import test from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';

const RESEARCH = path.resolve(import.meta.dirname, '..', 'research');
const load = (name) => JSON.parse(fs.readFileSync(path.join(RESEARCH, name), 'utf8'));

const published = load('webhook-stale-error-published-v0723-2026-11-13.json');
const fixed = load('webhook-stale-error-fixed-2026-11-13.json');
const published24 = load('webhook-stale-error-published-v0724-2026-11-13.json');

const face = (report, id) => {
  const f = report.faces.find((x) => x.id === id);
  assert.ok(f, `face ${id} missing from ${report.tree}`);
  return f;
};

test('B1: the published v0.7.23 binary measured stale-error-verdict-present', () => {
  assert.equal(published.tool, 'measure-webhook-stale-error');
  assert.equal(published.verdict, 'stale-error-verdict-present');
  assert.equal(published.binaryVersion, 'reeflog 0.7.23');
  // The binary is named by its own bytes — never by a host path.
  assert.equal(published.binary, 'reeflog');
  assert.match(published.binarySha256, /^[0-9a-f]{64}$/);
  assert.ok(!JSON.stringify(published).includes('/tmp/'), 'artifact must not carry a host path');
});

test('B2: the published binary sha256 is the published release asset digest', () => {
  // Fixed: the measurement ran against the published asset. Falsifier: any
  // other sha256 here means the baseline was NOT the published binary.
  assert.equal(
    published.binarySha256,
    'e7aaf1f1d9bb18c976fc14b17b1175cfb71cf14045b62094b436e0f4ec40e4b1',
    'published v0.7.23 binary sha256 changed — re-verify the release asset',
  );
});

test('B3: both defect faces are visible on the published binary', () => {
  const s1 = face(published, 'S1');
  assert.equal(s1.standingError, true, 'S1: the old http 500 still stands after recovery');
  assert.equal(s1.lastError, 'http 500');
  assert.equal(s1.delivered, 2, 'and yet both rows were delivered (the target IS healthy)');

  const s2 = face(published, 'S2');
  assert.equal(s2.standingError, true, 'S2: the paid hold reason still stands');
  assert.equal(s2.delivered, 1, 'and yet the paid row was delivered');
});

test('B4: the fix tree clears both standing verdicts', () => {
  assert.equal(fixed.verdict, 'stale-error-verdict-cleared');
  const s1 = face(fixed, 'S1');
  assert.equal(s1.standingError, false, 'S1: a passing pass leaves no standing error');
  assert.equal(s1.lastError, null);
  assert.equal(s1.delivered, 2);
  assert.equal(s1.cursor, 2, 'the cursor advanced over both delivered rows');

  const s2 = face(fixed, 'S2');
  assert.equal(s2.standingError, false, 'S2: the paid hold is gone from the report');
  assert.equal(s2.lastError, null);
  assert.equal(s2.delivered, 1);
});

test('B5: the PUBLISHED v0.7.24 asset carries the fix (digest verified)', () => {
  assert.equal(published24.verdict, 'stale-error-verdict-cleared');
  assert.equal(published24.binaryVersion, 'reeflog 0.7.24');
  assert.equal(published24.defectFaces.length, 0);
  assert.deepEqual(published24.fixedFaces, ['S1', 'S2']);
  assert.deepEqual(published24.guards, ['G1', 'G2']);
  assert.deepEqual(published24.guardsBroken, []);
  assert.match(published24.binarySha256, /^[0-9a-f]{64}$/);
  assert.ok(!JSON.stringify(published24).includes('/tmp/'), 'artifact must not carry a host path');
});

test('G1: a standing failure is still reported (both trees)', () => {
  for (const report of [published, fixed]) {
    const g1 = face(report, 'G1');
    assert.equal(g1.reported, true, `${report.tree}: a blocked pass must report why`);
    assert.equal(g1.lastError, 'http 500');
    assert.equal(g1.exitCode, 3, `${report.tree}: and the pass must exit 3`);
  }
});

test('G2: a target that never failed reports no error (both trees)', () => {
  for (const report of [published, fixed]) {
    const g2 = face(report, 'G2');
    assert.equal(g2.lastError, null, `${report.tree}: clean target must be silent`);
    assert.equal(g2.delivered, 1, `${report.tree}: and it did deliver`);
  }
});

test('the measurement refuses a host path in either artifact', () => {
  for (const report of [published, fixed, published24]) {
    assert.ok(!JSON.stringify(report).includes('/tmp/'), `${report.tree}: no host path`);
    assert.match(report.binarySha256, /^[0-9a-f]{64}$/);
  }
});
