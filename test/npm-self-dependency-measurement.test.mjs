// Pin for the SELF-DEPENDENCY measurements (2026-11-13).
//
// The defect: since cda750c (v0.7.21) the npm manifest declared a
// dependency on its OWN name, resolved through github:. The client imports
// nothing but `node:` builtins, so nothing needed it. What the stranger
// who ran the documented `npm install reeflog-client` actually got
// (measured on the PUBLISHED 0.7.24 bytes):
//
//   D1 a second copy of reeflog-client nested under the first;
//   D2 that nested copy STALE — 0.7.22 under 0.7.24, a different release
//      than the one requested, with a README one release behind;
//   D3 on a host without `git` the same command FAILS —
//      `npm error code ENOENT / syscall spawn git`, exit 254, nothing
//      installed. "No Rust toolchain needed" quietly became "and you need
//      git".
//
// This file pins the MEASUREMENTS so both sides of the claim stay
// measurements:
//
//   W1 the published-0.7.24 artifact: verdict self-dependency-present,
//      3/3 defect faces, both guards green — the defect measured, not the
//      channel broken;
//   W2 the fixed-tree artifact: verdict self-dependency-absent, 0
//      violations, the install succeeds WITHOUT git and leaves no nested
//      copy;
//   W3 the published tarball's digest — the bytes the baseline saw;
//   W4 no host path in any artifact;
//   F  falsification by construction: every face is compared with ===
//      against literals, so an edited artifact cannot re-bless itself.
import test from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';

const dir = path.resolve(import.meta.dirname, '..', 'research');
const load = (name) => JSON.parse(fs.readFileSync(path.join(dir, name), 'utf8'));

const published = load('npm-self-dependency-published-v0724-2026-11-13.json');
const fixed = load('npm-self-dependency-fixed-2026-11-13.json');
const face = (report, name) => report.faces[name];

test('W1: the PUBLISHED 0.7.24 tarball measured self-dependency-present — 3 defect faces, guards green', () => {
  assert.equal(published.source, 'published reeflog-client@0.7.24 from registry.npmjs.org, installed into an empty app');
  assert.equal(published.measuredAt, '2026-11-13');
  assert.equal(published.verdict, 'self-dependency-present');
  assert.equal(published.violationCount, 3);

  const d1 = face(published, 'D1_nested_self_copy_installed');
  assert.equal(d1.violated, true, 'D1: a second copy of the package was installed');
  assert.match(d1.detail, /nested 0\.7\.22 under top 0\.7\.24/,
    'D1: the nested copy is named in the artifact');
  assert.match(d1.detail, /git\+ssh:\/\/git@github\.com\/auroraxo\/reeflog\.git/,
    'D1: the nested copy was resolved over git+ssh — the install left the registry');

  const d2 = face(published, 'D2_nested_copy_is_stale');
  assert.equal(d2.violated, true, 'D2: the nested copy is a different release');
  assert.equal(d2.detail, 'top 0.7.24 vs nested 0.7.22');

  const d3 = face(published, 'D3_install_fails_without_git');
  assert.equal(d3.violated, true, 'D3: the install fails on a host without git');
  assert.match(d3.detail, /exit 254/);
  assert.match(d3.detail, /installed=false/);
  assert.match(d3.detail, /spawn git/);

  // The guards hold on the DEFECT side too — the channel itself works, it
  // just carries a second copy and needs git.
  assert.equal(face(published, 'G1_top_level_serves_requested_version').holds, true);
  assert.equal(face(published, 'G2_no_non_builtin_imports').holds, true);
});

test('W2: the manifest that shipped it named itself', () => {
  assert.deepEqual(published.installedManifestDependencies,
    { 'reeflog-client': 'github:auroraxo/reeflog#v0.7.22' },
    'the published manifest carried the self-dependency');
});

test('W3: the FIXED tree measures self-dependency-absent — 0 violations, installs without git', () => {
  assert.equal(fixed.verdict, 'self-dependency-absent');
  assert.equal(fixed.violationCount, 0);

  const d1 = face(fixed, 'D1_nested_self_copy_installed');
  assert.equal(d1.violated, false, 'D1: no second copy is installed');
  assert.equal(d1.detail, 'no nested copy');

  const d3 = face(fixed, 'D3_install_fails_without_git');
  assert.equal(d3.violated, false, 'D3: the install works on a host without git');
  assert.match(d3.detail, /exit 0/);
  assert.match(d3.detail, /installed=true/);

  assert.equal(face(fixed, 'G1_top_level_serves_requested_version').holds, true);
  assert.equal(face(fixed, 'G2_no_non_builtin_imports').holds, true);
});

test('W4: the fixed tree ships no dependencies at all', () => {
  assert.deepEqual(fixed.installedManifestDependencies, {},
    'the fixed manifest declares no dependencies');
  assert.deepEqual(fixed.manifestDependencies, {},
    'and the tree it was packed from declares none either');
});

test('W5: the artifacts name the bytes they saw, not a host path', () => {
  for (const [name, report] of [['published', published], ['fixed', fixed]]) {
    assert.match(report.publishedTarball.sha256, /^[0-9a-f]{64}$/, `${name}: a real digest`);
    assert.ok(!JSON.stringify(report).includes('/tmp/'), `${name}: no host path`);
    assert.ok(!JSON.stringify(report).includes('/home/'), `${name}: no host path`);
  }
  assert.equal(published.publishedTarball.origin, 'registry.npmjs.org',
    'the baseline names the registry as the source of the bytes');
});
