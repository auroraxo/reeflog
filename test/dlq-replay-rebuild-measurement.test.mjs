// Pin for the dlq-replay-rebuild measurement (2026-11-16).
//
// The defect was a disagreement between two surfaces of ONE journal:
// `webhook push` rebuilt the event body from the row (so a consumer saw
// `seal: "reverted"` after the chain unmade the transaction), while
// `webhook dlq replay` re-sent the body frozen into the quarantine at
// refusal time — `seal: "finalized"`, the claim the chain had unmade — and
// reported `replayed: 1` with nothing naming the divergence.
//
// This file pins the MEASUREMENT so the claim stays a measurement:
//
//   P1 the published artifact exists, names the published v0.7.31 binary
//      by sha256 and version, and is labelled as the PUBLISHED asset;
//   P2 the defect faces: replayedSeal "finalized" while the journal says
//      "reverted", push and replay disagreeing, no `rebuilt` in the report;
//   P3 the guards were GREEN on the published asset — the red faces are
//      the replay's, not the fixture's;
//   P4 the fixed-tree artifact holds every face with `rebuilt: 1`;
//   P5 no host path may appear in either artifact;
//   F  FALSIFICATION: exact strings/booleans compared with === and strict
//      array-length checks, so an edited artifact cannot re-bless itself.
import test from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';

const RESEARCH = path.resolve(import.meta.dirname, '..', 'research');
const read = (name) => JSON.parse(fs.readFileSync(path.join(RESEARCH, name), 'utf8'));

const PUB = read('dlq-replay-rebuild-published-v0731-2026-11-16.json');
const FIX = read('dlq-replay-rebuild-fixed-2026-11-16.json');

const face = (art, id) => {
  const f = art.faces.find((x) => x.face === id);
  assert.ok(f, `the artifact must carry the face ${id}`);
  return f;
};

test('P1: the published artifact names the published v0.7.31 binary', () => {
  assert.equal(PUB.binary.sha256, '19ea6fe54159084b982fc50910284e8961000620bc70e08969d8efab146ab1a4');
  assert.equal(PUB.binary.version, 'reeflog 0.7.31');
  assert.equal(PUB.label, 'published-v0.7.31');
  assert.equal(PUB.verdict, 'dlq-replay-rebuild-blind');
  assert.equal(PUB.faces.length, 6, 'D1-D3 plus G1-G3 — a dropped face must go red');
});

test('P2: the published asset re-sends the claim the chain unmade', () => {
  assert.equal(face(PUB, 'D1').ok, false);
  assert.equal(face(PUB, 'D1').observed.journalSeal, 'reverted');
  assert.equal(face(PUB, 'D1').observed.replayedSeal, 'finalized');
  assert.equal(face(PUB, 'D2').ok, false);
  assert.equal(face(PUB, 'D2').observed.sameBytes, false);
  assert.equal(face(PUB, 'D3').ok, false);
  assert.equal(face(PUB, 'D3').observed.rebuilt, null,
    'the published report carries no `rebuilt` field at all (JSON null)');
});

test('P3: the guards were green on the published asset', () => {
  for (const id of ['G1', 'G2', 'G3']) {
    assert.equal(face(PUB, id).ok, true, `guard ${id} must be green on v0.7.31`);
  }
  assert.equal(face(PUB, 'G1').observed.byteIdentical, true);
  assert.equal(face(PUB, 'G2').observed.sentVerbatim, true);
  assert.equal(face(PUB, 'G3').observed.exitCode, 3);
});

test('P4: the fixed tree holds the rule on every face', () => {
  assert.equal(FIX.verdict, 'dlq-replay-rebuild-held');
  assert.equal(FIX.faces.length, 6);
  for (const id of ['D1', 'D2', 'D3', 'G1', 'G2', 'G3']) {
    assert.equal(face(FIX, id).ok, true, `face ${id} must be green on the fixed tree`);
  }
  assert.equal(face(FIX, 'D1').observed.replayedSeal, 'reverted');
  assert.equal(face(FIX, 'D2').observed.sameBytes, true);
  assert.equal(face(FIX, 'D3').observed.rebuilt, 1);
  assert.equal(face(FIX, 'G1').observed.rebuilt, 0);
});

test('P5: neither artifact carries a host path', () => {
  for (const name of [
    'dlq-replay-rebuild-published-v0731-2026-11-16.json',
    'dlq-replay-rebuild-fixed-2026-11-16.json',
  ]) {
    const text = fs.readFileSync(path.join(RESEARCH, name), 'utf8');
    for (const marker of ['/home/', '/tmp/', '/Users/', 'C:\\']) {
      assert.ok(!text.includes(marker), `${name} carries a host path marker: ${marker}`);
    }
  }
});
