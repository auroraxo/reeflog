#!/usr/bin/env node
// Every documented `reeflog ...` line in README.md, run against the built
// binary — the flag parser must never refuse its own documentation.
//
// Why this exists (2026-11-07): reeflog 9dbd393 added a strict per-command
// flag parser (v0.5.1). Its guard — tests/arg_parsing.rs
// `every_documented_flag_still_works` — is a HAND-WRITTEN list of cases. That
// list drifts from the README: it proves the parser accepts the cases someone
// remembered, not the lines the document actually shows. A strict parser that
// refuses a documented line is, in src/flags.rs's own words, "worse than the
// defect it fixes".
//
// Extraction rules (measured against the real README):
//   * a line continuation `... \` joins with the following line, so the
//     three-line `start --seconds 60 [--repair-every 30] ...` form is one
//     command, not three;
//   * optional bracket groups are EXPANDED: `[--repair-every 30]` is run both
//     with and without — a documented optional flag must work in both forms;
//   * placeholders are substituted, not skipped: `<pubkey>` becomes the real
//     Whirlpool program the README uses everywhere else, `<slot>` a number;
//   * the FIRST `reeflog ` occurrence on a line wins, so prose lines like
//     "reeflog hydrates missing content" are not commands.
//
// Classification:
//   * unknown flag / missing value / duplicated flag / stray positional /
//     usage text = FLAG DEFECT;
//   * any other error (no journal, RPC unreachable, config missing) is NOT a
//     flag defect — the README describes a sequence, only the flag spelling
//     is the claim;
//   * a command that is STILL RUNNING after the per-command cap was ACCEPTED:
//     the parser did not refuse it. The parser runs before any work, so a
//     refusal is immediate — a live command is the positive signal.
import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { spawn, execFileSync } from 'node:child_process';

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const ROOT = path.resolve(__dirname, '..');
const README = path.join(ROOT, 'README.md');
const BIN = process.env.REEFLOG_BIN || path.join(ROOT, 'target', 'release', 'reeflog');
const OUT = process.argv.includes('--out')
  ? process.argv[process.argv.indexOf('--out') + 1]
  : path.join(ROOT, 'research', 'documented-surface-2026-11-07.json');
const CAP_MS = Number(process.env.DOCSURFACE_CAP_MS || 12000);

const PROGRAM = 'whirLbMiicVdio4qvUfM5KAg6Ct8VwpYzGff3uctyCc';

const read = fs.readFileSync(README, 'utf8');

// --- extract fenced blocks, joining line continuations ---------------------
const documented = [];
const lines = read.split('\n');
let inFence = false;
let fenceLang = '';
for (let i = 0; i < lines.length; i++) {
  const ln = lines[i];
  const open = ln.match(/^\s*```(\w*)\s*$/);
  if (open) {
    if (!inFence) { inFence = true; fenceLang = open[1] || ''; }
    else inFence = false;
    continue;
  }
  if (!inFence) continue;
  if (fenceLang && !['bash', 'sh', 'shell'].includes(fenceLang)) continue;
  const idx = ln.indexOf('reeflog ');
  if (idx < 0) continue;

  let joined = ln.slice(idx).replace(/\\\s*$/, '').trim();
  let j = i;
  while (joined.includes('\\') || /^\s*\[/.test(lines[j + 1] || '')) {
    j++;
    const next = (lines[j] || '').trim();
    if (!next) break;
    joined = `${joined} ${next.replace(/\\\s*$/, '').trim()}`.replace(/\s+/g, ' ');
    if (!next.endsWith('\\') && !next.startsWith('[')) break;
  }
  const hash = joined.indexOf('#');
  if (hash >= 0) joined = joined.slice(0, hash);
  joined = joined.replace(/\\\s*$/, '').trim();
  if (!joined.startsWith('reeflog ')) continue;
  documented.push({ line: i + 1, cmd: joined.replace(/\s+/g, ' ') });
}

// --- expand optional groups and placeholders -------------------------------
function expand(cmd) {
  const bracket = /\[([^\]]+)\]/;
  let work = [cmd];
  let guard = 0;
  while (work.some((c) => bracket.test(c)) && guard++ < 8) {
    const next = [];
    for (const c of work) {
      const m = c.match(bracket);
      if (!m) { next.push(c); continue; }
      next.push(c.replace(m[0], m[1].trim()));
      next.push(c.replace(m[0], '').replace(/\s+/g, ' ').trim());
    }
    work = next;
  }
  const out = [];
  for (const c of work) {
    out.push(c.replace(/<pubkey>/g, PROGRAM).replace(/<slot>/g, '1').replace(/<[^>]+>/g, 'x').replace(/\s+/g, ' ').trim());
  }
  return [...new Set(out)];
}

const cases = [];
for (const d of documented) {
  for (const v of expand(d.cmd)) cases.push({ line: d.line, cmd: v });
}

// --- run each line against the binary -------------------------------------
function runOne(tokens, cwd) {
  return new Promise((resolve) => {
    const c = spawn(BIN, tokens, { cwd, stdio: ['ignore', 'pipe', 'pipe'] });
    let out = ''; let err = '';
    c.stdout.on('data', (d) => { out += d; });
    c.stderr.on('data', (d) => { err += d; });
    let done = false;
    const finish = (payload) => { if (!done) { done = true; resolve(payload); } };
    c.on('close', (code) => finish({ code, out, err, timedOut: false }));
    c.on('error', (e) => finish({ code: null, out, err: `${err}\n${e}`, timedOut: false, spawnError: String(e) }));
    const t = setTimeout(() => {
      if (!done) { try { c.kill('SIGKILL'); } catch { /* already gone */ } }
      finish({ code: null, out, err, timedOut: true });
    }, CAP_MS);
    if (t.unref) t.unref();
  });
}

const work = fs.mkdtempSync('/tmp/reeflog-docsurface-');
const results = [];
for (const c of cases) {
  const tokens = c.cmd.split(' ').slice(1);
  const res = await runOne(tokens, work);
  const stderr = (res.err || '').trim();
  const stdout = (res.out || '').trim();
  const combined = `${stderr}\n${stdout}`.toLowerCase();
  const flagDefect =
    combined.includes('unknown flag') ||
    combined.includes('needs a value') ||
    combined.includes('given more than once') ||
    combined.includes('unexpected argument') ||
    combined.includes('does not take a value') ||
    /^usage:/m.test(stderr) ||
    combined.includes('did you mean');
  results.push({
    line: c.line,
    command: c.cmd,
    exitCode: res.code,
    timedOut: res.timedOut,
    accepted: !flagDefect,
    flagDefect,
    stderr: stderr.split('\n').slice(0, 4).join(' | '),
    ...(res.spawnError ? { error: res.spawnError } : {}),
  });
  process.stderr.write(`  ${flagDefect ? 'DEFECT' : 'ok     '} ${c.cmd}\n`);
}

const defects = results.filter((r) => r.flagDefect);
const report = {
  measuredAt: new Date().toISOString(),
  readme: path.relative(ROOT, README),
  binary: BIN,
  binaryVersion: (() => {
    try { return execFileSync(BIN, ['--version'], { encoding: 'utf8' }).trim(); } catch { return null; }
  })(),
  documentedLines: documented.length,
  invocationsRun: cases.length,
  accepted: results.filter((r) => r.accepted).length,
  flagDefects: defects.length,
  defects,
  results,
  verdict: defects.length === 0 ? 'clean' : 'flag-defect',
};

fs.mkdirSync(path.dirname(OUT), { recursive: true });
fs.writeFileSync(OUT, `${JSON.stringify(report, null, 2)}\n`);
console.log(JSON.stringify({ ...report, results: undefined }, null, 2));
fs.rmSync(work, { recursive: true, force: true });
process.exit(defects.length === 0 ? 0 : 1);
