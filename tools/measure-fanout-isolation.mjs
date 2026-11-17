#!/usr/bin/env node
// MEASUREMENT: ONE misconfigured target must not silence the WHOLE fan-out.
//
// The webhook config is a LIST of consumers. Each target carries its own
// delivery state, its own receipts and its own cursor — the journal holds
// one `webhook_state` row per target id. A consumer whose id was REPOINTED
// to a new endpoint is refused (the v0.7.15 endpoint-binding rule, measured
// 2026-11-11): that refusal is about ONE consumer.
//
// But the refusal is raised at the CONFIG level: `load_targets` succeeds,
// and the first `bind_endpoint` failure aborts the whole command (`die`).
// The consequence, measured on this tree (v0.7.33, 06694b0):
//
//   * `webhook push`   -> exit 1, NO report at all, and every OTHER target
//                         in the config was never delivered to: 2 events
//                         seeded after the control push never reach the
//                         healthy consumer while one sibling is repointed.
//   * `webhook status` -> exit 1, NO report at all. The surface a
//                         monitoring caller polls now says nothing — not
//                         even the healthy targets' own numbers.
//   * `webhook dlq replay` -> exit 1, no replay for ANY target.
//
// The refusal is right; the BLAST RADIUS is wrong. A misconfigured target
// is a fact about one consumer, and the operator needs to hear it ABOUT
// that consumer — while the rest of the fan-out keeps running.
//
// Faces (each a predicate run against BOTH trees):
//   F1 `webhook push` reports the healthy target, and the healthy consumer
//      actually RECEIVED the events that were pending when the sibling was
//      repointed (the blast radius is the defect, measured at the sink).
//   F2 `webhook push` NAMES the misconfigured target and the reason, in
//      the report and on stderr.
//   F3 `webhook status` still reports the healthy target's own numbers.
//   F4 `webhook status` names the misconfigured target (report or stderr).
//   F5 the misconfigured target is REFUSED: not one byte reaches the NEW
//      endpoint, and the recorded endpoint keeps its own state row (the
//      v0.7.15 rule must survive the isolation).
//   F6 the pass holds the exit code: a refused target is a debt, so push
//      and status must not exit 0 while one consumer cannot be served.
//
// Guards (green on BOTH trees):
//   G1 a config where EVERY target is healthy delivers to all of them and
//      exits 0 (the isolation must not break the normal case).
//   G2 a config with only the healthy target reports its own numbers and
//      exits 0.
//
//   node tools/measure-fanout-isolation.mjs
//   BIN=/path/to/reeflog OUT=/tmp/x.json LABEL=v0.7.33 node tools/measure-fanout-isolation.mjs
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { execFileSync, spawn, spawnSync } from 'node:child_process';
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

const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'reeflog-fanout-'));
const db = path.join(dir, 'j.db');

execFileSync(BIN, ['init', '--db', db, '--program', PROGRAM], { cwd: dir, stdio: 'ignore' });

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

// Three local sinks, so a body that reaches the WRONG endpoint is a fact,
// not an inference: `healthy` (the untouched consumer), `movedOld` (the
// endpoint the moved consumer USED to point at) and `movedNew` (the url the
// repoint points it at — it must receive nothing, ever).
function receiver(tag) {
  const seenFile = path.join(dir, `seen-${tag}.json`);
  const portFile = seenFile + '.port';
  const child = spawn(
    process.execPath,
    [
      '-e',
      `
const http = require('node:http');
const fs = require('node:fs');
const out = process.argv[1];
const seen = [];
const srv = http.createServer((req, res) => {
  let body = '';
  req.on('data', (c) => { body += c; });
  req.on('end', () => {
    seen.push({ eventId: req.headers['x-reeflog-event-id'] ?? null, seq: req.headers['x-reeflog-seq'] ?? null, len: body.length });
    fs.writeFileSync(out, JSON.stringify(seen, null, 2));
    res.statusCode = 200; res.end('ok');
  });
});
srv.listen(0, () => fs.writeFileSync(out + '.port', String(srv.address().port)));
`,
      seenFile,
    ],
    { stdio: 'ignore' }
  );
  return {
    child,
    async port() {
      for (let i = 0; i < 400 && !fs.existsSync(portFile); i++) await sleep(25);
      return fs.readFileSync(portFile, 'utf8').trim();
    },
    seen() {
      try {
        return JSON.parse(fs.readFileSync(seenFile, 'utf8'));
      } catch {
        return [];
      }
    },
  };
}

function seedEvents(n) {
  const conn = new DatabaseSync(db);
  const ins = conn.prepare(
    `INSERT INTO events(signature, slot, err, logs, source, first_seen, payload_state)
     VALUES (?, ?, NULL, ?, 'ws', ?, 'held')`
  );
  for (let i = 1; i <= n; i++) {
    // A per-batch prefix keeps `events.signature` UNIQUE across calls.
    const nonce = `${Date.now()}${Math.floor(Math.random() * 1e6)}`;
    ins.run(`sig${nonce}_${i}${'x'.repeat(40)}`, 100 + i, JSON.stringify(['["probe"]']), Date.now());
  }
  conn.close();
}

const healthy = receiver('healthy');
const healthyPort = await healthy.port();
const movedOld = receiver('moved-old');
const movedOldPort = await movedOld.port();
const movedNew = receiver('moved-new');
const movedNewPort = await movedNew.port();

const healthyUrl = `http://127.0.0.1:${healthyPort}/hook`;
const movedOldUrl = `http://127.0.0.1:${movedOldPort}/hook`;
const movedNewUrl = `http://127.0.0.1:${movedNewPort}/hook`;

function writeConfig(name, targets) {
  const p = path.join(dir, name);
  fs.writeFileSync(p, JSON.stringify({ targets }, null, 2));
  return p;
}

// One secret per target identity (since v0.7.16 one shared secret on two
// different endpoints is refused).
const healthyTarget = { id: 'healthy', url: healthyUrl, secret: 's-healthy', timeout_ms: 2000, dead_letter_after: 0 };
const movedTarget = { id: 'moved', url: movedOldUrl, secret: 's-moved', timeout_ms: 2000, dead_letter_after: 0 };

const cfgBoth = writeConfig('cfg-both.json', [healthyTarget, movedTarget]);
// The repointed target comes FIRST: the abort happens at the first
// failing target in config order, so this ordering is the WORST case for
// the healthy consumer behind it (measured pre-fix: the healthy sink
// starves as well when it sits after the broken one).
const cfgRepointed = writeConfig('cfg-repointed.json', [
  { ...movedTarget, url: movedNewUrl },
  healthyTarget,
]);
const cfgOnlyHealthy = writeConfig('cfg-only-healthy.json', [healthyTarget]);

function run(args) {
  const res = spawnSync(BIN, args, { cwd: dir, encoding: 'utf8' });
  let json = null;
  try {
    json = JSON.parse(res.stdout);
  } catch {
    json = null;
  }
  return { code: res.status, stdout: res.stdout, stderr: (res.stderr || '').trim(), json };
}

// CONTROL (G1): every target healthy. Both sinks receive the 3 events.
seedEvents(3);
const controlPush = run(['webhook', 'push', '--db', db, '--config', cfgBoth]);
// The control's OWN sink counts, snapshotted HERE: the repointed pass below
// delivers two more events to the healthy sink, so a count read afterwards
// would measure the control PLUS the repointed pass (measured 2026-11-17: the
// G1 face reported 5/3 and looked like a defect of the fix).
const controlHealthyBodies = healthy.seen().length;
const controlMovedOldBodies = movedOld.seen().length;
// G2 (guard) measured on the CAUGHT-UP queue, before the repointed pass can
// starve anything: a config with only the healthy target is clean.
const statusOnlyHealthy = run(['webhook', 'status', '--db', db, '--config', cfgOnlyHealthy]);

// Two MORE events become pending while nobody looks. Then `moved` is
// repointed. The healthy consumer must still receive them — the sibling's
// misconfiguration is not a fact about this queue.
seedEvents(2);
const push = run(['webhook', 'push', '--db', db, '--config', cfgRepointed]);
const status = run(['webhook', 'status', '--db', db, '--config', cfgRepointed]);

const faces = [];
function face(name, pass, detail) {
  faces.push({ name, pass: !!pass, detail });
}

const controlTargets = controlPush.json?.targets ?? [];
// Recorded BEFORE the repointed pass runs: the sink counts below are the
// control's own, not the control plus whatever the repointed pass did.
face(
  'G1_a_healthy_config_delivers_to_every_target',
  controlPush.code === 0 &&
    controlTargets.length === 2 &&
    controlTargets.every((t) => t.delivered === 3) &&
    controlHealthyBodies === 3 &&
    controlMovedOldBodies === 3,
  `control push exit ${controlPush.code}; targets ${JSON.stringify(controlTargets.map((t) => [t.target, t.delivered]))}; sinks healthy=${controlHealthyBodies}/3 movedOld=${controlMovedOldBodies}/3 (snapshotted at the control pass)`
);

const pushTargets = push.json?.targets ?? [];
const healthyRow = pushTargets.find((t) => t.target === 'healthy');
face(
  'F1_a_repointed_target_does_not_silence_the_healthy_one',
  !!healthyRow &&
    typeof healthyRow.delivered === 'number' &&
    healthyRow.delivered === 2 &&
    healthyRow.next_seq === 5 &&
    healthy.seen().length === 5 &&
    push.code !== 1,
  `push exit ${push.code}; targets ${JSON.stringify(pushTargets.map((t) => [t.target, t.delivered, t.next_seq]))}; the healthy sink got ${healthy.seen().length}/5 (the 2 pending events were the test)`
);

const movedRow = pushTargets.find((t) => t.target === 'moved');
const movedMentioned =
  pushTargets.some((t) => t.target === 'moved') || /(^|[^a-z])moved([^a-z]|$)/i.test(push.stderr);
const reasonNamed = /bound to endpoint|own target id|is not bound|endpoint/i.test(
  `${push.stderr}\n${JSON.stringify(movedRow ?? {})}`
);
face(
  'F2_the_misconfigured_target_is_named_with_its_reason',
  movedMentioned && reasonNamed,
  `push stderr=${JSON.stringify(push.stderr)}; moved row=${JSON.stringify(movedRow ?? null)}`
);

const statusTargets = status.json?.targets ?? [];
const healthyStatus = statusTargets.find((t) => t.target === 'healthy');
face(
  'F3_status_still_reports_the_healthy_target',
  !!healthyStatus &&
    healthyStatus.next_seq === 5 &&
    typeof healthyStatus.pending_events === 'number' &&
    status.code !== 1,
  `status exit ${status.code}; targets ${JSON.stringify(statusTargets.map((t) => [t.target, t.next_seq, t.delivered]))}`
);

const statusMentioned =
  statusTargets.some((t) => t.target === 'moved') || /moved/i.test(status.stderr);
face('F4_status_names_the_misconfigured_target', statusMentioned, `status stderr=${JSON.stringify(status.stderr)}; targets ${JSON.stringify(statusTargets.map((t) => t.target))}`);

{
  const conn = new DatabaseSync(db);
  const row = conn
    .prepare('SELECT endpoint, next_seq FROM webhook_state WHERE target=?1')
    .get('moved');
  conn.close();
  face(
    'F5_the_misconfigured_target_is_refused_and_its_state_is_untouched',
    movedNew.seen().length === 0 && !!row && row.endpoint === movedOldUrl,
    `the NEW endpoint received ${movedNew.seen().length} bodies (must be 0); the recorded state row is ${JSON.stringify(row ?? null)}`
  );
}

face(
  'F6_the_refused_target_holds_the_exit_code',
  push.code !== 0 && status.code !== 0 && push.code !== 1 && status.code !== 1,
  `push exit ${push.code}, status exit ${status.code} (a refused target is a debt: 3 = reported-and-held, 1 = abort-without-report, 0 = silent success)`
);

const onlyRow = statusOnlyHealthy.json?.targets?.[0];
face(
  'G2_a_single_healthy_target_reports_its_own_numbers',
  statusOnlyHealthy.code === 0 &&
    !!onlyRow &&
    onlyRow.target === 'healthy' &&
    onlyRow.next_seq === 3 &&
    onlyRow.pending_events === 0,
  `status exit ${statusOnlyHealthy.code}; row=${JSON.stringify(onlyRow ?? null)}`
);

healthy.child.kill();
movedOld.child.kill();
movedNew.child.kill();

const binarySha256 = createHash('sha256').update(fs.readFileSync(BIN)).digest('hex');
const binaryVersion = execFileSync(BIN, ['--version'], { cwd: dir }).toString().trim();
// When measuring a RELEASE seat, the archive digest the Release API
// publishes is the seat's own claim — recorded next to the binary's so the
// artifact can be checked against the release without re-deriving it.
const ARCHIVE_SHA256 = process.env.ARCHIVE_SHA256 || null;

const artifact = {
  tool: 'measure-fanout-isolation',
  label: LABEL,
  binary: path.basename(BIN),
  binarySha256,
  ...(ARCHIVE_SHA256 ? { archiveSha256: ARCHIVE_SHA256 } : {}),
  binaryVersion,
  program: PROGRAM,
  faces,
  context: {
    seeds: '3 events (control) + 2 events (pending when the sibling was repointed)',
    urls: { healthy: healthyUrl, moved_old: movedOldUrl, moved_new: movedNewUrl },
    control_push_exit: controlPush.code,
    control_sink_bodies: { healthy: controlHealthyBodies, moved_old: controlMovedOldBodies },
    control_push_targets: controlTargets.map((t) => ({ target: t.target, delivered: t.delivered, next_seq: t.next_seq })),
    repointed_push_exit: push.code,
    repointed_push_stderr: push.stderr,
    repointed_push_report_present: push.json !== null,
    repointed_push_targets: pushTargets.map((t) => ({
      target: t.target,
      delivered: t.delivered,
      next_seq: t.next_seq,
      blocked: t.blocked,
      error: t.error ?? null,
    })),
    repointed_status_exit: status.code,
    repointed_status_stderr: status.stderr,
    repointed_status_report_present: status.json !== null,
    repointed_status_targets: statusTargets.map((t) => ({
      target: t.target,
      next_seq: t.next_seq,
      delivered: t.delivered,
      pending_events: t.pending_events,
    })),
    sink_counts: {
      healthy: healthy.seen().length,
      moved_old: movedOld.seen().length,
      moved_new: movedNew.seen().length,
    },
    single_healthy_status_exit: statusOnlyHealthy.code,
  },
  verdict: faces.every((f) => f.pass) ? 'fanout-isolation-held' : 'fanout-isolation-blind',
};
if (OUT) {
  fs.writeFileSync(OUT, JSON.stringify(artifact, null, 2));
}
for (const f of faces) {
  console.log(`${f.pass ? 'PASS' : 'FAIL'} ${f.name} — ${f.detail}`);
}
console.log(`\nverdict: ${artifact.verdict} (${faces.filter((f) => f.pass).length}/${faces.length} faces pass)`);
process.exit(faces.every((f) => f.pass) ? 0 : 1);
