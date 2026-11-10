#!/usr/bin/env node
// The BEFORE/AFTER measurement for the PROGRAM IDENTITY: the same calls,
// run against a BASELINE binary and against the FIXED binary.
//
//   node tools/measure-program-identity.mjs <baseline-bin> <fixed-bin> <out.json> [baseline-label] [fixed-label]
//
// The baseline is usually the published asset (the release archive's sha256
// equals the digest GitHub Releases publishes). The fixed binary is the
// tree's own build — or a PUBLISHED asset, when the release itself is what
// must be measured.
//
// What it records per case: the exit code, whether the journal's recorded
// program CHANGED (the identity — read back from the journal, never from
// the report), and whether the refusal spoke in reeflog's own voice.
//
// The artifact NAMES the binaries it measured: each side's `--version` is
// read from the binary itself, and the label the caller passed is kept.
import { spawnSync } from 'node:child_process';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';

// The artifact is committed; it must not carry the machine it was produced
// on. Journal paths are temp dirs — they become `<journal>`.
function scrub(text) {
  if (!text) return text;
  return text.split(os.tmpdir()).join('<journal>');
}

const BASELINE = path.resolve(process.argv[2]);
const FIXED = path.resolve(process.argv[3]);
const OUT = process.argv[4];
const BASELINE_LABEL = process.argv[5] || null;
const FIXED_LABEL = process.argv[6] || null;
if (!BASELINE || !FIXED || !OUT) {
  console.error('usage: measure-program-identity.mjs <baseline-bin> <fixed-bin> <out.json> [baseline-label] [fixed-label]');
  process.exit(2);
}

function versionOf(bin) {
  const v = spawnSync(bin, ['--version'], { encoding: 'utf8' });
  const m = /(\d+\.\d+\.\d+)/.exec(v.stdout || '');
  return m ? m[1] : null;
}

const PROGRAM_A = '11111111111111111111111111111111';
const PROGRAM_B = 'whirLbMiicVdio4qvUfM5KAg6Ct8VwpYzGff3uctyCc';

// The journal's recorded program — the IDENTITY. Read back from the
// journal with `status`, never from the report of the command under test:
// the report is the thing that lied.
function recordedProgram(bin, dir, db) {
  const r = spawnSync(bin, ['status', '--db', db], { cwd: dir, encoding: 'utf8' });
  const stdout = r.stdout || '';
  const m = /"program":\s*"([^"]*)"/.exec(stdout);
  return m ? m[1] : null;
}

function runCase(bin, c) {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'reeflog-pi-'));
  const db = path.join(dir, 'j.db');
  spawnSync(bin, ['init', '--db', db, '--program', PROGRAM_A], { cwd: dir, encoding: 'utf8' });
  const before = recordedProgram(bin, dir, db);
  const args = c.args.map((a) => a.replace('DB', db));
  const r = spawnSync(bin, args, { cwd: dir, encoding: 'buffer' });
  const stderr = r.stderr ? r.stderr.toString('utf8') : '';
  const after = recordedProgram(bin, dir, db);
  const rec = {
    exitCode: r.status,
    refused: r.status !== 0,
    recordedProgramBefore: before,
    recordedProgramAfter: after,
    identityChanged: before !== after,
    namedRecordedProgram: stderr.includes(PROGRAM_A),
    namedGivenProgram: stderr.includes(PROGRAM_B),
    refusedInOwnVoice: stderr.startsWith('reeflog: error: '),
    stderrText: scrub(stderr.trim()),
  };
  fs.rmSync(dir, { recursive: true, force: true });
  return rec;
}

// The cases: three faces of the hole (init re-points, verify ignores,
// repair ignores) plus the guards.
const cases = [
  {
    case: 'init-repoints',
    defect:
      '`init --program <B>` on a journal watching <A> silently RE-POINTED the mailbox ' +
      '(exit 0, "initialized … for program B") — the journal changed identity behind the operator',
    args: ['init', '--db', 'DB', '--program', PROGRAM_B],
  },
  {
    case: 'verify-ignores',
    defect:
      '`verify --program <B>` on a journal watching <A> SILENTLY IGNORED the flag ' +
      '(exit 0, a full report) — the operator asked about B and got a verdict about A',
    args: ['verify', '--db', 'DB', '--program', PROGRAM_B, '--pages', '1'],
  },
  {
    case: 'repair-ignores',
    defect:
      '`repair --program <B>` on a journal watching <A> SILENTLY IGNORED the flag — ' +
      'on the command that WRITES to the journal',
    args: ['repair', '--db', 'DB', '--program', PROGRAM_B, '--pages', '1'],
  },
  {
    case: 'guard-matching-program',
    defect: null,
    args: ['verify', '--db', 'DB', '--program', PROGRAM_A, '--pages', '1'],
  },
  {
    case: 'guard-silent-caller',
    defect: null,
    args: ['verify', '--db', 'DB', '--pages', '1'],
  },
  {
    case: 'guard-idempotent-init',
    defect: null,
    args: ['init', '--db', 'DB', '--program', PROGRAM_A],
  },
];

const rows = [];
for (const c of cases) {
  const before = runCase(BASELINE, c);
  const after = runCase(FIXED, c);
  rows.push({ case: c.case, defect: c.defect, before, after });
  console.log(
    `${c.case.padEnd(26)} baseline: exit ${before.exitCode} identityChanged ${before.identityChanged}` +
      `   |   fixed: exit ${after.exitCode} identityChanged ${after.identityChanged} refused ${after.refused}`
  );
}

// A defect case is VIOLATED when the journal's identity changed, or when
// the command did NOT refuse the disagreement in reeflog's own voice naming
// BOTH programs. Exit codes alone cannot tell: a baseline `verify` that
// silently IGNORED the flag ends in an INCOMPLETE scan (exit 3) — about the
// journal's OWN program, not the one the caller asked about. A guard case
// is violated when the identity changed or the guard was met with an
// identity refusal.
function violated(rec, c) {
  if (rec.identityChanged) return true;
  if (c.defect) {
    return !(
      rec.refused &&
      rec.namedRecordedProgram &&
      rec.namedGivenProgram &&
      rec.refusedInOwnVoice
    );
  }
  return rec.stderrText.includes('refusing');
}

const out = {
  artifact: 'program-identity-before-after',
  measuredAt: new Date().toISOString(),
  baseline: {
    binary: BASELINE_LABEL || 'baseline binary',
    version: versionOf(BASELINE),
  },
  fixed: {
    binary: FIXED_LABEL || 'fixed binary',
    version: versionOf(FIXED),
  },
  method:
    'the same calls against both binaries; the journal\'s recorded program is read back from the ' +
    'journal itself, never from the report of the command under test — the report is the thing that ' +
    'lied. A defect case is violated when the identity CHANGED, or when the disagreement was not ' +
    'refused in reeflog\'s own voice naming BOTH programs (exit codes cannot tell: a baseline ' +
    '`verify` that silently ignored the flag still ends in an INCOMPLETE scan). A guard case is ' +
    'violated when the identity changed or the guard met an identity refusal.',
  cases: rows,
  summary: {
    total: rows.length,
    baselineViolations: rows.filter((r) => violated(r.before, r)).length,
    fixedViolations: rows.filter((r) => violated(r.after, r)).length,
    baselineIdentityChanges: rows.filter((r) => r.before.identityChanged).length,
    fixedIdentityChanges: rows.filter((r) => r.after.identityChanged).length,
  },
};
fs.writeFileSync(OUT, JSON.stringify(out, null, 2) + '\n');
console.log(JSON.stringify(out.summary, null, 2));
// The instrument refuses to certify a run where the baseline showed NO
// defect: that is the tool measuring nothing, not a fixed tree.
if (out.summary.baselineViolations === 0) {
  console.error('NO VERDICT: the baseline showed no violation — the measurement proves nothing');
  process.exit(1);
}
if (out.summary.fixedViolations !== 0) {
  console.error(`NO VERDICT: the fixed side still violates ${out.summary.fixedViolations} cases`);
  process.exit(1);
}
console.log('wrote', OUT);
