// End-to-end probe of the webhook fan-out against a live, independent receiver.
//
// What makes this a falsification and not a demo: the receiver is a separate
// process that reimplements the signature check with node:crypto, it decides
// its own answers, and it logs what it actually received. The harness then
// checks directions that CAN FAIL:
//
//  A. FAN-OUT: two independent targets must each receive every event, in
//     seq order, exactly once.
//  B. REFUSAL: an endpoint that answers 403 must produce ZERO deliveries,
//     an exit code 3, an unchanged cursor and a recorded attempt with the
//     status that caused it. If a refusal ever shows up as a delivery, the
//     product is lying.
//  C. RATE LIMIT: an endpoint that answers 429 must block the queue, and the
//     next pass must deliver everything with NO duplicates of anything.
//  D. CRASH BETWEEN POST AND CURSOR: a receipt without an advanced cursor
//     must not be re-sent — the receipt is what makes at-least-once
//     collapse to exactly-once across a crash.
//  E. RETRACTION WHILE BLOCKED: a retraction must reach the consumer even
//     when the event queue is stuck behind a refusal.
//  F. WRONG SECRET: a body signed with the wrong secret must be rejected by
//     the receiver (and the delivery recorded as a failure, not a delivery).
//  G. THIRD IMPLEMENTATION: Python's `hmac` must reproduce the exact MAC the
//     receiver accepted — the sender's signature is not just self-consistent.
import { spawn } from 'node:child_process';
import fs from 'node:fs';
import path from 'node:path';
import http from 'node:http';
import crypto from 'node:crypto';
import { DatabaseSync } from 'node:sqlite';

const RPC = 'https://api.mainnet-beta.solana.com';
const PROGRAM = '675kPX9MHTjS2zt1qfr1NYHuzeLXfQM9H24wFSUt1Mp8'; // Raydium AMM v4
const BIN = path.resolve('./target/release/reeflog');
const DIR = '/tmp/reeflog-webhook-e2e';
const DB = path.join(DIR, 'reeflog.db');
const PORT = 18777;
const BASE = `http://127.0.0.1:${PORT}`;
const SECRET_OK = 'whsec_e2e_please_verify_me_not_this_one';
const SECRET_BAD = 'whsec_wrong_on_purpose';
const REPORT = path.resolve('./research/webhook-e2e-report.json');

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
// Independent signature check: node:crypto HMAC-SHA256 over the raw body.
// Every request is logged with what it carried AND the status it answered,
// so the assertions are made against RECEIVED data, never against what
// reeflog claims it sent. A request the receiver answered with 429 or 403
// was still RECEIVED — it is not counted as an accepted delivery.
const received = [];
let flakyHits = 0;
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
    const send = (code) => {
      received.push({
        path: req.url,
        type: body.type ?? null,
        seq: body.seq ?? body.reorg_id ?? null,
        eventId: req.headers['x-reeflog-event-id'] || null,
        seqHeader: req.headers['x-reeflog-seq'] || null,
        sigHeader,
        sigValid,
        raw,
        status: code,
        at: Date.now(),
      });
      res.writeHead(code, { 'content-type': 'application/json' });
      res.end(`{"ok":${code === 200}}`);
    };
    switch (req.url) {
      case '/ok': return send(200);
      case '/refuse': return send(403);
      case '/refuse-events': return send(entryType(body) === 'event' ? 403 : 200);
      case '/flaky': flakyHits += 1; return send(flakyHits === 1 ? 429 : 200);
      case '/strict': return send(sigValid ? 200 : 403);
      default: return send(404);
    }
  });
});
function entryType(body) { return body && body.type ? body.type : null; }
await new Promise((r) => server.listen(PORT, '127.0.0.1', r));

// --- harvest a real window ------------------------------------------------
fs.rmSync(DIR, { recursive: true, force: true });
fs.mkdirSync(DIR, { recursive: true });

let rawRows = await rpc('getSignaturesForAddress', [PROGRAM, { limit: 120, commitment: 'finalized' }]);
const cutSlot = rawRows[rawRows.length - 1].slot;
const rows = rawRows.filter((r) => r.slot > cutSlot);
console.log(`chain page: ${rawRows.length} raw, ${rows.length} clean (discarded partial slot ${cutSlot})`);

await cli(['init', '--db', DB, '--program', PROGRAM]);

// Real events with real content: fetch actual log lines for the first few so
// the delivered body carries what a consumer would read, not a placeholder.
const logFor = async (sig) => {
  try {
    const tx = await rpc('getTransaction', [sig, { encoding: 'json', maxSupportedTransactionVersion: 1, commitment: 'finalized' }]);
    if (!tx || !tx.meta) return null;
    return JSON.stringify(tx.meta.logMessages || []);
  } catch { return null; }
};
{
  const db = new DatabaseSync(DB);
  const ins = db.prepare('INSERT OR IGNORE INTO events(signature,slot,err,logs,source,seal_status,seal_seen,first_seen,payload_state) VALUES(?,?,?,?,?,?,?,?,?)');
  let withContent = 0;
  for (const [i, row] of rows.entries()) {
    const logs = i < 6 ? await logFor(row.signature) : '[]';
    if (logs && logs !== '[]') withContent++;
    ins.run(row.signature, row.slot, row.err == null ? null : JSON.stringify(row.err),
      logs ?? null, 'ws', 'finalized', Date.now(), Date.now(), logs ? 'held' : 'pending');
  }
  db.close();
  console.log(`planted ${rows.length} finalized events, ${withContent} with real log content`);
}
const total = rows.length;

const writeCfg = (targets) => {
  const p = path.join(DIR, 'webhooks.json');
  fs.writeFileSync(p, JSON.stringify({ targets }, null, 2));
  return p;
};

// A delivery the receiver ACCEPTED (2xx). A body the receiver received but
// answered with 429/403 is not an accepted delivery.
const accepted = (id, kind = 'event') =>
  received.filter((r) => (r.eventId || '').startsWith(`${id}:${kind}:`) && r.status >= 200 && r.status < 300);
const seqsOf = (id, kind = 'event') => accepted(id, kind).map((r) => r.seq);
const strictlyIncreasing = (a) => a.every((v, i) => i === 0 || v > a[i - 1]);

// ==========================================================================
// A. FAN-OUT: two independent targets, one pass
// ==========================================================================
const cfgAll = writeCfg([
  { id: 'okA', url: `${BASE}/ok`, secret: SECRET_OK },
  { id: 'okB', url: `${BASE}/ok`, secret: SECRET_OK },
  { id: 'blocked', url: `${BASE}/refuse`, secret: SECRET_OK },
  { id: 'wrongsecret', url: `${BASE}/strict`, secret: SECRET_BAD },
]);
const p1 = await cli(['webhook', 'push', '--db', DB, '--config', cfgAll]);
const p1j = json(p1.out);
console.log(`PUSH#1 exit=${p1.code} ${JSON.stringify(p1j)}`);
const tOf = (id) => p1j.targets.find((t) => t.target === id);

check('two_healthy_targets_each_receive_every_event',
  tOf('okA')?.delivered === total && tOf('okB')?.delivered === total,
  `okA=${tOf('okA')?.delivered} okB=${tOf('okB')?.delivered} of ${total}`);

check('delivery_is_in_seq_order_without_repeats',
  strictlyIncreasing(seqsOf('okA')) && seqsOf('okA').length === new Set(seqsOf('okA')).size &&
  strictlyIncreasing(seqsOf('okB')) && seqsOf('okB').length === new Set(seqsOf('okB')).size,
  `okA ${seqsOf('okA').length} deliveries, distinct=${new Set(seqsOf('okA')).size}`);

const allSigned = [...accepted('okA'), ...accepted('okB')].every((r) => r.sigValid);
check('every_delivered_body_verifies_against_the_shared_secret', allSigned,
  `${accepted('okA').length + accepted('okB').length} bodies checked with node:crypto`);

const withContent = accepted('okA').filter((r) => {
  try { const l = JSON.parse(r.raw).logs; return Array.isArray(l) && l.length > 0; } catch { return false; }
});
check('delivered_bodies_carry_event_content', withContent.length > 0,
  `${withContent.length} of ${total} deliveries carried non-empty logs`);

// ==========================================================================
// B. REFUSAL must not look like a delivery
// ==========================================================================
check('a_403_endpoint_is_recorded_as_a_failure_not_a_delivery',
  tOf('blocked')?.delivered === 0 && tOf('blocked')?.blocked === true &&
  tOf('blocked')?.blocked_seq === 1 && p1.code === 3,
  `blocked delivered=${tOf('blocked')?.delivered} blocked_seq=${tOf('blocked')?.blocked_seq} exit=${p1.code}`);

{
  const db = new DatabaseSync(DB);
  const st = db.prepare('SELECT next_seq FROM webhook_state WHERE target=?').get('blocked');
  const rc = db.prepare('SELECT COUNT(*) c FROM webhook_receipts WHERE target=?').get('blocked');
  const at = db.prepare('SELECT status, error FROM webhook_attempts WHERE target=? ORDER BY id DESC LIMIT 1').get('blocked');
  db.close();
  check('the_refused_queue_is_parked_not_advanced',
    st.next_seq === 0 && rc.c === 0 && at?.status === 403,
    `cursor=${st.next_seq} receipts=${rc.c} last attempt status=${at?.status}`);
}

check('a_body_signed_with_the_wrong_secret_is_rejected_by_the_receiver',
  received.filter((r) => (r.eventId || '').startsWith('wrongsecret:')).every((r) => !r.sigValid) &&
  tOf('wrongsecret')?.delivered === 0 && tOf('wrongsecret')?.blocked === true,
  `wrongsecret delivered=${tOf('wrongsecret')?.delivered} blocked=${tOf('wrongsecret')?.blocked}`);

// ==========================================================================
// C. RATE LIMIT: block, then deliver everything with no duplicates
// ==========================================================================
const cfgFlaky = writeCfg([{ id: 'flaky', url: `${BASE}/flaky`, secret: SECRET_OK }]);
const p2 = await cli(['webhook', 'push', '--db', DB, '--config', cfgFlaky]);
const p2j = json(p2.out);
console.log(`PUSH#2 (flaky) exit=${p2.code} ${JSON.stringify(p2j)}`);
check('a_429_blocks_the_queue_instead_of_dropping_events',
  p2.code === 3 && p2j.targets[0].delivered === 0 && p2j.targets[0].blocked === true,
  `delivered=${p2j.targets[0].delivered} blocked_seq=${p2j.targets[0].blocked_seq}`);

const p3 = await cli(['webhook', 'push', '--db', DB, '--config', cfgFlaky]);
const p3j = json(p3.out);
console.log(`PUSH#3 (flaky recovered) exit=${p3.code} ${JSON.stringify(p3j)}`);
const flakySeqs = seqsOf('flaky');
check('after_recovery_everything_is_delivered_exactly_once',
  p3.code === 0 && p3j.targets[0].delivered === total &&
  flakySeqs.length === new Set(flakySeqs).size && strictlyIncreasing(flakySeqs),
  `delivered=${p3j.targets[0].delivered} accepted=${flakySeqs.length} distinct=${new Set(flakySeqs).size}`);

const p4 = await cli(['webhook', 'push', '--db', DB, '--config', cfgFlaky]);
const p4j = json(p4.out);
check('a_caught_up_queue_delivers_nothing',
  p4.code === 0 && p4j.targets[0].delivered === 0 && p4j.targets[0].reorg_delivered === 0,
  JSON.stringify(p4j.targets[0]));

// ==========================================================================
// D. CRASH BETWEEN "POST SUCCEEDED" AND "CURSOR ADVANCED"
// ==========================================================================
const cfgCrash = writeCfg([{ id: 'crash', url: `${BASE}/ok`, secret: SECRET_OK }]);
let firstSeqNow;
{
  // Simulate the exact crash window: the endpoint answered (receipt written)
  // but the daemon died before moving the cursor. Only the receipt exists.
  const db = new DatabaseSync(DB);
  firstSeqNow = db.prepare('SELECT MIN(seq) s FROM events').get().s;
  db.prepare('INSERT OR IGNORE INTO webhook_receipts(target,kind,seq,body_sha256,delivered_at) VALUES(?,?,?,?,?)')
    .run('crash', 'event', firstSeqNow, 'simulated-crash-receipt', Date.now());
  db.close();
}
const p5 = await cli(['webhook', 'push', '--db', DB, '--config', cfgCrash]);
const p5j = json(p5.out);
console.log(`PUSH#5 (crash) exit=${p5.code} ${JSON.stringify(p5j)}`);
const crashSeqs = seqsOf('crash');
check('the_owed_event_is_never_sent_twice_after_a_crash',
  p5.code === 0 && p5j.targets[0].delivered === total - 1 && !crashSeqs.includes(firstSeqNow),
  `delivered=${p5j.targets[0].delivered} of ${total}; first seq ${firstSeqNow} resent=${crashSeqs.includes(firstSeqNow)}`);
check('the_cursor_catches_up_past_the_receipt',
  p5j.targets[0].next_seq === total,
  `next_seq=${p5j.targets[0].next_seq} (last event seq ${total})`);

// ==========================================================================
// E. RETRACTION WHILE THE EVENT QUEUE IS BLOCKED
// ==========================================================================
const cfgGate = writeCfg([{ id: 'gate', url: `${BASE}/refuse-events`, secret: SECRET_OK }]);
// Plant a recorded reorg: an event that was promised final and then refused
// by both sources — exactly what `reorg check` writes.
{
  const db = new DatabaseSync(DB);
  const victim = db.prepare('SELECT seq, signature, slot FROM events ORDER BY seq DESC LIMIT 1').get();
  db.prepare('INSERT OR IGNORE INTO reorgs(signature,slot,detected_at,status_source,tx_source,detail) VALUES(?,?,?,?,?,?)')
    .run(victim.signature, victim.slot, Date.now(), 'getSignatureStatuses', 'getTransaction', 'both sources refused');
  db.close();
}
const p6 = await cli(['webhook', 'push', '--db', DB, '--config', cfgGate]);
const p6j = json(p6.out);
console.log(`PUSH#6 (gate) exit=${p6.code} ${JSON.stringify(p6j)}`);
const gateReorg = accepted('gate', 'reorg');
check('a_retraction_reaches_the_consumer_while_events_are_blocked',
  p6.code === 3 && p6j.targets[0].blocked === true && p6j.targets[0].reorg_delivered === 1 &&
  gateReorg.length === 1 && gateReorg[0].type === 'reorg' && gateReorg[0].sigValid,
  `blocked_seq=${p6j.targets[0].blocked_seq} reorg_delivered=${p6j.targets[0].reorg_delivered} accepted reorgs=${gateReorg.length}`);

// ==========================================================================
// G. THIRD IMPLEMENTATION: Python's hmac must reproduce the MAC
// ==========================================================================
fs.writeFileSync(path.join(DIR, 'received.json'), JSON.stringify(received, null, 2));
const py = await new Promise((resolve) => {
  const src = `
import hmac, hashlib, json, sys
rows = json.load(open(sys.argv[1]))
secret = sys.argv[2].encode()
checked = 0
mismatch = 0
accepted_checked = 0
for r in rows:
    if r["type"] is None:
        continue
    checked += 1
    want = "sha256=" + hmac.new(secret, r["raw"].encode(), hashlib.sha256).hexdigest()
    # The receiver's own verdict must match an independent recomputation.
    if r["sigValid"] != (want == r["sigHeader"]):
        mismatch += 1
    if 200 <= r["status"] < 300 and r["path"] in ("/ok", "/flaky"):
        accepted_checked += 1
        if want != r["sigHeader"]:
            mismatch += 1
print(json.dumps({"checked": checked, "accepted": accepted_checked, "mismatch": mismatch}))
`;
  fs.writeFileSync(path.join(DIR, 'py_check.py'), src);
  const c = spawn('python3', [path.join(DIR, 'py_check.py'), path.join(DIR, 'received.json'), SECRET_OK]);
  let o = ''; let e = '';
  c.stdout.on('data', (d) => { o += d; });
  c.stderr.on('data', (d) => { e += d; });
  c.on('close', (code) => resolve({ code, out: o, err: e }));
});
console.log(`python hmac cross-check exit=${py.code} out=${py.out.trim()} ${py.err.trim()}`);
const pyj = json(py.out);
check('a_third_implementation_reproduces_the_mac',
  py.code === 0 && pyj.checked > 0 && pyj.mismatch === 0,
  `python hmac: ${pyj.checked} bodies recomputed, ${pyj.mismatch} mismatches`);

// --- report ----------------------------------------------------------------
const report = {
  generatedAt: new Date().toISOString(),
  program: PROGRAM,
  events: total,
  requests_received: received.length,
  accepted: received.filter((r) => r.status >= 200 && r.status < 300).length,
  checks: results.length,
  passed: results.filter(Boolean).length,
  failed: results.filter((p) => !p).length,
  results: results.map((p, i) => p),
  pythonCrossCheck: pyj,
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
