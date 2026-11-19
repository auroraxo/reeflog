#!/usr/bin/env node
// measure-status-two-queue-reason.mjs — does `webhook status` carry BOTH
// queues' standing reasons, each attributed to its own channel?
//
// THE QUESTION: the event queue and the retraction queue are independent
// (a retraction must still reach a consumer whose event queue is blocked),
// and `webhook push` reports them on separate fields (`error` and
// `reorg_error`) since v0.7.33. `webhook state` stores ONE standing reason
// per target: `webhook_state.last_error`. `deliver_events` writes it, then
// `deliver_reorgs` writes the SAME column — the second write overwrites the
// first. `webhook status` — the report a monitoring caller polls — prints
// that single column as `last_error` and has no retraction-channel field at
// all.
//
// Measured on the PUBLISHED v0.7.36 asset (the linux archive whose sha256
// equals the digest the Release API publishes): with BOTH queues blocked
// (event held at an owed payload, retraction refused with 404), `push`
// reported `error: "the payload is still owed — run \`hydrate\`"` AND
// `reorg_error: "refused with http 404"`, while `status` reported only
// `last_error: "refused with http 404"` — the event queue's repair
// (`hydrate`) was gone from the report, and nothing in the status row said
// which channel the 404 belonged to.
//
// Faces (baseline = the published asset, fixed = this tree's build):
//   D1 with both queues blocked, status reports the EVENT channel's reason
//      (defect: only the retraction's reason survives, the payload-owed
//      repair is absent from the report)
//   D2 the retraction channel's reason is attributable to its channel
//      (defect: no `reorg_error` field exists on the status row)
//   D3 the event channel's reason survives the retraction pass's write in
//      the journal (defect: `last_error` holds the retraction's reason,
//      the payload-owed reason was overwritten)
//
// Guards (green on BOTH sides — the normal cases must not move):
//   G1 only the event queue blocked: status reports its reason, exit 3
//   G2 only the retraction queue blocked: status reports that reason, exit 3
//   G3 a clean journal: no reasons, exit 0
//
//   node tools/measure-status-two-queue-reason.mjs --baseline <bin> --fixed <bin> [--out f.json]
import { spawn } from 'node:child_process';
import crypto from 'node:crypto';
import fs from 'node:fs';
import net from 'node:net';
import os from 'node:os';
import path from 'node:path';

const argv = process.argv.slice(2);
function opt(name, dflt) {
  const i = argv.indexOf(name);
  return i >= 0 && argv[i + 1] !== undefined ? argv[i + 1] : dflt;
}
const BASELINE = opt('--baseline', null);
const FIXED = opt('--fixed', null);
const OUT = opt('--out', `research/status-two-queue-reason-${new Date().toISOString().slice(0, 10)}.json`);
const LABEL_BASE = opt('--label-baseline', 'baseline');
const LABEL_FIXED = opt('--label-fixed', 'fixed');
const die = (m) => { console.error(m); process.exit(2); };
if (!BASELINE || !FIXED) die('usage: measure-status-two-queue-reason.mjs --baseline <bin> --fixed <bin> [--out f.json]');
const BASELINE_BIN = path.resolve(BASELINE);
const FIXED_BIN = path.resolve(FIXED);

const PROGRAM = 'whirLbMiicVdio4qvUfM5KAg6Ct8VwpYzGff3uctyCc';
const sha256 = (bin) => crypto.createHash('sha256').update(fs.readFileSync(bin)).digest('hex');
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

const run = (bin, args, cwd) =>
  new Promise((resolve) => {
    const child = spawn(bin, args, { cwd, timeout: 30000 });
    let stdout = '';
    let stderr = '';
    child.stdout.on('data', (c) => { stdout += c; });
    child.stderr.on('data', (c) => { stderr += c; });
    child.on('error', (e) => resolve({ status: null, stdout, stderr: String(e) }));
    child.on('close', (status) => resolve({ status, stdout, stderr }));
  });

const versionOf = (bin) => (spawn(bin, ['--version'], { timeout: 10000 }) ? '' : '');
const versionAsync = async (bin) => (await run(bin, ['--version'], os.tmpdir())).stdout.trim();

// A live sink: 200 for event bodies, 404 for retraction bodies (a consumer
// that refuses retractions), or always 200 (the clean scenario).
function recorder(mode) {
  const hits = [];
  const state = { mode };
  const server = net.createServer((s) => {
    let acc = Buffer.alloc(0);
    s.on('data', (d) => {
      acc = Buffer.concat([acc, d]);
      const p = acc.indexOf('\r\n\r\n');
      if (p < 0) return;
      const head = acc.subarray(0, p).toString();
      const cl = Number((head.match(/content-length:\s*(\d+)/i) || [])[1] || 0);
      if (acc.length < p + 4 + cl) return;
      const body = acc.subarray(p + 4, p + 4 + cl).toString();
      hits.push({ line: head.split('\r\n')[0], reorg: body.includes('"type":"reorg"') });
      const code = state.mode === 'clean'
        ? 200
        : (body.includes('"type":"reorg"') ? 404 : 200);
      const resp = code === 200
        ? 'HTTP/1.1 200 OK\r\ncontent-length: 2\r\nconnection: close\r\n\r\nok'
        : 'HTTP/1.1 404 Not Found\r\ncontent-length: 0\r\nconnection: close\r\n\r\n';
      s.write(resp);
      s.end();
    });
    s.on('error', () => {});
  });
  return new Promise((resolve) => {
    server.listen(0, '127.0.0.1', () => resolve({
      server,
      url: `http://127.0.0.1:${server.address().port}/hook`,
      hits: () => hits.slice(),
    }));
  });
}

// Journal shape per scenario. `owePayload` adds a row the journal still
// owes its content for (the event queue's hold), `retract` adds a retraction
// for the first event (the retraction queue's hold when the sink 404s it).
async function scenario(bin, label, { owePayload, retract, sinkMode }) {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), `rl-status-2q-${label}-`));
  const db = path.join(dir, 'j.db');
  const init = await run(bin, ['init', '--db', db, '--program', PROGRAM], dir);
  if (init.status !== 0) die(`init failed (${label}): ${init.stderr}`);

  const { DatabaseSync } = await import('node:sqlite');
  const d = new DatabaseSync(db);
  d.exec(`INSERT INTO events(signature, slot, err, logs, source, first_seen, seal_status, payload_state)
          VALUES ('M1_${'x'.repeat(44)}', 44000001, NULL, '["a"]', 'ws', 1700000000000, 'sealed', 'held')`);
  if (owePayload) {
    d.exec(`INSERT INTO events(signature, slot, err, logs, source, first_seen, seal_status, payload_state)
            VALUES ('M2_${'y'.repeat(44)}', 44000002, NULL, NULL, 'ws', 1700000000000, 'sealed', NULL)`);
  }
  if (retract) {
    d.exec(`UPDATE events SET seal_status='reverted' WHERE seq=1`);
    d.exec(`INSERT INTO reorgs(signature, slot, detected_at, status_source, tx_source, detail)
            VALUES ('M1_${'x'.repeat(44)}', 44000001, 1700000000001, 'status', 'tx', 'probe')`);
  }
  d.close();

  const sink = await recorder(sinkMode);
  const cfg = path.join(dir, 'cfg.json');
  fs.writeFileSync(cfg, JSON.stringify({ targets: [{ id: 't1', url: sink.url, secret: 's1', timeout_ms: 3000, dead_letter_after: 0 }] }));

  const push = await run(bin, ['webhook', 'push', '--db', db, '--config', cfg], dir);
  const status = await run(bin, ['webhook', 'status', '--db', db, '--config', cfg], dir);
  let presp = {};
  let sresp = {};
  try { presp = JSON.parse(push.stdout); } catch {}
  try { sresp = JSON.parse(status.stdout); } catch {}
  const row = sresp.targets?.[0] || {};
  const prow = presp.targets?.[0] || {};

  const { DatabaseSync: DS2 } = await import('node:sqlite');
  const jd = new DS2(db);
  const lastReorgCol = jd
    .prepare("SELECT COUNT(*) AS n FROM pragma_table_info('webhook_state') WHERE name='last_reorg_error'")
    .get().n;
  // The baseline journal has no `last_reorg_error` column — selecting it
  // there would fail; the absence itself is part of the measurement.
  const stateRows = lastReorgCol > 0
    ? jd.prepare('SELECT last_error, last_reorg_error FROM webhook_state').all()
    : jd.prepare('SELECT last_error FROM webhook_state').all();
  const journalLastError = stateRows[0]?.last_error ?? null;
  const journalLastReorgError = lastReorgCol > 0 ? (stateRows[0]?.last_reorg_error ?? null) : undefined;
  jd.close();

  sink.server.close();

  return {
    pushExit: push.status,
    pushError: prow.error ?? null,
    pushReorgError: prow.reorg_error ?? null,
    pushBlockedSeq: prow.blocked_seq ?? null,
    pushBlockedReorg: prow.blocked_reorg_id ?? null,
    statusExit: status.status,
    statusKeys: Object.keys(row).sort(),
    statusLastError: 'last_error' in row ? row.last_error : undefined,
    statusReorgError: 'reorg_error' in row ? row.reorg_error : undefined,
    statusPendingEvents: row.pending_events ?? null,
    statusPendingRetractions: row.pending_retractions ?? null,
    statusFailingTargets: sresp.failing_targets ?? null,
    statusStderr: status.stderr.trim().split('\n').filter(Boolean),
    journalLastError,
    journalLastReorgError,
    journalHasReorgErrorColumn: lastReorgCol > 0,
    sinkHits: sink.hits(),
  };
}

async function measureSide(bin, label) {
  // The heart of it: BOTH queues blocked at once, with different reasons.
  const both = await scenario(bin, `${label}-both`, { owePayload: true, retract: true, sinkMode: 'reject-reorg' });
  const onlyEvent = await scenario(bin, `${label}-evt`, { owePayload: true, retract: false, sinkMode: 'reject-reorg' });
  const onlyReorg = await scenario(bin, `${label}-rg`, { owePayload: false, retract: true, sinkMode: 'reject-reorg' });
  const clean = await scenario(bin, `${label}-ok`, { owePayload: false, retract: false, sinkMode: 'clean' });
  return {
    label,
    binVersion: await versionAsync(bin),
    binSha256: sha256(bin),
    both,
    onlyEvent,
    onlyReorg,
    clean,
  };
}

const PAYLOAD_REASON = 'the payload is still owed';
const REORG_REASON = 'refused with http 404';

const base = await measureSide(BASELINE_BIN, LABEL_BASE);
const fixed = await measureSide(FIXED_BIN, LABEL_FIXED);

const faces = [];
const face = (name, basePred, fixedPred, detail) => {
  faces.push({ name, baseline: !!basePred, fixed: !!fixedPred, detail: detail ?? null });
};

// D1: with BOTH queues blocked, the status row carries the EVENT channel's
// reason (payload debt), not only the retraction's.
const evtReasonInStatus = (s) =>
  typeof s.statusLastError === 'string' && s.statusLastError.includes(PAYLOAD_REASON);
face(
  'D1 both queues blocked: status reports the EVENT channel reason (payload owed)',
  evtReasonInStatus(base.both),
  evtReasonInStatus(fixed.both),
  {
    base: { last_error: base.both.statusLastError, push_error: base.both.pushError, push_reorg_error: base.both.pushReorgError },
    fixed: { last_error: fixed.both.statusLastError, push_error: fixed.both.pushError, push_reorg_error: fixed.both.pushReorgError },
  },
);

// D2: the retraction channel's reason is attributable to its own channel on
// the status row (a field that says which queue it belongs to).
face(
  'D2 the retraction reason is attributable to the retraction channel on the status row',
  base.both.statusReorgError !== undefined && typeof base.both.statusReorgError === 'string' && base.both.statusReorgError.includes(REORG_REASON),
  fixed.both.statusReorgError !== undefined && typeof fixed.both.statusReorgError === 'string' && fixed.both.statusReorgError.includes(REORG_REASON),
  {
    base: { has_reorg_error_field: base.both.statusReorgError !== undefined, value: base.both.statusReorgError ?? null },
    fixed: { has_reorg_error_field: fixed.both.statusReorgError !== undefined, value: fixed.both.statusReorgError ?? null },
  },
);

// D3: in the JOURNAL, the event channel's standing reason survives the
// retraction pass writing its own (one column per channel).
const evtReasonInJournal = (s) =>
  typeof s.journalLastError === 'string' && s.journalLastError.includes(PAYLOAD_REASON);
face(
  'D3 the event channel reason survives in the journal after the retraction pass',
  evtReasonInJournal(base.both),
  evtReasonInJournal(fixed.both),
  {
    base: { last_error: base.both.journalLastError, last_reorg_error: base.both.journalLastReorgError ?? null, column: base.both.journalHasReorgErrorColumn },
    fixed: { last_error: fixed.both.journalLastError, last_reorg_error: fixed.both.journalLastReorgError ?? null, column: fixed.both.journalHasReorgErrorColumn },
  },
);

// G1: only the event queue blocked — the reason is still there, exit 3.
face(
  'G1 only the event queue blocked: status names its reason and exits 3 (unchanged)',
  base.onlyEvent.statusLastError?.includes(PAYLOAD_REASON) === true && base.onlyEvent.statusExit === 3,
  fixed.onlyEvent.statusLastError?.includes(PAYLOAD_REASON) === true && fixed.onlyEvent.statusExit === 3,
  { base: { last_error: base.onlyEvent.statusLastError, exit: base.onlyEvent.statusExit }, fixed: { last_error: fixed.onlyEvent.statusLastError, exit: fixed.onlyEvent.statusExit } },
);

// G2: only the retraction queue blocked — the reason is reported somewhere
// on the row and the exit code holds.
const reorgReasonVisible = (s) =>
  (typeof s.statusReorgError === 'string' && s.statusReorgError.includes(REORG_REASON)) ||
  (typeof s.statusLastError === 'string' && s.statusLastError.includes(REORG_REASON));
face(
  'G2 only the retraction queue blocked: status reports that reason and exits 3 (unchanged)',
  reorgReasonVisible(base.onlyReorg) && base.onlyReorg.statusExit === 3,
  reorgReasonVisible(fixed.onlyReorg) && fixed.onlyReorg.statusExit === 3,
  { base: { last_error: base.onlyReorg.statusLastError, reorg_error: base.onlyReorg.statusReorgError ?? null, exit: base.onlyReorg.statusExit }, fixed: { last_error: fixed.onlyReorg.statusLastError, reorg_error: fixed.onlyReorg.statusReorgError ?? null, exit: fixed.onlyReorg.statusExit } },
);

// G3: a clean journal still reports no reasons and exits 0.
face(
  'G3 a clean journal reports no standing reasons and exits 0 (unchanged)',
  base.clean.statusLastError == null && base.clean.statusReorgError == null && base.clean.statusExit === 0,
  fixed.clean.statusLastError == null && (fixed.clean.statusReorgError == null || fixed.clean.statusReorgError === undefined) && fixed.clean.statusExit === 0,
  { base: { last_error: base.clean.statusLastError ?? null, exit: base.clean.statusExit }, fixed: { last_error: fixed.clean.statusLastError ?? null, reorg_error: fixed.clean.statusReorgError ?? null, exit: fixed.clean.statusExit } },
);

const defectFaces = faces.filter((f) => !f.baseline);
const fixedClean = faces.filter((f) => !f.fixed);
const verdict = defectFaces.length === 3 && fixedClean.length === 0
  ? 'status-two-queue-reason-one-field'
  : 'status-two-queue-reason-not-reproduced';

const report = {
  verdict,
  baselineDefectFaces: defectFaces.length,
  fixedDefectFaces: fixedClean.length,
  baseline: base,
  fixed,
  faces,
  at: new Date().toISOString(),
  dir: '<tmp>/',
};
fs.mkdirSync(path.dirname(OUT), { recursive: true });
fs.writeFileSync(OUT, JSON.stringify(report, null, 2));
for (const f of faces) {
  const tag = f.baseline && f.fixed ? 'BOTH-HOLD' : f.baseline ? 'BASELINE-ONLY' : f.fixed ? 'FIXED-ONLY' : 'NEITHER';
  console.log(`${tag} ${f.name} — ${JSON.stringify(f.detail)}`);
}
console.log(`verdict ${verdict} (baseline ${defectFaces.length} defect faces, fixed ${fixedClean.length}) -> ${OUT}`);
process.exit(0);
