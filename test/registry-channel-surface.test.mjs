// THE REGISTRY-CHANNEL SURFACE: does the README that SHIPS INSIDE the
// npm tarball point the registry installer at the registry channel?
//
// WHY (measured 2026-11-13): `npm install reeflog-client` works — the
// published 0.7.20 tarball bootstraps `reeflog 0.7.20` and passed the
// stranger-path live check 8/8 against a real mainnet journal — but the
// README inside those exact bytes never says `npm install reeflog-client`.
// Every install line says `npm install github:auroraxo/reeflog#…`, which
// requires a GitHub account context and bypasses the version the reader is
// looking at. The defect is not a denial (signerd v0.2.18 class — G4 below
// pins that this README never had one); it is INVISIBILITY: the channel is
// live, the page on the channel hides the channel.
//
// THE CONTRACT (pure offline rules on THIS README — the file the `files`
// allowlist ships into the published tarball, so the working-tree bytes and
// the registry bytes are the same page):
//   R1 the README carries the registry install command
//      (`npm install reeflog-client`) at least once.
//   R2 the FIRST install paragraph (the **Status:** line) names the
//      registry command, not only the git-tag path — the stranger on the
//      npm page must not have to dig.
//   R3 the git-tag install path STAYS (fixing visibility must not delete
//      the documented channel — the false-positive guard).
//   R4 no publication-denial sentence anywhere (the signerd R1/R2 class;
//      green on the pre-fix tree by construction — this README never
//      denied, it stayed silent).
import test from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';

const README = fs.readFileSync(path.resolve(import.meta.dirname, '..', 'README.md'), 'utf8');
const REGISTRY_CMD = /npm (install|i) reeflog-client\b/;
const GIT_TAG_CMD = /npm (install|i) github:auroraxo\/reeflog#v\d+\.\d+\.\d+/;
const DENIAL = /not (yet )?published to npm|no package is published|publishing (is|remains) credential-blocked/i;

test('R1: the shipped README carries the registry install command', () => {
  assert.ok(REGISTRY_CMD.test(README),
    'the README inside the published tarball must say npm install reeflog-client');
});

test('R2: the first install paragraph names the registry channel', () => {
  const firstPara = README.split('\n').find((l) => l.startsWith('**Status:')) || '';
  assert.ok(firstPara.length > 0, 'the README must open with a Status install paragraph');
  assert.match(firstPara, REGISTRY_CMD,
    'the Status paragraph must offer the registry command, not only the git-tag path');
});

test('R3: the git-tag install path stays documented (guard)', () => {
  // Must hold BEFORE and AFTER the fix: adding a channel may not remove one.
  assert.match(README, GIT_TAG_CMD,
    'the documented git-tag install path must not be deleted by a registry fix');
});

test('R4: no publication denial anywhere (guard, green on the pre-fix tree)', () => {
  assert.doesNotMatch(README, DENIAL,
    'a published package may not tell its readers it is not published');
});
