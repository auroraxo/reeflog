// PULS V — the CAPTIVE-PORTAL measurement (2026-11-11).
//
// Pins `research/captive-portal-2026-11-11.json`: a 200-with-the-wrong-body
// answer at the release-API leg must produce a VERDICT IN THE PRODUCT'S
// VOICE — the published v0.7.18 tree threw a raw SyntaxError out of
// bootstrap() (HTML) and blamed the release page for a rate-limit answer
// (misverdict); this tree refuses with the endpoint's own words.
// Falsification: a tampered face outcome in the artifact flips this test RED.
// No host paths may appear in the committed artifact.
import test from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';

const ARTIFACT = path.resolve(import.meta.dirname, '../research/captive-portal-2026-11-11.json');

test('CP1: the measurement artifact exists and proves contract-holds', () => {
  assert.ok(fs.existsSync(ARTIFACT), 'research artifact must be committed');
  const raw = fs.readFileSync(ARTIFACT, 'utf8');
  assert.doesNotMatch(raw, new RegExp(process.env.HOME || '/home/aurora'), 'no host paths in committed artifact');
  const r = JSON.parse(raw);
  assert.equal(r.verdict, 'contract-holds');
  assert.equal(r.baselineShowsDefect, true);
  assert.equal(r.fixedClean, true);
});

test('CP2: baseline published v0.7.18 crashes raw and mis-verdicts', () => {
  const r = JSON.parse(fs.readFileSync(ARTIFACT, 'utf8'));
  assert.equal(r.baseline.tag, 'v0.7.18');
  assert.equal(r.baseline.rawCrash, 1, 'the HTML face must crash RAW on the baseline');
  assert.equal(r.baseline.misverdict, 1, 'the rate-limit face must blame the WRONG cause on the baseline');
  const faces = Object.fromEntries(r.baseline.faces.map((f) => [f.mode, f]));
  assert.equal(faces['F1 api-portal'].outcome, 'raw-crash');
  assert.match(faces['F1 api-portal'].message, /SyntaxError|not valid JSON/);
  assert.equal(faces['F2 api-ratelimit'].outcome, 'refused');
  assert.match(faces['F2 api-ratelimit'].message, /has no asset named/,
    'the misverdict IS the defect: refusing the release page instead of naming the rate limit');
});

test('CP3: this tree refuses in the product\'s voice — no raw crash, no misverdict', () => {
  const r = JSON.parse(fs.readFileSync(ARTIFACT, 'utf8'));
  assert.equal(r.fixed.rawCrash, 0);
  assert.equal(r.fixed.misverdict, 0);
  const faces = Object.fromEntries(r.fixed.faces.map((f) => [f.mode, f]));
  assert.equal(faces['F1 api-portal'].outcome, 'refused');
  assert.match(faces['F1 api-portal'].message, /did not answer JSON/);
  assert.match(faces['F1 api-portal'].message, /HTML/);
  assert.equal(faces['F2 api-ratelimit'].outcome, 'refused');
  assert.match(faces['F2 api-ratelimit'].message, /rate limit exceeded/,
    'the API\'s own words must reach the verdict');
  assert.doesNotMatch(faces['F2 api-ratelimit'].message, /has no asset named/);
});

test('CP4: the guards held on BOTH trees (asset digest, cache, healthy path)', () => {
  const r = JSON.parse(fs.readFileSync(ARTIFACT, 'utf8'));
  for (const side of ['baseline', 'fixed']) {
    const faces = Object.fromEntries(r[side].faces.map((f) => [f.mode, f]));
    assert.equal(faces['F3 asset-portal'].outcome, 'refused', `${side}: asset HTML must refuse on the digest`);
    assert.match(faces['F3 asset-portal'].message, /sha256 mismatch/);
    assert.equal(faces['F4 cached'].outcome, 'resolved', `${side}: a completed install must not need the network`);
    assert.equal(faces['F4 cached'].cached, true);
    assert.equal(faces['F4 cached'].primed, true, `${side}: F4 must really be primed first`);
  }
});
