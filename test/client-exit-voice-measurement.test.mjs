// PULS W — the EXIT-CODE VOICE measurement (2026-11-12).
//
// Pins `research/client-exit-voice-2026-11-12.json`: the published v0.7.19
// client classified only the documented codes (0/1/3) and treated every
// other close as SUCCESS (crash-after-rows, usage skew, panic) or forged a
// refusal (signal death); this tree refuses every one of them with the real
// code and the binary's own words.
//
// Falsification: a tampered face outcome in the artifact flips this test
// RED. No host paths may appear in the committed artifact.
import test from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';

const ARTIFACT = path.resolve(import.meta.dirname, '../research/client-exit-voice-2026-11-12.json');

test('EC1: the measurement artifact exists and proves contract-holds', () => {
  assert.ok(fs.existsSync(ARTIFACT), 'research artifact must be committed');
  const raw = fs.readFileSync(ARTIFACT, 'utf8');
  assert.doesNotMatch(raw, new RegExp(process.env.HOME || '/home/aurora'), 'no host paths in committed artifact');
  assert.doesNotMatch(raw, /\/tmp\//, 'no machine-local temp paths in committed artifact');
  const r = JSON.parse(raw);
  assert.equal(r.verdict, 'contract-holds');
  assert.equal(r.baselineShowsDefect, true);
  assert.equal(r.fixedClean, true);
});

test('EC2: the baseline published v0.7.19 shows all six defect faces', () => {
  const r = JSON.parse(fs.readFileSync(ARTIFACT, 'utf8'));
  assert.equal(r.baseline.tag, 'v0.7.19');
  assert.equal(r.baseline.baselineDefectCount, 6, 'every defect face must be visible on the baseline');
  const byMode = Object.fromEntries(r.baseline.faces.map((f) => [f.mode, f]));
  assert.equal(byMode['F1 tail crash-after-rows (exit 2)'].outcome, 'resolved',
    'the measured defect: a run that died with exit 2 resolved as a delivered batch');
  assert.equal(byMode['F1 tail crash-after-rows (exit 2)'].exitCode, 2);
  assert.equal(byMode['F2 tail usage-skew (exit 64)'].outcome, 'resolved',
    'an old binary\'s usage error resolved as a clean zero-event batch');
  assert.equal(byMode['F2b status usage-skew (exit 64)'].outcome, 'raw-crash',
    'the raw SyntaxError face: an engine crash no ReeflogError catch sees');
  assert.equal(byMode['F3 verify panic-after-report (exit 101)'].outcome, 'resolved',
    'a panicked scan resolved as a clean scan');
  assert.equal(byMode['F4 status killed-by-signal'].outcome, 'refused');
  assert.doesNotMatch(byMode['F4 status killed-by-signal'].message, /signal/i,
    'the forged refusal IS the defect: a kill reported as "exit 1" naming no signal');
});

test('EC3: this tree refuses every defect face with the real code and the binary\'s words', () => {
  const r = JSON.parse(fs.readFileSync(ARTIFACT, 'utf8'));
  assert.equal(r.fixed.fixedCleanCount, 6, 'every face must be clean on the fixed tree');
  const byMode = Object.fromEntries(r.fixed.faces.map((f) => [f.mode, f]));
  assert.equal(byMode['F1 tail crash-after-rows (exit 2)'].outcome, 'refused');
  assert.equal(byMode['F1 tail crash-after-rows (exit 2)'].exitCode, 2);
  assert.match(byMode['F1 tail crash-after-rows (exit 2)'].message, /journal write failed/,
    'the binary\'s last words must reach the verdict');
  assert.equal(byMode['F2 tail usage-skew (exit 64)'].exitCode, 64);
  assert.match(byMode['F2 tail usage-skew (exit 64)'].message, /version skew|does not know/);
  assert.equal(byMode['F2b status usage-skew (exit 64)'].outcome, 'refused',
    'the raw crash must be gone');
  assert.equal(byMode['F3 verify panic-after-report (exit 101)'].exitCode, 101);
  assert.match(byMode['F3 verify panic-after-report (exit 101)'].message, /panicked/);
  assert.match(byMode['F4 status killed-by-signal'].message, /SIGTERM/,
    'the signal must be named');
});

test('EC4: the guards held on BOTH trees (healthy 0, refusal voice, tail-hold, verify-incomplete)', () => {
  const r = JSON.parse(fs.readFileSync(ARTIFACT, 'utf8'));
  for (const side of ['baseline', 'fixed']) {
    assert.equal(r[side].guardsHold, true, `${side}: all four guards must hold`);
    const byMode = Object.fromEntries(r[side].guards.map((g) => [g.mode, g]));
    assert.equal(byMode['G1 healthy exit 0'].holds, true);
    assert.equal(byMode['G2 exit 1 refusal'].holds, true);
    assert.equal(byMode['G3a tail exit 3 hold'].holds, true);
    assert.equal(byMode['G3b verify exit 3 incomplete'].holds, true);
  }
});
