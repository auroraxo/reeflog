#!/usr/bin/env node
// MEASUREMENT: a blocked RETRACTION must be named as a retraction, and the
// retraction's reason must travel on its own field.
//
// The webhook fan-out has two independent queues: events (a `seq`) and
// retractions (a reorg id). A retraction is the one message whose lateness
// costs money, so a consumer that refuses it leaves that queue blocked —
// deliberately, never quarantined.
//
// Measured on this tree BEFORE the fix (2026-11-17): the report carried the
// facts (`blocked_reorg_id: 1`, `blocked: true`,
// `remaining_reorgs_behind_cursor: 3`) but the line the operator reads named
// a position that does not exist:
//
//   reeflog: blocked — t1 at seq None: refused with http 404
//
// The event cursor is `None` because the block is on the retraction
// channel. The operator looks in the wrong place, and the retraction — the
// message whose lateness costs money — is not named at all. The daemon
// printed nothing for a retraction block either.
//
// Faces (each a predicate run against ONE tree):
//   D1 the operator's line names the RETRACTION channel and the reorg id,
//      and never prints a non-existent event position.
//   D2 the operator's line carries no Rust debug formatting (`Some(1)`).
//   D3 the report carries the retraction's reason on `reorg_error`, and
//      the event channel's `error` stays null when only the retraction
//      queue is blocked.
//   D4 the report's channel facts are unchanged: `blocked_seq`,
//      `blocked_reorg_id`, `remaining_reorgs_behind_cursor`.
//
// Guards (must be green on BOTH trees):
//   G1 an accepted retraction is delivered, receipts, and advances the
//      cursor — the fix changes no delivery behaviour.
//   G2 a refused retraction is NEVER quarantined and never skips the
//      cursor: the pre-existing design rule ("a late retraction costs
//      money, so that queue stays blocked rather than being papered over").
//   G3 the queue facts the report already carried are still carried.
//
//   node tools/measure-retraction-block.mjs
//   BIN=/path/to/reeflog OUT=/tmp/x.json LABEL=published-v0.7.32 node tools/measure-retraction-block.mjs
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

function run(bin, args, cwd) {
  const r = spawnSync(bin, args, { cwd, encoding: 'utf8' });
  let json = null;
  try { json = JSON.parse((r.stdout || '').trim()); } catch { json = null; }
  return { exitCode: r.status, stdout: r.stdout || '', stderr: r.stderr || '', json };
}

/** A fresh journal in a fresh dir. */
function mkJournal(tag) {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), `reeflog-retr-block-${tag}-`));
  const db = path.join(dir, 'reeflog.db');
  execFileSync(BIN, ['init', '--db', db, '--program', PROGRAM], { cwd: dir, stdio: 'ignore' });
  return { dir, db };
}

function writeConfig(dir, url, deadLetterAfter) {
  const cfg = path.join(dir, 'wh.json');
  fs.writeFileSync(
    cfg,
    JSON.stringify({
      targets: [
        { id: 't1', url, secret: 's3cret', timeout_ms: 5000, dead_letter_after: deadLetterAfter },
      ],
    }),
  );
  return cfg;
}

/** One deliverable event and `n` recorded retractions. */
function plant(db, n) {
  const conn = new DatabaseSync(db);
  conn.exec(
    "INSERT INTO events(signature,slot,err,logs,source,first_seen,payload_state) " +
      "VALUES('sigEV1',101,NULL,'[\"log\"]','ws',1700000000000,'held')",
  );
  const ins = conn.prepare(
    'INSERT INTO reorgs(signature,slot,detected_at,status_source,tx_source,detail) ' +
      'VALUES(?,?,1700000000000,\'slot\',\'tx\',\'the chain unmade it\')',
  );
  for (let i = 1; i <= n; i++) ins.run(`sigRET${i}`, 200 + i);
  conn.close();
}

function journalFacts(db) {
  const conn = new DatabaseSync(db, { readOnly: true });
  const letters = conn
    .prepare('SELECT kind, seq, state FROM webhook_dead_letters ORDER BY id')
    .all();
  const nextReorg = conn
    .prepare("SELECT COALESCE((SELECT next_reorg_id FROM webhook_state WHERE target='t1'),0) AS n")
    .get();
  conn.close();
  return { deadLetters: letters, nextReorgId: nextReorg.n };
}

/**
 * A receiver, served from a CHILD PROCESS.
 *
 * The measurement drives the product with `spawnSync`, which blocks this
 * process's event loop — an in-process `http` server therefore can never
 * answer a request made by that very call, and the client times out against
 * a server that is alive but frozen. The receiver runs beside the
 * measurement instead of inside it (same shape as
 * `tools/measure-dlq-replay-page.mjs`), and reports deliveries over files.
 */
function startReceiver({ refuseRetractions }) {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'reeflog-retr-recv-'));
  const portFile = path.join(dir, 'port');
  const log = path.join(dir, 'bodies.log');
  // The receiver runs in a CHILD PROCESS: this script drives the product
  // with `spawnSync`, which blocks this process's event loop, so an
  // in-process server could never accept the connection that very call
  // makes. (Same measured mistake, same fix as
  // `tools/measure-dlq-replay-page.mjs`.)
  const child = spawn(process.execPath, ['-e', `
    const http = require('http'); const fs = require('fs');
    const refuse = ${refuseRetractions ? 'true' : 'false'};
    const LOG = ${JSON.stringify(log)}; const PORT = ${JSON.stringify(portFile)};
    const bodies = [];
    const server = http.createServer((req, res) => {
      let b = '';
      req.on('data', (d) => { b += d; });
      req.on('end', () => {
        bodies.push(b);
        const refused = refuse && b.includes('"type":"reorg"');
        res.writeHead(refused ? 404 : 200, {
          'content-type': 'application/json',
          'content-length': '2',
          connection: 'close',
        });
        res.end('{}');
        fs.writeFileSync(LOG, bodies.join('\\n---\\n'));
      });
    });
    server.listen(0, '127.0.0.1', () => fs.writeFileSync(PORT, String(server.address().port)));
  `], { stdio: 'ignore' });

  const deadline = Date.now() + 5000;
  while (!fs.existsSync(portFile) && Date.now() < deadline) spawnSync('sleep', ['0.05']);
  if (!fs.existsSync(portFile)) {
    try { child.kill(); } catch { /* best effort */ }
    throw new Error('receiver did not report a port');
  }
  const port = Number(fs.readFileSync(portFile, 'utf8').trim());
  return {
    url: `http://127.0.0.1:${port}/hook`,
    bodies: () => (fs.existsSync(log) ? fs.readFileSync(log, 'utf8').split('\n---\n').filter(Boolean) : []),
    close: () => {
      try { child.kill(); } catch { /* best effort */ }
      fs.rmSync(dir, { recursive: true, force: true });
    },
  };
}

/** A receiver that takes events and refuses retractions with http 404. */
function refusingServer() {
  return startReceiver({ refuseRetractions: true });
}

/** A receiver that accepts everything. */
function acceptingServer() {
  return startReceiver({ refuseRetractions: false });
}

const faces = [];
const guards = [];

function face(id, claim, expected, observed, ok, note) {
  faces.push({ id, claim, expected, observed, ok, note });
}
function guard(id, claim, expected, observed, ok, note) {
  guards.push({ id, claim, expected, observed, ok, note });
}

// ---------------------------------------------------------------- faces

// D1/D2/D3/D4 — one pass against a receiver that refuses retractions.
{
  const { dir, db } = mkJournal('faces');
  plant(db, 3);
  const srv = refusingServer();
  const cfg = writeConfig(dir, srv.url, 5);
  const r = run(BIN, ['webhook', 'push', '--db', db, '--config', cfg], dir);
  srv.close();

  const t = r.json?.targets?.[0] ?? {};
  const stderr = r.stderr || '';

  face(
    'D1',
    "the operator's line names the RETRACTION channel and the reorg id, and never prints a non-existent event position",
    { namesRetraction: true, namesReorgId: true, noPhantomPosition: true },
    {
      namesRetraction: /retraction/i.test(stderr),
      namesReorgId: /reorg 1/.test(stderr),
      noPhantomPosition: !stderr.includes('at seq None'),
      stderr: stderr.trim().split('\n').slice(-2).join(' | '),
    },
    /retraction/i.test(stderr) && /reorg 1/.test(stderr) && !stderr.includes('at seq None'),
    'the pre-fix tree prints `at seq None` — the event cursor of a retraction block',
  );

  face(
    'D2',
    "the operator's line carries no Rust debug formatting",
    { noDebugFormatting: true },
    {
      noDebugFormatting: !stderr.includes('Some(') && !stderr.includes('None'),
      stderr: stderr.trim().split('\n').slice(-2).join(' | '),
    },
    !stderr.includes('Some(') && !stderr.includes('None'),
    "the pre-fix tree prints `at seq Some(1)` for a blocked EVENT",
  );

  face(
    'D3',
    "the report carries the retraction's reason on its own field; the event channel's error stays null",
    { reorgErrorNamed: true, eventErrorNull: true },
    {
      reorgError: t.reorg_error ?? null,
      eventError: t.error ?? null,
      reorgErrorNamed: typeof t.reorg_error === 'string' && t.reorg_error.includes('404'),
      eventErrorNull: t.error === null || t.error === undefined,
    },
    typeof t.reorg_error === 'string' &&
      t.reorg_error.includes('404') &&
      (t.error === null || t.error === undefined),
    'the pre-fix tree has no `reorg_error`; the reason sat in the shared `blocked_error`',
  );

  face(
    'D4',
    'the report keeps naming the channel facts it already named',
    { blocked: true, blockedReorgId: 1, remainingReorgs: 3 },
    {
      blocked: t.blocked ?? null,
      blockedReorgId: t.blocked_reorg_id ?? null,
      remainingReorgs: t.remaining_reorgs_behind_cursor ?? null,
      nextReorgId: t.next_reorg_id ?? null,
    },
    t.blocked === true &&
      t.blocked_reorg_id === 1 &&
      t.remaining_reorgs_behind_cursor === 3 &&
      t.next_reorg_id === 0,
    'the report already carried these; the fix must not move a fact',
  );

  fs.rmSync(dir, { recursive: true, force: true });
}

// ---------------------------------------------------------------- guards

// G1 — an accepted retraction is delivered, receipts, advances the cursor.
{
  const { dir, db } = mkJournal('g1');
  plant(db, 2);
  const srv = acceptingServer();
  const cfg = writeConfig(dir, srv.url, 1);
  const r = run(BIN, ['webhook', 'push', '--db', db, '--config', cfg], dir);
  const retractionBodies = srv.bodies().filter((b) => b.includes('"type":"reorg"'));
  srv.close();
  const facts = journalFacts(db);
  const t = r.json?.targets?.[0] ?? {};

  guard(
    'G1',
    'an accepted retraction is delivered, receipts and advances the cursor — the fix changes no delivery behaviour',
    { reorgDelivered: 2, bodies: 2, nextReorgId: 2, deadLetters: 0, blocked: false },
    {
      reorgDelivered: t.reorg_delivered ?? null,
      bodies: retractionBodies.length,
      nextReorgId: facts.nextReorgId,
      deadLetters: facts.deadLetters.length,
      blocked: t.blocked ?? null,
    },
    t.reorg_delivered === 2 &&
      retractionBodies.length === 2 &&
      facts.nextReorgId === 2 &&
      facts.deadLetters.length === 0 &&
      t.blocked === false,
    'the fix touches the report and the lines, never the delivery',
  );

  fs.rmSync(dir, { recursive: true, force: true });
}

// G2 — a refused retraction is never quarantined and never skips the cursor.
{
  const { dir, db } = mkJournal('g2');
  plant(db, 3);
  const srv = refusingServer();
  // The setting under which an EVENT would be quarantined: a refused
  // retraction must still not be.
  const cfg = writeConfig(dir, srv.url, 1);
  for (let i = 0; i < 3; i++) run(BIN, ['webhook', 'push', '--db', db, '--config', cfg], dir);
  srv.close();
  const facts = journalFacts(db);

  guard(
    'G2',
    'a refused retraction is NEVER quarantined and never skips the cursor (the pre-existing design rule)',
    { deadLetters: 0, nextReorgId: 0 },
    { deadLetters: facts.deadLetters, nextReorgId: facts.nextReorgId },
    facts.deadLetters.length === 0 && facts.nextReorgId === 0,
    'the DLQ commit excludes retractions on purpose: a late retraction costs money, so that queue stays blocked',
  );

  fs.rmSync(dir, { recursive: true, force: true });
}

// G3 — the report keeps the facts it already carried (a second pass, fresh
// journal, so the fact is read from a clean state).
{
  const { dir, db } = mkJournal('g3');
  plant(db, 3);
  const srv = refusingServer();
  const cfg = writeConfig(dir, srv.url, 5);
  const r = run(BIN, ['webhook', 'push', '--db', db, '--config', cfg], dir);
  srv.close();
  const t = r.json?.targets?.[0] ?? {};

  guard(
    'G3',
    'the report keeps the queue facts it already carried',
    { blocked: true, blockedReorgId: 1, remainingReorgs: 3 },
    {
      blocked: t.blocked ?? null,
      blockedReorgId: t.blocked_reorg_id ?? null,
      remainingReorgs: t.remaining_reorgs_behind_cursor ?? null,
    },
    t.blocked === true &&
      t.blocked_reorg_id === 1 &&
      t.remaining_reorgs_behind_cursor === 3,
    'the facts were right before the fix and must stay right',
  );

  fs.rmSync(dir, { recursive: true, force: true });
}

// ---------------------------------------------------------------- verdict

const facesHeld = faces.filter((f) => f.ok).length;
const guardsHeld = guards.filter((g) => g.ok).length;
const allFacesHeld = facesHeld === faces.length;
const allGuardsHeld = guardsHeld === guards.length;

// The verdict says what the tree MEASURED, not what the fix claims.
const verdict = allFacesHeld && allGuardsHeld
  ? 'retraction-block-named'
  : allGuardsHeld
    ? 'retraction-block-blind'
    : 'guards-broken';

const sha = createHash('sha256').update(fs.readFileSync(BIN)).digest('hex');
const report = {
  measuredAt: new Date().toISOString(),
  label: LABEL,
  binary: { sha256: sha, version: run(BIN, ['--version'], process.cwd()).stdout.trim() },
  rule: [
    'the retraction queue is its own channel: an event is held at a seq, a retraction at a reorg id',
    "a blocked retraction is named as a retraction — the operator's line says which queue is stuck and at which id",
    "the retraction's reason travels on its own field; one queue's reason is never read as the other's",
    'a refused retraction is never quarantined: a late retraction costs money, so that queue stays blocked',
  ],
  faces,
  guards,
  verdict,
};

if (OUT) {
  fs.writeFileSync(OUT, JSON.stringify(report, null, 2) + '\n');
}

console.log(
  `verdict=${verdict} faces=${facesHeld}/${faces.length} guards=${guardsHeld}/${guards.length}`,
);
for (const f of faces) console.log(`  ${f.ok ? 'OK  ' : 'FAIL'} ${f.id} ${f.claim}`);
for (const g of guards) console.log(`  ${g.ok ? 'OK  ' : 'FAIL'} ${g.id} ${g.claim}`);

process.exit(allFacesHeld && allGuardsHeld ? 0 : 1);
