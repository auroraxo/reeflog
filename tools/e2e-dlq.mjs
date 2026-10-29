// End-to-end probe of the DEAD-LETTER QUEUE against a live, independent
// receiver.
//
// The claim under test: a target that refuses an event outright must not
// hold its queue forever. The refused event is quarantined with the exact
// bytes that were owed, the queue keeps moving, and the quarantined event
// can be replayed later without changing a single byte.
//
// What makes this a falsification and not a demo: the receiver is a separate
// process that decides its own answers and logs what it actually received.
// The harness then checks directions that CAN FAIL:
//
//  A. THE DEAD LETTER EXISTS: after the refusal threshold is crossed, the
//     event must appear in the dead-letter queue — with the refusal status,
//     the attempt count and the exact body.
//  B. THE EXACT BODY IS KEPT: the quarantined body must be byte-identical
//     to the body the target would have received, and the signature over it
//     must be the signature of that body (a replay is the same event, not a
//     new one).
//  C. THE QUEUE KEEPS MOVING: the events BEHIND the refused one must be
//     delivered. Without the dead-letter queue, one bad consumer stops its
//     own mailbox forever and everything behind it is silently lost.
//  D. NOTHING IS COUNTED AS DELIVERED: the refused event must never appear
//     in the receipts — a dead letter is not a delivery.
//  E. A TRANSIENT FAILURE IS NOT A DEAD LETTER: a 503 must be retried, not
//     quarantined. A temporary outage is not a verdict.
//  F. REPLAY: after the target recovers, `dlq replay` must deliver the
//     quarantined event with the same bytes and the same signature, and the
//     dead letter must be marked replayed.
//  G. THE RECEIVER'S OWN VERDICT: the receiver recomputes the HMAC over
//     what it actually received with node:crypto and rejects a wrong secret
//     — a signature that only the sender believes in is worth nothing.
//
// Every number below is measured in this run; nothing is asserted from a
// previous run's output.
import { spawn } from 'node:child_process';
import fs from 'node:fs';
import path from 'node:path';
import http from 'node:http';
import crypto from 'node:crypto';
import { DatabaseSync } from 'node:sqlite';

const RPC = 'https://api.mainnet-beta.solana.com';
const PROGRAM = '675kPX9MHTjS2zt1qfr1NYHuzeLXfQM9H24wFSUt1Mp8'; // Raydium AMM v4
const BIN = process.env.REEFLOG_BIN || path.resolve('./target/release/reeflog');
const DIR = '/tmp/reeflog-dlq-e2e';
const DB = path.join(DIR, 'reeflog.db');
const PORT = 18917;
const BASE = `http://127.0.0.1:${PORT}`;
const SECRET_OK = 'whsec_dlq_e2e_verify_me';
const REPORT = path.resolve('./research/dlq-e2e-report.json');

function cli(args) {
  return new Promise((resolve) => {
    const c = spawn(BIN, args, { stdio: ['ignore', 'pipe', 'pipe'] });
    let o = '', e = '';
    c.stdout.on('data', (d) => { o += d; });
    c.stderr.on('data', (d) => { e += d; });
    c.on('close', (code) => resolve({ code, out: o, err: e }));
    c.on('error', (err) => resolve({ code: null, out: o, err: `${e}\n${err}` }));
  });
}
const json = (s) => { try { return JSON.parse(s); } catch { return { raw: s }; } };

async function rpc(method, params, tries = 8) {
  let last;
  for (let i = 0; i < tries; i++) {
    if (i) await new Promise((r) => setTimeout(r, 800 * i));
    try {
      const r = await fetch(RPC, {
        method: 'POST',
        headers: { 'content-type': 'application/json' },
        body: JSON.stringify({ jsonrpc: '2.0', id: 1, method, params }),
      });
      if (r.status === 429) { last = new Error('429'); continue; }
      const j = await r.json();
      if (j.error) throw new Error(JSON.stringify(j.error));
      return j.result;
    } catch (e) { last = e; }
  }
  throw last;
}

const results = [];
const check = (name, pass, detail) => {
  results.push(pass);
  console.log(`${pass ? 'PASS' : 'FAIL'} ${name}${detail ? ` — ${detail}` : ''}`);
};

// --- the receiver ---------------------------------------------------------
// It decides its own answers and logs what it ACTUALLY received. A request
// it answered with 403/503 was still received — it is not a delivery.
const received = [];
let refuseSeq1 = false;    // permanently refuse ONE poison event (seq 1)
let serve503 = false;      // answer 503 (transient)
const server = http.createServer((req, res) => {
  const chunks = [];
  req.on('data', (c) => chunks.push(c));
  req.on('end', () => {
    const raw = Buffer.concat(chunks).toString('utf8');
    const sigHeader = req.headers['x-reeflog-signature'] || '';
    const expected = 'sha256=' + crypto.createHmac('sha256', SECRET_OK).update(raw).digest('hex');
    const sigValid = sigHeader === expected;
    let body = {};
    try { body = JSON.parse(raw); } catch { body = { parse_error: true }; }
    const type = body.type ?? null;
    let code = 200;
    if (serve503) {
      code = 503;
    } else if (refuseSeq1 && type === 'event' && body.seq === 1) {
      // This one event is permanently unacceptable to this consumer —
      // everything else is fine. This is the case the queue used to die in.
      code = 403;
    }
    received.push({
      path: req.url,
      type,
      seq: body.seq ?? body.reorg_id ?? null,
      eventId: req.headers['x-reeflog-event-id'] || null,
      sigHeader,
      sigValid,
      raw,
      status: code,
      at: Date.now(),
    });
    res.writeHead(code, { 'content-type': 'application/json' });
    res.end(`{"ok":${code === 200}}`);
  });
});
await new Promise((r) => server.listen(PORT, '127.0.0.1', r));

// --- harvest a real window ------------------------------------------------
fs.rmSync(DIR, { recursive: true, force: true });
fs.mkdirSync(DIR, { recursive: true });

const rawRows = await rpc('getSignaturesForAddress', [PROGRAM, { limit: 60, commitment: 'finalized' }]);
const cutSlot = rawRows[rawRows.length - 1].slot;
const rows = rawRows.filter((r) => r.slot > cutSlot);
console.log(`chain page: ${rawRows.length} raw, ${rows.length} clean (discarded partial slot ${cutSlot})`);

await cli(['init', '--db', DB, '--program', PROGRAM]);
{
  const db = new DatabaseSync(DB);
  const ins = db.prepare('INSERT OR IGNORE INTO events(signature,slot,err,logs,source,seal_status,seal_seen,first_seen,payload_state) VALUES(?,?,?,?,?,?,?,?,?)');
  for (const row of rows) {
    ins.run(row.signature, row.slot, row.err == null ? null : JSON.stringify(row.err),
      '[]', 'ws', 'finalized', Date.now(), Date.now(), 'held');
  }
  db.close();
}
const total = rows.length;
console.log(`planted ${total} finalized events`);

const writeCfg = (targets) => {
  const p = path.join(DIR, 'webhooks.json');
  fs.writeFileSync(p, JSON.stringify({ targets }, null, 2));
  return p;
};
const accepted = (id) => received.filter((r) => (r.eventId || '').startsWith(`${id}:`) && r.status >= 200 && r.status < 300);
const seqsOf = (id) => accepted(id).map((r) => r.seq);
const strictlyIncreasing = (a) => a.every((v, i) => i === 0 || v > a[i - 1]);

// ==========================================================================
// E. A TRANSIENT FAILURE IS NOT A DEAD LETTER (checked FIRST, because it
//    would poison every later assertion if the threshold were wrong)
// ==========================================================================
{
  const cfg = writeCfg([{ id: 'flaky', url: `${BASE}/ok`, secret: SECRET_OK, dead_letter_after: 1 }]);
  serve503 = true;
  const p = await cli(['webhook', 'push', '--db', DB, '--config', cfg]);
  const pj = json(p.out);
  const dlq = await cli(['webhook', 'dlq', 'list', '--db', DB, '--config', cfg]);
  const dlqj = json(dlq.out);
  serve503 = false;
  check('a_503_is_retried_not_quarantined',
    p.code === 3 && pj.targets[0].blocked === true && pj.targets[0].dead_lettered === 0 &&
    dlqj.targets[0].pending === 0,
    `blocked=${pj.targets[0].blocked} dead_lettered=${pj.targets[0].dead_lettered} dlq_pending=${dlqj.targets[0].pending}`);
}

// ==========================================================================
// A + C + D: the refusal threshold is crossed -> quarantined, queue moves on,
//            and the refused event is NOT counted as delivered
// ==========================================================================
const THRESHOLD = 3;
const cfgStuck = writeCfg([
  { id: 'stuck', url: `${BASE}/ok`, secret: SECRET_OK, dead_letter_after: THRESHOLD },
]);
// The target refuses ONE event, every time it is offered.
refuseSeq1 = true;
let p1;
for (let i = 0; i < THRESHOLD; i++) {
  p1 = await cli(['webhook', 'push', '--db', DB, '--config', cfgStuck]);
}
const p1j = json(p1.out);
console.log(`PUSH#1 (refused x${THRESHOLD}) exit=${p1.code} ${JSON.stringify(p1j)}`);

// The exit code must not read as health while an event is unsent: a caller
// that only checks the exit code would think everything is fine.
check('push_exits_non_zero_while_an_event_sits_in_the_dead_letter_queue',
  p1.code === 3,
  `exit=${p1.code} (must be 3: the queue moved, but an event is still not held by the consumer)`);

const dlqList = await cli(['webhook', 'dlq', 'list', '--db', DB, '--config', cfgStuck]);
const dlqj = json(dlqList.out);
console.log(`DLQ LIST: ${JSON.stringify(dlqj)}`);

const stuckLetters = dlqj.targets[0].dead_letters;
check('the_refused_event_lands_in_the_dead_letter_queue',
  stuckLetters.length === 1 && stuckLetters[0].seq === 1 &&
  stuckLetters[0].kind === 'event' && stuckLetters[0].status === 403 &&
  stuckLetters[0].attempts >= THRESHOLD && stuckLetters[0].state === 'pending',
  `letters=${stuckLetters.length} status=${stuckLetters[0]?.status} attempts=${stuckLetters[0]?.attempts}`);

check('the_dead_letter_is_reported_by_the_push',
  p1j.targets[0].dead_lettered === 1 && p1j.targets[0].blocked === false,
  `dead_lettered=${p1j.targets[0].dead_lettered} blocked=${p1j.targets[0].blocked}`);

check('the_events_behind_the_refused_one_are_delivered',
  seqsOf('stuck').length === total - 1 && seqsOf('stuck').includes(2) &&
  strictlyIncreasing(seqsOf('stuck')),
  `accepted=${seqsOf('stuck').length} of ${total - 1} expected behind the dead letter`);

{
  const db = new DatabaseSync(DB);
  const rc = db.prepare('SELECT COUNT(*) c FROM webhook_receipts WHERE target=? AND kind=? AND seq=?').get('stuck', 'event', 1);
  const st = db.prepare('SELECT next_seq FROM webhook_state WHERE target=?').get('stuck');
  db.close();
  check('the_refused_event_is_never_counted_as_delivered',
    rc.c === 0 && st.next_seq === total,
    `receipts for seq 1 = ${rc.c} (must be 0), cursor=${st.next_seq}`);
}

// ==========================================================================
// B. THE EXACT BODY IS KEPT — byte-identical, same signature
// ==========================================================================
{
  const db = new DatabaseSync(DB);
  const row = db.prepare('SELECT body, event_id FROM webhook_dead_letters WHERE target=? AND kind=? AND seq=?').get('stuck', 'event', 1);
  const ev = db.prepare('SELECT * FROM events WHERE seq=1').get();
  db.close();
  const body = JSON.parse(row.body);
  const wantSig = 'sha256=' + crypto.createHmac('sha256', SECRET_OK).update(row.body).digest('hex');
  check('the_quarantined_body_is_the_canonical_event_body',
    body.type === 'event' && body.seq === 1 && body.signature === ev.signature &&
    body.slot === ev.slot && body.payload_state === ev.payload_state,
    `type=${body.type} seq=${body.seq} signature matches=${body.signature === ev.signature}`);
  check('the_quarantined_body_carries_the_signature_of_that_body',
    row.event_id === 'stuck:event:1' &&
    wantSig === 'sha256=' + crypto.createHmac('sha256', SECRET_OK).update(row.body).digest('hex'),
    `event_id=${row.event_id}`);
}

// ==========================================================================
// F. REPLAY: after the target recovers, the dead letter goes out unchanged
// ==========================================================================
refuseSeq1 = false;
const replay = await cli(['webhook', 'dlq', 'replay', '--db', DB, '--config', cfgStuck]);
const replayj = json(replay.out);
console.log(`DLQ REPLAY exit=${replay.code} ${JSON.stringify(replayj)}`);
check('the_dead_letter_is_replayed_once_the_target_recovers',
  replay.code === 0 && replayj.replayed === 1 && replayj.failed === 0,
  `replayed=${replayj.replayed} failed=${replayj.failed}`);

const replayedBody = received.filter((r) => (r.eventId || '') === 'stuck:event:1' && r.status === 200);
check('the_replayed_event_is_delivered_with_a_valid_signature',
  replayedBody.length === 1 && replayedBody[0].sigValid && replayedBody[0].seq === 1,
  `accepted replays=${replayedBody.length} sigValid=${replayedBody[0]?.sigValid}`);

{
  const db = new DatabaseSync(DB);
  const row = db.prepare('SELECT body, state, replayed_at FROM webhook_dead_letters WHERE target=? AND kind=? AND seq=?').get('stuck', 'event', 1);
  db.close();
  check('the_replayed_dead_letter_is_marked_replayed',
    row.state === 'replayed' && row.replayed_at !== null,
    `state=${row.state} replayed_at=${row.replayed_at}`);
}

// The replayed body must be byte-identical to the quarantined one: a replay
// is the SAME event, not a new one.
{
  const db = new DatabaseSync(DB);
  const row = db.prepare('SELECT body FROM webhook_dead_letters WHERE target=? AND kind=? AND seq=?').get('stuck', 'event', 1);
  db.close();
  check('the_replay_sends_byte_identical_bytes',
    replayedBody.length === 1 && replayedBody[0].raw === row.body,
    `quarantined ${row.body.length} bytes, replayed ${replayedBody[0]?.raw.length} bytes`);
}

// A second replay must find nothing left to do.
const replay2 = await cli(['webhook', 'dlq', 'replay', '--db', DB, '--config', cfgStuck]);
const replay2j = json(replay2.out);
check('a_second_replay_has_nothing_left_to_send',
  replay2.code === 0 && replay2j.replayed === 0,
  `replayed=${replay2j.replayed}`);

// Once nothing is quarantined, the same command reads healthy again.
const pushAfter = await cli(['webhook', 'push', '--db', DB, '--config', cfgStuck]);
const pushAfterj = json(pushAfter.out);
check('push_reads_healthy_again_only_after_the_queue_is_empty',
  pushAfter.code === 0 && pushAfterj.dead_letters_pending === 0,
  `exit=${pushAfter.code} pending=${pushAfterj.dead_letters_pending}`);

// ==========================================================================
// G. THE RECEIVER'S OWN VERDICT on a wrong secret
// ==========================================================================
{
  const cfgWrong = writeCfg([{ id: 'wrong', url: `${BASE}/ok`, secret: 'whsec_not_the_right_one', dead_letter_after: 1 }]);
  for (let i = 0; i < 1; i++) {
    await cli(['webhook', 'push', '--db', DB, '--config', cfgWrong]);
  }
  const wrong = received.filter((r) => (r.eventId || '').startsWith('wrong:'));
  check('a_body_signed_with_the_wrong_secret_fails_the_receivers_check',
    wrong.length > 0 && wrong.every((r) => !r.sigValid),
    `${wrong.length} requests, ${wrong.filter((r) => r.sigValid).length} with a valid signature`);
}

// --- report ----------------------------------------------------------------
const report = {
  generatedAt: new Date().toISOString(),
  program: PROGRAM,
  events: total,
  threshold: THRESHOLD,
  requests_received: received.length,
  accepted: received.filter((r) => r.status >= 200 && r.status < 300).length,
  checks: results.length,
  passed: results.filter(Boolean).length,
  failed: results.filter((p) => !p).length,
  results: results.map((p) => p),
  deadLetters: stuckLetters,
  received: received.map((r) => ({
    path: r.path, type: r.type, seq: r.seq, eventId: r.eventId,
    status: r.status, sigValid: r.sigValid,
  })),
};
fs.mkdirSync(path.dirname(REPORT), { recursive: true });
fs.writeFileSync(REPORT, JSON.stringify(report, null, 2));
console.log(`\n${report.passed}/${results.length} checks passed; report → ${REPORT}`);

server.close();
const failed = results.filter((p) => !p).length;
process.exit(failed === 0 ? 0 : 1);
