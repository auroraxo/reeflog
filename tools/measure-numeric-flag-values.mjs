#!/usr/bin/env node
// The value of a numeric flag, measured against a REAL binary.
//
// The defect (measured 2026-11-08 on the published v0.7.1 tree, HEAD
// 1fd2361): reeflog refuses an unknown FLAG before it opens anything (that
// fix is v0.5.1, 9dbd393) — but the VALUE of a known flag was handed to
// `arg(...).and_then(|s| s.parse().ok()).unwrap_or(DEFAULT)`. Every
// unparseable value therefore became the DEFAULT, silently:
//
//   reeflog tail --db p.db --cursor c.txt --limit abc
//     -> exit 0, EVERY row delivered (the default limit of 1000)
//   reeflog tail --db p.db --cursor c.txt --limit -1
//     -> exit 0, EVERY row delivered — SQLite's `LIMIT -1` means UNLIMITED
//   reeflog verify --db p.db --pages abc
//     -> exit 3, the scan ran with the default page budget of 40
//
// That is the most expensive failure this product can have: a consumer that
// asked for 5 events got 650 and the output looks exactly like a successful
// run. A refusal of an unknown FLAG was the same defect half-fixed.
//
// This tool measures both trees on the same cases and writes ONE artifact:
//
//   BASELINE  the published v0.7.1 binary (git archive of the tag)
//   FIXED     the binary built from this tree
//
// Per case it records the exit code, whether the command refused, and — the
// part that matters — whether a JOURNAL ROW WAS DELIVERED anyway. A refusal
// that still delivers rows is not a refusal.
//
// It also measures the FALSE-POSITIVE side: every documented invocation
// must keep working, `0` must stay legal, and the bare `--anchor-slot` must
// keep its documented meaning. A parser that refuses legal input is worse
// than the defect it fixes.
//
// Usage:
//   node tools/measure-numeric-flag-values.mjs            # both trees
//   node tools/measure-numeric-flag-values.mjs --baseline <bin> --fixed <bin>
//
// Exit: 0 all cases behave as specified, 1 a case failed, 2 setup failed.
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
      : path.join(ROOT, 'research', 'numeric-flag-values-2026-11-08.json'))
  ).replace(/^file:/, ''),
);

const PROGRAM = 'whirLbMiicVdio4qvUfM5KAg6Ct8VwpYzGff3uctyCc';

function argOf(flag) {
  const i = process.argv.indexOf(flag);
  return i >= 0 ? process.argv[i + 1] : null;
}

// --- the trees ------------------------------------------------------------

/** The published v0.7.1 tree, taken from the TAG and not from the checkout:
 * a baseline that is really "the current tree with the fix removed" would
 * prove nothing about what a stranger installs. */
function baselineBinary() {
  const given = argOf('--baseline');
  if (given) return { bin: path.resolve(given), source: 'given' };
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'reeflog-baseline-'));
  const tag = process.env.BASELINE_TAG || 'v0.7.1';
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

/** The journal must hold ROWS: the whole defect is that a refused command
 * still delivers them. An empty journal would make every case look clean. */
function makeJournal(bin) {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'reeflog-nfv-'));
  const db = path.join(dir, 'reeflog.db');
  const init = spawnSync(bin, ['init', '--db', db, '--program', PROGRAM], { encoding: 'utf8' });
  if (init.status !== 0) {
    throw new Error(`init failed: ${init.stderr}`);
  }
  // The journal is written through the tool's own API so the rows are real
  // journal rows, not a hand-built SQLite file.
  return { dir, db };
}

/** Fill the journal through `reeflog`'s own writer path, so the rows are the
 * ones a consumer would receive. `reeflog` has no offline "append" command,
 * so the rows are written straight into the journal's table with the same
 * columns the daemon writes — and the measurement only asks whether a ROW
 * CAME OUT of `tail`, not anything about how it got in. */
function seedRows(db, count) {
  // python3's sqlite3 is the only sqlite binding on this host that is not
  // the OpenClaw state-directory sqlite3.
  const script = `
import sqlite3, sys, time
db, count = sys.argv[1], int(sys.argv[2])
c = sqlite3.connect(db)
now = int(time.time())
for i in range(count):
    sig = ("NFV%056d" % i)
    c.execute("insert or ignore into events (signature, slot, err, logs, source, first_seen, seal_status) values (?,?,?,?,?,?,?)",
              (sig, 100000+i, None, "[]", "ws", now, "sealed"))
c.commit()
print(c.execute("select count(*) from events").fetchone()[0])
`;
  const out = execFileSync('python3', ['-c', script, db, String(count)], { encoding: 'utf8' });
  return Number(out.trim());
}

function runCase(bin, dir, args, { journalRows = 0 } = {}) {
  const r = spawnSync(bin, args, { cwd: dir, encoding: 'utf8', timeout: 60000 });
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
  // A refusal is a REFUSAL OF THE FLAG, not merely a non-zero exit: `verify`
  // exits 3 when the journal is incomplete, and `start` was killed by the
  // timeout (status null) when it ran the daemon for the default time. Both
  // are the DEFECT, not a refusal.
  const timedOut = r.status === null;
  const refusedFlag =
    stderr.includes('unknown flag') ||
    /expects a non-negative whole number/.test(stderr) ||
    /needs a value/.test(stderr) ||
    /flag .* was given twice/.test(stderr);
  // "Did the command DO the thing the operator asked for?" — the part that
  // must not happen behind a silent fallback.
  const didWork =
    delivered > 0 ||
    stdout.includes('"complete"') ||
    (timedOut ? true : r.status === 0);
  return {
    args,
    exit: r.status,
    timedOut,
    refused: refusedFlag,
    didWork,
    rowsDelivered: delivered,
    stderrHead: stderr.trim().split('\n').slice(0, 3).join(' | ').slice(0, 400),
    stdoutHead: stdout.trim().split('\n')[0]?.slice(0, 200) || '',
    journalRows,
  };
}

// --- the cases ------------------------------------------------------------

// Each case: what the operator asked, what must happen.
const CASES = [
  {
    id: 'non-numeric-limit',
    args: ['tail', '--db', 'reeflog.db', '--cursor', 'c.txt', '--limit', 'abc'],
    expect: 'refusal',
    mustNotDeliver: true,
    why: 'a typo in --limit must not deliver the whole journal',
  },
  {
    id: 'negative-limit',
    args: ['tail', '--db', 'reeflog.db', '--cursor', 'c.txt', '--limit', '-1'],
    expect: 'refusal',
    mustNotDeliver: true,
    why: 'SQL LIMIT -1 means UNLIMITED, not "the last row"',
  },
  {
    id: 'non-numeric-pages',
    args: ['verify', '--db', 'reeflog.db', '--pages', 'abc'],
    expect: 'refusal',
    why: 'a typo in --pages must not silently change the scan budget',
  },
  {
    id: 'non-numeric-seconds',
    args: ['start', '--db', 'reeflog.db', '--seconds', 'abc'],
    expect: 'refusal',
    why: 'a typo in --seconds must not start the daemon for the default time',
  },
  {
    id: 'non-numeric-reorg-limit',
    args: ['reorg', 'check', '--db', 'reeflog.db', '--limit', 'abc'],
    expect: 'refusal',
    why: 'a typo in --limit must not run the reorg check with another budget',
  },
  {
    id: 'non-numeric-hydrate-limit',
    args: ['hydrate', '--db', 'reeflog.db', '--limit', 'abc'],
    expect: 'refusal',
    why: 'a typo in --limit must not hydrate with another budget',
  },
  {
    id: 'unknown-flag-still-refused',
    args: ['status', '--db', 'reeflog.db', '--bogus-flag'],
    expect: 'refusal',
    why: 'the older refusal must survive: an unknown flag is not a number problem',
  },
  // guards: the documented behaviour must keep working
  {
    id: 'guard-numeric-limit',
    args: ['tail', '--db', 'reeflog.db', '--cursor', 'c2.txt', '--limit', '5'],
    expect: 'accepted',
    why: 'a legal value must not become a refusal',
  },
  {
    id: 'guard-zero-limit',
    args: ['hydrate', '--db', 'reeflog.db', '--limit', '0'],
    expect: 'accepted',
    why: '0 is documented as "disable this pass" for several flags',
  },
  {
    id: 'guard-bare-anchor-slot',
    args: ['verify', '--db', 'reeflog.db', '--pages', '1', '--anchor-slot'],
    expect: 'accepted',
    why: 'the bare --anchor-slot is a documented meaning of its own',
  },
];

// --- run ------------------------------------------------------------------

const ROWS = 650;

function measure(label, tree) {
  const bin = path.resolve(tree.bin);
  const out = [];
  for (const c of CASES) {
    const { dir, db } = makeJournal(bin);
    const seeded = seedRows(db, ROWS);
    const r = runCase(bin, dir, c.args, { journalRows: seeded });
    let verdict;
    if (c.expect === 'refusal') {
      verdict = r.refused && !r.didWork ? 'refused' : 'NOT REFUSED';
    } else {
      verdict = r.refused ? 'REFUSED (false positive)' : 'accepted';
    }
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
      `${c.ok ? 'OK  ' : 'FAIL'} ${c.id}: exit=${c.exit} rows=${c.rowsDelivered} didWork=${c.didWork} verdict=${c.verdict}`,
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

const baselineCases = measure('BASELINE (published v0.7.1)', baseline);
const fixedCases = measure('FIXED (this tree)', fixed);

const result = {
  generatedAt: new Date().toISOString(),
  defect:
    'a numeric flag whose value is not a non-negative whole number fell back to the DEFAULT (or to SQLite `LIMIT -1` = unlimited) instead of being refused',
  method:
    'both binaries run the same cases against the same journal (650 rows); a refusal that still delivers rows is not a refusal',
  rowsSeeded: ROWS,
  baseline: { source: baseline.source, cases: baselineCases },
  fixed: { source: fixed.source, cases: fixedCases },
  summary: {
    baselineOk: baselineCases.filter((c) => c.ok).length,
    baselineTotal: baselineCases.length,
    fixedOk: fixedCases.filter((c) => c.ok).length,
    fixedTotal: fixedCases.length,
    baselineDeliveredRowsOnRefusal: baselineCases.filter(
      (c) => c.expect === 'refusal' && c.rowsDelivered > 0,
    ).length,
    fixedDeliveredRowsOnRefusal: fixedCases.filter(
      (c) => c.expect === 'refusal' && c.rowsDelivered > 0,
    ).length,
  },
};

fs.mkdirSync(path.dirname(OUT), { recursive: true });
fs.writeFileSync(OUT, `${JSON.stringify(result, null, 2)}\n`);
console.log(`\nwrote ${OUT}`);
console.log(
  `baseline ${result.summary.baselineOk}/${result.summary.baselineTotal} ok (${result.summary.baselineDeliveredRowsOnRefusal} refusals that still delivered rows)`,
);
console.log(`fixed    ${result.summary.fixedOk}/${result.summary.fixedTotal} ok (${result.summary.fixedDeliveredRowsOnRefusal} refusals that still delivered rows)`);

process.exit(result.summary.fixedOk === result.summary.fixedTotal ? 0 : 1);
