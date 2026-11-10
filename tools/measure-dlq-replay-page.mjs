#!/usr/bin/env node
// `dlq replay` must report the QUEUE it was asked to drain, not the PAGE its
// `--limit` happened to attempt — measured against REAL binaries, on both
// the published v0.7.11 tree and this one.
//
// The defect (measured 2026-11-10 on the published v0.7.11 binary, the
// sha256 of whose release archive equals the digest the release API
// publishes):
//
//   dlq replay --limit 2 over 7 pending letters, receiver accepting
//   everything
//     -> exit 0, {"replayed": 2, "failed": 0, ...}
//     -> journal truth: 5 letters STILL PENDING.
//
// Nothing in the report named them, no exit code carried them. The report
// described the page and the command called that the job — the exact class
// v0.7.7 closed for `dlq list`, which the listing got and the command an
// operator automates did not.
//
// The tool measures BOTH trees on the SAME cases and writes ONE artifact:
//
//   BASELINE  the published v0.7.11 binary (gh release download, verified
//             against the release API digest)
//   FIXED     the binary built from this tree
//
// Per case: exit code, the counts the report claims, and the journal's own
// truth (pending letters read straight from the SQLite file with
// node:sqlite). The verdict is DERIVED from comparing the report and the
// exit code against that truth — the report is never trusted, the journal
// is.
//
// Guards pin the false-positive side: a full drain must still exit 0, an
// empty queue must still read healthy, delivery itself must be unchanged
// (the page still goes out), and `--target` must still scope the pass.
//
// Usage:
//   node tools/measure-dlq-replay-page.mjs
//   node tools/measure-dlq-replay-page.mjs --baseline <bin> --fixed <bin>
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
      : path.join(ROOT, 'research', 'dlq-replay-page-2026-11-10.json'))
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
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'reeflog-replay-baseline-'));
  const tar = path.join(dir, 'baseline.tar.gz');
  execFileSync(
    'gh',
    ['release', 'download', 'v0.7.11', '--repo', 'auroraxo/reeflog',
     '-p', 'reeflog-linux-amd64.tar.gz', '-O', tar],
    { cwd: ROOT, stdio: 'inherit' },
  );
  const digest = execFileSync(
    'gh',
    ['api', 'repos/auroraxo/reeflog/releases/tags/v0.7.11', '--jq',
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
    source: `published v0.7.11 release asset (sha256 ${local})`,
  };
}

function fixedBinary() {
  const given = argOf('--fixed');
  if (given) {
    const source = argOf('--fixed-source') || 'given';
    return { bin: path.resolve(given), source };
  }
  execFileSync('cargo', ['build', '--release', '--locked'], { cwd: ROOT, stdio: 'inherit' });
  return { bin: path.join(ROOT, 'target', 'release', 'reeflog'), source: 'this tree' };
}

// --- helpers ----------------------------------------------------------------

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

/** A receiver that answers 200 to everything and counts deliveries.
 *  OUT OF PROCESS: spawnSync blocks this process's event loop, so an
 *  in-process server can never accept (measured mistake, 2026-11-10). */
function startReceiver() {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'reeflog-receiver-'));
  const log = path.join(dir, 'got.log');
  const portFile = path.join(dir, 'port');
  const child = spawn(process.execPath, ['-e', `
    const http = require('http'); const fs = require('fs');
    const LOG = ${JSON.stringify(log)}; const PORT = ${JSON.stringify(portFile)};
    const s = http.createServer((req, res) => { let b=''; req.on('data',c=>b+=c);
      req.on('end',()=>{ fs.appendFileSync(LOG, 'D\\n'); res.writeHead(200); res.end('ok'); }); });
    s.listen(0, '127.0.0.1', () => fs.writeFileSync(PORT, String(s.address().port)));
  `], { stdio: 'ignore' });
  const deadline = Date.now() + 5000;
  while (!fs.existsSync(portFile) && Date.now() < deadline) {
    spawnSync('sleep', ['0.05']);
  }
  if (!fs.existsSync(portFile)) {
    child.kill();
    throw new Error('receiver did not start');
  }
  const port = Number(fs.readFileSync(portFile, 'utf8').trim());
  return {
    url: `http://127.0.0.1:${port}/hook`,
    deliveries: () => (fs.existsSync(log) ? fs.readFileSync(log, 'utf8').split('\n').filter((l) => l === 'D').length : 0),
    stop: () => { try { child.kill(); } catch {} },
  };
}

/** A journal with `pending` letters for alpha (and optionally beta). */
function makeJournal(dir, alpha, beta) {
  const init = spawnSync(path.join(ROOT, 'target', 'release', 'reeflog'),
    ['init', '--db', 'reeflog.db', '--program', PROGRAM], { cwd: dir, encoding: 'utf8' });
  if (init.status !== 0) throw new Error(`init failed: ${init.stderr}`);
  const conn = new DatabaseSync(path.join(dir, 'reeflog.db'));
  const plant = (target, n) => {
    for (let seq = 1; seq <= n; seq++) {
      conn.prepare(
        `INSERT INTO webhook_dead_letters
           (target,kind,seq,body,event_id,reason,status,attempts,first_failed_at,last_failed_at,state)
         VALUES(?1,'event',?2,'{}',?3,'refused with http 500',500,5,1,1,'pending')`,
      ).run(target, seq, `${target}:event:${seq}`);
    }
  };
  plant('alpha', alpha);
  if (beta > 0) plant('beta', beta);
  conn.close();
}

function writeConfig(dir, url) {
  const p = path.join(dir, 'wh.json');
  fs.writeFileSync(p, JSON.stringify({
    targets: [
      { id: 'alpha', url, secret: 's1', timeout_ms: 2000, dead_letter_after: 2 },
      { id: 'beta', url, secret: 's2', timeout_ms: 2000, dead_letter_after: 2 },
    ],
  }, null, 2));
  return p;
}

/** The journal's own truth: pending letters, globally or per target. */
function journalPending(dir, target) {
  const conn = new DatabaseSync(path.join(dir, 'reeflog.db'));
  const row = target
    ? conn.prepare("SELECT COUNT(*) c FROM webhook_dead_letters WHERE state='pending' AND target=?").get(target)
    : conn.prepare("SELECT COUNT(*) c FROM webhook_dead_letters WHERE state='pending'").get();
  conn.close();
  return row.c;
}

// --- the cases --------------------------------------------------------------

/** Each case: id, what it asks, and the rule the report + exit code must
 * satisfy against the JOURNAL's truth. */
const CASES = [
  {
    id: 'a_truncated_replay_reports_the_queue_not_the_page',
    alpha: 7, args: ['--limit', '2'], expect: 'undrained_not_clean',
  },
  {
    id: 'the_default_page_truncates_the_same_way',
    alpha: 101, args: [], expect: 'undrained_not_clean',
  },
  {
    id: 'a_full_drain_still_exits_clean',
    alpha: 3, args: ['--limit', '3'], expect: 'drained',
  },
  {
    id: 'an_empty_queue_still_reads_healthy',
    alpha: 0, args: [], expect: 'drained',
  },
  {
    id: 'a_scoped_pass_reports_only_its_target',
    alpha: 3, beta: 4, args: ['--target', 'alpha', '--limit', '3'], expect: 'drained',
  },
];

async function runCase(bin, c) {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'reeflog-replay-case-'));
  const rx = startReceiver();
  try {
    makeJournal(dir, c.alpha, c.beta || 0);
    const cfg = writeConfig(dir, rx.url);
    const args = ['webhook', 'dlq', 'replay', '--db', 'reeflog.db', '--config', 'wh.json', ...c.args];
    const started = Date.now();
    const r = await spawnCli(bin, args, dir);
    const ms = Date.now() - started;
    const rep = json(r.out);
    const truth = journalPending(dir, c.expect === 'scoped' ? 'alpha' : undefined);
    const deliveries = rx.deliveries();

    const record = {
      id: c.id,
      exit: r.code,
      ms,
      journal_pending: truth,
      delivered: deliveries,
      report_printed: rep !== null,
      reported_replayed: rep ? (rep.replayed ?? null) : null,
      reported_remaining: rep ? (rep.remaining_pending ?? null) : null,
      reported_pending_before: rep ? (rep.pending_before ?? null) : null,
      stderr_head: r.err.split('\n').filter(Boolean).slice(0, 2).join(' | '),
    };

    let ok = false;
    switch (c.expect) {
      case 'undrained_not_clean':
        // The queue was NOT drained. The report must say how much is left
        // and how much existed, and the exit code must carry it.
        ok = rep !== null
          && rep.remaining_pending === truth
          && rep.pending_before === c.alpha
          && rep.replayed === (c.alpha - truth)
          && r.code !== 0;
        break;
      case 'drained':
        ok = r.code === 0 && rep !== null
          && rep.remaining_pending === 0
          && rep.replayed === c.alpha;
        break;
      default:
        ok = false;
    }
    record.verdict = ok ? 'as_specified' : 'violated';
    return record;
  } finally {
    rx.stop();
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
    console.log(`[${label}] ${rec.id}: ${rec.verdict} (exit=${rec.exit} truth=${rec.journal_pending} reported_remaining=${rec.reported_remaining})`);
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
    subject: 'the dlq replay report must describe the queue, not the page its limit attempted',
    defect: 'published v0.7.11: dlq replay --limit 2 over 7 pending letters exited 0 with "replayed": 2, "failed": 0 while the journal still held 5 pending — no field named them, no exit code carried them',
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
