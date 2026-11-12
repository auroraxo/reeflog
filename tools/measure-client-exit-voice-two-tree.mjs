#!/usr/bin/env node
// Two-tree runner for the EXIT-CODE VOICE measurement (PULS W, 2026-11-12).
//
// Runs tools/measure-client-exit-voice.mjs against BOTH trees — the
// PUBLISHED v0.7.19 tag (via `git archive`, the pin strangers install) and
// this working tree — and combines them into the committed artifact.
//
// The verdict rule (same class as every measurement in this repo):
//   - the BASELINE must show every defect face (a measurement that cannot
//     see the defect proves nothing — this runner exits 1 then);
//   - THIS TREE must refuse every defect face in the product's voice;
//   - the guards must hold on BOTH trees.
import { spawnSync } from 'node:child_process';
import { execFileSync } from 'node:child_process';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';

const ROOT = path.resolve(import.meta.dirname, '..');
const BASE_TAG = process.env.BASE_TAG || 'v0.7.19';
const OUT = process.env.OUT || path.join(ROOT, 'research/client-exit-voice-2026-11-12.json');

/** The face predicates — shared with the pins in
 *  test/client-exit-voice-measurement.test.mjs, which reads the SAME
 *  literals out of the committed artifact. */
const DEFECT_FACES = [
  {
    mode: 'F1 tail crash-after-rows (exit 2)',
    defect: (f) => f.outcome === 'resolved' && f.exitCode === 2,
    fixed: (f) => f.outcome === 'refused' && f.exitCode === 2 && /journal write failed/.test(f.message || ''),
  },
  {
    mode: 'F1b status crash-after-rows (exit 2)',
    defect: (f) => f.outcome === 'resolved',
    fixed: (f) => f.outcome === 'refused' && f.exitCode === 2,
  },
  {
    mode: 'F2 tail usage-skew (exit 64)',
    defect: (f) => f.outcome === 'resolved' && f.exitCode === 64,
    fixed: (f) => f.outcome === 'refused' && f.exitCode === 64 && /version skew|does not know/.test(f.message || ''),
  },
  {
    mode: 'F2b status usage-skew (exit 64)',
    defect: (f) => f.outcome === 'raw-crash',
    fixed: (f) => f.outcome === 'refused' && f.exitCode === 64,
  },
  {
    mode: 'F3 verify panic-after-report (exit 101)',
    defect: (f) => f.outcome === 'resolved',
    fixed: (f) => f.outcome === 'refused' && f.exitCode === 101 && /panicked/.test(f.message || ''),
  },
  {
    // The baseline forged a refusal ("exit 1") for a death that was never a
    // refusal: the defect is a refusal without the truth.
    mode: 'F4 status killed-by-signal',
    defect: (f) => f.outcome === 'refused' && !/signal/i.test(f.message || ''),
    fixed: (f) => f.outcome === 'refused' && /SIGTERM/.test(f.message || ''),
  },
];

function runTree(tree, tagLabel) {
  const out = path.join(os.tmpdir(), `client-exit-voice-${tagLabel}.json`);
  const r = spawnSync(process.execPath, [path.join(ROOT, 'tools/measure-client-exit-voice.mjs')], {
    env: { ...process.env, TREE: tree, OUT: out, TAG_LABEL: tagLabel },
    encoding: 'utf8',
  });
  if (r.status !== 0) {
    console.error(`measurement failed for ${tagLabel}:\n${r.stdout}\n${r.stderr}`);
    process.exit(1);
  }
  const report = JSON.parse(fs.readFileSync(out, 'utf8'));
  const byMode = Object.fromEntries(report.faces.map((x) => [x.mode, x]));
  const faces = DEFECT_FACES.map((spec) => {
    const f = byMode[spec.mode] || {};
    return {
      mode: spec.mode,
      outcome: f.outcome ?? null,
      exitCode: f.exitCode ?? null,
      message: f.message || f.detail || '',
      defect: spec.defect(f),
      fixed: spec.fixed(f),
    };
  });
  const guards = (report.guardResults || []).map((g) => ({ mode: g.mode, outcome: g.outcome, holds: g.holds }));
  return {
    tag: tagLabel,
    faces,
    guards,
    defectVisible: faces.every((x) => x.defect || x.fixed), // decisive per face
    baselineDefectCount: faces.filter((x) => x.defect).length,
    fixedCleanCount: faces.filter((x) => x.fixed).length,
    guardsHold: guards.length === 4 && guards.every((g) => g.holds),
  };
}

const baseDir = fs.mkdtempSync(path.join(os.tmpdir(), 'reeflog-exit-voice-base-'));
execFileSync('sh', ['-c', `git -C ${JSON.stringify(ROOT)} archive ${BASE_TAG} | tar -x -C ${JSON.stringify(baseDir)}`]);
const baseline = runTree(baseDir, BASE_TAG);
const fixed = runTree(ROOT, 'this-tree');
fs.rmSync(baseDir, { recursive: true, force: true });

const baselineShowsDefect = baseline.baselineDefectCount === DEFECT_FACES.length;
const fixedClean = fixed.fixedCleanCount === DEFECT_FACES.length;

const artifact = {
  measuredAt: new Date().toISOString(),
  defect: 'the client\'s run() classified only the documented codes (0/1/3) and treated every other close as SUCCESS: a run that died with the binary\'s journal-write code 2 AFTER printing rows resolved as a delivered batch; an old binary\'s usage exit 64 resolved as a clean empty tail (status crashed raw out of JSON.parse); a panic (101) after a printed report resolved as a clean scan; a kill by signal (close code null) was dressed up as a forged "exit 1" refusal naming no signal.',
  baseline,
  fixed,
  baselineShowsDefect,
  fixedClean,
  verdict: baselineShowsDefect && fixedClean && baseline.guardsHold && fixed.guardsHold
    ? 'contract-holds'
    : 'MEASUREMENT-INVALID',
};

fs.mkdirSync(path.dirname(OUT), { recursive: true });
fs.writeFileSync(OUT, `${JSON.stringify(artifact, null, 2)}\n`);
console.log(`verdict: ${artifact.verdict}`);
console.log(`baseline defect faces: ${baseline.baselineDefectCount}/${DEFECT_FACES.length}; fixed clean: ${fixed.fixedCleanCount}/${DEFECT_FACES.length}; guards: baseline ${baseline.guardsHold}, fixed ${fixed.guardsHold}`);
if (artifact.verdict !== 'contract-holds') process.exit(1);
