#!/usr/bin/env node
// Did the EXIT-CODE VOICE fix REACH the pin a stranger installs?
// (PULS W, 2026-11-12 — the delivery check for the fix, not a re-proof of
// the tree.)
//
// A green test suite on main says nothing about what `npm install
// github:auroraxo/reeflog#v0.7.20` actually hands out. This extracts the
// PUBLISHED TAG (git archive — the bytes GitHub serves for the tag) and
// runs the six defect faces and the four guards against the TAG's own
// index.js. It FAILS (exit 1) if any defect face is not refused or any
// guard does not hold there.
//
//   TAG=v0.7.20 OUT=research/client-exit-voice-delivery-v0720-2026-11-12.json \
//     node tools/measure-client-exit-voice-delivery.mjs
import { execFileSync } from 'node:child_process';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { pathToFileURL } from 'node:url';

const TAG = process.env.TAG || 'v0.7.20';
const OUT = process.env.OUT || null;

const root = path.resolve(import.meta.dirname, '..');
const tagDir = fs.mkdtempSync(path.join(os.tmpdir(), `exit-voice-${TAG}-`));
const tarball = execFileSync('git', ['archive', TAG], { cwd: root, maxBuffer: 64 * 1024 * 1024 });
execFileSync('tar', ['-x', '-C', tagDir], { input: tarball });

const { reeflog, ReeflogError } = await import(pathToFileURL(path.join(tagDir, 'index.js')).href);

// The version the TAG carries — the claim under test is THIS file's behavior.
const manifest = JSON.parse(fs.readFileSync(path.join(tagDir, 'package.json'), 'utf8')).version;

const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'exit-voice-delivery-bins-'));
const db = path.join(dir, 'j.db');
const fakeBin = (name, body) => {
  const p = path.join(dir, name);
  fs.writeFileSync(p, `#!/bin/sh\n${body}\n`, { mode: 0o755 });
  return p;
};
const TAIL_ROW = '{"seq":1,"signature":"sigA","slot":1,"err":null,"logs":["a"]}';
const BINS = {
  f1: fakeBin('f1.sh', `echo '${TAIL_ROW}'\necho "reeflog: journal write failed: disk i/o error" >&2\nexit 2`),
  f2: fakeBin('f2.sh', `echo "reeflog v0 - durable local mailbox" >&2\nexit 64`),
  f3: fakeBin('f3.sh', `echo '{"verified":true,"missing":[]}'\necho "thread 'main' panicked" >&2\nexit 101`),
  f4: fakeBin('f4.sh', `echo '{"db":"x"}'\nkill -TERM $$\nsleep 5`),
  g1: fakeBin('g1.sh', `echo '{"db":"x","events":0}'\nexit 0`),
  g2: fakeBin('g2.sh', `echo 'reeflog: error: no journal at ${db}' >&2\nexit 1`),
  g3t: fakeBin('g3t.sh', `echo '${TAIL_ROW}'\necho "reeflog: HELD at seq 2 sigB: owed" >&2\nexit 3`),
  g3v: fakeBin('g3v.sh', `echo "reeflog: scan INCOMPLETE" >&2\nexit 3`),
};

const box = (bin) => reeflog({ db, bin });
const results = [];
async function check(name, call, ok) {
  let r;
  try {
    const v = await call();
    r = { outcome: 'resolved', exitCode: v && typeof v.exitCode === 'number' ? v.exitCode : null, message: '' };
  } catch (e) {
    r = {
      outcome: e instanceof ReeflogError ? 'refused' : 'raw-crash',
      exitCode: e instanceof ReeflogError && typeof e.exitCode === 'number' ? e.exitCode : (e instanceof ReeflogError ? null : undefined),
      message: (String(e.message).split('\n').find((l) => l.trim()) || '').replaceAll(dir, '<tmpdir>'),
    };
    if (e instanceof ReeflogError && e.exitCode === null) r.exitCode = null;
  }
  const holds = ok(r);
  results.push({ mode: name, outcome: r.outcome, exitCode: r.exitCode ?? null, message: r.message.slice(0, 200), holds });
  return holds;
}

await check('F1 tail exit 2 after rows', () => box(BINS.f1).tail({ limit: 10 }),
  (r) => r.outcome === 'refused' && r.exitCode === 2 && /journal write failed/.test(r.message));
await check('F1b status exit 2', () => box(BINS.f1).status(),
  (r) => r.outcome === 'refused' && r.exitCode === 2);
await check('F2 tail usage skew 64', () => box(BINS.f2).tail({ limit: 10 }),
  (r) => r.outcome === 'refused' && r.exitCode === 64);
await check('F2b status usage skew 64', () => box(BINS.f2).status(),
  (r) => r.outcome === 'refused' && r.exitCode === 64);
await check('F3 verify panic 101', () => box(BINS.f3).verify({}),
  (r) => r.outcome === 'refused' && r.exitCode === 101);
await check('F4 killed names signal', () => box(BINS.f4).status(),
  (r) => r.outcome === 'refused' && /SIGTERM/.test(r.message));
await check('G1 healthy 0', () => box(BINS.g1).status(), (r) => r.outcome === 'resolved');
await check('G2 refusal keeps words', () => box(BINS.g2).status(),
  (r) => r.outcome === 'refused' && r.exitCode === 1 && /no journal at/.test(r.message));
await check('G3a tail hold shape', () => box(BINS.g3t).tail({ limit: 10 }),
  (r) => r.outcome === 'resolved' && r.exitCode === 3);
await check('G3b verify incomplete throws', () => box(BINS.g3v).verify({}),
  (r) => r.outcome === 'refused' && r.exitCode === 3 && /INCOMPLETE/.test(r.message));
await check('G4 raw stays unclassified', () => box(BINS.f2).raw(['bogus']),
  (r) => r.outcome === 'resolved' && r.exitCode === 64);

fs.rmSync(dir, { recursive: true, force: true });
fs.rmSync(tagDir, { recursive: true, force: true });

const defectsRefused = results.filter((r) => /^F/.test(r.mode)).every((r) => r.holds);
const guardsHold = results.filter((r) => /^G/.test(r.mode)).every((r) => r.holds);
const report = {
  measuredAt: new Date().toISOString(),
  tag: TAG,
  manifest,
  source: `the PUBLISHED ${TAG} tree (git archive — the bytes a stranger installs)`,
  results,
  defectsRefused,
  guardsHold,
  verdict: defectsRefused && guardsHold ? 'DELIVERED-TO-THE-PIN' : 'NOT-DELIVERED',
};
if (OUT) fs.writeFileSync(OUT, `${JSON.stringify(report, null, 2)}\n`);
console.log(JSON.stringify(report, null, 2));
process.exit(report.verdict === 'DELIVERED-TO-THE-PIN' ? 0 : 1);
