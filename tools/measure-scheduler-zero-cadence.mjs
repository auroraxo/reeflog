#!/usr/bin/env node
// measure-scheduler-zero-cadence.mjs — does a cadence of 0 mean OFF, or
// does the scheduler turn it into a 1 Hz pass?
//
// THE QUESTION: `reeflog start` takes four cadence flags (`--seal-every`,
// `--reorg-every`, `--repair-every`, `--webhook-every`) whose default values
// are 15 s, 60 s, 30 s and 5 s. The scheduler seeds its next-due instants as
// `now + Duration::from_secs(cadence.max(1))` — and re-seeds the same way
// after every pass. A cadence of 0 therefore collapses to 1 second: the pass
// runs every tick instead of never. The same run's SHUTDOWN gate reads the
// same value as "never" (`if repair_every_s > 0 { … } else { skipped }`), so
// ONE flag value carries TWO contradictory meanings inside one run — and the
// expensive one is the 1 Hz one.
//
// The zero is legal by design everywhere else in this product's flag
// domain: `flags.rs` keeps 0 legal in NUMERIC precisely BECAUSE "several
// flags document it as 'disable this pass'" — `--payload-limit 0` and
// `--repair-pages 0` are the documented spellings of "off" in the very same
// command. The scheduler is the one layer that re-read a documented zero
// into a busy schedule.
//
// Measured with the REAL binaries against a REAL local chain sink that
// counts every RPC call the daemon makes and every webhook body it
// delivers. One port speaks the three roles the run needs: a WebSocket
// upgrade for the ingest socket (held open so the run ends at its own
// deadline), JSON-RPC over separate HTTP connections for the periodic
// passes, and plain POSTs for webhook deliveries.
//
// Faces (each a predicate on BOTH trees):
//   D1 `--seal-every 0` runs the seal pass — the shutdown seal costs exactly
//      ONE getSignatureStatuses call on both trees (it is unconditional),
//      so a second call means the schedule ran.
//   D2 `--reorg-every 0` runs the reorg pass (>= 1 scheduled pass in the
//      summary) AND claims `final_reorg: {skipped: true}` at shutdown.
//   D3 `--repair-every 0` runs the repair pass (>= 1) AND claims
//      `final_repair: {skipped: true}` at shutdown.
//   D4 `--webhook-every 0` with a configured target POSTs signed bodies
//      (>= 1) — a delivery cadence of 0 delivered.
//   D5 with ALL four cadences at 0, the summary shows the two meanings at
//      once: scheduled passes ran (reorg_passes >= 1 and repair_passes >= 1)
//      while both final gates say skipped.
//
// Guards (green on BOTH trees — the honest forms must not move):
//   G1 an effectively-never cadence (3600) within a short run schedules
//      NOTHING: zero seal passes, zero webhook posts, and the final gates
//      still read the never-due value as "run once at shutdown" (a positive
//      cadence is a promise to run at the end, not a disable switch);
//   G2 a tight POSITIVE cadence (1 s) still runs — several passes, and the
//      final gates do not claim skipped;
//   G3 an unschedulable cadence value is still refused by the flag domain
//      (the zero rule narrows one corner, it must not widen another).
//
//   node tools/measure-scheduler-zero-cadence.mjs --baseline <bin> --fixed <bin> [--out f.json]
import { spawn } from 'node:child_process';
import crypto from 'node:crypto';
import fs from 'node:fs';
import net from 'node:net';
import os from 'node:os';
import path from 'node:path';
import { DatabaseSync } from 'node:sqlite';

const argv = process.argv.slice(2);
function opt(name, dflt) {
  const i = argv.indexOf(name);
  return i >= 0 && argv[i + 1] !== undefined ? argv[i + 1] : dflt;
}
const BASELINE = opt('--baseline', null);
const FIXED = opt('--fixed', null);
const OUT = opt('--out', `research/scheduler-zero-cadence-${new Date().toISOString().slice(0, 10)}.json`);
const LABEL_BASE = opt('--label-baseline', 'baseline');
const LABEL_FIXED = opt('--label-fixed', 'fixed');
const die = (m) => { console.error(m); process.exit(2); };
if (!BASELINE || !FIXED) die('usage: measure-scheduler-zero-cadence.mjs --baseline <bin> --fixed <bin> [--out f.json]');
const BASELINE_BIN = path.resolve(BASELINE);
const FIXED_BIN = path.resolve(FIXED);

const PROGRAM = 'whirLbMiicVdio4qvUfM5KAg6Ct8VwpYzGff3uctyCc';
const GUID = '258EAFA5-E914-47DA-95CA-C5AB0DC85B11';
const sha256 = (bin) => crypto.createHash('sha256').update(fs.readFileSync(bin)).digest('hex');
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

if (!fs.existsSync(BASELINE_BIN)) die(`no binary at ${BASELINE_BIN}`);
if (!fs.existsSync(FIXED_BIN)) die(`no binary at ${FIXED_BIN}`);

const run = (bin, args, cwd) =>
  new Promise((resolve) => {
    const child = spawn(bin, args, { cwd, timeout: 60000 });
    let stdout = '';
    let stderr = '';
    child.stdout.on('data', (c) => { stdout += c; });
    child.stderr.on('data', (c) => { stderr += c; });
    child.on('error', (e) => resolve({ status: null, stdout, stderr: String(e) }));
    child.on('close', (status) => resolve({ status, stdout, stderr }));
  });

const versionAsync = async (bin) => (await run(bin, ['--version'], os.tmpdir())).stdout.trim();

// The chain sink: one port, three roles. `serveFinalized` decides whether
// getSignatureStatuses answers with a finalized row (so the seal pass
// promotes the row it is asked about) or with nulls (so the row stays
// unsealed and every pass keeps finding work — the loudest shape for D1).
async function chainSink({ serveFinalized = false } = {}) {
  const count = { rpc: {}, posts: 0 };
  const server = net.createServer((s) => {
    let acc = Buffer.alloc(0);
    s.on('error', () => {});
    s.on('data', (d) => {
      acc = Buffer.concat([acc, d]);
      const p = acc.indexOf('\r\n\r\n');
      if (p < 0) return;
      const head = acc.subarray(0, p).toString();
      if (/upgrade:\s*websocket/i.test(head)) {
        const key = (head.match(/sec-websocket-key:\s*(.+)/i) || [])[1]?.trim();
        if (!key) { s.destroy(); return; }
        const accept = crypto.createHash('sha1').update(key + GUID).digest('base64');
        s.write(`HTTP/1.1 101 Switching Protocols\r\nupgrade: websocket\r\nconnection: upgrade\r\nsec-websocket-accept: ${accept}\r\n\r\n`);
        acc = Buffer.alloc(0);
        return;
      }
      const cl = Number((head.match(/content-length:\s*(\d+)/i) || [])[1] || 0);
      if (acc.length < p + 4 + cl) return;
      const body = acc.subarray(p + 4, p + 4 + cl).toString();
      acc = Buffer.alloc(0);
      let parsed = null;
      try { parsed = JSON.parse(body); } catch { /* not json */ }
      const method = parsed && typeof parsed.method === 'string' ? parsed.method : null;
      if (!method) {
        // A webhook delivery: a signed body with no JSON-RPC envelope.
        count.posts += 1;
        s.write('HTTP/1.1 200 OK\r\ncontent-length: 2\r\nconnection: close\r\n\r\nok');
        s.end();
        return;
      }
      count.rpc[method] = (count.rpc[method] || 0) + 1;
      let result = [];
      if (method === 'getSignaturesForAddress') result = [];
      if (method === 'getSignatureStatuses') {
        const n = Array.isArray(parsed.params?.[0]) ? parsed.params[0].length : 0;
        result = { value: Array.from({ length: n }, () =>
          (serveFinalized ? { confirmationStatus: 'finalized', err: null } : null)) };
      }
      if (method === 'getTransaction') result = null;
      const payload = JSON.stringify({ jsonrpc: '2.0', id: 1, result });
      s.write(`HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: ${Buffer.byteLength(payload)}\r\nconnection: close\r\n\r\n${payload}`);
      s.end();
    });
  });
  await new Promise((r) => server.listen(0, '127.0.0.1', r));
  return { server, port: server.address().port, count, url: (p) => `http://127.0.0.1:${p}` };
}

function plantedDir(tag) {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), `rl-zero-cad-${tag}-`));
  const db = path.join(dir, 'j.db');
  return { dir, db };
}

async function plant(db) {
  const d = new DatabaseSync(db);
  const now = Date.now();
  d.exec(`INSERT INTO events(signature, slot, err, logs, source, first_seen, seal_status, payload_state)
          VALUES ('Sealed1', 100, NULL, '["a"]', 'ws', ${now}, 'finalized', 'held')`);
  d.exec(`INSERT INTO events(signature, slot, err, logs, source, first_seen, seal_status, payload_state)
          VALUES ('Unsealed1', 101, NULL, '["b"]', 'ws', ${now}, NULL, 'held')`);
  d.close();
}

// One scenario: a short run against a fresh sink with the cadences the
// caller names. `expectSealWork` decides whether getSignatureStatuses
// answers nulls (D1 shape).
async function scenario(bin, label, cadences, { serveFinalized = false, webhook = false, seconds = '6' } = {}) {
  const { dir, db } = plantedDir(`${label}-${Math.random().toString(36).slice(2, 7)}`);
  await run(bin, ['init', '--db', db, '--program', PROGRAM], dir);
  await plant(db);
  const sink = await chainSink({ serveFinalized });
  let cfg = null;
  if (webhook) {
    cfg = path.join(dir, 'wh.json');
    fs.writeFileSync(cfg, JSON.stringify({ targets: [{ id: 't1', url: `http://127.0.0.1:${sink.port}/hook`, secret: 'whsec_measure_zero', timeout_ms: 3000, dead_letter_after: 0 }] }));
  }
  const args = [
    'start', '--db', db, '--program', PROGRAM,
    '--seconds', seconds,
    '--ws', `ws://127.0.0.1:${sink.port}`,
    '--rpc', `http://127.0.0.1:${sink.port}`,
    ...cadences,
    ...(cfg ? ['--webhook-config', cfg] : []),
  ];
  const out = await run(bin, args, dir);
  let summary = {};
  try { summary = JSON.parse(out.stdout); } catch { /* no summary */ }
  sink.server.close();
  return {
    exit: out.status,
    summary,
    rpc: sink.count.rpc,
    posts: sink.count.posts,
    stderr: out.stderr.trim().split('\n').filter(Boolean).slice(0, 3),
  };
}

const zero = ['--seal-every', '0', '--reorg-every', '0', '--repair-every', '0', '--webhook-every', '0'];
const never = ['--seal-every', '3600', '--reorg-every', '3600', '--repair-every', '3600', '--webhook-every', '3600'];
const tight = ['--seal-every', '1', '--reorg-every', '1', '--repair-every', '1', '--webhook-every', '1'];

async function measureSide(bin, label) {
  const d1 = await scenario(bin, `${label}-seal0`, ['--seal-every', '0', '--reorg-every', '3600', '--repair-every', '3600', '--webhook-every', '3600'], { serveFinalized: false });
  const d2 = await scenario(bin, `${label}-reorg0`, ['--seal-every', '3600', '--reorg-every', '0', '--repair-every', '3600', '--webhook-every', '3600'], { serveFinalized: true });
  const d3 = await scenario(bin, `${label}-repair0`, ['--seal-every', '3600', '--reorg-every', '3600', '--repair-every', '0', '--webhook-every', '3600'], { serveFinalized: false });
  const d4 = await scenario(bin, `${label}-webhook0`, ['--seal-every', '3600', '--reorg-every', '3600', '--repair-every', '3600', '--webhook-every', '0'], { serveFinalized: true, webhook: true });
  const d5 = await scenario(bin, `${label}-all0`, zero, { serveFinalized: false });
  const g1 = await scenario(bin, `${label}-never`, never, { serveFinalized: true, webhook: true });
  const g2 = await scenario(bin, `${label}-tight`, tight, { serveFinalized: true });
  return { label, d1, d2, d3, d4, d5, g1, g2 };
}

const base = await measureSide(BASELINE_BIN, LABEL_BASE);
const fixed = await measureSide(FIXED_BIN, LABEL_FIXED);

const faces = [];
// Every face states the FIXED behaviour: `baseline` true means the fixed
// behaviour was ALREADY there on the baseline (so that face is NOT a
// defect), `fixed` true means it holds on this tree. A defect face is one
// the baseline does NOT show (baseline false) and this tree does
// (fixed true).
const face = (name, basePred, fixedPred, detail) => {
  faces.push({ name, baseline: !!basePred, fixed: !!fixedPred, detail: detail ?? null });
};

// D1: `--seal-every 0` schedules NO seal pass. The summary's
// `seal_passes` counter counts SCHEDULED seal passes only (the shutdown
// seal is unconditional and is not counted there), so it is the
// discriminator: the raw getSignatureStatuses count cannot be, because
// the shutdown reorg check calls it too.
const sealDisabled = (s) => (s.summary.seal_passes || 0) === 0;
face('D1 --seal-every 0 schedules no seal pass', sealDisabled(base.d1), sealDisabled(fixed.d1), {
  base: { rpc: base.d1.rpc, seal_passes: base.d1.summary.seal_passes },
  fixed: { rpc: fixed.d1.rpc, seal_passes: fixed.d1.summary.seal_passes },
});

// D2: `--reorg-every 0` schedules no reorg pass, and the SAME run's final
// gate reads the same zero as "never" — one value, one meaning.
const reorgDisabled = (s) =>
  (s.summary.reorg_passes || 0) === 0 && s.summary.final_reorg?.skipped === true;
face('D2 --reorg-every 0 schedules no reorg pass (and the final gate says never)', reorgDisabled(base.d2), reorgDisabled(fixed.d2), {
  base: { reorg_passes: base.d2.summary.reorg_passes, final_reorg: base.d2.summary.final_reorg, rpc: base.d2.rpc },
  fixed: { reorg_passes: fixed.d2.summary.reorg_passes, final_reorg: fixed.d2.summary.final_reorg, rpc: fixed.d2.rpc },
});

// D3: `--repair-every 0` schedules no repair pass, and the final gate
// reads the same zero as never — one meaning on both sides.
const repairDisabled = (s) =>
  (s.summary.repair_passes || 0) === 0 && s.summary.final_repair?.skipped === true;
face('D3 --repair-every 0 schedules no repair pass (and the final gate says never)', repairDisabled(base.d3), repairDisabled(fixed.d3), {
  base: { repair_passes: base.d3.summary.repair_passes, final_repair: base.d3.summary.final_repair, rpc: base.d3.rpc },
  fixed: { repair_passes: fixed.d3.summary.repair_passes, final_repair: fixed.d3.summary.final_repair, rpc: fixed.d3.rpc },
});

// D4: `--webhook-every 0` delivers no signed bodies.
const webhookSilent = (s) => s.posts === 0;
face('D4 --webhook-every 0 delivers no signed bodies', webhookSilent(base.d4), webhookSilent(fixed.d4), {
  base: { posts: base.d4.posts, webhook_passes: base.d4.summary.webhook?.passes },
  fixed: { posts: fixed.d4.posts, webhook_passes: fixed.d4.summary.webhook?.passes },
});

// D5: with every cadence at 0, ONE value means ONE thing: nothing
// scheduled, both gates "never". The pre-fix summary shows both at once.
const allZeroOneMeaning = (s) =>
  (s.summary.reorg_passes || 0) === 0
  && (s.summary.repair_passes || 0) === 0
  && s.summary.final_reorg?.skipped === true
  && s.summary.final_repair?.skipped === true;
face('D5 with every cadence 0, ONE value means ONE thing (nothing scheduled, both gates never)', allZeroOneMeaning(base.d5), allZeroOneMeaning(fixed.d5), {
  base: { reorg_passes: base.d5.summary.reorg_passes, repair_passes: base.d5.summary.repair_passes, final_reorg: base.d5.summary.final_reorg, final_repair: base.d5.summary.final_repair },
  fixed: { reorg_passes: fixed.d5.summary.reorg_passes, repair_passes: fixed.d5.summary.repair_passes, final_reorg: fixed.d5.summary.final_reorg, final_repair: fixed.d5.summary.final_repair },
});

// G1: an effectively-never cadence must schedule nothing, and a POSITIVE
// cadence keeps its "run once at shutdown" meaning (the final gates are not
// a disable switch for a never-due value).
const neverSchedulesNothing = (s) =>
  (s.summary.seal_passes || 0) === 0
  && s.posts === 0
  && s.summary.final_reorg?.skipped !== true
  && s.summary.final_repair?.skipped !== true;
face('G1 a never-due cadence schedules nothing and the final gates keep their meaning (unchanged)', neverSchedulesNothing(base.g1), neverSchedulesNothing(fixed.g1), {
  base: { seal_passes: base.g1.summary.seal_passes, posts: base.g1.posts, final_reorg: base.g1.summary.final_reorg },
  fixed: { seal_passes: fixed.g1.summary.seal_passes, posts: fixed.g1.posts, final_reorg: fixed.g1.summary.final_reorg },
});

// G2: a tight POSITIVE cadence still runs — several passes per side.
const tightRuns = (s) => (s.summary.seal_passes || 0) >= 1 && (s.summary.reorg_passes || 0) >= 1 && s.summary.final_reorg?.skipped !== true;
face('G2 a tight positive cadence still runs (unchanged)', tightRuns(base.g2), tightRuns(fixed.g2), {
  base: { seal_passes: base.g2.summary.seal_passes, reorg_passes: base.g2.summary.reorg_passes, final_reorg: base.g2.summary.final_reorg },
  fixed: { seal_passes: fixed.g2.summary.seal_passes, reorg_passes: fixed.g2.summary.reorg_passes, final_reorg: fixed.g2.summary.final_reorg },
});

const defectFaces = faces.filter((f) => !f.baseline);
const fixedClean = faces.filter((f) => !f.fixed);
const verdict = defectFaces.length === 5 && fixedClean.length === 0
  ? 'scheduler-zero-cadence-busy'
  : 'scheduler-zero-cadence-not-reproduced';

const report = {
  verdict,
  baselineDefectFaces: defectFaces.length,
  fixedDefectFaces: fixedClean.length,
  baseline: { label: LABEL_BASE, binVersion: await versionAsync(BASELINE_BIN), binSha256: sha256(BASELINE_BIN), ...base },
  fixed: { label: LABEL_FIXED, binVersion: await versionAsync(FIXED_BIN), binSha256: sha256(FIXED_BIN), ...fixed },
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
