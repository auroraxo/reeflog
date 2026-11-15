#!/usr/bin/env node
// MEASUREMENT: `webhook status` must report the DEAD-LETTER debt of targets
// that LEFT the configuration.
//
// The journal's pending dead letters belong to a TARGET, not to a config
// entry. When a target is removed from the config, its quarantined events
// stay in the journal — and they can never be replayed from here, because
// the replay path needs the target's endpoint. The repo already knows this
// on two surfaces:
//
//   * `webhook push` reports `orphaned_dead_letters` (the JOURNAL's total
//     minus the config's share, v0.7.6);
//   * `webhook dlq list` lists the orphan with `in_config: false`.
//
// `webhook status` — the report a monitoring caller polls — reported
// NEITHER. It walks the CONFIG's targets and prints a `dead_letters` count
// for each; a target that left the config is not in the config, so its
// pending letters were not printed anywhere. Measured on the PUBLISHED
// v0.7.28 binary (2026-11-15, verdict `status-orphan-dead-letter-blind`):
// the report said `dead_letters: 0` for the healthy target and exited 0,
// while the same journal read through `push` said `orphaned_dead_letters: 1`.
//
// Faces (each a predicate run against BOTH trees):
//   R1 status reports the orphan count for a pending letter whose target
//      left the config.
//   R2 status names WHICH target holds the stranded letters (in JSON and in
//      the stderr line the operator reads).
//   R3 status is not blind when the ONLY debt is an orphan — the face that
//      separates "reports the count" from "reports a number that is always
//      zero", and the face that separates "reports it" from "reports it but
//      still exits 0".
//
//   node tools/measure-status-orphan-dead-letters.mjs
//   BIN=/path/to/reeflog OUT=/tmp/x.json LABEL=v0.7.28 node tools/measure-status-orphan-dead-letters.mjs
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

const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'reeflog-status-orphan-'));
const db = path.join(dir, 'j.db');
const cfgPath = path.join(dir, 'wh.json');

fs.writeFileSync(
  cfgPath,
  JSON.stringify({ targets: [{ id: 't1', url: 'http://127.0.0.1:9/never', secret: 's3cret', timeout_ms: 2000, dead_letter_after: 0 }] })
);

execFileSync(BIN, ['init', '--db', db, '--program', PROGRAM], { cwd: dir, stdio: 'ignore' });

// A pending letter for a target that LEFT the config. The target's own
// webhook_state row is planted too: a departed target still has a cursor in
// the journal, and the report must not be able to confuse "left the config"
// with "never existed".
const conn = new DatabaseSync(db);
conn
  .prepare(
    `INSERT INTO webhook_state(target,endpoint,next_seq,next_reorg_id,last_error,updated_at)
     VALUES('gone','http://127.0.0.1:9/old',1,1,NULL,1700000000000)`
  )
  .run();
const ins = conn.prepare(
  `INSERT INTO webhook_dead_letters(target,kind,seq,body,event_id,reason,status,attempts,first_failed_at,last_failed_at,state)
   VALUES(?1,'event',?2,'{}',?3,'http 500',500,3,1700000000000,1700000000000,'pending')`
);
ins.run('gone', 0, 'EVA');
ins.run('gone', 1, 'EVB');
conn.close();

function status() {
  const r = spawnSync(BIN, ['webhook', 'status', '--db', db, '--config', cfgPath], { cwd: dir, encoding: 'utf8' });
  let json = null;
  try { json = JSON.parse((r.stdout || '').trim()); } catch { json = null; }
  return { exitCode: r.status, stdout: r.stdout || '', stderr: r.stderr || '', json };
}

function push() {
  const r = spawnSync(BIN, ['webhook', 'push', '--db', db, '--config', cfgPath], { cwd: dir, encoding: 'utf8' });
  try { return JSON.parse((r.stdout || '').trim()); } catch { return null; }
}

const s = status();
const p = push();

const reportedOrphans = s.json && typeof s.json.orphaned_dead_letters === 'number' ? s.json.orphaned_dead_letters : null;
const reportedTargets = Array.isArray(s.json?.orphaned_dead_letter_targets)
  ? s.json.orphaned_dead_letter_targets.map((e) => e.target).filter(Boolean)
  : [];
const pushOrphans = p && typeof p.orphaned_dead_letters === 'number' ? p.orphaned_dead_letters : null;

const faces = [
  {
    face: 'R1',
    claim: 'the status report names the orphaned dead-letter count',
    expected: 2,
    observed: reportedOrphans,
    ok: reportedOrphans === 2,
    note: 'two pending letters for a target that left the config',
  },
  {
    face: 'R2',
    claim: 'the status report names WHICH target holds the stranded letters, in JSON and in stderr',
    expected: ['gone'],
    observed: reportedTargets,
    stderrMentionsTarget: /gone/.test(s.stderr),
    stderrMentionsKind: /dead[- ]letter/i.test(s.stderr),
    ok: reportedTargets.includes('gone') && /gone/.test(s.stderr) && /dead[- ]letter/i.test(s.stderr),
  },
  {
    face: 'R3',
    claim: 'the status report is not silent about an orphan-only debt (a non-zero count and a non-zero exit)',
    expected: { count: 2, exitCode: 3 },
    observed: { count: reportedOrphans, exitCode: s.exitCode },
    ok: reportedOrphans === 2 && s.exitCode === 3,
  },
];

// The surfaces must agree: the same journal truth, one number per surface.
const parity = {
  statusOrphans: reportedOrphans,
  pushOrphans,
  agree: reportedOrphans !== null && pushOrphans !== null && reportedOrphans === pushOrphans,
};

const verdict = faces.every((f) => f.ok) && parity.agree
  ? 'status-orphan-dead-letter-reported'
  : 'status-orphan-dead-letter-blind';

const artifact = {
  measuredAt: new Date().toISOString(),
  label: LABEL,
  // The binary is NAMED BY ITS BYTES, never by its host path: an artifact
  // that carries /tmp/... is not reproducible on another machine and leaks
  // the measuring host's layout.
  binary: {
    sha256: createHash('sha256').update(fs.readFileSync(BIN)).digest('hex'),
    version: spawnSync(BIN, ['--version'], { encoding: 'utf8' }).stdout?.trim() ?? null,
  },
  program: PROGRAM,
  fixture: {
    description: 'one configured target (t1) plus two pending dead letters for a target (gone) that left the config',
    configTargets: ['t1'],
    orphanTargets: ['gone'],
    pendingDeadLetters: 2,
  },
  faces,
  parity,
  statusExitCode: s.exitCode,
  statusStderr: s.stderr,
  verdict,
};

if (OUT) {
  fs.writeFileSync(OUT, JSON.stringify(artifact, null, 2) + '\n');
  console.error(`wrote ${OUT}`);
}

// No host paths in the artifact.
const serialized = JSON.stringify(artifact);
if (serialized.includes(dir) || serialized.includes(BIN) || /"\/(tmp|home)\//.test(serialized)) {
  console.error('ARTIFACT LEAK: a host path is in the artifact');
  process.exit(1);
}

console.log(JSON.stringify({ verdict, faces: faces.map((f) => ({ face: f.face, ok: f.ok })), parity }, null, 2));
process.exit(faces.every((f) => f.ok) && parity.agree ? 0 : 1);
