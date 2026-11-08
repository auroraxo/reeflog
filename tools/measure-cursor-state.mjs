#!/usr/bin/env node
// The CURSOR FILE's value domain, measured against REAL binaries.
//
// The defect (measured 2026-11-08 on the published v0.7.3 tree, HEAD
// 3461f0d): `read_cursor` was `.ok().and_then(parse).unwrap_or(0)`. The
// cursor file is the product's central promise ("a consumer resumes without
// gaps", "second iteration: 0 rows (no replay)") — and every way an EXISTING
// cursor file can fail to carry a cursor collapsed into "start from the
// beginning", delivered in full, with exit 0:
//
//   echo abc > c.txt && reeflog tail --cursor c.txt
//                                            -> exit 0, ALL rows replayed
//   echo 9223372036854775808 > c.txt && ...  -> exit 0, ALL rows replayed
//     (outside the consumer's i64 space; this value never reaches the flag
//      parser — it is read from state, so v0.7.2/v0.7.3's numeric flag gate
//      never sees it)
//   echo -1 > c.txt && ...                   -> exit 0, ALL rows replayed
//   : > c.txt && ...                         -> exit 0, ALL rows replayed
//     (what a crash between truncate and write leaves behind)
//
// The tool measures both trees on the same cases and writes ONE artifact:
//
//   BASELINE  the published v0.7.3 binary (git archive of the tag)
//   FIXED     the binary built from this tree
//
// Per case: exit code, rows delivered (a refusal must deliver 0), whether the
// file was left untouched (a refusal that rewrites the cursor destroys the
// evidence), and whether stderr names file + content. Guards pin the
// false-positive side: the missing file is a first run, `0` in a file stays
// legal, the resume stays exactly-once, and the file write_cursor itself
// produces stays readable.
//
// Usage:
//   node tools/measure-cursor-state.mjs
//   node tools/measure-cursor-state.mjs --baseline <bin> --fixed <bin>
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
      : path.join(ROOT, 'research', 'cursor-state-2026-11-08.json'))
  ).replace(/^file:/, ''),
);

const PROGRAM = 'whirLbMiicVdio4qvUfM5KAg6Ct8VwpYzGff3uctyCc';
const ROWS = 20;

function argOf(flag) {
  const i = process.argv.indexOf(flag);
  return i >= 0 ? process.argv[i + 1] : null;
}

// --- the trees ------------------------------------------------------------

/** The published v0.7.3 tree, taken from the TAG and not from the checkout:
 * a baseline that is "the current tree with the fix removed" would prove
 * nothing about what a stranger installs. */
function baselineBinary() {
  const given = argOf('--baseline');
  if (given) return { bin: path.resolve(given), source: 'given' };
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'reeflog-cursor-baseline-'));
  const tag = process.env.BASELINE_TAG || 'v0.7.3';
  const tar = path.join(dir, 'baseline.tar');
  execFileSync('git', ['archive', '-o', tar, tag], { cwd: ROOT, stdio: 'inherit' });
  const src = path.join(dir, 'src');
  fs.mkdirSync(src);
  execFileSync('tar', ['-xf', tar, '-C', src], { stdio: 'inherit' });
  execFileSync('cargo', ['build', '--release', '--locked'], { cwd: src, stdio: 'inherit' });
  return { bin: path.join(src, 'target', 'release', 'reeflog'), source: `git archive ${tag}` };
}

function fixedBinary() {
  const given = argOf('--fixed');
  if (given) return { bin: path.resolve(given), source: 'given' };
  execFileSync('cargo', ['build', '--release', '--locked'], { cwd: ROOT, stdio: 'inherit' });
  return { bin: path.join(ROOT, 'target', 'release', 'reeflog'), source: 'this tree' };
}

// --- a journal with real rows ---------------------------------------------

function makeJournal(tag) {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), `reeflog-cursor-${tag}-`));
  const db = path.join(dir, 'reeflog.db');
  const init = spawnSync(binOf(tag) || '', ['init', '--db', db, '--program', PROGRAM], {
    encoding: 'utf8',
  });
  // (init runs per-tree in runCases; this helper is only used after bin is known)
  return { dir, db, init };
}

function seed(db) {
  const script = `
import sqlite3, sys, time
db = sys.argv[1]
c = sqlite3.connect(db)
now = int(time.time())
for i in range(${ROWS}):
    c.execute("insert or ignore into events (signature, slot, err, logs, source, first_seen, seal_status) values (?,?,?,?,?,?,?)",
              ("CUR%057d" % i, 44000000+i, None, "[]", "ws", now, "sealed"))
c.commit()
print(c.execute("select count(*) from events").fetchone()[0])
`;
  const out = execFileSync('python3', ['-c', script, db], { encoding: 'utf8' });
  if (Number(out.trim()) !== ROWS) throw new Error(`seeding failed: ${out}`);
}

function withJournal(bin, tag) {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), `reeflog-cursor-${tag}-`));
  const db = path.join(dir, 'reeflog.db');
  const init = spawnSync(bin, ['init', '--db', db, '--program', PROGRAM], { encoding: 'utf8' });
  if (init.status !== 0) throw new Error(`init failed: ${init.stderr}`);
  seed(db);
  return { dir, db };
}

// --- running one case -----------------------------------------------------

function runCase(bin, setup, args, { timeout = 30000 } = {}) {
  const r = spawnSync(bin, args, { cwd: setup.dir, encoding: 'utf8', timeout });
  const stdout = r.stdout || '';
  const stderr = r.stderr || '';
  const rows = stdout.split('\n').filter((l) => l.startsWith('{')).length;
  return {
    exit: r.signal ? `killed:${r.signal}` : r.status,
    rows,
    refused: r.status !== 0,
    stderr: stderr.trim(),
    stdoutBytes: Buffer.byteLength(stdout),
  };
}

// --- the cases ------------------------------------------------------------

/**
 * Every case: an EXISTING cursor file carrying the given content, and what
 * must happen. `must` is what the FIXED tree must do; `baselineExpect` is
 * what the defect looks like (recorded, not required — a baseline that has
 * already been fixed elsewhere would still be a fine world).
 */
function cases() {
  return [
    {
      id: 'C1-corrupt-content',
      kind: 'defect',
      cursor: 'abc\n',
      args: ['tail', '--cursor', 'c.txt'],
      must: 'refused',
      why: 'not a number; baseline replays every row at cursor 0 with exit 0',
    },
    {
      id: 'C2-beyond-i64',
      kind: 'defect',
      cursor: '9223372036854775808\n',
      args: ['tail', '--cursor', 'c.txt'],
      must: 'refused',
      why: 'outside the consumer i64 space; never reaches the flag parser',
    },
    {
      id: 'C3-negative',
      kind: 'defect',
      cursor: '-1\n',
      args: ['tail', '--cursor', 'c.txt'],
      must: 'refused',
      why: 'a position is never negative; baseline replays everything',
    },
    {
      id: 'C4-empty-file',
      kind: 'defect',
      cursor: '',
      args: ['tail', '--cursor', 'c.txt'],
      must: 'refused',
      why: 'what a truncate-then-crash leaves; baseline restarts at 0',
    },
    {
      id: 'C5-float',
      kind: 'defect',
      cursor: '15.0\n',
      args: ['tail', '--cursor', 'c.txt'],
      must: 'refused',
      why: 'a human number, not a seq position; guessing 15 is a guess',
    },
    {
      id: 'C6-missing-file-first-run',
      kind: 'guard',
      cursor: null, // no file
      args: ['tail', '--cursor', 'c.txt'],
      must: 'delivered-all',
      why: 'the documented first run must keep delivering every row',
    },
    {
      id: 'G1-zero-in-file',
      kind: 'guard',
      cursor: '0\n',
      args: ['tail', '--cursor', 'c.txt'],
      must: 'delivered-all',
      why: '`0` in a file is a deliberate rewind and stays legal',
    },
    {
      id: 'G2-valid-resume',
      kind: 'guard',
      cursor: '15\n',
      args: ['tail', '--cursor', 'c.txt'],
      must: 'delivered-5',
      why: 'resumes exactly: rows 16..20, no replay',
    },
    {
      id: 'G3-write-cursor-file-form',
      kind: 'guard',
      cursor: '4\n',
      args: ['tail', '--cursor', 'c.txt'],
      must: 'delivered-16',
      why: 'the exact form write_cursor produces must stay readable',
    },
    {
      id: 'G4-ahead-of-tip',
      kind: 'guard',
      cursor: '5000\n',
      args: ['tail', '--cursor', 'c.txt'],
      must: 'delivered-0-ok',
      why: 'ahead of the tip is legal: 0 rows, exit 0, file untouched',
    },
    {
      id: 'G5-reorg-channel',
      kind: 'guard',
      cursor: '0\n',
      args: ['reorg', 'tail', '--cursor', 'r.txt'],
      must: 'delivered-0-ok',
      why: 'the retraction channel reads cursors the same way',
    },
  ];
}

function verdictFor(c, res, cursorBefore) {
  const m = c.must;
  const ok =
    m === 'refused'
      ? res.refused && res.rows === 0
      : m === 'delivered-all'
        ? !res.refused && res.rows === ROWS
        : m === 'delivered-0-ok'
          ? !res.refused && res.rows === 0
          : m.startsWith('delivered-')
            ? !res.refused && res.rows === Number(m.slice('delivered-'.length))
            : false;
  // A refusal must name file + content and must not touch the file.
  const details = {};
  if (m === 'refused' && ok) {
    const name = path.basename(c.args[c.args.indexOf('--cursor') + 1]);
    details.namesFile = res.stderr.includes(name);
    const trimmed = (c.cursor || '').trim();
    details.namesContent = trimmed ? res.stderr.includes(trimmed) : null;
    details.fileUntouched = cursorBefore === null ? null : res.fileAfter === cursorBefore;
    if (!details.namesFile) return { ok: false, reason: 'refusal does not name the file' };
    if (details.namesContent === false) return { ok: false, reason: 'refusal does not name the content' };
    if (details.fileUntouched === false) return { ok: false, reason: 'refusal rewrote the cursor file' };
  }
  if (m !== 'refused' && cursorBefore !== null && res.fileAfter !== cursorBefore && c.id !== 'G2-valid-resume' && c.id !== 'G3-write-cursor-file-form') {
    // Only cases that consume rows may rewrite the file.
    if (res.rows === 0) return { ok: false, reason: 'an unchanged cursor was rewritten' };
  }
  return { ok, reason: ok ? null : `expected ${m}, got exit=${res.exit} rows=${res.rows}` };
}

function main() {
  const base = baselineBinary();
  const fixed = fixedBinary();
  const artifact = {
    tool: 'measure-cursor-state',
    date: '2026-11-08',
    rows: ROWS,
    // Labels only: a committed measurement must not carry the path of the
    // machine that produced it (the lesson from sememe's artifact, which
    // went red in CI on the author's home directory).
    baseline: { source: base.source, bin: 'baseline-reeflog' },
    fixed: { source: fixed.source, bin: 'this-tree-reeflog' },
    cases: [],
  };
  let failures = 0;

  for (const c of cases()) {
    const entry = { id: c.id, kind: c.kind, must: c.must, why: c.why, cursor: c.cursor, trees: {} };
    for (const [name, tree] of [['baseline', base], ['fixed', fixed]]) {
      const setup = withJournal(tree.bin, `${name}-${c.id}`);
      const cursorPath = path.join(setup.dir, c.args[c.args.indexOf('--cursor') + 1]);
      if (c.cursor !== null) fs.writeFileSync(cursorPath, c.cursor);
      const args = [c.args[0]];
      if (c.args[0] === 'reorg') args.push('tail');
      args.push('--db', 'reeflog.db', '--cursor', path.basename(cursorPath));
      const before = fs.existsSync(cursorPath) ? fs.readFileSync(cursorPath, 'utf8') : null;
      const res = runCase(tree.bin, setup, args);
      res.fileAfter = fs.existsSync(cursorPath) ? fs.readFileSync(cursorPath, 'utf8') : null;
      const v = verdictFor(c, res, before);
      entry.trees[name] = { ...res, verdict: v };
      if (name === 'fixed' && !v.ok) failures++;
      fs.rmSync(setup.dir, { recursive: true, force: true });
    }
    const b = entry.trees.baseline;
    const f = entry.trees.fixed;
    entry.measured = `${c.kind === 'defect' ? 'baseline defect' : 'guard'}: baseline exit=${b.exit} rows=${b.rows} | fixed exit=${f.exit} rows=${f.rows}`;
    artifact.cases.push(entry);
    console.log(
      `${entry.trees.fixed.verdict.ok ? 'PASS' : 'FAIL'} ${c.id}: baseline exit=${b.exit} rows=${b.rows} | fixed exit=${f.exit} rows=${f.rows}${entry.trees.fixed.verdict.reason ? ' — ' + entry.trees.fixed.verdict.reason : ''}`,
    );
  }

  artifact.failures = failures;
  artifact.verdict = failures === 0 ? 'all cases behave as specified on the FIXED tree' : `${failures} case(s) failed`;
  fs.mkdirSync(path.dirname(OUT), { recursive: true });
  fs.writeFileSync(OUT, JSON.stringify(artifact, null, 2) + '\n');
  console.log(`artifact: ${OUT}`);
  console.log(artifact.verdict);
  process.exit(failures === 0 ? 0 : 1);
}

try {
  main();
} catch (e) {
  console.error(`setup failed: ${e.message}`);
  process.exit(2);
}
