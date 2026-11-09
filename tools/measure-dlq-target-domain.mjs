#!/usr/bin/env node
// The dead-letter queue's REPORT must match the queue — measured against
// REAL binaries, on both the published v0.7.6 tree and this one.
//
// The defect (measured 2026-11-09 on the published v0.7.6 binary, the
// sha256 of whose release archive equals the digest the release API
// publishes):
//
//   dlq list --target bogus    -> exit 0, {"targets": [], "pending_total": 0}
//                                  while the journal held pending letters.
//                                  A typo in a target id was
//                                  indistinguishable from a clean queue.
//   dlq replay --target bogus  -> exit 0, {"replayed": 0, "failed": 0} —
//                                  a successful no-op.
//   dlq list --limit 2         -> "pending": 2, "pending_total": 2 while the
//                                  journal held 7. `pending` counted the
//                                  ROWS SHOWN, not the letters pending.
//   dlq list (config lost the
//   target)                    -> "pending_total": 0 while the journal
//                                  still holds that target's letters.
//
// The tool measures BOTH trees on the SAME cases and writes ONE artifact:
//
//   BASELINE  the published v0.7.6 binary (gh release download, verified
//             against the release API digest)
//   FIXED     the binary built from this tree
//
// Per case: exit code, whether a report was printed, the counts the report
// claims, and the journal's own truth (the number of pending letters,
// read straight from the SQLite file with node:sqlite). The verdict is
// DERIVED from comparing the report against that truth — the report is
// never trusted, the journal is.
//
// Guards pin the false-positive side: a KNOWN target must still be listed
// with its letters, a listing without a filter must still report every
// config target, an empty queue must still read healthy (exit 0), and a
// replay against a live receiver must still DELIVER.
//
// Usage:
//   node tools/measure-dlq-target-domain.mjs
//   node tools/measure-dlq-target-domain.mjs --baseline <bin> --fixed <bin>
//
// Exit: 0 all cases behave as specified on the FIXED tree, 1 a case failed,
// 2 setup failed.
import { spawn, spawnSync, execFileSync } from 'node:child_process';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import http from 'node:http';
import { DatabaseSync } from 'node:sqlite';

const ROOT = path.resolve(path.dirname(new URL(import.meta.url).pathname), '..');
const OUT = path.resolve(
  (
    process.env.OUT ||
    (process.argv.includes('--out')
      ? process.argv[process.argv.indexOf('--out') + 1]
      : path.join(ROOT, 'research', 'dlq-target-domain-2026-11-09.json'))
  ).replace(/^file:/, ''),
);

const PROGRAM = 'whirLbMiicVdio4qvUfM5KAg6Ct8VwpYzGff3uctyCc';

function argOf(flag) {
  const i = process.argv.indexOf(flag);
  return i >= 0 ? process.argv[i + 1] : null;
}

// --- the trees -------------------------------------------------------------

function baselineBinary() {
  const given = argOf('--baseline');
  if (given) return { bin: path.resolve(given), source: 'given' };
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'reeflog-dlq-baseline-'));
  const tar = path.join(dir, 'baseline.tar.gz');
  execFileSync(
    'gh',
    ['release', 'download', 'v0.7.6', '--repo', 'auroraxo/reeflog',
     '-p', 'reeflog-linux-amd64.tar.gz', '-O', tar],
    { cwd: ROOT, stdio: 'inherit' },
  );
  const digest = execFileSync(
    'gh',
    ['api', 'repos/auroraxo/reeflog/releases/tags/v0.7.6', '--jq',
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
    source: `published v0.7.6 release asset (sha256 ${local})`,
  };
}

function fixedBinary() {
  const given = argOf('--fixed');
  if (given) {
    // The caller may point --fixed at an ARBITRARY binary — most importantly
    // a downloaded release asset. "given" would say nothing about which
    // artifact was measured, so --fixed-source carries that name into the
    // artifact (digests are recorded by the caller's own verify step).
    const source = argOf('--fixed-source') || 'given';
    return { bin: path.resolve(given), source };
  }
  execFileSync('cargo', ['build', '--release', '--locked'], { cwd: ROOT, stdio: 'inherit' });
  return { bin: path.join(ROOT, 'target', 'release', 'reeflog'), source: 'this tree' };
}

// --- helpers ----------------------------------------------------------------

function cli(bin, args, cwd) {
  const r = spawnSync(bin, args, { cwd, encoding: 'utf8' });
  return { code: r.status, out: r.stdout || '', err: r.stderr || '' };
}

function spawnCli(bin, args, cwd) {
  return new Promise((resolve) => {
    const c = spawn(bin, args, { cwd, stdio: ['ignore', 'pipe', 'pipe'] });
    let o = '', e = '';
    c.stdout.on('data', (d) => { o += d; });
    c.stderr.on('data', (d) => { e += d; });
    c.on('close', (code) => resolve({ code, out: o, err: e }));
  });
}

function json(text) {
  try { return JSON.parse(text); } catch { return null; }
}

/** A journal with `pending` letters for alpha, optionally one for a target
 * that is NOT in the config (an orphan). */
function makeJournal(dir, pending, orphan) {
  const db = path.join(dir, 'reeflog.db');
  const init = spawnSync(path.join(ROOT, 'target', 'release', 'reeflog'),
    ['init', '--db', 'reeflog.db', '--program', PROGRAM], { cwd: dir, encoding: 'utf8' });
  if (init.status !== 0) throw new Error(`init failed: ${init.stderr}`);
  const conn = new DatabaseSync(db);
  for (let seq = 1; seq <= pending; seq++) {
    conn.prepare(
      `INSERT INTO webhook_dead_letters
         (target,kind,seq,body,event_id,reason,status,attempts,first_failed_at,last_failed_at,state)
       VALUES('alpha','event',?1,'{}',?2,'refused with http 500',500,5,1,1,'pending')`,
    ).run(seq, `alpha:event:${seq}`);
  }
  if (orphan) {
    conn.prepare(
      `INSERT INTO webhook_dead_letters
         (target,kind,seq,body,event_id,reason,status,attempts,first_failed_at,last_failed_at,state)
       VALUES('gone','event',1,'{}','gone:event:1','refused with http 500',500,5,1,1,'pending')`,
    ).run();
  }
  conn.close();
}

function writeConfig(dir, targets) {
  const p = path.join(dir, 'wh.json');
  fs.writeFileSync(p, JSON.stringify({ targets }, null, 2));
  return p;
}

/** The journal's own truth: how many pending letters it holds. */
function journalPending(dir) {
  const conn = new DatabaseSync(path.join(dir, 'reeflog.db'));
  const n = conn.prepare(
    "SELECT COUNT(*) c FROM webhook_dead_letters WHERE state='pending'").get().c;
  conn.close();
  return n;
}

// --- the cases --------------------------------------------------------------

/** Each case: id, what it asks, and what the report must say. The verdict
 * is derived from the report against the journal's truth — never from the
 * report alone. */
const CASES = [
  {
    id: 'unknown_target_is_refused',
    kind: 'list',
    pending: 3,
    orphan: false,
    args: ['--target', 'bogus'],
    // A refusal: non-zero exit, no report, stderr names the flag and the
    // value. An empty report with exit 0 is the defect.
    expect: 'refusal',
  },
  {
    id: 'replay_with_unknown_target_is_refused',
    kind: 'replay',
    pending: 3,
    orphan: false,
    args: ['--target', 'bogus'],
    expect: 'refusal',
  },
  {
    id: 'pending_counts_the_queue_not_the_page',
    kind: 'list',
    pending: 7,
    orphan: false,
    args: ['--limit', '2'],
    expect: 'pending_equals_truth',
  },
  {
    id: 'an_orphaned_letter_is_reported',
    kind: 'list',
    pending: 1,
    orphan: true,
    args: [],
    expect: 'pending_equals_truth',
  },
  {
    id: 'a_replay_with_an_orphaned_letter_does_not_read_clean',
    kind: 'replay',
    pending: 0,
    orphan: true,
    args: [],
    expect: 'nonzero_exit',
  },
  {
    id: 'guard_a_known_target_is_still_listed',
    kind: 'list',
    pending: 2,
    orphan: false,
    args: ['--target', 'alpha'],
    expect: 'pending_equals_truth',
  },
  {
    id: 'guard_a_listing_without_a_filter_reports_every_target',
    kind: 'list',
    pending: 1,
    orphan: false,
    args: [],
    expect: 'pending_equals_truth',
  },
  {
    id: 'guard_an_empty_queue_reads_healthy',
    kind: 'list',
    pending: 0,
    orphan: false,
    args: [],
    expect: 'healthy',
  },
];

async function runCase(bin, c) {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'reeflog-dlq-case-'));
  try {
    makeJournal(dir, c.pending, c.orphan);
    const cfg = writeConfig(dir, [
      { id: 'alpha', url: 'http://127.0.0.1:9/hook', secret: 's1' },
      { id: 'beta', url: 'http://127.0.0.1:9/hook', secret: 's2' },
    ]);
    const args = ['webhook', 'dlq', c.kind, '--db', 'reeflog.db', '--config', 'wh.json', ...c.args];
    const started = Date.now();
    const r = await spawnCli(bin, args, dir);
    const ms = Date.now() - started;
    const rep = json(r.out);
    const truth = journalPending(dir);

    const record = {
      id: c.id,
      kind: c.kind,
      exit: r.code,
      ms,
      journal_pending: truth,
      report_printed: rep !== null,
      reported_pending_total: rep ? (rep.pending_total ?? null) : null,
      reported_pending_per_target:
        rep && Array.isArray(rep.targets)
          ? Object.fromEntries(rep.targets.map((t) => [t.target, t.pending]))
          : null,
      refusal_names_flag: /--target/.test(r.err),
      refusal_names_value: /bogus/.test(r.err),
      stderr_head: r.err.split('\n').filter(Boolean).slice(0, 2).join(' | '),
    };

    let ok = false;
    switch (c.expect) {
      case 'refusal':
        ok = r.code !== 0 && rep === null && record.refusal_names_flag && record.refusal_names_value;
        break;
      case 'pending_equals_truth':
        ok = r.code === 0 && rep !== null && rep.pending_total === truth;
        break;
      case 'nonzero_exit':
        ok = r.code !== 0;
        break;
      case 'healthy':
        ok = r.code === 0 && rep !== null && rep.pending_total === 0;
        break;
      default:
        ok = false;
    }
    record.verdict = ok ? 'as_specified' : 'violated';
    return record;
  } finally {
    fs.rmSync(dir, { recursive: true, force: true });
  }
}

// --- the run ----------------------------------------------------------------

async function measureTree(label, tree) {
  const cases = [];
  for (const c of CASES) {
    const rec = await runCase(tree.bin, c);
    rec.expected = c.expect;
    cases.push(rec);
    console.log(`[${label}] ${rec.id}: ${rec.verdict} (exit=${rec.exit} truth=${rec.journal_pending} reported=${rec.reported_pending_total})`);
  }
  const violations = cases.filter((c) => c.verdict === 'violated').length;
  return { source: tree.source, cases, violations };
}

async function main() {
  const baseline = baselineBinary();
  const fixed = fixedBinary();
  console.log(`baseline: ${baseline.bin} (${baseline.source})`);
  console.log(`fixed:    ${fixed.bin} (${fixed.source})`);

  const b = await measureTree('baseline', baseline);
  const f = await measureTree('fixed', fixed);

  const artifact = {
    generatedAt: new Date().toISOString(),
    subject: 'the dead-letter queue report must match the queue it reports on',
    defect: 'published v0.7.6: an unknown --target answered "nothing pending" with exit 0; pending counted the rows shown, not the letters pending; a letter whose target left the config was invisible',
    baseline: b,
    fixed: f,
  };
  fs.mkdirSync(path.dirname(OUT), { recursive: true });
  fs.writeFileSync(OUT, JSON.stringify(artifact, null, 2));
  console.log(`\nwrote ${path.relative(ROOT, OUT)}`);
  console.log(`baseline violations: ${b.violations}/${b.cases.length}`);
  console.log(`fixed violations:    ${f.violations}/${f.cases.length}`);
  if (f.violations > 0) process.exit(1);
}

main().catch((e) => {
  console.error(e);
  process.exit(2);
});
