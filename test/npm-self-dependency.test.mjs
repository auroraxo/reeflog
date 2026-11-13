// GATE: the package must not depend on ITSELF — the zero-dependency client
// that the registry channel hands a stranger.
//
// Why this exists (measured 2026-11-13, this repo): commit cda750c (v0.7.21)
// added
//
//   "dependencies": { "reeflog-client": "github:auroraxo/reeflog#v0.7.20" }
//
// to package.json — the package declaring a dependency on its own name,
// resolved through git. Every release since has bumped the pin along with
// the version, so the defect travels instead of being noticed. What it does
// to a stranger who runs exactly the command the README leads with
// (`npm install reeflog-client`), measured on the PUBLISHED 0.7.24 bytes:
//
//   D1 a SECOND copy of reeflog-client is installed, nested under the
//      first — npm reports "added 2 packages" for a client whose runtime
//      imports are all `node:` builtins;
//   D2 the nested copy is STALE: 0.7.24 on top, 0.7.22 underneath, a
//      different release than the one the reader asked for, carrying a
//      README one release behind (no contract point 3);
//   D3 on a host without `git` on PATH (minimal containers, `node:alpine`,
//      locked-down CI images) the install FAILS —
//      `npm error code ENOENT / syscall spawn git`, exit 254, nothing
//      installed. The promise the package is built on, "no Rust toolchain
//      needed", quietly becomes "and you need git".
//
// The pins below are RED against that tree and GREEN once the dependency is
// gone. T4/T5 are the false-positive guards: they hold on BOTH sides, and
// they are what makes deleting the block safe rather than hopeful — the
// client imports nothing but `node:` builtins, so there is no runtime edge
// this dependency was holding up.
import test from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';
import { execFileSync } from 'node:child_process';
import os from 'node:os';

const root = path.resolve(import.meta.dirname, '..');
const manifest = JSON.parse(fs.readFileSync(path.join(root, 'package.json'), 'utf8'));

test('T1: the manifest declares no dependency on its own name', () => {
  const deps = manifest.dependencies || {};
  assert.ok(!(manifest.name in deps),
    `self-dependency: ${manifest.name} depends on ${JSON.stringify(deps[manifest.name])} — ` +
    'a package that depends on itself pulls a second copy of itself through git');
});

test('T2: the client carries no dependencies at all (zero-dep tarball)', () => {
  const deps = manifest.dependencies || {};
  assert.deepEqual(deps, {},
    `expected no dependencies, found ${JSON.stringify(deps)} — ` +
    'every runtime import in index.js is a node: builtin');
});

test('T3: the PACKED tarball carries no dependencies either', () => {
  // A fix that edits the tree but ships the old manifest is the pin-lag
  // class again: judge the bytes npm would publish, not the checkout.
  const tmp = fs.mkdtempSync(path.join(os.tmpdir(), 'packgate-'));
  try {
    execFileSync('npm', ['pack', '--pack-destination', tmp, '--silent'],
      { cwd: root, stdio: 'ignore' });
    const tgz = fs.readdirSync(tmp).find((f) => f.endsWith('.tgz'));
    assert.ok(tgz, 'npm pack produced no tarball');
    const json = JSON.parse(execFileSync('tar',
      ['-xzOf', path.join(tmp, tgz), 'package/package.json'], { encoding: 'utf8' }));
    assert.deepEqual(json.dependencies || {}, {},
      `the packed tarball still declares ${JSON.stringify(json.dependencies)}`);
  } finally {
    fs.rmSync(tmp, { recursive: true, force: true });
  }
});

test('T4 guard (green on both trees): the manifest still names the crate\'s release', () => {
  // Removing the dependency must not disturb the B10 coupling.
  const cargo = fs.readFileSync(path.join(root, 'Cargo.toml'), 'utf8');
  const crate = /^version\s*=\s*"([^"]+)"/m.exec(cargo)?.[1];
  assert.equal(manifest.version, crate,
    'package.json version must equal Cargo.toml version');
});

test('T5 guard (green on both trees): the client needs no dependency to run', () => {
  // The evidence that deleting `dependencies` is safe: index.js imports
  // nothing outside node: builtins, so no runtime edge is being cut.
  const src = fs.readFileSync(path.join(root, 'index.js'), 'utf8');
  const specifiers = [...src.matchAll(/^\s*import\s[^'"]*from\s+['"]([^'"]+)['"]/gm)]
    .map((m) => m[1])
    .filter((s) => !s.startsWith('.') && !s.startsWith('node:'));
  assert.deepEqual(specifiers, [],
    `index.js imports non-builtin modules: ${specifiers.join(', ')}`);
});
