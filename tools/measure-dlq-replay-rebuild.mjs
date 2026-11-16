#!/usr/bin/env node
// MEASUREMENT: `webhook dlq replay` must answer ONE fact with ONE body —
// the journal's current row, the same body `webhook push` builds.
//
// The dead letter stores the bytes that were owed at QUARANTINE time. That
// is a snapshot of a moment, not a fact: between quarantine and replay the
// chain can unmake the transaction (journal seal `reverted` + a `reorgs`
// row), `hydrate` can pay a payload the frozen body still shows as null,
// and only `push` (which reads `events_after`) ever noticed — the replay
// re-sent the freeze. Measured on the published v0.7.31 asset: the consumer
// was refused the event once (quarantined), the chain unmade the tx, `push`
// delivered the RETRACTION, and then `dlq replay` handed the consumer a
// body claiming `seal: "finalized"` — the last word for that signature was
// a claim the chain had unmade, and the report did not name the divergence.
//
// Faces (each a predicate run against ONE binary):
//   D1 the replayed body carries the seal the journal holds for that event
//      (published: `finalized` while the journal says `reverted`).
//   D2 replay and push answer the SAME event with the SAME body: a fresh
//      second target read through `push` on the same journal must receive
//      the bytes the replayed target receives for that seq.
//   D3 the report names the divergence (`rebuilt`) — a body that came from
//      the journal instead of the freeze is a fact the operator sees.
//
// Guards (must be green on BOTH binaries):
//   G1 an UNCHANGED letter still replays byte-for-byte (the documented
//      signature guarantee holds where it is true).
//   G2 a letter with no journal row (planted / pre-events) still replays
//      its stored bytes and exits 0 — nothing is ever dropped.
//   G3 a replay refused again stays pending and exits 3 (v0.7.7/v0.7.12
//      rules must not regress).
//
//   node tools/measure-dlq-replay-rebuild.mjs
//   BIN=/path/to/reeflog OUT=/tmp/x.json LABEL=published-v0.7.31 node tools/measure-dlq-replay-rebuild.mjs
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import http from 'node:http';
import { spawn, spawnSync, execFileSync } from 'node:child_process';
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

// The receiver lives in THIS process, so the binary must run ASYNCHRONOUSLY:
// a synchronous child would block the event loop that has to answer the
// POST, and every delivery would end as a timeout.
function run(bin, args, cwd) {
  return new Promise((resolve) => {
    const child = spawn(bin, args, { cwd });
    let stdout = '', stderr = '';
    child.stdout.on('data', (d) => { stdout += d; });
    child.stderr.on('data', (d) => { stderr += d; });
    child.on('close', (code) => {
      let json = null;
      try { json = JSON.parse(stdout.trim()); } catch { json = null; }
      resolve({ exitCode: code, stdout, stderr, json });
    });
  });
}

const sha = (s) => createHash('sha256').update(s).digest('hex');

/** A receiver that records every body per path, refusing on demand. */
async function startReceiver() {
  const received = []; // { path, body, headers }
  let refuseNext = 0;  // refuse this many requests to /hook
  let refuseAll = false;
  const srv = http.createServer((req, res) => {
    const chunks = [];
    req.on('data', (c) => chunks.push(c));
    req.on('end', () => {
      received.push({ path: req.url, body: Buffer.concat(chunks).toString('utf8'), headers: req.headers });
      const refuse = req.url === '/hook' && (refuseAll || refuseNext > 0);
      if (refuse) {
        if (refuseNext > 0) refuseNext -= 1;
        res.writeHead(403).end('{"e":"no"}');
        return;
      }
      res.writeHead(200).end('{}');
    });
  });
  await new Promise((r) => srv.listen(0, '127.0.0.1', r));
  const port = srv.address().port;
  return {
    srv,
    base: `http://127.0.0.1:${port}`,
    received,
    refuseOnce: () => { refuseNext = 1; },
    setRefuseAll: (v) => { refuseAll = v; },
    since: (mark) => received.slice(mark),
    close: () => new Promise((r) => srv.close(r)),
  };
}

/** A fresh journal in a fresh dir. */
function mkJournal(tag) {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), `reeflog-dlq-rebuild-${tag}-`));
  const db = path.join(dir, 'reeflog.db');
  execFileSync(BIN, ['init', '--db', db, '--program', PROGRAM], { cwd: dir, stdio: 'ignore' });
  return { dir, db };
}

function writeConfig(dir, targets) {
  const cfg = path.join(dir, 'wh.json');
  fs.writeFileSync(cfg, JSON.stringify({ targets }));
  return cfg;
}

const SIG = 'SigRevert1111111111111111111111111111111111111';
const PLANTED_SIG = 'SigClean0111111111111111111111111111111111111';

/** One event, sealed finalized, with content — the state at ingest. */
function plantEvent(db, signature, seqSlot) {
  const c = new DatabaseSync(db);
  c.prepare(
    `INSERT INTO events(signature, slot, err, logs, source, first_seen, seal_status, payload_state)
     VALUES (?, ?, NULL, ?, 'ws', ?, 'finalized', 'held')`,
  ).run(signature, seqSlot, '["log one"]', Date.now());
  c.close();
}

/** The chain unmakes the transaction: seal flips, reorg record appears —
 *  exactly the two writes `mark_reverted` performs. */
function unmake(db, signature, slot) {
  const c = new DatabaseSync(db);
  c.prepare("UPDATE events SET seal_status='reverted', seal_seen=? WHERE signature=?").run(Date.now(), signature);
  c.prepare(
    `INSERT INTO reorgs(signature, slot, detected_at, status_source, tx_source, detail)
     VALUES (?, ?, ?, 'status', 'tx', 'planted by the measurement')`,
  ).run(signature, slot, Date.now());
  c.close();
}

const t1 = (base) => ({ id: 't1', url: `${base}/hook`, secret: 's1', timeout_ms: 5000, dead_letter_after: 1 });
const t2 = (base) => ({ id: 't2', url: `${base}/hook2`, secret: 's2', timeout_ms: 5000, dead_letter_after: 1 });

const rx = await startReceiver();

// --- D1 + D2: the chain unmakes the transaction between quarantine and replay
const d1 = await (async () => {
  const { dir, db } = mkJournal('revert');
  const cfg1 = writeConfig(dir, [t1(rx.base)]);
  plantEvent(db, SIG, 44000011);

  // 1) the consumer refuses once -> quarantined with the bytes of that moment.
  rx.refuseOnce();
  const push1 = await run(BIN, ['webhook', 'push', '--db', db, '--config', cfg1], dir);

  // 2) the chain unmakes the transaction (journal now says `reverted`).
  unmake(db, SIG, 44000011);

  // 3) a FRESH second target appears; `push` builds its body from the row.
  const cfg2 = writeConfig(dir, [t1(rx.base), t2(rx.base)]);
  const mark = rx.received.length;
  const push2 = await run(BIN, ['webhook', 'push', '--db', db, '--config', cfg2], dir);
  const afterPush = rx.since(mark);

  // 4) replay the quarantined letter to t1.
  const mark2 = rx.received.length;
  const replay = await run(BIN, ['webhook', 'dlq', 'replay', '--db', db, '--config', cfg2], dir);
  const afterReplay = rx.since(mark2);

  const c = new DatabaseSync(db);
  const journalSeal = c.prepare('SELECT seal_status FROM events WHERE signature=?').get(SIG)?.seal_status ?? null;
  const frozen = c.prepare("SELECT body FROM webhook_dead_letters WHERE target='t1' AND kind='event'").get()?.body ?? null;
  const receipt = c.prepare("SELECT body_sha256 FROM webhook_receipts WHERE target='t1' AND kind='event'").get()?.body_sha256 ?? null;
  c.close();

  const replayedBody = afterPush.length + afterReplay.length > 0
    ? (afterReplay.find((r) => r.path === '/hook')?.body ?? null)
    : null;
  const pushedBody = afterPush.find((r) => r.path === '/hook2' && (() => {
    try { return JSON.parse(r.body).type === 'event'; } catch { return false; }
  })())?.body ?? null;

  const parsed = (s) => { try { return JSON.parse(s); } catch { return null; } };

  return {
    dir, push1, push2, replay, journalSeal, frozen, receipt,
    replayedBody, pushedBody,
    replayedSeal: parsed(replayedBody)?.seal ?? null,
    pushedSeal: parsed(pushedBody)?.seal ?? null,
    rebuilt: replay.json?.rebuilt ?? null,
    retractionDeliveredToT1: afterPush.some((r) => r.path === '/hook' && (() => {
      try { return JSON.parse(r.body).type === 'reorg'; } catch { return false; }
    })()),
    replayExit: replay.exitCode,
    replayReport: replay.json,
  };
})();

// --- G1: an UNCHANGED letter replays byte-for-byte ---------------------------
const g1 = await (async () => {
  const { dir, db } = mkJournal('unchanged');
  const cfg = writeConfig(dir, [t1(rx.base)]);
  plantEvent(db, PLANTED_SIG, 44000021);
  rx.refuseOnce();
  await run(BIN, ['webhook', 'push', '--db', db, '--config', cfg], dir);
  const c = new DatabaseSync(db);
  const frozen = c.prepare("SELECT body FROM webhook_dead_letters WHERE target='t1' AND kind='event'").get()?.body ?? null;
  c.close();
  const mark = rx.received.length;
  const replay = await run(BIN, ['webhook', 'dlq', 'replay', '--db', db, '--config', cfg], dir);
  const sent = rx.since(mark).find((r) => r.path === '/hook')?.body ?? null;
  return { dir, replay, frozen, sent, rebuilt: replay.json?.rebuilt ?? null };
})();

// --- G2: a letter with no journal row still replays its stored bytes --------
const g2 = await (async () => {
  const { dir, db } = mkJournal('norow');
  const cfg = writeConfig(dir, [t1(rx.base)]);
  const c = new DatabaseSync(db);
  const planted = '{"type":"event","seq":1,"planted":true}';
  c.prepare(
    `INSERT INTO webhook_dead_letters(target,kind,seq,body,event_id,reason,status,attempts,
                                      first_failed_at,last_failed_at,state)
     VALUES('t1','event',1,?,'t1:event:1','refused with http 403',403,1,1700000000000,1700000000000,'pending')`,
  ).run(planted);
  c.close();
  const mark = rx.received.length;
  const replay = await run(BIN, ['webhook', 'dlq', 'replay', '--db', db, '--config', cfg], dir);
  const sent = rx.since(mark).find((r) => r.path === '/hook')?.body ?? null;
  const c2 = new DatabaseSync(db);
  const state = c2.prepare("SELECT state FROM webhook_dead_letters WHERE target='t1'").get()?.state ?? null;
  c2.close();
  return { dir, replay, planted, sent, state };
})();

// --- G3: a replay refused again stays pending and exits 3 -------------------
const g3 = await (async () => {
  const { dir, db } = mkJournal('refused');
  const cfg = writeConfig(dir, [t1(rx.base)]);
  plantEvent(db, 'SigStillRefused1111111111111111111111111111', 44000031);
  rx.refuseOnce();
  await run(BIN, ['webhook', 'push', '--db', db, '--config', cfg], dir);
  rx.setRefuseAll(true);
  const replay = await run(BIN, ['webhook', 'dlq', 'replay', '--db', db, '--config', cfg], dir);
  rx.setRefuseAll(false);
  const c = new DatabaseSync(db);
  const state = c.prepare("SELECT state FROM webhook_dead_letters WHERE target='t1'").get()?.state ?? null;
  const pending = c.prepare("SELECT COUNT(*) n FROM webhook_dead_letters WHERE state='pending'").get()?.n ?? 0;
  c.close();
  return { dir, replay, state, pending };
})();

await rx.close();
for (const s of [d1, g1, g2, g3]) {
  fs.rmSync(s.dir, { recursive: true, force: true });
}

const faces = [
  {
    face: 'D1',
    claim: 'the replayed body carries the seal the JOURNAL holds for that event (not the seal the quarantine froze)',
    expected: { journalSeal: 'reverted', replayedSeal: 'reverted' },
    observed: { journalSeal: d1.journalSeal, replayedSeal: d1.replayedSeal },
    ok: d1.journalSeal === 'reverted' && d1.replayedSeal === 'reverted',
    note: `the chain unmade the tx between quarantine and replay; push delivered the retraction to t1 first: ${d1.retractionDeliveredToT1}; the replay then sent seal="${d1.replayedSeal}" (frozen body said "finalized"), exit ${d1.replayExit}`,
  },
  {
    face: 'D2',
    claim: 'replay and push answer the SAME event with the SAME body — a fresh target read through `push` receives what the replayed target receives',
    expected: 'identical bodies',
    observed: { replay: d1.replayedSeal, push: d1.pushedSeal, sameBytes: d1.replayedBody !== null && d1.replayedBody === d1.pushedBody },
    ok: d1.replayedBody !== null && d1.replayedBody === d1.pushedBody,
    note: `one journal, two surfaces: push built its body from the row (seal "${d1.pushedSeal}"), the replay sent the freeze (seal "${d1.replayedSeal}")`,
  },
  {
    face: 'D3',
    claim: 'the replay report names the letters whose body came from the journal instead of the freeze (`rebuilt`)',
    expected: { rebuilt: 1 },
    observed: { rebuilt: d1.rebuilt },
    ok: d1.rebuilt === 1,
    note: `report: ${JSON.stringify(d1.replayReport ?? null)}`,
  },
  {
    face: 'G1',
    claim: 'an UNCHANGED letter still replays byte-for-byte (the documented signature guarantee where it is true)',
    expected: { byteIdentical: true, exitCode: 0 },
    observed: { byteIdentical: g1.frozen !== null && g1.frozen === g1.sent, exitCode: g1.replay.exitCode, rebuilt: g1.rebuilt },
    ok: g1.frozen !== null && g1.frozen === g1.sent && g1.replay.exitCode === 0,
    note: `frozen ${g1.frozen?.length ?? -1} bytes, sent ${g1.sent?.length ?? -1} bytes, rebuilt ${g1.rebuilt}`,
  },
  {
    face: 'G2',
    claim: 'a letter with no journal row replays its STORED bytes and exits 0 — nothing is ever dropped',
    expected: { sentVerbatim: true, exitCode: 0, state: 'replayed' },
    observed: { sentVerbatim: g2.planted === g2.sent, exitCode: g2.replay.exitCode, state: g2.state },
    ok: g2.planted === g2.sent && g2.replay.exitCode === 0 && g2.state === 'replayed',
    note: `planted ${g2.planted?.length ?? -1} bytes, sent ${g2.sent?.length ?? -1} bytes`,
  },
  {
    face: 'G3',
    claim: 'a replay refused again stays pending and exits 3 (the queue keeps holding it)',
    expected: { exitCode: 3, state: 'pending', pending: 1 },
    observed: { exitCode: g3.replay.exitCode, state: g3.state, pending: g3.pending },
    ok: g3.replay.exitCode === 3 && g3.state === 'pending' && g3.pending === 1,
    note: `failed=${g3.replay.json?.failed ?? 'n/a'}, remaining_pending=${g3.replay.json?.remaining_pending ?? 'n/a'}`,
  },
];

const defectFaces = faces.filter((f) => !f.face.startsWith('G'));
const guardFaces = faces.filter((f) => f.face.startsWith('G'));
const verdict = defectFaces.every((f) => f.ok) && guardFaces.every((f) => f.ok)
  ? 'dlq-replay-rebuild-held'
  : 'dlq-replay-rebuild-blind';

const artifact = {
  measuredAt: new Date().toISOString(),
  label: LABEL,
  // The binary is NAMED BY ITS BYTES, never by its host path.
  binary: {
    sha256: createHash('sha256').update(fs.readFileSync(BIN)).digest('hex'),
    version: spawnSync(BIN, ['--version'], { encoding: 'utf8' }).stdout?.trim() ?? null,
  },
  program: PROGRAM,
  rule: [
    'the dead letter stores the bytes that were owed at quarantine time — a snapshot, not a fact',
    'the journal row is the fact: seal, payload and accounts can all move while a letter waits',
    'push already rebuilt the body from the row (deliver_events reads events_after); only the replay read the freeze',
    'one fact, one verdict per surface: the same seq must reach a consumer as the same body whichever command delivers it',
  ],
  faces,
  verdict,
};

if (OUT) {
  fs.writeFileSync(OUT, JSON.stringify(artifact, null, 2) + '\n');
  console.error(`wrote ${OUT}`);
}

// No host paths in the artifact.
const dirs = [d1, g1, g2, g3].map((s) => s.dir);
const serialized = JSON.stringify(artifact);
if (serialized.includes(BIN) || dirs.some((d) => serialized.includes(d)) || /"\/(tmp|home)\//.test(serialized)) {
  console.error('ARTIFACT LEAK: a host path is in the artifact');
  process.exit(1);
}

console.log(JSON.stringify({
  verdict,
  faces: faces.map((f) => ({ face: f.face, ok: f.ok, observed: f.observed })),
}, null, 2));
process.exit(verdict === 'dlq-replay-rebuild-held' ? 0 : 1);
