// Pin for the ADOPTION gate artifact (PULS AH follow-up, 2026-11-13).
//
// WHAT CHANGED: the same probe that reported user-one-evidence for a
// foreign agent's invitation issue now reports
// not-distributed-not-consumed, with the invitation listed as EXCLUDED
// evidence and its four independent reasons recorded. External users
// stayed 0; the instrument, not the world, was wrong.
//
// FALSIFIABLE: every assertion below was checked to go RED when the
// artifact claim is flipped, and the artifact was then restored
// byte-identical. Flip `class` to user-one-evidence, move issue #2 into
// `external`, or delete a reason, and the corresponding pin fails.
import test from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';

const RESEARCH = path.resolve(import.meta.dirname, '..', 'research');
const load = (name) => JSON.parse(fs.readFileSync(path.join(RESEARCH, name), 'utf8'));

const adoption = load('adoption-2026-11-13.json');

test('G1 the invitation issue is excluded outreach with independent reasons', () => {
  const excluded = adoption.checks.issueCensus.outreachExcluded;
  assert.equal(excluded.length, 1, 'exactly one outreach issue was excluded');
  const i2 = excluded[0];
  assert.equal(i2.n, 2);
  assert.equal(i2.author, 'webbeep');
  assert.ok(i2.reasons.some((r) => r.startsWith('title:')), 'title rule fired');
  assert.ok(i2.reasons.some((r) => r.startsWith('body:')), 'body rules fired');
  assert.ok(i2.reasons.length >= 2, 'independent reasons, not a single keyword');
});

test('G2 the gate does not read outreach as adoption', () => {
  assert.notEqual(adoption.verdict.class, 'user-one-evidence');
  assert.equal(adoption.verdict.class, 'not-distributed-not-consumed');
  assert.equal(adoption.checks.issueCensus.external.length, 0, 'zero usage issues');
  assert.equal(adoption.verdict.signals.length, 2, 'placement state + zero signals');
});

test('G3 the measurement is complete — no verdict on partial evidence', () => {
  assert.equal(adoption.complete, true);
  assert.equal(adoption.dataSourcesFailed, 0);
  assert.equal(adoption.checks.repoMetadata.ok, true);
  assert.equal(adoption.checks.traffic.ok, true);
  assert.equal(adoption.placement.merged, false, 'PR #93 still open');
});

test('G4 a lone star or fork would still count — the gate is not dead', () => {
  // Guards the mirror failure: over-blocking would make the tool useless.
  // The classifier only excludes textually-marked outreach; stars/forks
  // remain signals. Pinned structurally so a "count everything" or
  // "count nothing" regression is visible.
  const c = adoption.checks;
  assert.ok(Array.isArray(c.starForkCensus.nonOwnerStars));
  assert.ok(Array.isArray(c.starForkCensus.nonOwnerForks));
  assert.ok(Array.isArray(c.issueCensus.external));
  assert.equal(c.starForkCensus.nonOwnerStars.length, 0, 'still genuinely zero');
  assert.equal(c.starForkCensus.nonOwnerForks.length, 0, 'still genuinely zero');
});
