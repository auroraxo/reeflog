// Pin for the webhook-payload-hold measurements (2026-11-13, PULS AF).
//
// The defect: `deliver_events` posted a row whose payload the journal still
// owes (`logs IS NULL AND payload_state IS NULL`) as a full, SIGNED event
// with `logs: null`, advanced the target's `next_seq` past it, and wrote a
// receipt — so when `hydrate` later paid the debt, the second pass delivered
// NOTHING. The finished event was permanently unreachable for that
// consumer, one channel over from the `tail` loss PULS AE fixed; the webhook
// is the channel for the consumer that is NOT on this machine, and its body
// is signed, so the receiver cannot tell a signature without content from a
// real event. This file pins the MEASUREMENTS so both sides stay
// measurements:
//
//   B1 the PUBLISHED v0.7.22 asset (sha256 1c43c8f7… == the bytes the
//      release API publishes) measured webhook-payload-hold-defect-present:
//      W1 the owed row posted with logs:null and the cursor past it, W2 the
//      paid debt never reached the SAME target;
//   B2 the guards held on the baseline: a full row delivers (G1), a
//      recorded outcome travels with its marker (G2) — the defect is the
//      hold rule's absence, not chaos;
//   F1 the fix tree measured webhook-payload-hold-fixed: the owed row is
//      not posted, the cursor stays behind it, the hold is named (exit 3),
//      and the paid debt arrives on the SAME target WITH its payload;
//   F2 the fixed faces' guards stayed green (G1/G2 unchanged);
//   H  no host path in any artifact;
//   F  falsification by construction: every face is compared with ===
//      against literals, so an edited artifact cannot re-bless itself.
import test from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';

const dir = path.resolve(import.meta.dirname, '..', 'research');
const load = (name) => JSON.parse(fs.readFileSync(path.join(dir, name), 'utf8'));

const base = load('webhook-payload-hold-baseline-v0722-2026-11-13.json');
const fix = load('webhook-payload-hold-fixed-2026-11-13.json');

const face = (report, id) => report.faces.find((f) => f.id === id);

test('B1: the published v0.7.22 asset measured webhook-payload-hold-defect-present', () => {
  assert.equal(base.verdict, 'webhook-payload-hold-defect-present');
  assert.equal(base.binaryVersion, 'reeflog 0.7.22');
  assert.equal(
    base.binarySha256,
    '1c43c8f7fe8b2a7c29d782ea6cb2db1dc7b1c22dfae7263626fdf8911980b6e8',
    'the measured binary is the bytes the release API publishes'
  );
  assert.equal(base.summary.defectVisible, true);
  assert.equal(base.summary.fixedHolds, false);
  // W1 — the defect: the owed row was POSTED with logs:null, cursor past.
  const w1 = face(base, 'W1');
  assert.equal(w1.owedRowPosted, true, 'the baseline posted the owed row');
  assert.equal(w1.owedPayloadInBody, null, 'and the body carried NO payload');
  assert.equal(w1.cursorPastTheOwedRow, true, 'the target cursor advanced past it');
  assert.equal(w1.exitCode, 0, 'exit 0 claimed success');
  // W2 — the loss: after the debt was paid, the SAME target got nothing.
  const w2 = face(base, 'W2');
  assert.equal(w2.theFilledRowArrived, false, 'the paid payload was lost');
  assert.equal(w2.theFilledPayload, 'never delivered');
  assert.equal(w2.posted.length, 0);
  // No hold was named on the baseline.
  const w3 = face(base, 'W3');
  assert.equal(w3.namedTheHold, false);
  assert.equal(w3.cursorBehindTheOwedRow, false);
});

test('B2: the guards held on the baseline (the defect is the rule, not chaos)', () => {
  const g1 = face(base, 'G1');
  assert.equal(g1.exitCode, 0);
  assert.equal(g1.posted.length, 1, 'a full row still delivered');
  assert.equal(g1.nextSeq, 1);
  assert.equal(g1.receipts, 1);
  const g2 = face(base, 'G2');
  assert.equal(g2.posted.length, 2, 'a recorded outcome still delivered');
  assert.deepEqual(g2.marker, { logs: null, payload_state: 'unavailable' });
});

test('F1: the fix tree measured webhook-payload-hold-fixed', () => {
  assert.equal(fix.verdict, 'webhook-payload-hold-fixed');
  assert.equal(fix.summary.fixedHolds, true);
  assert.equal(fix.summary.guardsGreen, true);
  // W1 — the owed row is NOT posted, the cursor stays behind it.
  const w1 = face(fix, 'W1');
  assert.equal(w1.owedRowPosted, false);
  assert.equal(w1.cursorPastTheOwedRow, false);
  assert.equal(w1.nextSeq, 1, 'the cursor stopped behind the owed row');
  assert.equal(w1.exitCode, 3, 'a held target is a blocked target');
  // W3 — the hold is named.
  const w3 = face(fix, 'W3');
  assert.equal(w3.exitCode, 3);
  assert.equal(w3.namedTheHold, true);
  assert.equal(w3.cursorBehindTheOwedRow, true);
  // W2 — the paid debt reaches the SAME target WITH its payload.
  const w2 = face(fix, 'W2');
  assert.equal(w2.theFilledRowArrived, true);
  assert.deepEqual(w2.theFilledPayload, ['log B paid']);
  assert.equal(w2.nextSeq, 2);
  assert.equal(w2.receipts, 2);
});

test('F2: the guards stayed green on the fix tree', () => {
  const g1 = face(fix, 'G1');
  assert.equal(g1.exitCode, 0);
  assert.equal(g1.posted.length, 1);
  const g2 = face(fix, 'G2');
  assert.equal(g2.posted.length, 2);
  assert.deepEqual(g2.marker, { logs: null, payload_state: 'unavailable' });
});

test('H: no artifact carries a host path', () => {
  for (const [name, report] of [['baseline', base], ['fix', fix]]) {
    const text = JSON.stringify(report);
    assert.equal(
      /\/home\/|\/Users\/|\/tmp\//.test(text),
      false,
      `${name} artifact must not carry a host path`
    );
  }
});
