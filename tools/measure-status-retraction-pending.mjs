#!/usr/bin/env node
// MEASUREMENT: `webhook status` must report the RETRACTION queue as its own
// truth, not only the event queue.
//
// The reorg (retraction) channel is a separate cursor (`next_reorg_id`) and
// the repo's own doctrine is that "a retraction is the one message whose
// lateness can cost money" (src/webhook.rs). `webhook push` reports BOTH
// queues per target — `remaining_behind_cursor` and
// `remaining_reorgs_behind_cursor` (v0.7.26, "a bounded pass reports
// remaining_behind_cursor and truncated so a page can never stand in for the
// queue"). `webhook status` reports only ONE of them: `pending_events` (the
// event queue). It prints `next_reorg_id` — a bare cursor value — but no
// count of the retractions still behind it.
//
// So a monitoring caller polling `webhook status` sees a target whose event
// queue is drained (`pending_events: 0`) and concludes the consumer is caught
// up, while the events the chain UNMADE have never reached it. That is the
// same defect class this repo has closed three times (AD registry
// visibility, AG stale error, AJ gap ledger): a report that says a target is
// whole while it is not.
//
// Faces (each a predicate run against BOTH trees):
//   R1 status reports the pending-retraction count for a target whose
//      `next_reorg_id` is behind the journal's newest reorg.
//   R2 status reports 0 pending retractions when the reorg queue is drained
//      (false-positive guard).
//   R3 status is not blind when ONLY retractions are behind (the event queue
//      is drained) — the face that separates "reports the count" from
//      "reports a number that is always zero".
//
//   node tools/measure-status-retraction-pending.mjs
//   BIN=/path/to/reeflog OUT=/tmp/x.json LABEL=v0.7.27 node tools/measure-status-retraction-pending.mjs
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { execFileSync, spawnSync } from 'node:child_process';
import { createHash } from 'node:crypto';
import { DatabaseSync } from 'node:sqlite';

const BIN = process.env.BIN || path.resolve(import.meta.dirname, '..', 'target/release/reeflog');
const OUT = process.env.OUT || null;
const LABEL = process.env.LABEL || 'this-tree';
const PROGRAM = 'whirLbMiicVdio4qvUfM5KAg6Ct8VwpYzGff3uctyCc';

if (!fs.existsSync(BIN)) {
  console.error(`no reeflog binary at ${BIN} — refusing to measure without one`);
  process.exit(2);
}

const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'reeflog-status-retr-'));
const db = path.join(dir, 'j.db');
const cfgPath = path.join(dir, 'wh.json');
const cfgUrl = 'http://127.0.0.1:9/never-called';

fs.writeFileSync(
  cfgPath,
  JSON.stringify({ targets: [{ id: 't1', url: cfgUrl, secret: 's3cret', timeout_ms: 2000, dead_letter_after: 0 }] })
);

execFileSync(BIN, ['init', '--db', db, '--program', PROGRAM], { cwd: dir, stdio: 'ignore' });

function seed() {
  const conn = new DatabaseSync(db);
  const insEvent = conn.prepare(
    `INSERT INTO events(signature, slot, err, logs, source, first_seen, payload_state, account_state)
     VALUES (?, ?, NULL, ?, 'ws', ?, 'held', 'held')`
  );
  insEvent.run('sigA', 100, JSON.stringify(['["Program invoke"]']), Date.now());
  insEvent.run('sigB', 101, JSON.stringify(['["Program invoke"]']), Date.now());
  const insReorg = conn.prepare(
    `INSERT INTO reorgs(signature, slot, detected_at, status_source, tx_source, detail)
     VALUES (?, ?, ?, 'known', 'refused', 'measured')`
  );
  insReorg.run('sigA', 100, Date.now());
  insReorg.run('sigB', 101, Date.now());
  conn.close();
}

// A target state row: the two cursor lines are the truth `webhook status`
// reports on. (0,0) leaves both queues behind; (2,2) leaves neither.
function setTarget(nextSeq, nextReorg) {
  const conn = new DatabaseSync(db);
  conn
    .prepare(
      `INSERT INTO webhook_state(target, endpoint, next_seq, next_reorg_id, last_error, updated_at)
       VALUES ('t1', ?, ?, ?, NULL, ?)
       ON CONFLICT(target) DO UPDATE SET next_seq=excluded.next_seq,
                                         next_reorg_id=excluded.next_reorg_id,
                                         updated_at=excluded.updated_at`
    )
    // The endpoint is written on the FIRST set only: the config points at
    // one url for the whole run, and `bind_endpoint` refuses a repointed id
    // (the v0.7.15 rule). Later faces move the CURSORS, never the url.
    .run(cfgUrl, nextSeq, nextReorg, Date.now());
  conn.close();
}

function statusOf() {
  const out = execFileSync(BIN, ['webhook', 'status', '--db', db, '--config', cfgPath], { cwd: dir }).toString();
  return JSON.parse(out).targets[0];
}

function countOf(table) {
  const conn = new DatabaseSync(db);
  const n = conn.prepare(`SELECT COUNT(*) AS n FROM ${table}`).get().n;
  conn.close();
  return n;
}

// `push` against an endpoint that never answers: the pass is blocked and
// exits 3, but it still PRINTS the report — which is the contrast this
// measurement is about.
function pushReport() {
  const res = spawnSync(BIN, ['webhook', 'push', '--db', db, '--config', cfgPath], { cwd: dir, encoding: 'utf8' });
  let report = null;
  try {
    report = JSON.parse(res.stdout);
  } catch {
    report = null;
  }
  return { code: res.status, report, stderr: (res.stderr || '').trim() };
}

const faces = [];
function face(name, pass, detail) {
  faces.push({ name, pass: !!pass, detail });
}

seed();
const events = countOf('events');
const reorgs = countOf('reorgs');

// The CONTRAST first: `webhook push` on the same journal state. It is the
// surface that DOES name the retraction queue, and it is also the surface
// that MOVES the cursors — so it runs before the status faces, each of
// which sets both cursors from scratch.
setTarget(0, 0);
const push = pushReport();

// R3: the event queue is drained, the retraction queue is NOT.
// BOTH cursors are set from scratch for every face: the report must read
// the journal's own state, and a face that inherits the previous face's
// cursor would be measuring the cursor, not the report.
setTarget(events, 0);
const onlyReorgsBehind = statusOf();
const truthBehind = reorgs - (onlyReorgsBehind.next_reorg_id ?? 0);
face(
  'R3_retraction_queue_behind_is_visible',
  onlyReorgsBehind.pending_events === 0 &&
    typeof onlyReorgsBehind.pending_retractions === 'number' &&
    onlyReorgsBehind.pending_retractions === truthBehind,
  `event queue drained (pending_events=${onlyReorgsBehind.pending_events}, journal events=${events}); retraction queue behind by ${truthBehind} (reorgs=${reorgs}, next_reorg_id=${onlyReorgsBehind.next_reorg_id}); status fields=${Object.keys(onlyReorgsBehind).join(',')}`
);

// R1: both queues behind — the report must name both.
setTarget(0, 0);
const bothBehind = statusOf();
face(
  'R1_both_queues_reported',
  typeof bothBehind.pending_events === 'number' &&
    typeof bothBehind.pending_retractions === 'number' &&
    bothBehind.pending_events === events &&
    bothBehind.pending_retractions === reorgs,
  `pending_events=${bothBehind.pending_events} pending_retractions=${bothBehind.pending_retractions} (journal events=${events}, reorgs=${reorgs})`
);

// R2: false-positive guard — both drained must report 0/0.
setTarget(events, reorgs);
const drained = statusOf();
face(
  'R2_drained_reports_zero',
  drained.pending_events === 0 && drained.pending_retractions === 0,
  `pending_events=${drained.pending_events} pending_retractions=${drained.pending_retractions} after both queues drained`
);

// The contrast: the same journal state seen by `webhook push`, which DOES
// name the retraction queue. (Run above, before the status faces moved the
// cursors.)

const binarySha256 = createHash('sha256').update(fs.readFileSync(BIN)).digest('hex');
const binaryVersion = execFileSync(BIN, ['--version'], { cwd: dir }).toString().trim();

const artifact = {
  tool: 'measure-status-retraction-pending',
  label: LABEL,
  // The binary is named by its BYTES, never by the path it happened to
  // live at: a host path in an artifact is a host path in every repo that
  // copies the file.
  binary: path.basename(BIN),
  binarySha256,
  binaryVersion,
  program: PROGRAM,
  faces,
  context: {
    events_total: events,
    reorgs_total: reorgs,
    push_exit_code: push.code,
    push_target_report: push.report ? push.report.targets[0] : null,
    push_truncated_targets: push.report ? push.report.truncated_targets : null,
    status_fields: Object.keys(drained),
  },
  verdict: faces.every((f) => f.pass)
    ? 'status-retraction-queue-reported'
    : 'status-retraction-queue-blind',
};
if (OUT) {
  fs.writeFileSync(OUT, JSON.stringify(artifact, null, 2));
}
for (const f of faces) {
  console.log(`${f.pass ? 'PASS' : 'FAIL'} ${f.name} — ${f.detail}`);
}
console.log(
  `\ncontext: push on the same state -> exit ${push.code}, target report ${JSON.stringify(push.report ? push.report.targets[0] : null)}`
);
console.log(`verdict: ${artifact.verdict} (${faces.filter((f) => f.pass).length}/${faces.length} faces pass)`);
process.exit(faces.every((f) => f.pass) ? 0 : 1);
