// Pin for the payload-hold measurements (2026-11-13).
//
// The defect was data loss behind the DEFAULT path: plain `reeflog tail`
// delivered a row whose payload the journal still owed (`logs IS NULL AND
// payload_state IS NULL`), advanced the cursor past it, exit 0 — and after
// `hydrate` filled the payload, the SAME cursor reported 0 events. The
// finished payload existed in the journal and never reached the consumer
// that had already been walked past it. This file pins the MEASUREMENTS so
// both sides stay measurements:
//
//   B1 the PUBLISHED v0.7.21 binary (sha256 == the extracted release asset,
//      digest == what the release API publishes) measured
//      payload-hold-defect-present: F1 the owed row delivered with
//      logs:null and the cursor past it, F2 the paid debt lost to the SAME
//      cursor, F2b a fresh cursor reads it (the payload existed);
//   B2 the guards held on the baseline: a decided row travels with its
//      marker (F4), --no-logs is never held (F5) — the defect is the hold
//      rule's absence, not chaos;
//   F1 the fix tree measured payload-hold-fixed: F1 not delivered / cursor
//      behind, F3 the hold NAMED (HELD + hydrate) at exit 3, F2 the paid
//      debt arrives on the same cursor WITH its payload;
//   F2 the PUBLISHED v0.7.22 asset measures payload-hold-fixed too — the
//      fix reached the release bytes a stranger downloads;
//   F3 every fixed face's guard stayed green (F4/F5 unchanged);
//   H  no host path in any artifact;
//   F  falsification by construction: every face is compared with ===
//      against literals, so an edited artifact cannot re-bless itself.
import test from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';

const dir = path.resolve(import.meta.dirname, '..', 'research');
const load = (name) => JSON.parse(fs.readFileSync(path.join(dir, name), 'utf8'));

const base = load('payload-hold-baseline-v0721-2026-11-13.json');
const fix = load('payload-hold-fix-tree-2026-11-13.json');
const published = load('payload-hold-published-v0722-2026-11-13.json');

const face = (report, id) => report.faces.find((f) => f.id === id);

test('B1: the published v0.7.21 binary measured payload-hold-defect-present', () => {
  assert.equal(base.verdict, 'payload-hold-defect-present');
  assert.equal(base.binaryVersion, 'reeflog 0.7.21');
  assert.equal(base.summary.defectVisible, true);
  assert.equal(base.summary.fixedHolds, false);
  // F1 — the defect: the owed row was delivered with logs:null, cursor past.
  const f1 = face(base, 'F1');
  assert.equal(f1.owedRowDelivered, true, 'the baseline delivered the owed row');
  assert.equal(f1.owedRowPayload, null, 'and it carried NO payload');
  assert.equal(f1.cursorPastTheOwedRow, true, 'the cursor advanced past it');
  assert.equal(f1.exitCode, 0, 'exit 0 claimed success');
  // F2 — the loss: after the debt was paid, the SAME cursor got nothing.
  const f2 = face(base, 'F2');
  assert.equal(f2.theFilledRowArrived, false, 'the paid payload was lost');
  assert.equal(f2.theFilledPayload, 'never delivered');
  assert.equal(f2.delivered.length, 0);
  // F3 — no hold was named.
  const f3 = face(base, 'F3');
  assert.equal(f3.namedTheHold, false);
  assert.equal(f3.exitCode, 0);
});

test('B2: the baseline guards were green — the defect was the missing hold, not chaos', () => {
  const f2b = face(base, 'F2b');
  assert.deepEqual(f2b.theFilledPayload, ['log B paid'],
    'a fresh cursor read the same row: the payload existed, only the walked-past cursor lost it');
  const f4 = face(base, 'F4');
  assert.equal(f4.exitCode, 0);
  assert.deepEqual(f4.marker, { logs: null, payload_state: 'unavailable' },
    'a decided row travels with its marker on the baseline too');
  const f5 = face(base, 'F5');
  assert.equal(f5.exitCode, 0);
  assert.equal(f5.compact, true, '--no-logs stays compact on the baseline too');
  assert.equal(f5.cursorAfter, 2);
});

test('F1: the fix tree measured payload-hold-fixed — hold, name, resume', () => {
  assert.equal(fix.verdict, 'payload-hold-fixed');
  assert.equal(fix.binaryVersion, 'reeflog 0.7.22');
  assert.equal(fix.summary.fixedHolds, true);
  assert.equal(fix.summary.defectGone, true);
  const f1 = face(fix, 'F1');
  assert.equal(f1.owedRowDelivered, false, 'the owed row is NOT delivered');
  assert.equal(f1.cursorPastTheOwedRow, false, 'the cursor stays behind it');
  assert.equal(f1.cursorAfter, 1, 'the row before the hold is the last consumed');
  const f3 = face(fix, 'F3');
  assert.equal(f3.exitCode, 3, 'a hold is exit 3, never success');
  assert.equal(f3.namedTheHold, true, 'the hold line names the row');
  assert.equal(f3.namesHydrate, true, 'and the command that fills it');
  const f2 = face(fix, 'F2');
  assert.equal(f2.theFilledRowArrived, true, 'the paid debt arrives on the SAME cursor');
  assert.deepEqual(f2.theFilledPayload, ['log B paid'], 'with its payload');
  assert.equal(f2.exitCode, 0);
});

test('F2: the PUBLISHED v0.7.22 asset measured payload-hold-fixed too', () => {
  assert.equal(published.verdict, 'payload-hold-fixed');
  assert.equal(published.binaryVersion, 'reeflog 0.7.22');
  assert.equal(published.binary, 'reeflog', 'the binary is named by its own bytes, not a path');
  // The asset that ships: name it by its digest so the claim is not one
  // download away from rotting.
  assert.match(published.binarySha256, /^[0-9a-f]{64}$/);
  const f1 = face(published, 'F1');
  assert.equal(f1.owedRowDelivered, false);
  assert.equal(f1.cursorAfter, 1);
  const f3 = face(published, 'F3');
  assert.equal(f3.exitCode, 3);
  assert.equal(f3.namedTheHold, true);
  const f2 = face(published, 'F2');
  assert.equal(f2.theFilledRowArrived, true);
  assert.deepEqual(f2.theFilledPayload, ['log B paid']);
});

test('F3: the guards stayed green on both fixed runs (the rule did not leak)', () => {
  for (const report of [fix, published]) {
    assert.equal(report.summary.guardsGreen, true);
    const f4 = face(report, 'F4');
    assert.equal(f4.exitCode, 0, 'a decided row must never wedge the consumer');
    assert.equal(f4.delivered.length, 3, 'decided rows are delivered, not skipped');
    assert.equal(f4.marker.payload_state, 'unavailable');
    const f5 = face(report, 'F5');
    assert.equal(f5.exitCode, 0, '--no-logs callers are never held');
    assert.equal(f5.delivered.length, 2);
    assert.equal(f5.compact, true);
    assert.equal(f5.cursorAfter, 2);
  }
});

test('H: no host path leaks into the artifacts', () => {
  for (const name of [
    'payload-hold-baseline-v0721-2026-11-13.json',
    'payload-hold-fix-tree-2026-11-13.json',
    'payload-hold-published-v0722-2026-11-13.json',
  ]) {
    const text = fs.readFileSync(path.join(dir, name), 'utf8');
    assert.doesNotMatch(text, /\/home\/|\/tmp\/|\/Users\/|C:\\/,
      `${name} must not pin the claim to one machine`);
  }
});
