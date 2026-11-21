// Pin for the CLAIMED-DELIVERIES census (check 9b) of the adoption
// instrument (2026-11-21, card dd800d7f "Adoption-Messung schaerfen").
//
// WHAT CHANGED: the distribution lane records passed proofs that name
// delivery URLs, but nothing ever re-asked those URLs. The instrument now
// fetches every delivery a pulse ever claimed on EVERY run and records
// survive-or-not as a measured fact (alive/dead per comment id).
//
// THE FINDING IT RECORDS: proof f3c2d4b6 (2026-11-20, card 7b4a1741)
// claimed the agave#16654 comment issuecomment-7045265720 as posted AND
// round-trip verified. On 2026-11-21 (07:29Z, measured independently in
// auroraxo/agave-skipped-slots-measure research/delivery-status-2026-11-21.json
// and again by this snapshot) that comment is DEAD: 404, 0 comments on the
// issue, no delete event in the timeline, and the account is blocked from
// commenting there (repost 403/GraphQL "User is blocked"). 4 laserstream
// comments + the yellowstone-grpc pivot survive with author auroraxo.
//
// FALSIFIABLE: every assertion below was checked to go RED when the
// artifact claim is flipped (verdict class forged, dead URL deleted from
// the dead list, the dead comment flipped to exists:true, the alive count
// inflated, an alive comment flipped dead, an author forged, ok=true with
// an unverifiable delivery, complete=false, the block deleted) and the
// artifact was then restored byte-identical.
import test from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';

const RESEARCH = path.resolve(import.meta.dirname, '..', 'research');
const load = (name) => JSON.parse(fs.readFileSync(path.join(RESEARCH, name), 'utf8'));

const d = load('adoption-claimed-deliveries-2026-11-21.json');
const block = d.distributionSurfaces.claimedDeliveries;
const ls115 = block.deliveries.find((x) => x.repo === 'helius-labs/laserstream-sdk');
const agave = block.deliveries.find((x) => x.repo === 'anza-xyz/agave');
const ysg = block.deliveries.find((x) => x.repo === 'rpcpool/yellowstone-grpc');

test('G-block the census exists with its rule and the full claimed set', () => {
  assert.ok(block, 'claimedDeliveries block present under distributionSurfaces');
  assert.equal(
    block.rule,
    'a claimed delivery is evidence only while it exists; every pulse re-asks every claim'
  );
  assert.equal(block.total, 6, 'every delivery the lane ever claimed is re-asked');
  assert.equal(block.alive, 5);
  assert.equal(block.dead.length, 1);
  assert.equal(block.ok, true, 'no unverifiable delivery — a real 404 is a finding, not a source failure');
});

test('G-agave the PULS-7 agave delivery is recorded as dead, with its faces', () => {
  assert.ok(agave, 'the claimed agave comment is in the census');
  assert.equal(agave.commentId, 7045265720);
  assert.equal(agave.exists, false);
  assert.equal(agave.liveFetch, 'http-404');
  assert.equal(
    block.dead.includes(agave.url),
    true,
    'the dead URL is named in the dead list'
  );
  assert.ok(agave.fetchedAt, 'fetched live this run, not copied from a proof');
});

test('G-ls the surviving laserstream deliveries keep their author faces', () => {
  const ids = block.deliveries
    .filter((x) => x.repo === 'helius-labs/laserstream-sdk')
    .map((x) => x.commentId)
    .sort((a, b) => a - b);
  assert.deepEqual(ids, [7016745189, 7022923160, 7027762124, 7034421932]);
  for (const x of block.deliveries.filter((x) => x.repo === 'helius-labs/laserstream-sdk')) {
    assert.equal(x.exists, true, `${x.commentId} alive`);
    assert.equal(x.author, 'auroraxo', `${x.commentId} author is the owner`);
    assert.equal(x.authorMatchesOwner, true);
    assert.ok(x.createdAt, `${x.commentId} carries its live created_at`);
  }
  assert.equal(ls115.createdAt, '2026-11-19T17:27:29Z', 'the presentation comment keeps its measured time');
});

test('G-ys the yellowstone-grpc pivot delivery survives as claimed', () => {
  assert.ok(ysg, 'the pivot comment is in the census');
  assert.equal(ysg.commentId, 7059360753);
  assert.equal(ysg.exists, true);
  assert.equal(ysg.author, 'auroraxo');
  assert.equal(ysg.createdAt, '2026-11-21T07:35:47Z');
});

test('G-verdict the adoption verdict is untouched by a dead delivery', () => {
  // A dead delivery must NOT change the adoption classes: the agave
  // comment carried no reeflog signal when it was alive, and its death
  // removes no user-one evidence either. Measured value, not a guess.
  assert.equal(
    d.distributionSurfaces.verdict.class,
    'measurement-field-use-zero-reeflog-consumption'
  );
  assert.equal(d.distributionSurfaces.verdict.userOneEvidence, false);
  assert.equal(d.verdict.class, 'not-distributed-not-consumed');
  assert.equal(d.verdict.class.includes('user-one'), false);
  const census = d.distributionSurfaces.reeflogCensusAtMeasurement;
  assert.equal(census.nonOwnerStars, 0);
  assert.equal(census.nonOwnerForks, 0);
  assert.equal(census.usageIssues, 0);
});

test('G-integrity ok=true only when every delivery was really asked', () => {
  assert.equal(d.complete, true, 'no verdict on partial evidence');
  assert.equal(d.dataSourcesFailed, 0);
  for (const x of block.deliveries) {
    assert.notEqual(x.state, 'unverifiable', `${x.repo}#issuecomment-${x.commentId} was really fetched`);
    assert.ok(x.fetchedAt, 'every row carries its live fetch time');
  }
});
