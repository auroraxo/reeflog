// Pin for the DISTRIBUTION-SURFACES extension of the adoption instrument
// (2026-11-19, card dd800d7f "Adoption-Messung schaerfen").
//
// WHAT CHANGED: the repo-scoped census (stars/forks/issues/traffic/npm) is
// blind to everything that happens OFF the reeflog repo — but the
// distribution lane's outbound actions live elsewhere: a measurement
// comment posted on a foreign thread (helius-labs/laserstream-sdk#115,
// presented 2026-11-19T17:27:29Z), two placement PRs, and the public
// measurement harness. check 9 measures those surfaces live.
//
// THE SIGNAL IT RECORDS: 24 minutes after a follow-up comment, the reply
// from akshatcoder-hash (2026-11-19T18:20:13Z) accepted the measured
// numbers ("that's a clean repro ... still skips 92 to 121") and changed
// the SDK's fix direction. This is the first external reaction to any
// distribution action. Honest classification: field-use of the
// MEASUREMENT, not consumption of reeflog — no reply mentions reeflog or
// any github.com/auroraxo/* link, so reeflog's user-one claim stays 0.
//
// FALSIFIABLE: every assertion below was checked to go RED when the
// artifact claim is flipped (classification, verdict class, deleted
// reply, a fabricated harness star, a "merged" placement, a reeflog
// mention) and the artifact was then restored byte-identical.
import test from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';

const RESEARCH = path.resolve(import.meta.dirname, '..', 'research');
const load = (name) => JSON.parse(fs.readFileSync(path.join(RESEARCH, name), 'utf8'));

const d = load('adoption-distribution-surfaces-2026-11-19.json');
const block = d.distributionSurfaces;
const ls = block.surfaces.find((s) => s.surface === 'helius-labs/laserstream-sdk#115');

test('G1 the instrument sees the surface the repo census cannot', () => {
  assert.ok(ls, 'the live foreign thread is measured');
  assert.equal(ls.kind, 'issue-thread');
  assert.equal(ls.ownerPresentedAt, '2026-11-19T17:27:29Z', 'first owner comment = presentation');
  assert.equal(ls.ownerComments, 2, 'presentation + crates.io follow-up');
  assert.equal(ls.reactionsOnOwnerComments, 0, 'no emoji reactions recorded');
  assert.equal(ls.nonOwnerRepliesAfter.length, 1, 'exactly one reply arrived after presentation');
  const r = ls.nonOwnerRepliesAfter[0];
  assert.equal(r.author, 'akshatcoder-hash');
  assert.equal(r.createdAt, '2026-11-19T18:20:13Z');
  assert.equal(r.mentionsReeflog, false);
  assert.equal(r.mentionsAuroraxoRepo, false);
});

test('G2 the reply is measurement field-use, classified apart from reeflog use', () => {
  assert.equal(ls.classification, 'measurement-field-use-not-reeflog-consumption');
  assert.ok(
    ls.reasons.some((x) => x.startsWith('thread-is-foreign:')),
    'the thread belongs to another owner, not auroraxo'
  );
  assert.ok(
    ls.reasons.some((x) => x.startsWith('no-reeflog-mention:')),
    '0 foreign replies mention reeflog or an auroraxo repo link'
  );
  assert.ok(
    ls.reasons.some((x) => x.startsWith('content-engagement:')),
    'the reply engages with the measured content, not with reeflog'
  );
});

test('G3 the verdict never converts the reply into reeflog adoption', () => {
  assert.equal(
    block.verdict.class,
    'measurement-field-use-zero-reeflog-consumption'
  );
  assert.ok(
    block.verdict.signals.some((s) => s.includes('akshatcoder-hash')),
    'the signal names the measured reply'
  );
  assert.ok(
    block.verdict.signals.some((s) => s.toLowerCase().includes('not reeflog adoption')),
    'the verdict says explicitly what it is not'
  );
  assert.equal(block.verdict.userOneEvidence, false, 'reeflog user-one stays unproven');
});

test('G4 harness and reeflog census stay zero — consistency with the baseline', () => {
  assert.equal(block.harnessRepo.repo, 'auroraxo/laserstream-cursor-measure');
  assert.equal(block.harnessRepo.public, true);
  assert.equal(block.harnessRepo.nonOwnerStars.length, 0);
  assert.equal(block.harnessRepo.nonOwnerForks.length, 0);
  const base = load('adoption-baseline-2026-11-19.json');
  assert.equal(block.reeflogCensusAtMeasurement.nonOwnerStars, base.checks.starForkCensus.nonOwnerStars.length);
  assert.equal(block.reeflogCensusAtMeasurement.nonOwnerForks, base.checks.starForkCensus.nonOwnerForks.length);
  assert.equal(block.reeflogCensusAtMeasurement.usageIssues, base.checks.issueCensus.external.length);
});

test('G5 both placement PRs recorded live, neither merged', () => {
  assert.equal(d.complete, true, 'no verdict on partial evidence');
  assert.equal(d.dataSourcesFailed, 0);
  const p93 = block.placements.find((p) => p.pr === 93);
  const p280 = block.placements.find((p) => p.pr === 280);
  assert.ok(p93 && p280, 'both placements measured');
  assert.equal(p93.state, 'open');
  assert.equal(p93.merged, false);
  assert.equal(p93.reviews, 0);
  assert.equal(p280.state, 'open');
  assert.equal(p280.merged, false);
  assert.equal(p280.reviews, 0);
});
