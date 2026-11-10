#!/usr/bin/env node
// The BEFORE/AFTER measurement for the REFUSAL VOICE: the same hostile
// values, run against a BASELINE binary and against the FIXED binary.
//
//   node tools/measure-error-voice.mjs <baseline-bin> <fixed-bin> <out.json> [baseline-label] [fixed-label]
//
// The baseline is usually the published asset (the release archive's sha256
// equals the digest GitHub Releases publishes). The fixed binary is the
// tree's own build — or a PUBLISHED asset, when the release itself is what
// must be measured.
//
// What it records per case: the exit code, the raw stderr bytes, how many
// LINES of it speak in reeflog's own voice (the real refusal is the first;
// every further one was FORGED by the value), and how many raw control
// bytes reached the stream.
//
// The artifact NAMES the binaries it measured: each side's `--version` is
// read from the binary itself, and the label the caller passed is kept.
import { spawnSync } from 'node:child_process';
import fs from 'node:fs';
import path from 'node:path';

const BASELINE = path.resolve(process.argv[2]);
const FIXED = path.resolve(process.argv[3]);
const OUT = process.argv[4];
const BASELINE_LABEL = process.argv[5] || null;
const FIXED_LABEL = process.argv[6] || null;
if (!BASELINE || !FIXED || !OUT) {
  console.error('usage: measure-error-voice.mjs <baseline-bin> <fixed-bin> <out.json> [baseline-label] [fixed-label]');
  process.exit(2);
}

function versionOf(bin) {
  const v = spawnSync(bin, ['--version'], { encoding: 'utf8' });
  const m = /(\d+\.\d+\.\d+)/.exec(v.stdout || '');
  return m ? m[1] : null;
}

const PROGRAM = 'whirLbMiicVdio4qvUfM5KAg6Ct8VwpYzGff3uctyCc';

function runCase(bin, c) {
  const dir = fs.mkdtempSync('/tmp/reeflog-ev-');
  const db = path.join(dir, 'j.db');
  spawnSync(bin, ['init', '--db', db, '--program', PROGRAM], { cwd: dir, encoding: 'utf8' });
  const cfg = path.join(dir, 'cfg.json');
  if (c.config) {
    // The hostile config: the EXACT bytes must reach the loader, so it is
    // serialised from the case (JSON.stringify escapes the control bytes and
    // the loader unescapes them again).
    fs.writeFileSync(cfg, JSON.stringify({ targets: [{ ...c.config, secret: 's' }] }));
  } else {
    fs.writeFileSync(cfg, JSON.stringify({ targets: [{ id: 'alpha', url: 'http://127.0.0.1:9/hook', secret: 's' }] }));
  }
  const args = c.args.map((a) => a.replace('CFG', cfg).replace('DB', db));
  const r = spawnSync(bin, args, { cwd: dir, encoding: 'buffer' });
  const stderr = r.stderr ? Buffer.from(r.stderr) : Buffer.alloc(0);
  const stdout = r.stdout ? r.stdout.toString('utf8') : '';
  const rec = {
    exitCode: r.status,
    refused: r.status !== 0,
    stdoutEmpty: stdout.trim() === '',
    reeflogLines: countReeflogLines(stderr),
    forgedLines: Math.max(0, countReeflogLines(stderr) - 1),
    controlBytes: countControlBytes(stderr),
    stderrText: stderr.toString('utf8'),
  };
  fs.rmSync(dir, { recursive: true, force: true });
  return rec;
}

function countReeflogLines(buf) {
  return buf
    .toString('utf8')
    .split('\n')
    .filter((l) => l.startsWith('reeflog: ')).length;
}

// Every C0 control byte except HTAB, plus DEL — excluding the ONE trailing
// newline the message ends with, which is the line terminator and not part
// of the message.
function countControlBytes(buf) {
  let body = buf;
  if (body.length && body[body.length - 1] === 0x0a) body = body.slice(0, -1);
  let n = 0;
  for (const b of body) {
    if ((b < 0x20 && b !== 0x09) || b === 0x7f) n++;
  }
  return n;
}

// The hostile values. Each one must be REFUSED by both binaries — the
// defect is never the verdict, it is the VOICE the verdict speaks in.
const cases = [
  {
    case: 'config-id-newline',
    hostileValue: 'a config `id` carrying a newline',
    args: ['webhook', 'status', '--db', 'DB', '--config', 'CFG'],
    config: { id: 't1\nreeflog: error: forged', url: 'http://127.0.0.1:9/hook' },
  },
  {
    case: 'config-url-newline',
    hostileValue: 'a config `url` carrying a newline',
    args: ['webhook', 'status', '--db', 'DB', '--config', 'CFG'],
    config: { id: 't1', url: 'http://127.0.0.1:9/a\nreeflog: error: forged' },
  },
  {
    case: 'flag-rpc-newline',
    hostileValue: 'a `--rpc` value carrying a newline',
    args: ['start', '--db', 'DB', '--program', PROGRAM, '--rpc', 'http://h/a\nreeflog: error: forged'],
    config: null,
  },
  {
    case: 'flag-numeric-newline',
    hostileValue: 'a `--seconds` value carrying a newline',
    args: ['start', '--db', 'DB', '--program', PROGRAM, '--seconds', '5\nreeflog: error: forged'],
    config: null,
  },
  {
    case: 'flag-target-newline',
    hostileValue: 'a `--target` value carrying a newline',
    args: ['webhook', 'dlq', 'list', '--db', 'DB', '--config', 'CFG', '--target', 'a\nreeflog: error: forged'],
    config: null,
  },
  {
    case: 'config-id-esc',
    hostileValue: 'a config `id` carrying an ESC sequence',
    args: ['webhook', 'status', '--db', 'DB', '--config', 'CFG'],
    config: { id: 't1\u001b[31mRED', url: 'http://127.0.0.1:9/hook' },
  },
  {
    case: 'flag-target-esc',
    hostileValue: 'a `--target` value carrying an ESC sequence',
    args: ['webhook', 'dlq', 'list', '--db', 'DB', '--config', 'CFG', '--target', 'a\u001b[31mRED'],
    config: null,
  },
  {
    case: 'plain-refusal',
    hostileValue: 'a PLAIN value (the guard: the message must not change)',
    args: ['start', '--db', 'DB', '--program', PROGRAM, '--seconds', 'abc'],
    config: null,
  },
];

const rows = [];
for (const c of cases) {
  // The config-based cases need the hostile config IN PLACE, so they are run
  // through a variant that writes it.
  const before = runCase(BASELINE, c);
  const after = runCase(FIXED, c);
  rows.push({ case: c.case, hostileValue: c.hostileValue, before, after });
  console.log(
    `${c.case.padEnd(24)} baseline: lines ${before.reeflogLines} forged ${before.forgedLines} ctrl ${before.controlBytes}` +
      `   |   fixed: lines ${after.reeflogLines} forged ${after.forgedLines} ctrl ${after.controlBytes}`
  );
}

const out = {
  artifact: 'error-voice-before-after',
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
    'the same hostile values against both binaries; the verdict is recorded too, because the ' +
    'defect is never the verdict — it is the VOICE the refusal speaks in: a refusal must be ONE ' +
    'line of reeflog\'s own, and no value it quotes may reach the stream as a raw control byte',
  cases: rows,
  summary: {
    total: rows.length,
    baselineRefused: rows.filter((r) => r.before.refused).length,
    baselineForgedLines: rows.reduce((n, r) => n + r.before.forgedLines, 0),
    baselineControlBytes: rows.reduce((n, r) => n + r.before.controlBytes, 0),
    fixedRefused: rows.filter((r) => r.after.refused).length,
    fixedForgedLines: rows.reduce((n, r) => n + r.after.forgedLines, 0),
    fixedControlBytes: rows.reduce((n, r) => n + r.after.controlBytes, 0),
  },
};
fs.writeFileSync(OUT, JSON.stringify(out, null, 2) + '\n');
console.log(JSON.stringify(out.summary, null, 2));
console.log('wrote', OUT);
