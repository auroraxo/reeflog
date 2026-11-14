// Pin for the WEBHOOK-LIMIT-ZERO / PAGE-DOMAIN measurement (PULS AI,
// 2026-11-14).
//
// FALSIFIABLE, like every house pin: each assertion below was checked to go
// RED when the artifact claim is flipped, and both artifacts were then
// restored byte-identical.
//
// What the artifacts prove:
//   * the PUBLISHED v0.7.25 binary carries three defects: `--limit 0` on
//     `webhook push` delivered one event per pass and moved the cursor,
//     `--limit 0` on `webhook dlq replay` moved one quarantined letter, and
//     a bounded push report described the PAGE instead of the QUEUE;
//   * this tree carries none of them, and both guards hold on BOTH trees
//     (a positive limit still delivers exactly that many; the default pass
//     still drains the queue).
import test from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';

const RESEARCH = path.resolve(import.meta.dirname, '..', 'research');
const load = (name) => JSON.parse(fs.readFileSync(path.join(RESEARCH, name), 'utf8'));

const published = load('webhook-limit-zero-published-v0725-2026-11-14.json');
const fixed = load('webhook-limit-zero-fix-tree-2026-11-14.json');
const published26 = load('webhook-limit-zero-published-v0726-2026-11-14.json');

const face = (report, id) => {
  const f = report.faces.find((x) => x.id === id);
  assert.ok(f, `face ${id} missing from ${report.label}`);
  return f;
};

test('B1: the published v0.7.25 binary measured defect-present', () => {
  assert.equal(published.kind, 'webhook-limit-zero-page-domain');
  assert.equal(published.verdict, 'defect-present');
  assert.equal(published.binaryVersion, 'reeflog 0.7.25');
  // The binary is named by its own bytes — never by a host path.
  assert.equal(published.binary, 'reeflog');
  assert.match(published.binarySha256, /^[0-9a-f]{64}$/);
  assert.ok(!JSON.stringify(published).includes('/tmp/'), 'artifact must not carry a host path');
  assert.ok(!JSON.stringify(published).includes('/home/'), 'artifact must not carry a host path');
});

test('B2: the published binary sha256 is the published release asset digest', () => {
  // Fixed: the measurement ran against the PUBLISHED asset, extracted from
  // the release archive whose sha256 equals the digest the GitHub release
  // API publishes. Falsifier: any other sha256 here means the baseline was
  // NOT the published binary.
  assert.equal(
    published.binarySha256,
    'b40e03e02a038be94ffd5fde5623c2916559b4ee6e3246b6984e27990fe87c6f',
    'published v0.7.25 binary sha256 changed — re-verify the release asset',
  );
});

test('B3: `--limit 0` delivered on the published binary', () => {
  const z1 = face(published, 'Z1');
  assert.equal(z1.zeroDelivered, false, 'Z1: the published binary delivered on --limit 0');
  assert.equal(z1.noBytesLeft, false, 'and bytes left the machine');
  assert.equal(z1.cursorUnmoved, false, 'and the cursor moved');
  assert.equal(z1.cursor, 2, 'two passes consumed two of the five planted rows');
  assert.equal(z1.pageLimit, 'missing', 'the report did not even state the budget it was given');
});

test('B4: the published push report described the page, not the queue', () => {
  const z2 = face(published, 'Z2');
  assert.equal(z2.namesTheRest, false, 'Z2: nothing named the rows behind the page');
  assert.equal(z2.remainingBehindCursor, 'missing');
  assert.equal(z2.truncated, 'missing');
  assert.equal(z2.pageDeliveredTwo, true, 'the page itself delivered two');
  assert.equal(z2.exitCode, 0, 'and the pass read as clean');
});

test('B5: the published dlq replay moved a letter on --limit 0', () => {
  const z3 = face(published, 'Z3');
  assert.equal(z3.zeroReplayed, false, 'Z3: one quarantined letter was replayed');
  assert.equal(z3.replayed, 1);
  assert.equal(z3.bodies, 1, 'and its bytes left the machine');
  assert.equal(z3.lettersLeft, 1, 'and the letter left the queue');
});

test('F1: this tree delivers nothing on --limit 0', () => {
  const z1 = face(fixed, 'Z1');
  assert.equal(fixed.verdict, 'fixed-and-guards-held');
  assert.equal(z1.zeroDelivered, true);
  assert.equal(z1.noBytesLeft, true);
  assert.equal(z1.cursorUnmoved, true);
  assert.equal(z1.cursor, 0);
  assert.equal(z1.pageLimit, 0, 'the report states the budget it was given');
  assert.equal(z1.noRetractionLeft, true, 'the retraction channel honours zero too');
  assert.equal(z1.reorgCursor, 0);
});

test('F2: this tree names the rows behind the page', () => {
  const z2 = face(fixed, 'Z2');
  assert.equal(z2.namesTheRest, true);
  assert.equal(z2.remainingBehindCursor, 3, 'three rows were still behind the cursor');
  assert.equal(z2.truncated, true);
  assert.equal(z2.delivered, 2, 'the page still delivered exactly two');
  assert.equal(z2.exitCode, 0, 'a bounded pass that got through still exits 0');
});

test('F3: this tree moves no letter on a dlq replay --limit 0', () => {
  const z3 = face(fixed, 'Z3');
  assert.equal(z3.zeroReplayed, true);
  assert.equal(z3.replayed, 0);
  assert.equal(z3.bodies, 0);
  assert.equal(z3.lettersLeft, 2, 'both letters stay in the queue');
  assert.equal(z3.exitCode, 3, 'and the pass holds at exit 3, naming them');
  assert.equal(z3.namesThePending, true);
});

test('G1: the guards hold on BOTH trees', () => {
  for (const report of [published, fixed]) {
    const g1 = face(report, 'G1');
    assert.equal(g1.exact, true, `G1 on ${report.label}: a positive limit delivers exactly that many`);
    const g2 = face(report, 'G2');
    assert.equal(g2.delivered, 4, `G2 on ${report.label}: the default pass drains the queue`);
  }
});

test('B6: the artifact carries no host path', () => {
  for (const report of [published, fixed]) {
    const text = JSON.stringify(report);
    assert.ok(!text.includes('/tmp/'), `${report.label} must not carry /tmp/`);
    assert.ok(!text.includes('/home/'), `${report.label} must not carry /home/`);
    assert.ok(!text.includes('/Users/'), `${report.label} must not carry /Users/`);
  }
});

test('P1: the PUBLISHED v0.7.26 asset measures fixed-and-guards-held', () => {
  // The rule is what a stranger actually installs, not just this tree.
  assert.equal(published26.binaryVersion, 'reeflog 0.7.26');
  assert.equal(published26.verdict, 'fixed-and-guards-held');
  assert.equal(
    published26.binarySha256,
    'a49d8706ac930b03bda052d817486b226bf38602d6ad65342639bd4cbc63c8bf',
    'published v0.7.26 binary sha256 changed — re-verify the release asset',
  );
  const z1 = face(published26, 'Z1');
  assert.equal(z1.zeroDelivered, true);
  assert.equal(z1.noBytesLeft, true);
  assert.equal(z1.cursorUnmoved, true);
  assert.equal(z1.pageLimit, 0);
  const z2 = face(published26, 'Z2');
  assert.equal(z2.namesTheRest, true, 'the published report names the rows behind the page');
  const z3 = face(published26, 'Z3');
  assert.equal(z3.zeroReplayed, true);
  assert.equal(z3.exitCode, 3, 'and the pending letters keep the replay at exit 3');
});
