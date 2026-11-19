#!/usr/bin/env node
// MEASUREMENT: the retraction queue's standing verdict must survive a pass
// that never reaches it.
//
// Claimed context (v0.7.37, PULS AT): the two delivery queues got their own
// standing-reason columns (`last_error` for events, `last_reorg_error` for
// retractions) precisely so one channel's write never erases the other's.
//
// But the CLEAR is shared: `deliver_events` calls `webhook_clear_error`
// (src/journal.rs), which sets `last_error = NULL, last_reorg_error = NULL`
// in ONE statement. `deliver_reorgs` never clears its own column, and it
// re-establishes the verdict only when it actually examines a row. So any
// pass whose event phase runs while the retraction phase examines NO row —
// a drained retraction queue under `webhook push --limit 0`, the pass the
// README documents as sending zero bytes and moving no cursor — erases the
// retraction verdict on disk and re-establishes nothing.
//
// Why `--limit 0` is the sharpest face: it is a pass the operator asks to
// move NOTHING, yet it silently rewrites the retraction queue's standing
// verdict in the journal. `webhook status` then reports the target as
// healthy and exits 0, while the retraction queue is still held.
//
// Faces (each a predicate run against BOTH trees):
//   F1 `webhook push --limit 0` on a journal whose retraction queue carries
//      a standing verdict and whose recorded reorgs are all delivered: the
//      verdict SURVIVES the pass in the journal.
//   F2 same journal: `webhook status` afterwards still reports
//      `reorg_error` and holds exit 3.
//   F3 same pass, but the retraction queue is held at an UNDELIVERED reorg:
//      the verdict survives and status still holds exit 3.
//   F4 `webhook push` (default limit) with a drained event queue and a
//      recorded verdict, and a NEW undelivered reorg behind the cursor: the
//      new retraction is delivered AND the pass does not fabricate a
//      verdict for the event channel (a through-pass may clear its own
//      channel — that is the v0.7.23 rule).
//   F5 the EVENT channel under the same pass: a recorded `last_error`
//      survives `push --limit 0` — the zero-byte pass erases both columns
//      today, and a fix that only moved the retraction column would leave
//      the identical hole open on the event side.
//
// Guards (green on BOTH trees — the normal cases must not move):
//   G1 a pass whose EVENT queue is blocked records `last_error` naming the
//      refusal (the write path still works).
//   G2 the same pass keeps the retraction channel's own column: the
//      v0.7.37 two-column rule holds for the WRITES.
//   G3 `push --limit 0` sends zero bytes to the sink and moves no cursor.
//   G4 a pass that GETS THROUGH clears both columns (the v0.7.23
//      stale-error rule must survive the fix).
//
//   node tools/measure-reorg-verdict-survival.mjs
//   BIN=/path/to/reeflog OUT=/tmp/x.json LABEL=v0.7.39 node tools/measure-reorg-verdict-survival.mjs
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

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const results = { label: LABEL, bin: BIN, faces: {}, guards: {}, raw: {} };

// A sink whose answer is scripted per request; every body is recorded, so
// "the body never left the machine" is a fact and not an inference.
function receiver(tag, script = []) {
  const seenFile = path.join(os.tmpdir(), `reeflog-verdict-seen-${tag}-${Date.now()}.json`);
  const portFile = seenFile + '.port';
  const child = spawn(
    process.execPath,
    [
      '-e',
      `
const http = require('node:http');
const fs = require('node:fs');
const out = process.argv[1];
const script = JSON.parse(process.argv[2]);
const seen = [];
let i = 0;
const srv = http.createServer((req, res) => {
  let body = '';
  req.on('data', (c) => { body += c; });
  req.on('end', () => {
    const answer = script.length ? script[Math.min(i, script.length - 1)] : 200;
    i += 1;
    seen.push({ eventId: req.headers['x-reeflog-event-id'] ?? null, seq: req.headers['x-reeflog-seq'] ?? null, len: body.length, answer });
    fs.writeFileSync(out, JSON.stringify(seen, null, 2));
    res.statusCode = answer;
    res.end('ok');
  });
});
srv.listen(0, () => fs.writeFileSync(out + '.port', String(srv.address().port)));
`,
      seenFile,
      JSON.stringify(script),
    ],
    { stdio: 'ignore' },
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
    stop() {
      try { child.kill(); } catch {}
    },
  };
}

function makeDir(tag) {
  return fs.mkdtempSync(path.join(os.tmpdir(), `reeflog-verdict-${tag}-`));
}

function initDb(dir) {
  const db = path.join(dir, 'j.db');
  execFileSync(BIN, ['init', '--db', db, '--program', PROGRAM], { cwd: dir, stdio: 'ignore' });
  return db;
}

function writeConfig(dir, name, targets) {
  const p = path.join(dir, name);
  fs.writeFileSync(p, JSON.stringify({ targets }, null, 2));
  return p;
}

function run(dir, args) {
  const res = spawnSync(BIN, args, { cwd: dir, encoding: 'utf8', timeout: 30000 });
  let json = null;
  try { json = JSON.parse(res.stdout); } catch { json = null; }
  return { code: res.status, stdout: res.stdout ?? '', stderr: (res.stderr ?? '').trim(), json };
}

function seedEvent(conn, slot, sig, payloadState = 'held') {
  conn
    .prepare(
      `INSERT INTO events(signature, slot, err, logs, source, first_seen, payload_state)
       VALUES (?, ?, NULL, ?, 'ws', ?, ?)`,
    )
    .run(sig, slot, JSON.stringify(['probe']), Date.now(), payloadState);
}

function seedReorg(conn, slot, sig, detail) {
  conn
    .prepare(
      `INSERT INTO reorgs(signature, slot, detected_at, status_source, tx_source, detail)
       VALUES (?, ?, ?, 'probe', 'probe', ?)`,
    )
    .run(sig, slot, Date.now(), detail);
}

function stateOf(db) {
  const conn = new DatabaseSync(db);
  const row = conn.prepare("SELECT last_error, last_reorg_error, next_seq, next_reorg_id FROM webhook_state WHERE target='t1'").get();
  conn.close();
  return row;
}

function setState(db, fields) {
  const conn = new DatabaseSync(db);
  conn
    .prepare(
      `INSERT INTO webhook_state (target, endpoint, next_seq, next_reorg_id, last_error, last_reorg_error, updated_at)
       VALUES ('t1', ?, ?, ?, ?, ?, ?)
       ON CONFLICT(target) DO UPDATE SET
         endpoint=excluded.endpoint, next_seq=excluded.next_seq,
         next_reorg_id=excluded.next_reorg_id, last_error=excluded.last_error,
         last_reorg_error=excluded.last_reorg_error, updated_at=excluded.updated_at`,
    )
    .run(
      fields.endpoint ?? '',
      fields.next_seq ?? 0,
      fields.next_reorg_id ?? 0,
      fields.last_error ?? null,
      fields.last_reorg_error ?? null,
      Date.now(),
    );
  conn.close();
}

function statusRow(res, id = 't1') {
  const t = (res.json?.targets ?? []).find((x) => x.target === id) ?? {};
  // `webhook status` names the event channel `last_error` and the retraction
  // channel `reorg_error` (the push report names the event channel `error`).
  return { exit: res.code, last_error: t.last_error ?? null, reorg_error: t.reorg_error ?? null };
}

async function main() {
  // ---------- F1/F2: drained retraction queue, --limit 0 -----------------
  {
    const dir = makeDir('f1');
    const db = initDb(dir);
    const sink = receiver('f1', [200]);
    const port = await sink.port();
    const cfg = writeConfig(dir, 'cfg.json', [
      { id: 't1', url: `http://127.0.0.1:${port}/hook`, secret: 's-t1', timeout_ms: 2000, dead_letter_after: 0 },
    ]);
    const conn = new DatabaseSync(db);
    seedEvent(conn, 900, `sig-ev-f1-${Date.now()}xxxx`);
    seedReorg(conn, 890, `sig-re-f1-${Date.now()}xxxx`, 'f1 retraction');
    conn.close();
    // The retraction queue is DRAINED (cursor past the only reorg) but the
    // queue carries a standing verdict from an earlier incident.
    setState(db, { endpoint: `http://127.0.0.1:${port}/hook`, next_seq: 1, next_reorg_id: 1, last_reorg_error: 'refused with http 404' });

    const push = run(dir, ['webhook', 'push', '--db', db, '--config', cfg, '--limit', '0']);
    const row = stateOf(db);
    results.raw.f1_row = row;
    results.faces.F1_verdict_survives_idle_pass = row?.last_reorg_error === 'refused with http 404';

    const status = run(dir, ['webhook', 'status', '--db', db, '--config', cfg]);
    const srow = statusRow(status);
    results.raw.f1_status = srow;
    results.faces.F2_status_reports_reorg_error_exit3 =
      srow.exit === 3 && typeof srow.reorg_error === 'string' && srow.reorg_error.includes('404');
    results.guards.G3_limit0_zero_bytes = sink.seen().length === 0;
    results.raw.f1_push = { exit: push.code, stderr: push.stderr };
    sink.stop();
  }

  // ---------- F3: retraction queue held at an UNDELIVERED reorg ----------
  {
    const dir = makeDir('f3');
    const db = initDb(dir);
    const sink = receiver('f3', [404]);
    const port = await sink.port();
    const cfg = writeConfig(dir, 'cfg.json', [
      { id: 't1', url: `http://127.0.0.1:${port}/hook`, secret: 's-t1', timeout_ms: 2000, dead_letter_after: 0 },
    ]);
    const conn = new DatabaseSync(db);
    seedEvent(conn, 900, `sig-ev-f3-${Date.now()}xxxx`);
    seedReorg(conn, 890, `sig-re-f3a-${Date.now()}xxxx`, 'f3 retraction a');
    seedReorg(conn, 895, `sig-re-f3b-${Date.now()}xxxx`, 'f3 retraction b');
    conn.close();
    // cursor at reorg 1 -> reorg 2 is still owed; the standing verdict names
    // the refusal that stopped the queue.
    setState(db, { endpoint: `http://127.0.0.1:${port}/hook`, next_seq: 1, next_reorg_id: 1, last_reorg_error: 'refused with http 404' });

    const push = run(dir, ['webhook', 'push', '--db', db, '--config', cfg, '--limit', '0']);
    const row = stateOf(db);
    results.raw.f3_row = row;
    results.faces.F3_held_verdict_survives_idle_pass = row?.last_reorg_error === 'refused with http 404';
    const status = run(dir, ['webhook', 'status', '--db', db, '--config', cfg]);
    const srow = statusRow(status);
    results.raw.f3_status = srow;
    results.faces.F3_status_exit3 = srow.exit === 3 && typeof srow.reorg_error === 'string' && srow.reorg_error.includes('404');
    results.raw.f3_push = { exit: push.code, stderr: push.stderr };
    sink.stop();
  }

  // ---------- F4: default pass, new reorg delivered ----------------------
  {
    const dir = makeDir('f4');
    const db = initDb(dir);
    const sink = receiver('f4', [200]);
    const port = await sink.port();
    const cfg = writeConfig(dir, 'cfg.json', [
      { id: 't1', url: `http://127.0.0.1:${port}/hook`, secret: 's-t1', timeout_ms: 2000, dead_letter_after: 0 },
    ]);
    const conn = new DatabaseSync(db);
    seedEvent(conn, 900, `sig-ev-f4-${Date.now()}xxxx`);
    seedReorg(conn, 890, `sig-re-f4a-${Date.now()}xxxx`, 'f4 retraction a');
    seedReorg(conn, 895, `sig-re-f4b-${Date.now()}xxxx`, 'f4 retraction b');
    conn.close();
    setState(db, { endpoint: `http://127.0.0.1:${port}/hook`, next_seq: 1, next_reorg_id: 1, last_reorg_error: 'refused with http 404' });

    const push = run(dir, ['webhook', 'push', '--db', db, '--config', cfg]);
    const row = stateOf(db);
    results.raw.f4_row = row;
    // The pass GETS THROUGH (200): the event queue is drained and the one
    // owed retraction is delivered -> the pass succeeded, so the standing
    // verdict of BOTH channels is cleared (the v0.7.23 rule).
    results.guards.G4_through_pass_clears_both = row?.last_reorg_error === null && row?.last_error === null;
    // and the retraction really was handed over (the fix must not cost
    // queue progress).
    results.faces.F4_new_retraction_delivered = sink.seen().length >= 1;
    results.raw.f4_push = { exit: push.code, stderr: push.stderr };
    sink.stop();
  }

  // ---------- F5: event-channel verdict survives the idle pass ----------
  {
    const dir = makeDir('f5');
    const db = initDb(dir);
    const sink = receiver('f5', [200]);
    const port = await sink.port();
    const cfg = writeConfig(dir, 'cfg.json', [
      { id: 't1', url: `http://127.0.0.1:${port}/hook`, secret: 's-t1', timeout_ms: 2000, dead_letter_after: 0 },
    ]);
    const conn = new DatabaseSync(db);
    seedEvent(conn, 900, `sig-ev-f5-${Date.now()}xxxx`);
    conn.close();
    // Event queue DRAINED (cursor past the only event), a standing event
    // verdict from an earlier incident — the mirror image of F1.
    setState(db, {
      endpoint: `http://127.0.0.1:${port}/hook`,
      next_seq: 1,
      next_reorg_id: 0,
      last_error: 'the payload is still owed — run `hydrate`',
    });
    const push = run(dir, ['webhook', 'push', '--db', db, '--config', cfg, '--limit', '0']);
    const row = stateOf(db);
    results.raw.f5_row = row;
    results.faces.F5_event_verdict_survives_idle_pass =
      typeof row?.last_error === 'string' && row.last_error.includes('hydrate');
    results.raw.f5_push = { exit: push.code, stderr: push.stderr };
    sink.stop();
  }

  // ---------- G1/G2: the WRITES keep their own columns ------------------
  {
    const dir = makeDir('g12');
    const db = initDb(dir);
    const sink = receiver('g12', [200, 404, 404]);
    const port = await sink.port();
    const cfg = writeConfig(dir, 'cfg.json', [
      { id: 't1', url: `http://127.0.0.1:${port}/hook`, secret: 's-t1', timeout_ms: 2000, dead_letter_after: 0 },
    ]);
    const conn = new DatabaseSync(db);
    seedEvent(conn, 900, `sig-ev-g12a-${Date.now()}xxxx`);
    seedEvent(conn, 905, `sig-ev-g12b-${Date.now()}xxxx`);
    seedReorg(conn, 890, `sig-re-g12-${Date.now()}xxxx`, 'g12 retraction');
    conn.close();
    setState(db, { endpoint: `http://127.0.0.1:${port}/hook`, next_seq: 0, next_reorg_id: 0, last_reorg_error: 'refused with http 500' });

    // pass 1: event 1 -> 200, event 2 -> 404 (blocked), then the retraction
    // phase examines reorg 1 -> 404 (blocked). Both columns get a verdict.
    const p1 = run(dir, ['webhook', 'push', '--db', db, '--config', cfg]);
    const row1 = stateOf(db);
    results.raw.g12_row1 = row1;
    results.raw.g12_push1 = { exit: p1.code, stderr: p1.stderr };
    results.guards.G1_event_write_names_refusal =
      typeof row1?.last_error === 'string' && row1.last_error.includes('404');
    // G2: the retraction column holds its OWN verdict (v0.7.37), i.e. the
    // event channel's write did not take the retraction's column.
    results.guards.G2_reorg_column_holds_own_verdict =
      typeof row1?.last_reorg_error === 'string' && row1.last_reorg_error.includes('404');
    sink.stop();
  }

  results.bin_sha256 = createHash('sha256').update(fs.readFileSync(BIN)).digest('hex');
  // Artifacts are public: no host paths survive the write (the repo gate
  // `no_committed_artifact_carries_a_host_path` reads this file).
  const scrub = (s) => (typeof s === 'string' ? s.replace(/\/(tmp|home)\/[^"\s]*/g, '(host-path)') : s);
  results.bin = scrub(results.bin);
  for (const [k, v] of Object.entries(results.raw)) results.raw[k] = JSON.parse(scrub(JSON.stringify(v)));
  results.faces_count = Object.values(results.faces).filter(Boolean).length;
  results.faces_total = Object.keys(results.faces).length;
  results.guards_green = Object.values(results.guards).filter(Boolean).length;
  results.guards_total = Object.keys(results.guards).length;
  results.verdict =
    results.faces_count === results.faces_total
      ? 'reorg-verdict-survives'
      : 'reorg-verdict-erased-by-idle-pass';
  if (OUT) fs.writeFileSync(OUT, JSON.stringify(results, null, 2));
  console.log(JSON.stringify(results, null, 2));
}

main().catch((e) => {
  console.error('MEASUREMENT FAILED:', e && e.message);
  process.exit(2);
});
