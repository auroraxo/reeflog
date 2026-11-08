// The client contract, against the real binary and a real journal file.
//
// What is under test is NOT the SQLite schema (the client never opens the
// journal — the binary does) but the EXIT-CODE + CURSOR contract a consumer
// lives by, because getting it wrong is the one defect that loses events:
//
//   * exit 0  — batch delivered, cursor advanced, nothing replayed;
//   * exit 3 on tail — HELD at an owed row: rows BEFORE it are delivered,
//     the cursor does NOT pass it, and the next call must return it again
//     once the debt is paid (a "failure" here replays; a "success" here
//     drops the row forever);
//   * exit 1 — the refusal, with the binary's actionable words intact;
//   * exit 3 on verify/repair — an incomplete scan proves nothing and must
//     be an error to the caller, never a clean result.
//
// Fixture rows are inserted into a journal that `reeflog init` created, so
// the schema under test is the binary's own — no hand-written DDL.
import test from 'node:test';
import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { DatabaseSync } from 'node:sqlite';
import { reeflog, ReeflogError, EXIT } from '../index.js';

const BIN = process.env.REEFLOG_BIN || path.resolve('target/release/reeflog');
const PROGRAM = 'whirLbMiicVdio4qvUfM5KAg6Ct8VwpYzGff3uctyCc';

function tmpJournal(name) {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), `reeflog-client-${name}-`));
  const db = path.join(dir, 'reeflog.db');
  const out = spawnSync(BIN, ['init', '--db', db, '--program', PROGRAM], { encoding: 'utf8' });
  assert.equal(out.status, 0, `init failed: ${out.stderr}`);
  return { dir, db };
}

const binaryMissing = !fs.existsSync(BIN);

test('status returns the parsed health of a journal the binary owns', { skip: binaryMissing }, async () => {
  const { dir, db } = tmpJournal('status');
  const s = await reeflog({ db, bin: BIN }).status();
  assert.equal(s.program, PROGRAM);
  assert.equal(s.events, 0);
  assert.equal(s.max_seq, 0);
  // the report must name its subject, so a typo'd db cannot pass as healthy
  assert.equal(s.db, db);
  fs.rmSync(dir, { recursive: true, force: true });
});

test('tail delivers in seq order, advances the cursor, and never replays', { skip: binaryMissing }, async () => {
  const { dir, db } = tmpJournal('tail');
  const conn = new DatabaseSync(db);
  const now = Date.now();
  for (let i = 1; i <= 3; i++) {
    conn.prepare(
      `INSERT INTO events(signature, slot, err, logs, source, first_seen, payload_state)
       VALUES (?, ?, NULL, ?, 'ws', ?, 'held')`,
    ).run(`sig${i}`, 400000000 + i, JSON.stringify([`Program log ${i}`]), now);
  }
  conn.close();

  const box = reeflog({ db, bin: BIN });
  const first = await box.tail({ limit: 10 });
  assert.equal(first.events.length, 3);
  assert.deepEqual(first.events.map((e) => e.seq), [1, 2, 3]);
  assert.equal(first.events[0].signature, 'sig1');
  assert.deepEqual(first.events[2].logs, ['Program log 3']);
  assert.equal(first.held, null);
  assert.equal(first.exitCode, EXIT.OK);
  assert.equal(first.cursorValue, 3);

  // the cursor is durable: a fresh client with the same cursor resumes at 3
  const second = await reeflog({ db, bin: BIN }).tail({ cursor: first.cursor, limit: 10 });
  assert.equal(second.events.length, 0, 'a delivered batch must never come back');

  fs.rmSync(dir, { recursive: true, force: true });
});

test('tail --with-accounts HOLDS at an owed row: before it delivered, at/after it not consumed', { skip: binaryMissing }, async () => {
  const { dir, db } = tmpJournal('held');
  const conn = new DatabaseSync(db);
  const now = Date.now();
  // seq 1: full envelope (instructions + flags source present, held)
  conn.prepare(
    `INSERT INTO events(signature, slot, err, logs, source, first_seen, payload_state, accounts, account_state)
     VALUES (?, ?, NULL, ?, 'ws', ?, 'held', ?, 'held')`,
  ).run('done', 400000001, '["a"]', now, JSON.stringify({ instructions: [], accounts_flags_source: 'json' }));
  // seq 2: logs present, envelope still owed -> the hold row
  conn.prepare(
    `INSERT INTO events(signature, slot, err, logs, source, first_seen, payload_state)
     VALUES (?, ?, NULL, ?, 'ws', ?, 'held')`,
  ).run('owed', 400000002, '["b"]', now);
  // seq 3: deliverable, but it sits BEHIND the hold — must not be consumed
  conn.prepare(
    `INSERT INTO events(signature, slot, err, logs, source, first_seen, payload_state, accounts, account_state)
     VALUES (?, ?, NULL, ?, 'ws', ?, 'held', ?, 'held')`,
  ).run('behind', 400000003, '["c"]', now, JSON.stringify({ instructions: [], accounts_flags_source: 'json' }));
  conn.close();

  const box = reeflog({ db, bin: BIN });
  const r = await box.tail({ limit: 10, withAccounts: true });
  assert.equal(r.exitCode, EXIT.INCOMPLETE, 'an owed row is exit 3, never success');
  assert.deepEqual(r.events.map((e) => e.signature), ['done'], 'only the rows before the hold are delivered');
  assert.ok(r.held, 'the hold must be reported to the caller');
  assert.equal(r.held.seq, 2);
  assert.equal(r.held.signature, 'owed');
  assert.equal(r.cursorValue, 1, 'the cursor must not pass the owed row');

  // pay the debt, then the same cursor must deliver the held row (and behind it)
  const conn2 = new DatabaseSync(db);
  conn2.prepare(
    'UPDATE events SET accounts = ?, account_state = ? WHERE signature = ?',
  ).run(JSON.stringify({ instructions: [], accounts_flags_source: 'json' }), 'held', 'owed');
  conn2.close();

  const after = await box.tail({ limit: 10, withAccounts: true, cursor: r.cursor });
  assert.equal(after.exitCode, EXIT.OK);
  assert.deepEqual(after.events.map((e) => e.signature), ['owed', 'behind'], 'the owed row arrives once paid, nothing lost');
  assert.equal(after.held, null);

  fs.rmSync(dir, { recursive: true, force: true });
});

test('the two channels keep separate cursors by default', { skip: binaryMissing }, async () => {
  const { dir, db } = tmpJournal('channels');
  const box = reeflog({ db, bin: BIN });
  const t = await box.tail({ limit: 1 });
  const r = await box.reorgs({ limit: 1 });
  assert.notEqual(t.cursor, r.cursor, 'an event cursor and a retraction cursor must not collide');
  fs.rmSync(dir, { recursive: true, force: true });
});

test('a refusal keeps the binary\'s actionable words (exit 1 -> ReeflogError)', { skip: binaryMissing }, async () => {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'reeflog-client-refuse-'));
  const missing = path.join(dir, 'nope.db');
  await assert.rejects(
    () => reeflog({ db: missing, bin: BIN }).status(),
    (e) => {
      assert.ok(e instanceof ReeflogError);
      assert.equal(e.exitCode, EXIT.REFUSED);
      assert.match(e.stderr ?? '', /no journal|reeflog init/, 'the fix line must survive the mapping');
      return true;
    },
  );
  fs.rmSync(dir, { recursive: true, force: true });
});

test('a corrupt existing cursor is a refusal through the client API, not a silent full replay', { skip: binaryMissing }, async () => {
  const { dir, db } = tmpJournal('cursor-corrupt');
  const conn = new DatabaseSync(db);
  const now = Date.now();
  for (let i = 1; i <= 3; i++) {
    conn.prepare(
      `INSERT INTO events(signature, slot, err, logs, source, first_seen, payload_state)
       VALUES (?, ?, NULL, ?, 'ws', ?, 'held')`,
    ).run(`sig${i}`, 400000000 + i, JSON.stringify([`Program log ${i}`]), now);
  }
  conn.close();

  const cursor = path.join(dir, 'c.txt');
  fs.writeFileSync(cursor, 'not-a-cursor');

  // The refusal must reach the caller with the binary's words — a client
  // that swallowed it and returned [] would hand a consumer a "clean" empty
  // batch while the cursor file it owns sits unread.
  await assert.rejects(
    () => reeflog({ db, bin: BIN }).tail({ cursor }),
    (e) => {
      assert.ok(e instanceof ReeflogError, 'a corrupt cursor must be a ReeflogError');
      assert.equal(e.exitCode, EXIT.REFUSED);
      assert.match(e.stderr ?? '', /cursor/, 'the refusal must name the cursor file');
      assert.match(e.stderr ?? '', /not-a-cursor/, 'the refusal must name the offending content');
      return true;
    },
  );
  assert.equal(
    fs.readFileSync(cursor, 'utf8'),
    'not-a-cursor',
    'a refusal must leave the cursor file untouched — no evidence destroyed',
  );
  fs.rmSync(dir, { recursive: true, force: true });
});

test('an incomplete verify is an error to the caller, never a clean result', { skip: binaryMissing }, async () => {
  const { dir, db } = tmpJournal('verify');
  const box = reeflog({ db, bin: BIN });
  // an empty journal's scan cannot prove anything -> exit 3
  await assert.rejects(
    () => box.verify({ pages: 1 }),
    (e) => {
      assert.ok(e instanceof ReeflogError);
      assert.equal(e.exitCode, EXIT.INCOMPLETE);
      return true;
    },
  );
  fs.rmSync(dir, { recursive: true, force: true });
});

test('a missing binary names the install instead of crashing', async () => {
  const box = reeflog({ db: '/nonexistent/j.db', bin: '/definitely/not/reeflog' });
  await assert.rejects(
    () => box.status(),
    (e) => {
      assert.ok(e instanceof ReeflogError);
      assert.match(e.message, /reeflog binary not found/);
      return true;
    },
  );
});

test('Reeflog requires a journal path', () => {
  assert.throws(() => reeflog({}), (e) => e instanceof ReeflogError);
});
