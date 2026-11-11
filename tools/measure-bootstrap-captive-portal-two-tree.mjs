// Two-tree runner for the captive-portal measurement (PULS V, 2026-11-11).
//
// Runs tools/measure-bootstrap-captive-portal.mjs against BOTH trees —
// the PUBLISHED v0.7.18 tag (via `git archive`, the pin that strangers
// install) and this working tree — and combines them into the committed
// artifact. It FAILS (exit 1) if the baseline tree shows no defect: a
// measurement that cannot distinguish the trees proves nothing.
import { execFileSync, spawnSync } from 'node:child_process';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';

const ROOT = path.resolve(import.meta.dirname, '..');
const BASE_TAG = process.env.BASE_TAG || 'v0.7.18';
const OUT = process.env.OUT || path.join(ROOT, `research/captive-portal-${new Date().toISOString().slice(0, 10)}.json`);

function runTree(tree, tagLabel) {
  const out = path.join(os.tmpdir(), `captive-portal-${tagLabel}.json`);
  const r = spawnSync(process.execPath, [path.join(ROOT, 'tools/measure-bootstrap-captive-portal.mjs')], {
    env: { ...process.env, TREE: tree, OUT: out, TAG_LABEL: tagLabel },
    encoding: 'utf8',
  });
  if (r.status !== 0) {
    console.error(`measurement failed for ${tagLabel}:\n${r.stdout}\n${r.stderr}`);
    process.exit(1);
  }
  const report = JSON.parse(fs.readFileSync(out, 'utf8'));
  const faces = report.faces.map((f) => ({
    mode: f.mode,
    outcome: f.outcome,
    ...(f.message ? { message: f.message } : {}),
    ...(typeof f.cached === 'boolean' ? { cached: f.cached, primed: f.primed === true } : {}),
  }));
  const mis = faces.filter((f) => f.outcome === 'refused' && /has no asset named/.test(f.message || '')
    && f.mode !== 'F2 api-ratelimit-guard').length;
  // misverdict counts ONLY the rate-limit face blaming the release page;
  // G2's honest "has no asset named" is not in the measured face set.
  const misverdict = faces.filter((f) => f.mode === 'F2 api-ratelimit' && /has no asset named/.test(f.message || '')).length;
  const rawCrash = faces.filter((f) => f.outcome === 'raw-crash').length;
  return { tag: tagLabel, faces, rawCrash, misverdict, mis };
}

// Baseline: the published tag, extracted clean.
const baseDir = fs.mkdtempSync(path.join(os.tmpdir(), 'reeflog-captive-base-'));
execFileSync('sh', ['-c', `git -C ${JSON.stringify(ROOT)} archive ${BASE_TAG} | tar -x -C ${JSON.stringify(baseDir)}`]);
const baseline = runTree(baseDir, BASE_TAG);
const fixed = runTree(ROOT, 'this-tree');
fs.rmSync(baseDir, { recursive: true, force: true });

const baselineShowsDefect = baseline.rawCrash >= 1 && baseline.misverdict >= 1;
const fixedClean = fixed.rawCrash === 0 && fixed.misverdict === 0;

const artifact = {
  measuredAt: new Date().toISOString(),
  defect: `bootstrap() trusted the release-API leg to answer a release object: HTML (captive portal/proxy) crashed out of it with a raw SyntaxError, and a rate-limit JSON was refused as "the release has no asset" — a verdict that blames a release page that is fine`,
  baseline,
  fixed,
  baselineShowsDefect,
  fixedClean,
  verdict: baselineShowsDefect && fixedClean ? 'contract-holds' : 'MEASUREMENT-INVALID',
};
fs.writeFileSync(OUT, JSON.stringify(artifact, null, 2) + '\n');
console.log(JSON.stringify({ OUT, verdict: artifact.verdict, baseline: `${baseline.rawCrash} raw-crash / ${baseline.misverdict} misverdict`, fixed: `${fixed.rawCrash} raw-crash / ${fixed.misverdict} misverdict` }));
if (!baselineShowsDefect || !fixedClean) process.exit(1);
