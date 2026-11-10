#!/usr/bin/env node
// CURSOR PROVENANCE, measured against REAL binaries.
//
// The defect (measured 2026-11-10 on the published v0.7.12 binary and this
// tree's pre-fix state): a cursor file was a bare whole number with NO record
// of who wrote it. reeflog runs two channels per journal — `tail` (events)
// and `reorg tail` (retractions) — each with its own cursor file. Nothing
// stopped an operator from pointing one channel's cursor at the other, or
// copying a cursor between journals. Every crossing was accepted with exit 0
// and delivered the WRONG thing:
//
//   tail writes c.txt at 20;  reorg tail --cursor c.txt
//     -> exit 0, "0 reorg(s)" — while the journal holds 3 undelivered reorgs.
//        Silent starvation: they are never re-fetched, they just do not exist.
//   reorg tail writes c.txt at 3;  tail --cursor c.txt
//     -> exit 0, ALL events replayed — already-delivered rows, twice.
//   cursor copied to a different journal (backups, clones, staging):
//     -> exit 0, "0 events after cursor 20" — journal B's 20 events are never
//        delivered to this consumer. Silent gap.
//
// The rule the fix implements: a cursor reeflog WROTE carries provenance
// ({"v":1,"channel":…,"journal":…,"value":…}) and is honoured only by the
// same channel of the same journal; a crossing is refused — nothing
// delivered, exit != 0, file untouched, the refusal names both sides and the
// repair. A BARE hand-written number stays legal: it is the documented
// deliberate rewind/resume and carries no provenance claim.
//
// The tool measures both trees on the same cases and writes ONE artifact:
//
//   BASELINE  the published v0.7.12 release binary (gh release download,
//             digest-verified) — what a stranger actually runs today
//   FIXED     the binary built from this tree
//
// Defect cases (D*) must be REFUSED on the fixed tree (nonzero exit, zero
// rows delivered, cursor file byte-identical, refusal names file + both
// sides). Guard cases (G*) pin the false-positive side: first run, bare
// number resume, same-channel exact resume, per-channel files, invalid
// content still refused, and the numeric summary line.
//
// Usage:
//   node tools/measure-cursor-provenance.mjs
//   node tools/measure-cursor-provenance.mjs --baseline <bin> --fixed <bin>
//   BASELINE_TAG=v0.7.12 OUT=/tmp/x.json node tools/measure-cursor-provenance.mjs
//
// Exit: 0 all cases behave as specified on the FIXED tree, 1 a case failed,
// 2 setup failed.
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { execFileSync, spawnSync } from 'node:child_process';
import { DatabaseSync } from 'node:sqlite';
import { fileURLToPath } from 'node:url';

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const ROOT = path.resolve(__dirname, '..');
const OUT = path.resolve(
  process.env.OUT ||
    (process.argv.includes('--out')
      ? process.argv[process.argv.indexOf('--out') + 1]
      : path.join(ROOT, 'research', 'cursor-provenance-2026-11-10.json')),
);

const PROGRAM = 'whirLbMiicVdio4qvUfM5KAg6Ct8VwpYzGff3uctyCc';
const EVENTS = 20;
const REORGS = 3;

function argOf(flag) {
  const i = process.argv.indexOf(flag);
  return i >= 0 ? process.argv[i + 1] : null;
}

// --- the trees -------------------------------------------------------------

function baselineBinary() {
  const given = argOf('--baseline');
  if (given) return { bin: path.resolve(given), source: 'given' };
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'reeflog-prov-baseline-'));
  const tag = process.env.BASELINE_TAG || 'v0.7.12';
  const tar = path.join(dir, 'baseline.tar.gz');
  execFileSync(
    'gh',
    ['release', 'download', tag, '--repo', 'auroraxo/reeflog',
     '-p', 'reeflog-linux-amd64.tar.gz', '-O', tar],
    { cwd: ROOT, stdio: 'inherit' },
  );
  const digest = execFileSync(
    'gh',
    ['api', `repos/auroraxo/reeflog/releases/tags/${tag}`, '--jq',
     '.assets[] | select(.name=="reeflog-linux-amd64.tar.gz") | .digest'],
    { cwd: ROOT, encoding: 'utf8' },
  ).trim().replace(/^sha256:/, '');
  const local = execFileSync('sha256sum', [tar], { encoding: 'utf8' }).split(/\s+/)[0];
  if (digest && local !== digest) {
    throw new Error(`the downloaded archive does not match the release digest (${local} != ${digest})`);
  }
  execFileSync('tar', ['xzf', tar, '-C', dir], { stdio: 'inherit' });
  return {
    bin: path.join(dir, 'reeflog'),
    source: `published ${tag} release asset (sha256 ${local})`,
  };
}

function fixedBinary() {
  const given = argOf('--fixed');
  if (given) return { bin: path.resolve(given), source: 'given' };
  execFileSync('cargo', ['build', '--release', '--locked'], { cwd: ROOT, stdio: 'inherit' });
  return { bin: path.join(ROOT, 'target', 'release', 'reeflog'), source: 'this tree' };
}

// --- journals with real rows (events AND retractions) -----------------------

function mkJournal(bin) {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'reeflog-prov-'));
  const db = path.join(dir, 'reeflog.db');
  const init = spawnSync(bin, ['init', '--db', db, '--program', PROGRAM], { encoding: 'utf8' });
  if (init.status !== 0) throw new Error(`init failed: ${init.stderr}`);
  const c = new DatabaseSync(db);
  const now = Date.now();
  for (let i = 0; i < EVENTS; i++) {
    c.prepare(
      `INSERT INTO events(signature,slot,err,logs,source,first_seen,payload_state)
       VALUES(?,?,NULL,?,'ws',?,'held')`,
    ).run(`Sig${String(i).padStart(44, '0')}`, 440000000 + i, JSON.stringify(['log']), now);
  }
  for (let i = 0; i < REORGS; i++) {
    c.prepare(
      'INSERT INTO reorgs(signature,slot,detected_at,status_source,tx_source,detail) VALUES(?,?,?,?,?,?)',
    ).run(`Reorg${i}`, 440000000 + i, now, 'rpc', 'archive', 'planted');
  }
  c.close();
  return { dir, db };
}

function run(bin, dir, args) {
  const r = spawnSync(bin, args, { cwd: dir, encoding: 'utf8', timeout: 30000 });
  const stdout = r.stdout || '';
  return {
    exit: r.signal ? `killed:${r.signal}` : r.status,
    rows: stdout.split('\n').filter((l) => l.startsWith('{')).length,
    refused: r.status !== 0,
    stderr: (r.stderr || '').trim(),
    stdout,
  };
}

// A cursor is "the same file, byte-identical" or not — provenance refusals
// must never rewrite it.
function readCursor(dir, name) {
  const p = path.join(dir, name);
  return fs.existsSync(p) ? fs.readFileSync(p, 'utf8') : null;
}

// --- the cases ---------------------------------------------------------------
// Each case is a function (bin) => { steps: [...], verdict(res) }.
// `res` is the LAST step's result; the verdict checks behaviour plus the
// file states recorded on the step objects.

function cases() {
  return [
    {
      id: 'D1-events-cursor-on-reorgs-channel',
      kind: 'defect',
      why: 'tail cursor handed to `reorg tail`: baseline exits 0 with "0 reorg(s)" while 3 reorgs sit undelivered — silent starvation',
      plan(bin) {
        const { dir, db } = mkJournal(bin);
        const first = run(bin, dir, ['tail', '--db', db, '--cursor', 'c.txt']);
        const written = readCursor(dir, 'c.txt');
        const crossing = run(bin, dir, ['reorg', 'tail', '--db', db, '--cursor', 'c.txt']);
        return { dir, steps: [first, crossing], written, final: crossing, cursorAfter: readCursor(dir, 'c.txt') };
      },
    },
    {
      id: 'D2-reorgs-cursor-on-events-channel',
      kind: 'defect',
      why: 'reorg cursor handed to `tail`: baseline exits 0 and REPLAYS already-delivered events — duplicate delivery sold as a resume',
      plan(bin) {
        const { dir, db } = mkJournal(bin);
        const first = run(bin, dir, ['reorg', 'tail', '--db', db, '--cursor', 'c.txt']);
        const written = readCursor(dir, 'c.txt');
        const crossing = run(bin, dir, ['tail', '--db', db, '--cursor', 'c.txt']);
        return { dir, steps: [first, crossing], written, final: crossing, cursorAfter: readCursor(dir, 'c.txt') };
      },
    },
    {
      id: 'D3-cursor-copied-to-another-journal',
      kind: 'defect',
      why: 'cursor file copied to a different journal (backup/clone/staging): baseline exits 0 with "0 events after cursor 20" — journal B never delivers a single row',
      plan(bin) {
        const a = mkJournal(bin);
        run(bin, a.dir, ['tail', '--db', a.db, '--cursor', 'c.txt']);
        const written = readCursor(a.dir, 'c.txt');
        const b = mkJournal(bin);
        fs.copyFileSync(path.join(a.dir, 'c.txt'), path.join(b.dir, 'c.txt'));
        const crossing = run(bin, b.dir, ['tail', '--db', b.db, '--cursor', 'c.txt']);
        return { dir: a.dir, dir2: b.dir, steps: [crossing], written, final: crossing, cursorAfter: readCursor(b.dir, 'c.txt') };
      },
    },
    {
      id: 'D4-crossing-refusal-leaves-file-untouched',
      kind: 'defect',
      why: 'the refusal may not "help" by resetting the cursor — the crossing file must stay byte-identical so the operator can still see what it was',
      plan(bin) {
        const { dir, db } = mkJournal(bin);
        run(bin, dir, ['tail', '--db', db, '--cursor', 'c.txt']);
        const written = readCursor(dir, 'c.txt');
        const crossing = run(bin, dir, ['reorg', 'tail', '--db', db, '--cursor', 'c.txt']);
        const after = readCursor(dir, 'c.txt');
        const res = { ...crossing };
        if (crossing.refused && after !== written) res.resetTheCursor = true;
        return { dir, steps: [crossing], written, final: res, cursorAfter: after };
      },
    },
    {
      id: 'G1-first-run-no-file-delivers-all',
      kind: 'guard',
      why: 'no cursor file is still the documented first run: every event delivered, exit 0',
      plan(bin) {
        const { dir, db } = mkJournal(bin);
        const s = run(bin, dir, ['tail', '--db', db, '--cursor', 'c.txt']);
        return { dir, steps: [s], final: s, expectRows: EVENTS };
      },
    },
    {
      id: 'G2-bare-hand-written-number-stays-legal',
      kind: 'guard',
      why: 'a bare `15` written by a human is a deliberate resume position — it must keep working on BOTH channels of the journal it names by argument',
      plan(bin) {
        const { dir, db } = mkJournal(bin);
        fs.writeFileSync(path.join(dir, 'c.txt'), '15\n');
        const s = run(bin, dir, ['tail', '--db', db, '--cursor', 'c.txt']);
        return { dir, steps: [s], final: s, expectRows: EVENTS - 15 };
      },
    },
    {
      id: 'G3-same-channel-resume-exactly',
      kind: 'guard',
      why: 'the happy path must not regress: first run delivers 20, second run on the SAME file delivers 0 and the cursor value stays at the tip',
      plan(bin) {
        const { dir, db } = mkJournal(bin);
        const first = run(bin, dir, ['tail', '--db', db, '--cursor', 'c.txt']);
        const second = run(bin, dir, ['tail', '--db', db, '--cursor', 'c.txt']);
        return { dir, steps: [first, second], final: second, expectRows: 0, requireFirst: first };
      },
    },
    {
      id: 'G4-two-channel-files-both-work',
      kind: 'guard',
      why: 'the documented layout — separate files per channel — must keep working: events cursor in e.txt, reorgs cursor in r.txt, each honoured',
      plan(bin) {
        const { dir, db } = mkJournal(bin);
        run(bin, dir, ['tail', '--db', db, '--cursor', 'e.txt']);
        const s = run(bin, dir, ['reorg', 'tail', '--db', db, '--cursor', 'r.txt']);
        return { dir, steps: [s], final: s, expectRows: REORGS };
      },
    },
    {
      id: 'G5-invalid-content-still-refused',
      kind: 'guard',
      why: 'the v0.7.4 content gate survives the new format: `abc` in a cursor file is refused, not treated as provenance',
      plan(bin) {
        const { dir, db } = mkJournal(bin);
        fs.writeFileSync(path.join(dir, 'c.txt'), 'abc\n');
        const s = run(bin, dir, ['tail', '--db', db, '--cursor', 'c.txt']);
        return { dir, steps: [s], final: s, mustRefuse: true };
      },
    },
    {
      id: 'G6-summary-line-names-a-plain-number',
      kind: 'guard',
      why: 'operators grep the summary — "cursor now N" must still carry the plain numeric position after a delivering run',
      plan(bin) {
        const { dir, db } = mkJournal(bin);
        const s = run(bin, dir, ['tail', '--db', db, '--cursor', 'c.txt']);
        return { dir, steps: [s], final: s, needSummary: new RegExp(`cursor now ${EVENTS}(\\b|$| )`) };
      },
    },
  ];
}

function verdictFor(c, planned) {
  const res = planned.final;
  if (c.kind === 'defect') {
    if (!res.refused) return { ok: false, reason: `crossing accepted: exit=${res.exit} rows=${res.rows}` };
    if (res.rows !== 0) return { ok: false, reason: `a refusal delivered ${res.rows} row(s)` };
    if (res.resetTheCursor) return { ok: false, reason: 'the refusal rewrote the cursor file' };
    if (planned.cursorAfter !== planned.written) {
      return { ok: false, reason: 'the cursor file was not left byte-identical' };
    }
    if (!res.stderr.includes('c.txt')) return { ok: false, reason: 'the refusal does not name the file' };
    // The refusal must state BOTH sides ("written by X … this command reads
    // Y" / "names journal X … this journal is Y") and the repair.
    if (!res.stderr.includes('does not belong here')) {
      return { ok: false, reason: 'the refusal does not say the cursor belongs elsewhere' };
    }
    if (!res.stderr.includes('another channel or journal')) {
      return { ok: false, reason: 'the refusal does not name the channel/journal rule or the repair' };
    }
    return { ok: true, reason: null };
  }
  // guards
  if (planned.mustRefuse || c.mustRefuse) {
    return res.refused ? { ok: true, reason: null } : { ok: false, reason: `invalid cursor accepted: exit=${res.exit}` };
  }
  if (res.refused) return { ok: false, reason: `guard refused: exit=${res.exit} stderr=${res.stderr.slice(0, 120)}` };
  if (c.expectRows !== undefined && res.rows !== c.expectRows) {
    return { ok: false, reason: `expected ${c.expectRows} rows, got ${res.rows}` };
  }
  if (c.requireFirst && c.requireFirst.rows !== EVENTS) {
    return { ok: false, reason: `first run delivered ${c.requireFirst.rows}, not ${EVENTS}` };
  }
  if (c.needSummary && !c.needSummary.test(res.stderr)) {
    return { ok: false, reason: 'summary line does not name the numeric position' };
  }
  return { ok: true, reason: null };
}

// Cursor files may carry a journal id (16 hex) — keep it, never a path.
function shortenCursor(s) {
  if (s === null || s === undefined) return null;
  const one = s.replace(/\s+/g, ' ').trim();
  return one.length > 160 ? `${one.slice(0, 160)}…` : one;
}

function main() {
  const base = baselineBinary();
  const fixed = fixedBinary();
  const artifact = {
    tool: 'measure-cursor-provenance',
    date: '2026-11-10',
    events: EVENTS,
    reorgs: REORGS,
    // Labels only: a committed measurement must not carry the path of the
    // machine that produced it.
    baseline: { source: base.source, bin: 'baseline-reeflog' },
    fixed: { source: fixed.source, bin: 'this-tree-reeflog' },
    defect:
      'published v0.7.12: a cursor file was a bare number with no channel and no journal; handing it to the other channel or another journal exited 0 and delivered the wrong thing — silent starvation (0 rows, undelivered backlog), duplicate replay, or a permanent gap',
    rule:
      'a cursor reeflog wrote is honoured only by the same channel of the same journal; a crossing is refused with nothing delivered and the file untouched; a bare hand-written number stays legal',
    cases: [],
  };
  let failures = 0;

  for (const c of cases()) {
    const entry = { id: c.id, kind: c.kind, why: c.why, trees: {} };
    for (const [name, tree] of [['baseline', base], ['fixed', fixed]]) {
      let planned;
      try {
        planned = c.plan(tree.bin);
      } catch (e) {
        entry.trees[name] = { setup_error: String(e.message).slice(0, 200) };
        if (name === 'fixed') failures++;
        continue;
      }
      const v = verdictFor(c, planned);
      entry.trees[name] = {
        exit: planned.final.exit,
        rows: planned.final.rows,
        refused: planned.final.refused,
        stderr: planned.final.stderr.slice(0, 240),
        cursorWrittenByFirstStep: shortenCursor(planned.written),
        cursorAfterFinalStep: shortenCursor(planned.cursorAfter),
        verdict: v,
      };
      if (name === 'fixed' && !v.ok) failures++;
      for (const d of [planned.dir, planned.dir2]) {
        if (d) fs.rmSync(d, { recursive: true, force: true });
      }
    }
    const b = entry.trees.baseline;
    const f = entry.trees.fixed;
    entry.measured =
      `${c.kind === 'defect' ? 'defect' : 'guard'}: baseline exit=${b.exit ?? '?'} rows=${b.rows ?? '?'} refused=${b.refused ?? '?'} | fixed exit=${f.exit ?? '?'} rows=${f.rows ?? '?'} refused=${f.refused ?? '?'}`;
    artifact.cases.push(entry);
    const okMark = f.verdict?.ok ? 'PASS' : 'FAIL';
    console.log(`${okMark} ${c.id}: ${entry.measured}${f.verdict && !f.verdict.ok ? ' — ' + f.verdict.reason : ''}`);
  }

  artifact.failures = failures;
  artifact.verdict =
    failures === 0
      ? 'all cases behave as specified on the FIXED tree (4 crossings refused, 6 guards green)'
      : `${failures} case(s) failed on the FIXED tree`;
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
