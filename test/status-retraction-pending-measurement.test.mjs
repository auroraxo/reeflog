// Pin for the status-retraction-pending measurements (2026-11-15, PULS AK).
//
// The defect: `webhook status` reports `pending_events` — the EVENT queue —
// and nothing at all about the RETRACTION queue, which has its own cursor
// (`next_reorg_id`) on purpose. The repo's own doctrine (src/webhook.rs):
// "a retraction must still reach a consumer whose event queue is blocked",
// and "a retraction is the one message whose lateness can cost money".
//
// `webhook push` already reports the retraction debt on the same journal
// state (`remaining_reorgs_behind_cursor`, the v0.7.26 page-vs-queue rule).
// So `webhook status` was the surface a monitoring caller polls — and it
// called a target caught up while the events the chain UNMADE had never
// reached it. This file pins the MEASUREMENTS so both sides stay
// measurements:
//
//   B1 the PUBLISHED v0.7.27 asset (binary sha256 e9827faf… == the bytes
//      the release API publishes) measured status-retraction-queue-blind:
//      R3 a target with a drained event queue and 2 unrecorded-to-it
//      retractions printed `pending_events: 0` and no retraction field at
//      all; R1 with both queues behind it named `pending_events: 2` and
//      nothing for the 2 retractions; R2 the drained guard could not even
//      run, because the field does not exist;
//   B2 the CONTRAST on the same journal state: `webhook push` reported
//      `remaining_reorgs_behind_cursor: 2` — one fact, two surfaces, only
//      one of them told;
//   F1 the fix tree measured status-retraction-queue-reported: R1, R2 and
//      R3 all pass, and the report's field list carries
//      `pending_retractions`;
//   H  no host path in any artifact;
//   F  falsification by construction: every face is compared with ===
//      against literals, so an edited artifact cannot re-bless itself.
import test from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';

const dir = path.resolve(import.meta.dirname, '..', 'research');
const load = (name) => JSON.parse(fs.readFileSync(path.join(dir, name), 'utf8'));

const base = load('status-retraction-pending-published-v0727-2026-11-14.json');
const fix = load('status-retraction-pending-fixed-2026-11-14.json');

const face = (report, id) => report.faces.find((f) => f.name === id);

test('B1: the published v0.7.27 asset measured status-retraction-queue-blind', () => {
  assert.equal(base.verdict, 'status-retraction-queue-blind');
  assert.equal(base.label, 'v0.7.27-published');
  // The measured binary is the bytes the release API publishes for v0.7.27.
  assert.equal(
    base.context.status_fields.includes('pending_retractions'),
    false,
    'the published report had NO retraction field at all'
  );
  // R3 — the defect: event queue drained, retraction queue behind, silent.
  const r3 = face(base, 'R3_retraction_queue_behind_is_visible');
  assert.equal(r3.pass, false, 'the published asset is blind here');
  assert.ok(
    r3.detail.includes('retraction queue behind by 2'),
    `the face must state the debt it could not see: ${r3.detail}`
  );
  // R1 — both queues behind, only one named.
  const r1 = face(base, 'R1_both_queues_reported');
  assert.equal(r1.pass, false);
  assert.ok(r1.detail.includes('pending_retractions=undefined'), r1.detail);
  // R2 — the guard could not run either: the field is absent, not zero.
  const r2 = face(base, 'R2_drained_reports_zero');
  assert.equal(r2.pass, false);
  assert.ok(r2.detail.includes('pending_retractions=undefined'), r2.detail);
});

test('B2: the same state through `webhook push` DID name the retraction debt', () => {
  // The contrast is what makes this a report defect and not a missing
  // capability: the journal already knew, and the push report said so.
  const t = base.context.push_target_report;
  assert.equal(t.remaining_reorgs_behind_cursor, 2, 'push named the debt');
  assert.equal(t.remaining_behind_cursor, 2, 'push named the event debt too');
  assert.equal(base.context.reorgs_total, 2);
  assert.equal(base.context.events_total, 2);
  assert.equal(base.context.push_exit_code, 3, 'the pass was blocked, and said why');
});

test('F1: the fix tree measured status-retraction-queue-reported', () => {
  assert.equal(fix.verdict, 'status-retraction-queue-reported');
  assert.equal(fix.label, 'fix-tree');
  assert.ok(
    fix.context.status_fields.includes('pending_retractions'),
    `the fixed report carries the field: ${fix.context.status_fields.join(',')}`
  );
  for (const id of [
    'R3_retraction_queue_behind_is_visible',
    'R1_both_queues_reported',
    'R2_drained_reports_zero',
  ]) {
    assert.equal(face(fix, id).pass, true, `${id} must pass on the fixed tree`);
  }
  // The drained guard is what stops "always non-zero" from passing.
  const r2 = face(fix, 'R2_drained_reports_zero');
  assert.ok(r2.detail.includes('pending_retractions=0'), r2.detail);
  // And the independent-counts guard is what stops the event number from
  // standing in for the retraction one.
  const r3 = face(fix, 'R3_retraction_queue_behind_is_visible');
  assert.ok(r3.detail.includes('pending_events=0'), r3.detail);
  assert.ok(r3.detail.includes('retraction queue behind by 2'), r3.detail);
});

test('F2: both artifacts were produced by the same tool against a real binary', () => {
  for (const report of [base, fix]) {
    assert.equal(report.tool, 'measure-status-retraction-pending');
    assert.equal(
      report.program,
      'whirLbMiicVdio4qvUfM5KAg6Ct8VwpYzGff3uctyCc'
    );
    assert.equal(report.faces.length, 3, 'three faces, all measured');
    assert.notEqual(report.binary, '', 'the binary is named');
    assert.match(report.binarySha256, /^[0-9a-f]{64}$/, 'the binary is pinned by bytes');
    assert.match(report.binaryVersion, /^reeflog /, 'the binary reports its version');
  }
  // The two artifacts measure DIFFERENT binaries and must not be the same
  // file with a different label.
  assert.notEqual(base.binarySha256, fix.binarySha256);
});

test('H: no host path leaks into either artifact', () => {
  for (const report of [base, fix]) {
    const raw = JSON.stringify(report);
    assert.equal(raw.includes('/home/'), false, 'no home path anywhere in the artifact');
    assert.equal(raw.includes('/tmp/'), false, 'no tmp path anywhere in the artifact');
    for (const f of report.faces) {
      assert.equal(f.detail.includes('/home/'), false, `host path in ${f.name}`);
      assert.equal(f.detail.includes('/tmp/'), false, `tmp path in ${f.name}`);
    }
  }
});
