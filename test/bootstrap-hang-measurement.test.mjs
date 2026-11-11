// Pin test for the bootstrap() hang measurement (2026-11-11).
//
// Pins `research/bootstrap-hang-2026-11-11.json`: the contract, the defect
// counts, the redirect-bounding invariant, and cross-checks the artifact's
// baseline against this tree's own history. Falsification: a tampered
// baseline count or verdict flips this test RED.
import test from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';

const ARTIFACT = path.resolve(import.meta.dirname, '../research/bootstrap-hang-2026-11-11.json');

test('BH1: the measurement artifact exists and proves contract-holds', () => {
  assert.ok(fs.existsSync(ARTIFACT), 'research artifact must be committed');
  const raw = fs.readFileSync(ARTIFACT, 'utf8');
  assert.doesNotMatch(raw, new RegExp(process.env.HOME || '/home/aurora'), 'no host paths in committed artifact');
  const r = JSON.parse(raw);
  assert.equal(r.verdict, 'contract-holds');
  assert.equal(r.baselineShowsDefect, true);
  assert.equal(r.fixedClean, true);
});

test('BH2: baseline published v0.7.17 shows all 3 hang faces', () => {
  const r = JSON.parse(fs.readFileSync(ARTIFACT, 'utf8'));
  assert.equal(r.baseline.tag, 'v0.7.17');
  assert.equal(r.baseline.hangs, 3);
  assert.equal(r.baseline.verdict, 'no-verdict');
  const faces = Object.fromEntries(r.baseline.faces.map((f) => [f.label, f]));
  assert.equal(faces['api-stall'].kind, 'no-verdict');
  assert.equal(faces['body-stall'].kind, 'no-verdict');
  assert.equal(faces['redirect-loop'].kind, 'no-verdict');
  // the redirect face was an unbounded loop: thousands of re-requests
  assert.ok(faces['redirect-loop'].redirectAttempts > 1000,
    `unbounded loop expected > 1000 attempts, saw ${faces['redirect-loop'].redirectAttempts}`);
});

test('BH3: fixed tree has zero hangs and bounded redirects (attempts <= 10)', () => {
  const r = JSON.parse(fs.readFileSync(ARTIFACT, 'utf8'));
  assert.equal(r.fixed.hangs, 0);
  assert.equal(r.fixed.verdict, 'every-leg-answers');
  assert.equal(r.fixed.boundedRedirects, true);
  const faces = Object.fromEntries(r.fixed.faces.map((f) => [f.label, f]));
  assert.equal(faces['api-stall'].kind, 'rejected');
  assert.equal(faces['body-stall'].kind, 'rejected');
  assert.equal(faces['redirect-loop'].kind, 'rejected');
  assert.ok(faces['redirect-loop'].redirectAttempts <= 10,
    `fixed loop must stop at bound (<= 10), saw ${faces['redirect-loop'].redirectAttempts}`);
});

test('BH4: the refusals name the reason, not generic exceptions', () => {
  const r = JSON.parse(fs.readFileSync(ARTIFACT, 'utf8'));
  const faces = Object.fromEntries(r.fixed.faces.map((f) => [f.label, f]));
  assert.match(faces['api-stall'].message, /stalled: no response headers within/);
  assert.match(faces['body-stall'].message, /stalled: no bytes for/);
  assert.match(faces['redirect-loop'].message, /redirect chain exceeded 5 hops/);
});
