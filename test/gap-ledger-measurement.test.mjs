// Pin for the GAP-LEDGER measurement (PULS AJ, 2026-11-15).
//
// FALSIFIABLE, like every house pin: each assertion was checked to go RED
// when the claim is flipped, and both artifacts restored byte-identical.
//
// What the artifacts prove:
//   * the PUBLISHED v0.7.26 binary counts SCANS, not EPISODES: two
//     `verify` runs over ONE hole leave TWO open gap rows, and after the
//     hole is filled and the chain calls the journal whole, `status` still
//     reports `gaps_open: 1` — a permanent false alarm;
//   * this tree counts episodes: one row per episode, a heal closes the
//     whole episode (including duplicates a pre-0.7.27 verify wrote), and
//     a narrowed `--since-slot` pass leaves a deeper episode open.
import test from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';

const RESEARCH = path.resolve(import.meta.dirname, '..', 'research');
const load = (name) => JSON.parse(fs.readFileSync(path.join(RESEARCH, name), 'utf8'));

const published = load('gap-ledger-published-v0726-2026-11-14.json');
const fixed = load('gap-ledger-fix-tree-2026-11-14.json');
const published27 = load('gap-ledger-published-v0727-2026-11-14.json');

const face = (report, id) => {
  const f = report.faces.find((x) => x.id === id);
  assert.ok(f, `face ${id} missing from ${report.label}`);
  return f;
};

test('L1: the published v0.7.26 binary measured defect-present', () => {
  assert.equal(published.kind, 'gap-ledger-episode-accounting');
  assert.equal(published.verdict, 'defect-present');
  // The binary is named by its own bytes — never by a host path.
  assert.equal(published.binary, 'reeflog');
  assert.match(published.binarySha256, /^[0-9a-f]{64}$/);
  assert.ok(!JSON.stringify(published).includes('/tmp/'), 'artifact must not carry a host path');
  assert.ok(!JSON.stringify(published).includes('/home/'), 'artifact must not carry a host path');
});

test('L2: the published binary is the one the release API publishes', () => {
  // Fixed: the measurement ran against the PUBLISHED asset (archive
  // sha256 8b9216c8… == the digest GitHub publishes, binary a49d8706…).
  assert.equal(
    published.binarySha256,
    'a49d8706ac930b03bda052d817486b226bf38602d6ad65342639bd4cbc63c8bf',
    'published v0.7.26 binary sha256 changed — re-verify the release asset',
  );
});

test('L3: two verifies over ONE hole left TWO gap rows on the published binary', () => {
  const f1 = face(published, 'F1');
  assert.equal(f1.oneRow, false, 'F1: the published binary opened a second row');
  assert.equal(f1.rowsAfterSecond, 2, 'and a second row was open');
  assert.equal(f1.openAfterSecond, 2, 'so one hole alarmed twice');
  assert.equal(f1.detail.length, 114, 'the second row did not even accumulate detail');
});

test('L4: the published binary alarmed on a healed hole', () => {
  const f2 = face(published, 'F2');
  assert.equal(f2.filled, 1, 'the hole was filled');
  assert.equal(f2.quiet, false, 'and the ledger still alarmed');
  assert.equal(f2.statusGaps, 2, 'with the phantom second row');
  assert.equal(f2.statusGapsOpen, 1, 'the field every caller reads');
});

test('L5: the published alarm never stopped, even on a clean verify', () => {
  const f3 = face(published, 'F3');
  assert.equal(f3.complete, true, 'the chain and the journal agree');
  assert.equal(f3.quiet, false, 'and the ledger still reported an open gap');
  assert.equal(f3.statusGapsOpen, 1, 'permanently');
});

test('L6: the fixed tree counts EPISODES — one row per hole', () => {
  assert.equal(fixed.verdict, 'fixed-and-guards-held');
  const f1 = face(fixed, 'F1');
  assert.equal(f1.oneRow, true, 'F1: the repeated scan reused the open row');
  assert.equal(f1.rowsAfterFirst, 1);
  assert.equal(f1.rowsAfterSecond, 1, 'one hole, one row');
  assert.ok(
    f1.detail.includes('verify found 1 signature(s)') &&
      f1.detail.split('verify found').length - 1 === 2,
    `both scans accumulated on the row: ${f1.detail}`,
  );
});

test('L7: the fixed tree heals the episode and stops the alarm', () => {
  const f2 = face(fixed, 'F2');
  assert.equal(f2.quiet, true, 'F2: status reports no open gap after the heal');
  assert.equal(f2.statusGaps, 1, 'the history keeps the episode');
  assert.equal(f2.statusBackfilled, 1, 'the fill is counted once');
  const f3 = face(fixed, 'F3');
  assert.equal(f3.quiet, true, 'and a clean verify after the heal stays quiet');
  assert.equal(f3.statusGapsOpen, 0, 'permanently');
});

test('L8: guards hold on BOTH trees', () => {
  for (const report of [published, fixed]) {
    const g1 = face(report, 'G1');
    assert.equal(g1.historyKept, true, `G1 history kept on ${report.label}`);
    const g3 = face(report, 'G3');
    assert.equal(g3.noGaps, true, `G3 no gaps on a healthy journal on ${report.label}`);
  }
  // G2 only holds on the fixed tree: the published binary inserts rows.
  assert.equal(face(fixed, 'G2').accumulated, true, 'the fixed row accumulates detail');
  assert.equal(face(published, 'G2').accumulated, false, 'and the published one does not');
});

test('L9: both artifacts name the same tree lineage and carry no host path', () => {
  assert.equal(published.label, 'v0.7.26-published');
  assert.equal(fixed.label, 'fix-tree');
  assert.equal(fixed.kind, published.kind, 'same measurement, two trees');
  assert.match(fixed.binarySha256, /^[0-9a-f]{64}$/);
  const all = JSON.stringify([published, fixed]);
  assert.ok(!all.includes('/tmp/'), 'artifact must not carry a host path');
  assert.ok(!all.includes('/home/'), 'artifact must not carry a host path');
});

test('L10: the PUBLISHED v0.7.27 asset carries the fix a stranger installs', () => {
  // Fixed: the measurement ran against the asset downloaded from the
  // v0.7.27 release (archive sha256 b0e95493… == the digest the release
  // API publishes, binary e9827faf… reports 'reeflog 0.7.27').
  assert.equal(published27.kind, 'gap-ledger-episode-accounting');
  assert.equal(published27.verdict, 'fixed-and-guards-held');
  assert.equal(published27.binaryVersion, 'reeflog 0.7.27');
  assert.equal(published27.binary, 'reeflog');
  assert.equal(
    published27.binarySha256,
    'e9827faf8371dbee23303f1f38dbd82a0595dacdf429493fe64cc5e7b86cba89',
    'published v0.7.27 binary sha256 changed — re-verify the release asset',
  );
  const f1 = face(published27, 'F1');
  assert.equal(f1.oneRow, true, 'one hole, one row — in the published bytes');
  assert.equal(face(published27, 'F2').quiet, true, 'a healed hole does not alarm');
  assert.equal(face(published27, 'F3').quiet, true, 'and the alarm never returns');
  const all = JSON.stringify(published27);
  assert.ok(!all.includes('/tmp/'), 'artifact must not carry a host path');
  assert.ok(!all.includes('/home/'), 'artifact must not carry a host path');
});
