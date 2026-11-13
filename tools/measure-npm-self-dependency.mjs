#!/usr/bin/env node
// THE SELF-DEPENDENCY, MEASURED: what `npm install reeflog-client` actually
// puts on a stranger's disk, and what it does to a host without git.
//
// WHY (2026-11-13, this repo): since cda750c (v0.7.21) the package declares
// a dependency on its OWN name resolved through git —
// `"reeflog-client": "github:auroraxo/reeflog#v0.7.20"`. Every release since
// bumped the pin with the version, so nobody looked at what it installs.
// The client's runtime imports are all `node:` builtins: nothing needs it.
//
// FACES (all judged on a real `npm install` of the PUBLISHED tarball into an
// EMPTY app, never against this working tree):
//   D1  the install puts a second copy of reeflog-client on disk
//       (node_modules/reeflog-client/node_modules/reeflog-client exists);
//   D2  that nested copy is a DIFFERENT, older release than the one
//       installed on top — the reader asked for X and got X plus X-n;
//   D3  on a host with no `git` on PATH the same documented command fails
//       outright (ENOENT spawn git, exit != 0, nothing installed) — the
//       package promises "no Rust toolchain needed" and quietly adds
//       "but you need git".
//   G1  guard (must hold on BOTH sides): the top-level install DOES serve
//       the manifest version the reader asked for;
//   G2  guard: index.js imports nothing outside `node:` builtins, so D1 is
//       weight, not a requirement.
//
// Verdict: self-dependency-present | self-dependency-absent | no-verdict(2).
//
//   OUT=research/npm-self-dependency-2026-11-13.json node tools/measure-npm-self-dependency.mjs
//   PKG_REF=/path/to/reeflog-client-0.7.25.tgz TREE="fixed tree (npm pack)" OUT=… node tools/…
// Exit: 0 absent (fixed), 1 present (defect), 2 no verdict.
import { execFileSync, spawnSync } from 'node:child_process';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { createHash } from 'node:crypto';
import { fileURLToPath } from 'node:url';

const PKG = process.env.PKG || 'reeflog-client';
const here = path.dirname(fileURLToPath(import.meta.url));
const repoRoot = path.resolve(here, '..');
const manifest = JSON.parse(fs.readFileSync(path.join(repoRoot, 'package.json'), 'utf8'));
const wantVersion = manifest.version;
// What to hand `npm install`: the registry name@version by default, or a
// local tarball for the fixed-tree side of the measurement. The version
// is taken from the REF, so the baseline side stays re-derivable after the
// tree has moved on (the published 0.7.24 stays measurable).
const PKG_REF = process.env.PKG_REF || `${PKG}@${manifest.version}`;
const refVersion = /@(\d+\.\d+\.\d+)$/.exec(PKG_REF)?.[1] || manifest.version;
const TREE = process.env.TREE || `published ${PKG}@${refVersion} from registry.npmjs.org, installed into an empty app`;
const OUT = process.env.OUT || null;

function sh(cmd, args, opts = {}) {
  return spawnSync(cmd, args, { encoding: 'utf8', ...opts });
}

function mkapp() {
  const app = fs.mkdtempSync(path.join(os.tmpdir(), 'selfdep-'));
  sh('npm', ['init', '-y'], { cwd: app, stdio: 'ignore' });
  return app;
}

// The npm cache must be per-run: a warm cache can hide the git resolution.
const cache = fs.mkdtempSync(path.join(os.tmpdir(), 'selfdep-cache-'));
const npmCommon = ['--no-audit', '--no-fund', `--cache=${cache}`];

// D1/D2 — a normal install of the published package into an empty app.
const app1 = mkapp();
const r1 = sh('npm', ['install', PKG_REF, ...npmCommon], { cwd: app1 });
if (r1.status !== 0) {
  console.error(`no verdict: baseline install failed:\n${r1.stderr?.slice(0, 800)}`);
  process.exit(2);
}
const topDir = path.join(app1, 'node_modules', 'reeflog-client');
const nestedDir = path.join(topDir, 'node_modules', 'reeflog-client');
const topVersion = JSON.parse(fs.readFileSync(path.join(topDir, 'package.json'), 'utf8')).version;
const nestedExists = fs.existsSync(nestedDir);
const nestedVersion = nestedExists
  ? JSON.parse(fs.readFileSync(path.join(nestedDir, 'package.json'), 'utf8')).version
  : null;
const nestedResolved = (() => {
  try {
    const lock = JSON.parse(fs.readFileSync(path.join(app1, 'package-lock.json'), 'utf8'));
    const key = `node_modules/reeflog-client/node_modules/reeflog-client`;
    return lock.packages?.[key]?.resolved || null;
  } catch { return null; }
})();

const d1 = nestedExists;
const d2 = nestedExists && nestedVersion !== topVersion;

// D3 — the SAME command on a host without git. A shim bin dir holds links
// to everything npm needs except git; that is a minimal container, not a
// hypothetical.
const gitPath = (() => {
  for (const dir of (process.env.PATH || '').split(path.delimiter)) {
    const p = path.join(dir, 'git');
    if (fs.existsSync(p)) return p;
  }
  return null;
})();
if (!gitPath) {
  console.error('no verdict: cannot build the no-git host without having seen git');
  process.exit(2);
}
const shim = path.join(cache, 'noglobins');
fs.mkdirSync(shim, { recursive: true });
for (const exe of ['node', 'npm', 'npx', 'sh', 'bash', 'tar', 'gzip', 'env', 'true', 'false', 'cat', 'uname', 'mktemp', 'rm']) {
  const p = (() => {
    for (const dir of (process.env.PATH || '').split(path.delimiter)) {
      const q = path.join(dir, exe);
      if (fs.existsSync(q)) return q;
    }
    return null;
  })();
  if (p) fs.symlinkSync(p, path.join(shim, exe));
}
const cache2 = fs.mkdtempSync(path.join(os.tmpdir(), 'selfdep-cache2-'));
const app2 = mkapp();
const r2 = sh('npm', ['install', PKG_REF, '--no-audit', '--no-fund', `--cache=${cache2}`], {
  cwd: app2,
  env: { ...process.env, PATH: shim, GIT_SSH_COMMAND: '/bin/false' },
});
const installedWithoutGit = fs.existsSync(path.join(app2, 'node_modules', 'reeflog-client', 'package.json'));
const d3 = r2.status !== 0 || !installedWithoutGit;

// G1 — the top-level install serves the version the reader asked for.
const g1 = topVersion === refVersion;
// G2 — index.js needs no non-builtin import.
const src = fs.readFileSync(path.join(repoRoot, 'index.js'), 'utf8');
const nonBuiltin = [...src.matchAll(/^\s*import\s[^'"]*from\s+['"]([^'"]+)['"]/gm)]
  .map((m) => m[1]).filter((s) => !s.startsWith('.') && !s.startsWith('node:'));
const g2 = nonBuiltin.length === 0;

const violations = [d1 && 'D1', d2 && 'D2', d3 && 'D3'].filter(Boolean);
const verdict = violations.length ? 'self-dependency-present' : 'self-dependency-absent';

const report = {
  measuredAt: new Date().toISOString().slice(0, 10),
  source: TREE,
  defect: 'the package declares a dependency on its own name resolved through github:, so every registry install also clones a second, older copy of itself over git+ssh, and fails outright on a host without git',
  faces: {
    D1_nested_self_copy_installed: { violated: d1, detail: nestedExists ? `nested ${nestedVersion} under top ${topVersion}, resolved ${nestedResolved}` : 'no nested copy' },
    D2_nested_copy_is_stale: { violated: d2, detail: nestedExists ? `top ${topVersion} vs nested ${nestedVersion}` : 'n/a without a nested copy' },
    D3_install_fails_without_git: { violated: d3, detail: `exit ${r2.status}, installed=${installedWithoutGit}, stderr=${String(r2.stderr || '').split('\n').filter((l) => /error code|syscall/.test(l)).join(' ') || 'n/a'}` },
    G1_top_level_serves_requested_version: { holds: g1, detail: `asked ${refVersion}, got ${topVersion}` },
    G2_no_non_builtin_imports: { holds: g2, detail: nonBuiltin.length ? nonBuiltin.join(', ') : 'only node: builtins' },
  },
  manifestDependencies: manifest.dependencies || {},
  installedManifestDependencies: (() => {
    try {
      return JSON.parse(fs.readFileSync(path.join(topDir, 'package.json'), 'utf8')).dependencies || {};
    } catch { return null; }
  })(),
  verdict,
  violationCount: violations.length,
};

// SHA of the published tarball, so the artifact names the bytes it saw
// instead of a host path.
let tarballSha = null;
try {
  let tgz;
  if (PKG_REF.endsWith('.tgz')) {
    tgz = path.join(cache, 'local.tgz');
    fs.copyFileSync(path.resolve(PKG_REF), tgz);
  } else {
    const doc = JSON.parse(execFileSync('npm', ['view', `${PKG}@${refVersion}`, '--json'],
      { encoding: 'utf8', maxBuffer: 32 * 1024 * 1024 }));
    tgz = path.join(cache, 'p.tgz');
    execFileSync('curl', ['-sS', '--fail', '-o', tgz, doc.dist.tarball]);
  }
  tarballSha = createHash('sha256').update(fs.readFileSync(tgz)).digest('hex');
  report.publishedTarball = {
    version: refVersion,
    sha256: tarballSha,
    origin: PKG_REF.endsWith('.tgz') ? 'local npm pack of this tree' : 'registry.npmjs.org',
  };
} catch {
  report.publishedTarball = { version: refVersion, sha256: null };
}

for (const [dir, keep] of [[app1, 0], [app2, 0], [cache, 0], [cache2, 0]]) void keep;
const text = JSON.stringify(report, null, 2);
if (OUT) {
  const p = path.isAbsolute(OUT) ? OUT : path.join(repoRoot, OUT);
  fs.mkdirSync(path.dirname(p), { recursive: true });
  fs.writeFileSync(p, text + '\n');
  console.log(`wrote ${OUT}`);
}
console.log(text);
process.exit(violations.length ? 1 : 0);
