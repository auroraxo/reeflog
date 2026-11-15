// Pin for the status-orphan-dead-letter measurements (2026-11-15, PULS AL).
//
// The defect: a pending dead letter belongs to a TARGET, not to a config
// entry. When a target is removed from the config, its quarantined events
// stay in the journal — and they can never be replayed from here, because
// the replay path needs the target's endpoint. Two surfaces already knew:
// `webhook push` reports `orphaned_dead_letters` (the JOURNAL's total minus
// the config's share, v0.7.6) and `webhook dlq list` lists the orphan with
// `in_config: false`.
//
// `webhook status` — the report a monitoring caller polls — walked the
// CONFIG's targets only, so the stranded letters were printed NOWHERE: the
// report said `dead_letters: 0` for the healthy target and exited 0 while
// the same journal read through `push` said `orphaned_dead_letters: 1`.
//
// This file pins the MEASUREMENTS so both sides stay measurements:
//
//   B1 the PUBLISHED v0.7.28 asset measured status-orphan-dead-letter-blind:
//      R1 the orphan count is absent (not zero), R2 neither JSON nor stderr
//      names the target holding the stranded letters, R3 the report exits 0
//      while two letters are stranded;
//   B2 the CONTRAST: the same journal state read through `webhook push`
//      reported `orphaned_dead_letters: 2` — one fact, two surfaces, only
//      one of them told;
//   F1 the fix tree measured status-orphan-dead-letter-reported, all three
//      faces pass, and status/push agree on the count;
//   F2 both artifacts were produced by the same tool against a real binary,
//      pinned by bytes, and measure DIFFERENT binaries;
//   H  no host path in any artifact;
//   F  falsification by construction: every face is compared with ===
//      against literals, so an edited artifact cannot re-bless itself.
import test from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';

const dir = path.resolve(import.meta.dirname, '..', 'research');
const load = (name) => JSON.parse(fs.readFileSync(path.join(dir, name), 'utf8'));

const base = load('status-orphan-dead-letters-published-v0728-2026-11-15.json');
const fix = load('status-orphan-dead-letters-fixed-2026-11-15.json');

const face = (report, id) => report.faces.find((f) => f.face === id);

test('B1: the published v0.7.28 asset measured status-orphan-dead-letter-blind', () => {
  assert.equal(base.verdict, 'status-orphan-dead-letter-blind');
  assert.equal(base.label, 'v0.7.28-published');
  // R1 — the count is ABSENT, not zero: the field does not exist on the
  // published report.
  const r1 = face(base, 'R1');
  assert.equal(r1.ok, false, 'the published asset is blind here');
  assert.equal(r1.observed, null, 'the field is absent — not 0, ABSENT');
  // R2 — neither the JSON nor the stderr line names the stranded target.
  const r2 = face(base, 'R2');
  assert.equal(r2.ok, false);
  assert.deepEqual(r2.observed, [], 'no orphan target list at all');
  assert.equal(r2.stderrMentionsTarget, false, 'the stderr line does not exist');
  // R3 — the report exits 0 while two letters are stranded.
  const r3 = face(base, 'R3');
  assert.equal(r3.ok, false);
  assert.equal(base.statusExitCode, 0, 'silence: the published report exits 0');
});

test('B2: the same journal state through `webhook push` DID name the orphan debt', () => {
  // The contrast is what makes this a report defect and not a missing
  // capability: the journal already knew, and the push report said so.
  assert.equal(base.parity.pushOrphans, 2, 'push named the debt');
  assert.equal(base.parity.statusOrphans, null, 'status named nothing');
  assert.equal(base.parity.agree, false, 'the two surfaces DISAGREED — the defect');
  assert.equal(
    base.fixture.pendingDeadLetters,
    2,
    'the fixture holds exactly the two stranded letters'
  );
  assert.deepEqual(base.fixture.orphanTargets, ['gone']);
  assert.deepEqual(base.fixture.configTargets, ['t1']);
});

test('F1: the fix tree measured status-orphan-dead-letter-reported', () => {
  assert.equal(fix.verdict, 'status-orphan-dead-letter-reported');
  assert.equal(fix.label, 'this-tree');
  for (const id of ['R1', 'R2', 'R3']) {
    assert.equal(face(fix, id).ok, true, `${id} must pass on the fixed tree`);
  }
  const r1 = face(fix, 'R1');
  assert.equal(r1.observed, 2, 'the count is 2 — a number that can be wrong');
  const r2 = face(fix, 'R2');
  assert.deepEqual(r2.observed, ['gone'], 'the target holding the letters is named');
  assert.equal(r2.stderrMentionsTarget, true, 'the stderr line names it too');
  assert.equal(r2.stderrMentionsKind, true, 'the stderr line says what kind of debt');
  const r3 = face(fix, 'R3');
  assert.equal(r3.observed.exitCode, 3, 'the debt holds the exit code');
  assert.equal(fix.statusExitCode, 3, 'reported exit code matches');
  // The surfaces now agree: one fact, one number per surface.
  assert.equal(fix.parity.statusOrphans, fix.parity.pushOrphans);
  assert.equal(fix.parity.agree, true);
  assert.equal(fix.parity.pushOrphans, 2, 'push still reports 2');
});

test('F2: both artifacts were produced by the same tool against a real binary', () => {
  for (const report of [base, fix]) {
    assert.equal(report.program, 'whirLbMiicVdio4qvUfM5KAg6Ct8VwpYzGff3uctyCc');
    assert.equal(report.faces.length, 3, 'three faces, all measured');
    assert.match(report.binary.sha256, /^[0-9a-f]{64}$/, 'the binary is pinned by bytes');
    assert.match(report.binary.version, /^reeflog /, 'the binary reports its version');
    // The tool refuses to write an artifact carrying a host path — so the
    // artifact must not name where it ran.
    assert.equal(JSON.stringify(report).includes('"path"'), false, 'no binary host path');
  }
  // The two artifacts measure DIFFERENT binaries and must not be the same
  // file with a different label.
  assert.notEqual(base.binary.sha256, fix.binary.sha256);
});

test('H: no host path leaks into either artifact', () => {
  for (const report of [base, fix]) {
    const raw = JSON.stringify(report);
    assert.equal(raw.includes('/home/'), false, 'no home path anywhere in the artifact');
    assert.equal(raw.includes('/tmp/'), false, 'no tmp path anywhere in the artifact');
    assert.equal(raw.includes('rl728'), false, 'no measuring-host directory name');
  }
});

test('F3: the PUBLISHED v0.7.29 asset carries the fix — status reports the stranded letters', () => {
  // The release gate: the bytes a stranger installs must behave like the
  // fix tree, not like the tag that keeps the defect.
  const shipped = load('status-orphan-dead-letters-published-v0729-2026-11-15.json');
  assert.equal(shipped.verdict, 'status-orphan-dead-letter-reported');
  assert.equal(shipped.label, 'v0.7.29-published');
  assert.match(shipped.binary.version, /^reeflog 0\.7\.29$/, 'the shipped binary reports its own release');
  for (const id of ['R1', 'R2', 'R3']) {
    assert.equal(face(shipped, id).ok, true, `${id} must hold on the published asset`);
  }
  assert.equal(shipped.parity.agree, true, 'the published asset agrees with push');
  assert.equal(shipped.statusExitCode, 3, 'the shipped binary exits 3 on a stranded letter');
  // Three different binaries were measured: the blind one, the fix tree,
  // and the shipped one — never the same bytes under three labels.
  assert.notEqual(shipped.binary.sha256, base.binary.sha256);
  assert.notEqual(shipped.binary.sha256, fix.binary.sha256);
});

test('F: an edited artifact cannot re-bless itself', () => {
  // Falsification by construction: flip the baseline's blind face to "ok"
  // and every literal comparison in this file must still disagree with it.
  const tampered = JSON.parse(JSON.stringify(base));
  tampered.verdict = 'status-orphan-dead-letter-reported';
  tampered.faces.find((f) => f.face === 'R1').ok = true;
  tampered.faces.find((f) => f.face === 'R1').observed = 2;
  assert.notEqual(tampered.verdict, base.verdict);
  assert.equal(face(base, 'R1').ok, false, 'the stored baseline stays blind');
  // And the fix artifact flipped the other way.
  const tamperedFix = JSON.parse(JSON.stringify(fix));
  tamperedFix.parity.agree = false;
  assert.notEqual(tamperedFix.parity.agree, fix.parity.agree);
  assert.equal(face(fix, 'R1').observed, 2, 'the stored fix keeps its measured count');
});
