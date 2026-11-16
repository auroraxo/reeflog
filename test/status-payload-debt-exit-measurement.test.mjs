// Pin for the status payload-debt measurements (2026-11-16, PULS AM).
//
// The defect: `status` is the report a MONITORING caller polls — it asks
// "is this journal whole?" and reads the exit code. It prints
// `payload_pending`, the count of rows the journal holds a SIGNATURE for but
// not what happened (`logs IS NULL AND payload_state IS NULL`) — the exact
// predicate the default payload-bearing `tail` HOLDS at (cursor stays, exit
// 3, `hydrate` named), the one `webhook push` holds its target cursor at
// (since v0.7.23), the one `hydrate` exits 3 on while `pending_left` is
// non-zero, and the one the daemon exits 3 on at shutdown.
//
// On the PUBLISHED v0.7.30 asset the report printed `payload_pending: 1`
// and exited 0, while `tail` on the same journal held and exited 3.
//
// This file pins the MEASUREMENTS so both sides stay measurements:
//
//   B1 the PUBLISHED v0.7.30 asset measured status-payload-debt-exit-blind:
//      D1 the report exits 0 while it prints the debt, D2 status and tail
//      DISAGREE on the exit code for the same journal, D3 the verdict does
//      not track the debt (paying it changes nothing);
//   B2 the CONTRAST: on the same journal the payload-bearing tail DID hold
//      and DID exit 3 — one fact, two surfaces, only one of them told;
//   F1 the fix tree measured status-payload-debt-exit-held: all three defect
//      faces pass, and the three guard faces stay green on BOTH trees (the
//      verdict must not widen to the opt-in queues, to recorded outcomes or
//      to an open gap);
//   F2 both artifacts were produced by the same tool against real binaries,
//      pinned by bytes, and they measure DIFFERENT binaries;
//   H  no host path in any artifact;
//   F  falsification by construction: every face is compared with ===
//      against literals, so an edited artifact cannot re-bless itself.
import test from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';

const dir = path.resolve(import.meta.dirname, '..', 'research');
const load = (name) => JSON.parse(fs.readFileSync(path.join(dir, name), 'utf8'));

const base = load('status-payload-debt-exit-published-v0730-2026-11-16.json');
const fix = load('status-payload-debt-exit-fixed-tree-2026-11-16.json');

const face = (report, id) => report.faces.find((f) => f.face === id);

// The digest the Release API publishes for the v0.7.30 linux asset — the
// same bytes the existing webhook measurement pinned, so a swapped binary
// cannot pass as the published one.
const PUBLISHED_V0730_SHA =
  'b1eac6c03d3b4aa3cf0e3a31d4bc6e746afd31b3b835dfdbcdb80629552742fa';

test('B1: the published v0.7.30 asset measured status-payload-debt-exit-blind', () => {
  assert.equal(base.verdict, 'status-payload-debt-exit-blind');
  assert.equal(base.label, 'published-v0.7.30');
  assert.equal(base.binary.sha256, PUBLISHED_V0730_SHA, 'the artifact names the published asset by digest');
  assert.equal(base.binary.version, 'reeflog 0.7.30', 'the version is read from that binary');

  // D1 — the report prints the debt and exits 0.
  const d1 = face(base, 'D1');
  assert.equal(d1.ok, false, 'the published asset is blind here');
  assert.equal(d1.observed.exitCode, 0, 'silence: the published report exits 0');
  assert.equal(d1.observed.printed, 1, 'and it PRINTS the debt it calls health');

  // D2 — status and tail DISAGREE on the same journal.
  const d2 = face(base, 'D2');
  assert.equal(d2.ok, false);
  assert.equal(d2.observed.status, 0);
  assert.equal(d2.observed.tail, 3, 'the payload-bearing tail DOES hold');

  // D3 — the verdict does not track the debt.
  const d3 = face(base, 'D3');
  assert.equal(d3.ok, false);
  assert.equal(d3.observed.before, 0);
  assert.equal(d3.observed.after, 0, 'paying the debt changes nothing');
});

test('B2: the guards were green on the published asset too', () => {
  // The defect faces must be red because of the EXIT CODE, not because the
  // fixture was broken: the guards on the SAME published binary are green.
  for (const id of ['G1', 'G2', 'G3']) {
    assert.equal(face(base, id).ok, true, `${id} must be green on the published asset`);
  }
  assert.equal(face(base, 'G1').observed.exitCode, 0, 'a clean journal was already exit 0');
  assert.equal(face(base, 'G2').observed.unavailable, 1);
  assert.equal(face(base, 'G2').observed.invalid, 1);
  assert.equal(face(base, 'G3').observed.accountsMissing, 5, 'the opt-in queue was counted, as before');
});

test('F1: the fix tree measured status-payload-debt-exit-held', () => {
  assert.equal(fix.verdict, 'status-payload-debt-exit-held');
  assert.equal(fix.label, 'fixed-tree');
  for (const id of ['D1', 'D2', 'D3']) {
    assert.equal(face(fix, id).ok, true, `${id} must pass on the fixed tree`);
  }
  const d1 = face(fix, 'D1');
  assert.equal(d1.observed.exitCode, 3, 'the debt holds the exit code');
  assert.equal(d1.observed.printed, 1, 'and the count is still printed');
  const d2 = face(fix, 'D2');
  assert.equal(d2.observed.status, d2.observed.tail, 'one fact, one verdict');
  assert.equal(d2.observed.status, 3);
  const d3 = face(fix, 'D3');
  assert.equal(d3.observed.before, 3, 'owed');
  assert.equal(d3.observed.after, 0, 'paid — the verdict comes back down');
});

test('F2: the guards stayed green on the fix tree — the verdict did not widen', () => {
  for (const id of ['G1', 'G2', 'G3']) {
    assert.equal(face(fix, id).ok, true, `${id} must stay green on the fixed tree`);
  }
  const g1 = face(fix, 'G1');
  assert.equal(g1.observed.exitCode, 0, 'a clean journal still exits 0');
  assert.equal(g1.observed.pending, 0);
  const g2 = face(fix, 'G2');
  assert.equal(g2.observed.exitCode, 0, 'a recorded outcome is not a debt');
  assert.equal(g2.observed.pending, 0);
  const g3 = face(fix, 'G3');
  assert.equal(g3.observed.exitCode, 0, 'the opt-in queues do not hold the verdict');
  assert.equal(g3.observed.accountsMissing, 5, 'and they are still counted');
});

test('F3: both artifacts measured DIFFERENT binaries, by bytes', () => {
  assert.notEqual(fix.binary.sha256, base.binary.sha256, 'the fix tree is not the published asset');
  assert.match(fix.binary.sha256, /^[0-9a-f]{64}$/);
  assert.match(base.binary.sha256, /^[0-9a-f]{64}$/);
});

test('H: no artifact carries a host path', () => {
  for (const [name, artifact] of [['published', base], ['fixed', fix]]) {
    const text = JSON.stringify(artifact);
    assert.doesNotMatch(text, /"\/(tmp|home|root)\//, `${name}: a host path is in the artifact`);
    assert.doesNotMatch(text, /\/tmp\/reeflog-/, `${name}: a fixture dir is in the artifact`);
  }
});

test('F: the rule the artifact carries names the surfaces that already held this code', () => {
  // The artifact is not just numbers: it carries the REASON, so a later
  // reader knows which surfaces already disagreed with it.
  const rule = base.rule.join(' ');
  assert.match(rule, /payload_pending/);
  assert.match(rule, /tail/);
  assert.match(rule, /hydrate/);
  assert.match(rule, /webhook push/);
  assert.match(rule, /recorded outcomes/);
  assert.match(rule, /opt-in/);
});
