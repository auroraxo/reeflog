// End-to-end probe of the webhook fan-out INSIDE the running daemon.
//
// The CLI probe (tools/e2e-webhook.mjs) proves the delivery contract. This
// one proves the product path: `reeflog start --webhook-config` delivers
// events that the daemon itself ingested over a live WebSocket, without any
// operator running a push command.
//
// The receiver is a separate process with an independent signature check.
// The daemon is killed mid-run on purpose — a mailbox that only survives a
// clean shutdown is not a mailbox — and the final state must show that the
// consumer still received the events the daemon had already ingested.
import { spawn } from 'node:child_process';
import fs from 'node:fs';
import path from 'node:path';
import http from 'node:http';
import crypto from 'node:crypto';
import { DatabaseSync } from 'node:sqlite';

const BIN = process.env.REEFLOG_BIN || path.resolve('./target/release/reeflog');
const PROGRAM = '675kPX9MHTjS2zt1qfr1NYHuzeLXfQM9H24wFSUt1Mp8'; // Raydium AMM v4
const DIR = '/tmp/reeflog-webhook-daemon-e2e';
const DB = path.join(DIR, 'reeflog.db');
const PORT = 18778;
const BASE = `http://127.0.0.1:${PORT}`;
const SECRET = 'whsec_daemon_e2e_secret_value';
const REPORT = path.resolve('./research/webhook-daemon-e2e-report.json');

const results = [];
const check = (name, pass, detail) => {
  results.push(pass);
  console.log(`${pass ? 'PASS' : 'FAIL'} ${name}${detail ? ` — ${detail}` : ''}`);
};

const received = [];
const server = http.createServer((req, res) => {
  const chunks = [];
  req.on('data', (c) => chunks.push(c));
  req.on('end', () => {
    const raw = Buffer.concat(chunks).toString('utf8');
    const sigHeader = req.headers['x-reeflog-signature'] || '';
    const expected = 'sha256=' + crypto.createHmac('sha256', SECRET).update(raw).digest('hex');
    let body = {};
    try { body = JSON.parse(raw); } catch { body = {}; }
    received.push({
      type: body.type ?? null,
      seq: body.seq ?? body.reorg_id ?? null,
      eventId: req.headers['x-reeflog-event-id'] || null,
      sigValid: sigHeader === expected,
      raw,
      at: Date.now(),
    });
    res.writeHead(200, { 'content-type': 'application/json' });
    res.end('{"ok":true}');
  });
});
await new Promise((r) => server.listen(PORT, '127.0.0.1', r));

fs.rmSync(DIR, { recursive: true, force: true });
fs.mkdirSync(DIR, { recursive: true });

const cfg = path.join(DIR, 'webhooks.json');
fs.writeFileSync(cfg, JSON.stringify({
  targets: [{ id: 'daemon', url: `${BASE}/hook`, secret: SECRET, timeout_ms: 3000 }],
}, null, 2));

await new Promise((resolve) => {
  const c = spawn(BIN, ['init', '--db', DB, '--program', PROGRAM], { stdio: 'ignore' });
  c.on('close', resolve);
});

// Start the daemon with the webhook pass enabled. The run is cut short with
// SIGKILL: the point is that a mailbox which only survives a clean shutdown
// is not a mailbox.
const daemon = spawn(BIN, [
  'start', '--db', DB,
  '--seconds', '75',
  '--webhook-config', cfg,
  '--webhook-every', '3',
  '--webhook-limit', '200',
  '--seal-every', '5',
  '--reorg-every', '10',
  '--repair-every', '15',
  '--repair-lookback', '600',
  '--repair-pages', '10',
], { stdio: ['ignore', 'pipe', 'pipe'] });
let daemonOut = '', daemonErr = '';
daemon.stdout.on('data', (d) => { daemonOut += d; });
daemon.stderr.on('data', (d) => { daemonErr += d; });

const killAt = Date.now() + 35000;
const poll = setInterval(() => {
  if (Date.now() > killAt && daemon.exitCode === null) {
    console.log('SIGKILL the daemon mid-run (no clean shutdown)');
    daemon.kill('SIGKILL');
  }
}, 500);

const done = new Promise((resolve) => daemon.on('close', (code, sig) => resolve({ code, sig })));
const res = await done;
clearInterval(poll);
console.log(`daemon exited code=${res.code} signal=${res.sig}`);

// --- what the consumer received vs what the journal holds -----------------
let journal = { events: 0, max_seq: 0, withPayload: 0 };
try {
  const db = new DatabaseSync(DB);
  const row = db.prepare('SELECT COUNT(*) events, COALESCE(MAX(seq),0) max_seq FROM events').get();
  journal.events = Number(row.events);
  journal.max_seq = Number(row.max_seq);
  journal.withPayload = Number(db.prepare('SELECT COUNT(*) c FROM events WHERE logs IS NOT NULL').get().c);
  db.close();
} catch (e) { console.error('journal read failed', e); }

const accepted = received.filter((r) => r.type === 'event');
const seqs = accepted.map((r) => r.seq);
const strictlyIncreasing = (a) => a.every((v, i) => i === 0 || v > a[i - 1]);

console.log(`journal events=${journal.events} max_seq=${journal.max_seq} with_payload=${journal.withPayload}`);
console.log(`receiver got ${accepted.length} events, seqs ${seqs[0]}..${seqs[seqs.length - 1]}`);

check('the_daemon_delivers_events_it_ingested_itself',
  accepted.length > 0 && journal.events > 0 && accepted.length <= journal.events,
  `delivered ${accepted.length} of ${journal.events} journal events`);

check('the_daemon_delivery_is_in_seq_order_without_repeats',
  strictlyIncreasing(seqs) && seqs.length === new Set(seqs).size,
  `${seqs.length} deliveries, distinct=${new Set(seqs).size}`);

check('every_daemon_delivery_is_signed_correctly',
  accepted.every((r) => r.sigValid),
  `${accepted.length} bodies verified with node:crypto`);

check('the_daemon_delivery_covers_the_journal_it_ingested',
  seqs.length > 0 && Math.max(...seqs) <= journal.max_seq &&
  Math.max(...seqs) >= Math.min(...seqs),
  `delivered seqs ${Math.min(...seqs)}..${Math.max(...seqs)} vs journal max ${journal.max_seq}`);

// The state after a SIGKILL must be honest: the cursor says what was
// delivered, and a follow-up push must not re-send any of it. Events the
// daemon ingested AFTER its last webhook pass were still owed — the kill
// does not make them delivered, and it must not make them lost either.
let deliveredSet = new Set();
{
  const db = new DatabaseSync(DB);
  const st = db.prepare('SELECT next_seq FROM webhook_state WHERE target=?').get('daemon');
  const rc = db.prepare('SELECT COUNT(*) c FROM webhook_receipts WHERE target=?').get('daemon');
  db.close();
  deliveredSet = new Set(seqs);
  check('the_cursor_survives_a_sigkill_and_matches_the_receipts',
    st && rc && st.next_seq === Math.max(...seqs) && rc.c === accepted.length,
    `cursor=${st?.next_seq} receipts=${rc?.c} delivered=${accepted.length}`);
}

const before = accepted.length;
const followUp = spawn(BIN, ['webhook', 'push', '--db', DB, '--config', cfg], { stdio: ['ignore', 'pipe', 'pipe'] });
let fOut = '';
followUp.stdout.on('data', (d) => { fOut += d; });
await new Promise((r) => followUp.on('close', r));
const fj = (() => { try { return JSON.parse(fOut); } catch { return {}; } })();
const afterRows = received.filter((r) => r.type === 'event');
const afterSeqs = afterRows.map((r) => r.seq);
const resumptions = afterSeqs.slice(before);
console.log(`follow-up push delivered ${fj.targets?.[0]?.delivered} (resumed remainder ${resumptions.length})`);
check('the_follow_up_resumes_only_the_owed_remainder',
  fj.targets?.[0]?.delivered === resumptions.length &&
  resumptions.every((s) => !deliveredSet.has(s)) &&
  afterSeqs.length === new Set(afterSeqs).size,
  `resumed ${resumptions.length} of ${journal.events - before} owed; re-sends=${resumptions.filter((s) => deliveredSet.has(s)).length}`);
check('after_the_follow_up_the_consumer_holds_the_whole_journal',
  new Set(afterSeqs).size === journal.events,
  `consumer holds ${new Set(afterSeqs).size} distinct of ${journal.events} journal events`);

const report = {
  generatedAt: new Date().toISOString(),
  program: PROGRAM,
  daemonExit: { code: res.code, signal: res.sig },
  journal,
  delivered: accepted.length,
  checks: results.length,
  passed: results.filter(Boolean).length,
  failed: results.filter((p) => !p).length,
  results: results.map((p) => p),
  daemonSummaryTail: daemonOut.slice(-2000),
  daemonStderrTail: daemonErr.slice(-2000),
};
fs.mkdirSync(path.dirname(REPORT), { recursive: true });
fs.writeFileSync(REPORT, JSON.stringify(report, null, 2));
console.log(`\n${report.passed}/${results.length} checks passed; report → ${REPORT}`);
server.close();
process.exit(results.every(Boolean) ? 0 : 1);
