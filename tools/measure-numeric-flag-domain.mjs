#!/usr/bin/env node
// The DOMAIN of a numeric flag's value, measured against REAL binaries.
//
// The defect (measured 2026-11-08 on the published v0.7.2 tree, HEAD
// 2c05d31): v0.7.2 closed the "not a number" fallback — but the CHECKER
// parses `u64` while the CONSUMERS parse `i64` or add the value to a clock.
// A value inside the checker's space and outside the consumer's space fell
// straight back into the same silent-default hole, one layer up:
//
//   tail --limit 18446744073709551615     -> exit 0, 1000 rows (the
//     DEFAULT limit): the `i64` parse failed, the failure became the default
//   verify --since-slot 9223372036854775808
//                                           -> exit 3, `"since_slot": null`:
//     the operator's window was DROPPED, the scan ran over another window
//   start --seconds 18446744073709551615  -> panic "overflow when adding
//     duration to instant" (main.rs:534)
//   start --repair-every 18446744073709551615
//                                           -> the scheduler panics on the
//     runtime worker (main.rs:657) and the daemon HANGS (killed at 15 s)
//
// The tool measures both trees on the same cases and writes ONE artifact:
//
//   BASELINE  the published v0.7.2 binary (git archive of the tag)
//   FIXED     the binary built from this tree
//
// Per case: exit code, refusal, rows delivered, timeout (a run that never
// ends is worse than one that misbehaves), and whether stdout claimed the
// requested window. Guards pin the false-positive side: legal values and
// `0` must keep working.
//
// Usage:
//   node tools/measure-numeric-flag-domain.mjs
//   node tools/measure-numeric-flag-domain.mjs --baseline <bin> --fixed <bin>
//
// Exit: 0 all cases behave as specified on the FIXED tree, 1 a case failed,
// 2 setup failed.
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { execFileSync, spawnSync } from 'node:child_process';
import { fileURLToPath } from 'node:url';

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const ROOT = path.resolve(__dirname, '..');
const OUT = path.resolve(
  (
    process.env.OUT ||
    (process.argv.includes('--out')
      ? process.argv[process.argv.indexOf('--out') + 1]
      : path.join(ROOT, 'research', 'numeric-flag-domain-2026-11-08.json'))
  ).replace(/^file:/, ''),
);

const PROGRAM = 'whirLbMiicVdio4qvUfM5KAg6Ct8VwpYzGff3uctyCc';
const U64_MAX = '18446744073709551615';
const I64_MAX_PLUS_1 = '9223372036854775808';
const I64_MAX = '9223372036854775807';

function argOf(flag) {
  const i = process.argv.indexOf(flag);
  return i >= 0 ? process.argv[i + 1] : null;
}

// --- the trees ------------------------------------------------------------

/** The published v0.7.2 tree, taken from the TAG and not from the checkout:
 * a baseline that is "the current tree with the fix removed" would prove
 * nothing about what a stranger installs. */
function baselineBinary() {
  const given = argOf('--baseline');
  if (given) return { bin: path.resolve(given), source: 'given' };
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'reeflog-domain-baseline-'));
  const tag = process.env.BASELINE_TAG || 'v0.7.2';
  const tar = path.join(dir, 'baseline.tar');
  execFileSync('git', ['archive', '-o', tar, tag], { cwd: ROOT, stdio: 'inherit' });
  const src = path.join(dir, 'src');
  fs.mkdirSync(src);
  execFileSync('tar', ['-xf', tar, '-C', src]);
  execFileSync('cargo', ['build', '--release', '--locked'], { cwd: src, stdio: 'inherit' });
  return { bin: path.join(src, 'target', 'release', 'reeflog'), source: `git archive ${tag}` };
}

function fixedBinary() {
  const given = argOf('--fixed');
  if (given) return { bin: path.resolve(given), source: 'given' };
  execFileSync('cargo', ['build', '--release', '--locked'], { cwd: ROOT, stdio: 'inherit' });
  return { bin: path.join(ROOT, 'target', 'release', 'reeflog'), source: 'this tree' };
}

// --- a journal with real rows -------------------------------------------

function makeJournal(bin, rows) {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'reeflog-nfd-'));
  const db = path.join(dir, 'reeflog.db');
  const init = spawnSync(bin, ['init', '--db', db, '--program', PROGRAM], { encoding: 'utf8' });
  if (init.status !== 0) throw new Error(`init failed: ${init.stderr}`);
  if (rows > 0) {
    const script = `
import sqlite3, sys, time
db, count = sys.argv[1], int(sys.argv[2])
c = sqlite3.connect(db)
now = int(time.time())
for i in range(count):
    sig = ("NFD%056d" % i)
    c.execute("insert or ignore into events (signature, slot, err, logs, source, first_seen, seal_status) values (?,?,?,?,?,?,?)",
              (sig, 44000000+i, None, "[]", "ws", now, "sealed"))
c.commit()
print(c.execute("select count(*) from events").fetchone()[0])
`;
    const out = execFileSync('python3', ['-c', script, db, String(rows)], { encoding: 'utf8' });
    if (Number(out.trim()) !== rows) throw new Error(`seeding failed: ${out}`);
  }
  return { dir, db };
}

function runCase(bin, dir, args, { timeout = 30000 } = {}) {
  const r = spawnSync(bin, args, { cwd: dir, encoding: 'utf8', timeout });
  const stdout = r.stdout || '';
  const stderr = r.stderr || '';
  let delivered = 0;
  for (const line of stdout.split('\n')) {
    if (!line.trim()) continue;
    try {
      const row = JSON.parse(line);
      if (row && typeof row === 'object' && ('signature' in row || 'seq' in row)) delivered += 1;
    } catch {
      // not a row line
    }
  }
  const timedOut = r.signal === 'SIGTERM' || r.status === null;
  const refused = /expects a (non-negative whole number|delay)/.test(stderr);
  const panicked = stderr.includes('panicked');
  const didWork = delivered > 0 || stdout.includes('"since_slot"') || (!timedOut && r.status === 0);
  return {
    args,
    exit: r.status,
    timedOut,
    panicked,
    refused,
    didWork,
    rowsDelivered: delivered,
    stderrHead: stderr.trim().split('\n').slice(0, 3).join(' | ').slice(0, 400),
    stdoutHead: stdout.trim().split('\n').slice(0, 1).join('').slice(0, 200),
  };
}

// --- the cases ------------------------------------------------------------

const CASES = [
  {
    id: 'limit-above-i64',
    args: ['tail', '--db', 'reeflog.db', '--cursor', 'c.txt', '--limit', U64_MAX],
    rows: 1200,
    expect: 'refusal',
    mustNotDeliver: true,
    why: 'a limit the consumer cannot hold must be refused, not parsed into the default (1000 rows)',
  },
  {
    id: 'limit-i64-max-plus-one',
    args: ['tail', '--db', 'reeflog.db', '--cursor', 'c.txt', '--limit', I64_MAX_PLUS_1],
    rows: 1200,
    expect: 'refusal',
    mustNotDeliver: true,
    why: 'the exact boundary the i64 consumer cannot parse',
  },
  {
    id: 'hydrate-limit-above-i64',
    args: ['hydrate', '--db', 'reeflog.db', '--limit', I64_MAX_PLUS_1],
    rows: 5,
    expect: 'refusal',
    why: 'same consumer class: the hydration budget must not become the default',
  },
  {
    id: 'since-slot-above-i64',
    args: ['verify', '--db', 'reeflog.db', '--program', PROGRAM, '--pages', '1', '--since-slot', I64_MAX_PLUS_1],
    rows: 5,
    expect: 'refusal',
    why: 'a dropped window means the scan ran over a different window than asked',
  },
  {
    id: 'seconds-unschedulable',
    args: ['start', '--db', 'reeflog.db', '--program', PROGRAM, '--seconds', U64_MAX, '--rpc', 'http://127.0.0.1:9', '--ws', 'ws://127.0.0.1:9'],
    rows: 0,
    expect: 'refusal',
    why: 'adding this to the clock panics (main.rs:534); a panic is not a refusal',
    timeout: 15000,
  },
  {
    id: 'seconds-count-legal-but-unschedulable',
    args: ['start', '--db', 'reeflog.db', '--program', PROGRAM, '--seconds', I64_MAX, '--rpc', 'http://127.0.0.1:9', '--ws', 'ws://127.0.0.1:9'],
    rows: 0,
    expect: 'refusal',
    why: 'i64::MAX passes the range gate but is not schedulable — pins the duration gate apart',
    timeout: 15000,
  },
  {
    id: 'repair-every-unschedulable',
    args: ['start', '--db', 'reeflog.db', '--program', PROGRAM, '--seconds', '1', '--repair-every', U64_MAX, '--rpc', 'http://127.0.0.1:9', '--ws', 'ws://127.0.0.1:9'],
    rows: 0,
    expect: 'refusal',
    why: 'the scheduler panics on the runtime worker and the daemon HANGS',
    timeout: 15000,
  },
  // guards: documented behaviour must not change
  {
    id: 'guard-limit-5',
    args: ['tail', '--db', 'reeflog.db', '--cursor', 'c2.txt', '--limit', '5'],
    rows: 20,
    expect: 'accepted',
    mustDeliver: 5,
    why: 'a legal limit must deliver exactly what was asked',
  },
  {
    id: 'guard-limit-zero',
    args: ['tail', '--db', 'reeflog.db', '--cursor', 'c3.txt', '--limit', '0'],
    rows: 20,
    expect: 'accepted',
    why: '0 stays legal (documented as "disable this pass")',
  },
  {
    id: 'guard-limit-i64-max',
    args: ['tail', '--db', 'reeflog.db', '--cursor', 'c4.txt', '--limit', I64_MAX],
    rows: 20,
    expect: 'accepted',
    mustDeliver: 20,
    why: 'the largest value the consumer CAN hold must stay legal — refusing it would be a false positive',
  },
  {
    id: 'guard-since-slot-inside-domain',
    args: ['verify', '--db', 'reeflog.db', '--program', PROGRAM, '--pages', '1', '--since-slot', '440000010'],
    rows: 20,
    expect: 'accepted',
    why: 'a window inside the domain must be reported back, not refused',
  },
];

// --- run ------------------------------------------------------------------

function verdictOf(c, r) {
  if (c.expect === 'refusal') {
    if (r.timedOut) return 'TIMED OUT (never ended)';
    if (r.panicked) return 'PANICKED';
    return r.refused && !r.didWork ? 'refused' : 'NOT REFUSED';
  }
  if (r.refused) return 'REFUSED (false positive)';
  if (r.timedOut) return 'TIMED OUT';
  if (c.mustDeliver !== undefined && r.rowsDelivered !== c.mustDeliver) {
    return `delivered ${r.rowsDelivered} (want ${c.mustDeliver})`;
  }
  return 'accepted';
}

function measure(label, tree) {
  const bin = path.resolve(tree.bin);
  const out = [];
  for (const c of CASES) {
    const { dir } = makeJournal(bin, c.rows);
    const r = runCase(bin, dir, c.args, { timeout: c.timeout || 30000 });
    const verdict = verdictOf(c, r);
    const want = c.expect === 'refusal' ? 'refused' : 'accepted';
    out.push({
      id: c.id,
      why: c.why,
      expect: c.expect,
      verdict,
      ok: verdict === want,
      ...r,
    });
    fs.rmSync(dir, { recursive: true, force: true });
  }
  console.log(`\n== ${label} (${tree.source})`);
  for (const c of out) {
    console.log(
      `${c.ok ? 'OK  ' : 'FAIL'} ${c.id}: exit=${c.exit} rows=${c.rowsDelivered} timedOut=${c.timedOut} panicked=${c.panicked} verdict=${c.verdict}`,
    );
    if (!c.ok) console.log(`      stderr: ${c.stderrHead}`);
  }
  return out;
}

let baseline, fixed;
try {
  baseline = baselineBinary();
  fixed = fixedBinary();
} catch (error) {
  console.error(`setup failed: ${error.message}`);
  process.exit(2);
}

const baselineCases = measure('BASELINE (published v0.7.2)', baseline);
const fixedCases = measure('FIXED (this tree)', fixed);

const result = {
  generatedAt: new Date().toISOString(),
  defect:
    "the numeric-flag checker validated `u64` while consumers parse `i64` or add to a clock: an out-of-domain value silently became the DEFAULT (--limit 1000 rows, --since-slot null) or a panic/hang (start --seconds/--*-every)",
  method:
    'both binaries run the same cases against the same journal (1200/20/5 rows as noted); a refusal that still delivers rows, claims the wrong window, panics or never ends is not a refusal',
  baseline: { source: baseline.source, cases: baselineCases },
  fixed: { source: fixed.source, cases: fixedCases },
  summary: {
    baselineOk: baselineCases.filter((c) => c.ok).length,
    baselineTotal: baselineCases.length,
    fixedOk: fixedCases.filter((c) => c.ok).length,
    fixedTotal: fixedCases.length,
    baselineSilentDefaults: baselineCases.filter(
      (c) => c.expect === 'refusal' && (c.rowsDelivered > 0 || c.stdoutHead.includes('since_slot')),
    ).length,
    fixedSilentDefaults: fixedCases.filter(
      (c) => c.expect === 'refusal' && (c.rowsDelivered > 0 || c.stdoutHead.includes('since_slot')),
    ).length,
    baselineHangs: baselineCases.filter((c) => c.timedOut).length,
    fixedHangs: fixedCases.filter((c) => c.timedOut).length,
    baselinePanics: baselineCases.filter((c) => c.panicked).length,
    fixedPanics: fixedCases.filter((c) => c.panicked).length,
  },
};

fs.mkdirSync(path.dirname(OUT), { recursive: true });
fs.writeFileSync(OUT, `${JSON.stringify(result, null, 2)}\n`);
console.log(`\nwrote ${OUT}`);
console.log(
  `baseline ${result.summary.baselineOk}/${result.summary.baselineTotal} ok ` +
    `(silent defaults ${result.summary.baselineSilentDefaults}, hangs ${result.summary.baselineHangs}, panics ${result.summary.baselinePanics})`,
);
console.log(
  `fixed    ${result.summary.fixedOk}/${result.summary.fixedTotal} ok ` +
    `(silent defaults ${result.summary.fixedSilentDefaults}, hangs ${result.summary.fixedHangs}, panics ${result.summary.fixedPanics})`,
);

process.exit(result.summary.fixedOk === result.summary.fixedTotal ? 0 : 1);
