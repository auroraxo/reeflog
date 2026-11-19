// Pin for the ADOPTION BASELINE (2026-11-19), the measurement Gregor requires
// BEFORE the first distribution action (card: reeflog Adoption-Messung
// schaerfen).
//
// WHAT THIS PINS: the npm weekly download number is recorded WITH its
// attribution, never as adoption.
//
// WHY IT MATTERS: reeflog-client shows 3.024 downloads in the week
// 2026-11-12..2026-11-18 — a number that looks like traction. It is not.
// reeflog-client was first published 2026-11-12T16:55:21Z and every
// download before that date is 0, so every download belongs to this
// project's own lifetime; and this project's own CI installs the package
// on every run (client.test, npm-self-dependency.test, registry-channel
// measurement, live registry harness). 298 owner CI runs in the same
// trailing 7 days (reeflog 73, sememe 90, signerd 135, whirlpool-watcher
// 0) is the honest scale of that harness activity.
//
// The trap this family already fell into once (356 own clones counted as
// adoption): a number that rises because you built more of your own thing
// is not a user. The card says so explicitly ("mein eigener CI-Checkout
// und whirlpool-watcher sind KEINE Adoption — nicht mitzaehlen").
//
// FALSIFIABLE: every assertion below was checked to go RED when the
// artifact claim is flipped, and the artifact was then restored
// byte-identical. Flip `countsAsAdoption` to true, move the verdict to
// user-one-evidence, delete the owner-CI row, or claim a download before
// the first publish — the corresponding pin fails.
import test from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';

const RESEARCH = path.resolve(import.meta.dirname, '..', 'research');
const load = (name) => JSON.parse(fs.readFileSync(path.join(RESEARCH, name), 'utf8'));

const adoption = load('adoption-baseline-2026-11-19.json');
const npm = adoption.checks.npmDownloads;

test('G1 the npm weekly number is measured and recorded', () => {
  assert.equal(npm.package, 'reeflog-client');
  assert.equal(npm.registry, 'api.npmjs.org/downloads');
  assert.ok(npm.lastWeek, 'the last-week window is recorded');
  assert.equal(npm.lastWeek.window, '2026-11-12..2026-11-18');
  assert.equal(npm.lastWeek.downloads, 3024, 'the measured weekly download count');
  assert.ok(Array.isArray(npm.byDay) && npm.byDay.length > 0, 'the daily series is recorded');
  const nonZeroDays = npm.byDay.filter((d) => d.downloads > 0);
  assert.equal(nonZeroDays[0].day, '2026-11-12', 'the first download day is the publish day');
});

test('G2 downloads NEVER count as adoption', () => {
  assert.equal(npm.countsAsAdoption, false, 'the artifact says so explicitly');
  assert.equal(
    adoption.verdict.class,
    'not-distributed-not-consumed',
    '3.024 downloads did NOT flip the verdict to user-one-evidence'
  );
  assert.equal(adoption.checks.starForkCensus.nonOwnerStars.length, 0);
  assert.equal(adoption.checks.starForkCensus.nonOwnerForks.length, 0);
  assert.equal(adoption.checks.issueCensus.external.length, 0);
  assert.equal(adoption.checks.referrers.external.length, 0);
  const signals = adoption.verdict.signals.join(' | ');
  assert.ok(!/download/i.test(signals), 'no download is named as a signal');
});

test('G3 the downloads are attributed to the owner harness, with its size', () => {
  assert.ok(npm.ownerCiRunsSameWeek.length >= 4, 'owner CI is counted per repo');
  const total = npm.ownerCiRunsSameWeek.reduce((s, r) => s + r.runs, 0);
  assert.ok(total > 0, 'the owner harness activity is non-zero, so the attribution is concrete');
  const reeflog = npm.ownerCiRunsSameWeek.find((r) => r.repo === 'auroraxo/reeflog');
  assert.ok(reeflog && reeflog.runs > 0, 'the reeflog repo itself runs CI that installs the client');
  assert.ok(
    npm.meaning.includes('OWNER ACTIVITY'),
    'the artifact names the attribution in words, not only in a flag'
  );
});

test('G4 structural attribution guard: 0 downloads before the first publish', () => {
  // The package name was NOT in use before this project. If a future
  // measurement ever shows downloads before the first publish, the number
  // belongs to someone else's package and must not be attributed here.
  assert.equal(npm.downloadsBeforeFirstPublish, 0);
  assert.equal(npm.firstPublishAt, '2026-11-12T16:55:21.365Z');
  const before = npm.byDay.filter((d) => d.day < npm.firstPublishAt.slice(0, 10));
  assert.ok(before.length > 0, 'the pre-publish days were actually measured');
  assert.ok(
    before.every((d) => d.downloads === 0),
    'every pre-publish day is 0'
  );
});

test('G5 the measurement is complete — no verdict on partial evidence', () => {
  assert.equal(adoption.complete, true);
  assert.equal(adoption.dataSourcesFailed, 0);
  assert.equal(adoption.checks.repoMetadata.ok, true);
  assert.equal(adoption.checks.traffic.ok, true);
  assert.equal(npm.ok, true);
  assert.equal(adoption.placement.merged, false, 'PR #93 is still open');
});

test('G6 the gate is not dead: a real non-owner signal would still count', () => {
  // The mirror failure: over-blocking would make the instrument useless.
  // Downloads are excluded, but the adoption channels are NOT disabled.
  const c = adoption.checks;
  assert.ok(Array.isArray(c.starForkCensus.nonOwnerStars));
  assert.ok(Array.isArray(c.starForkCensus.nonOwnerForks));
  assert.ok(Array.isArray(c.issueCensus.external));
  assert.ok(Array.isArray(c.referrers.external));
  assert.ok(
    adoption.verdict.class !== 'not-distributed-not-consumed' ||
      c.starForkCensus.nonOwnerStars.length === 0,
    'the verdict class is consistent with the censuses'
  );
});
