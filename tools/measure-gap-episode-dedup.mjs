#!/usr/bin/env node
// measure-gap-episode-dedup.mjs — measure ONE disconnect episode against the
// gap ledger (published-carrier vs fixed-tree, same byte-identical probe).
//
// THE QUESTION: `gaps_open` is the number a consumer reads as "this journal
// owes me history". The daemon opens a gap row per failed connect/subscribe
// and closes ONLY the last row (`open_gap.take()`). One outage that takes N
// connect attempts to recover leaves N-1 open rows behind — a healed journal
// reporting `gaps_open: N-1` is the same permanent false alarm v0.7.27
// removed from the hole rows.
//
// The faces (aligned with tests/gap_episode_dedup.rs — an outage is LOUD
// while the endpoint is down, and a heal turns the episode into history):
//   F1  one episode = exactly ONE open connect_fail row   (defect: N rows)
//   F2  that row carries every attempt's detail           (defect: 1 attempt)
//   F3  status gaps_open == 1 while the endpoint is down  (defect: N)
//   F4  after the heal: ZERO open rows                    (defect: N-1)
//   F5  after the heal: the episode stays as history (1 resolved row)
//   F6  after the heal: status gaps_open == 0             (defect: N-1)
//   F7  a second episode adds its own row; the heal resolves it too
//       (resolved >= 2, open == 0, gaps_open == 0)
//
// The probe drives the REAL daemon: first against a dead TCP port (refused
// connects), then against a local sink that performs a REAL WebSocket
// upgrade and serves the signature walk over plain HTTP — the daemon only
// resumes after a real upgrade, so the heal is honest.
//
// Usage:
//   node tools/measure-gap-episode-dedup.mjs --bin ./target/release/reeflog
//   node tools/measure-gap-episode-dedup.mjs --bin <published> --label prefix
import { spawnSync, spawn } from 'node:child_process';
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
const BIN = opt('--bin', path.join(process.cwd(), 'target/release/reeflog'));
const LABEL = opt('--label', 'run');
const ATTEMPTS = Number(opt('--attempts', '6')); // connect tries per episode

const PROGRAM = 'whirLbMiicVdio4qvUfM5KAg6Ct8VwpYzGff3uctyCc';
const DIR = fs.mkdtempSync(path.join(os.tmpdir(), `reeflog-gap-${LABEL}-`));
const DB = path.join(DIR, 'reeflog.db');

const die = (m) => { console.error(m); process.exit(2); };
if (!fs.existsSync(BIN)) die(`binary not found: ${BIN}`);
const BIN_SHA256 = crypto.createHash('sha256').update(fs.readFileSync(BIN)).digest('hex');

function sqlite3(db, sql) {
  const r = spawnSync('sqlite3', [db, sql], { encoding: 'utf8' });
  if (r.status !== 0) throw new Error(`sqlite3 failed: ${r.stderr}`);
  return r.stdout.trim();
}

const run = (args, timeoutMs) => spawnSync(BIN, args, { timeout: timeoutMs, encoding: 'utf8' });

// ---- empty journal: no boundary, no restart gap — the ONLY gap rows this
// probe can produce come from the connect faces ----
let r = run(['init', '--db', DB, '--program', PROGRAM], 20000);
if (r.status !== 0) die(`init failed: ${r.stderr || r.stdout}`);

const counts = () => ({
  open: Number(sqlite3(DB, 'SELECT COUNT(*) FROM gaps WHERE resolved_at IS NULL;')),
  total: Number(sqlite3(DB, 'SELECT COUNT(*) FROM gaps;')),
  resolved: Number(sqlite3(DB, 'SELECT COUNT(*) FROM gaps WHERE resolved_at IS NOT NULL;')),
  byKind: sqlite3(DB, 'SELECT kind || \'=\' || COUNT(*) FROM gaps GROUP BY kind;'),
  openByKind: sqlite3(DB, 'SELECT kind || \'=\' || COUNT(*) FROM gaps WHERE resolved_at IS NULL GROUP BY kind;'),
  // connect attempts on the ledger: one row per attempt pre-fix; post-fix the
  // retries ride on the ONE episode row as "; ws connect" detail suffixes.
  attempts: Number(sqlite3(DB,
    'SELECT COALESCE(SUM(1 + (LENGTH(detail) - LENGTH(REPLACE(detail, \'; ws connect\', \'\'))) / 12), 0) FROM gaps;')),
});

const status = () => {
  const s = run(['status', '--db', DB], 20000);
  try { return JSON.parse(s.stdout); } catch { return { _raw: s.stdout, _err: s.stderr }; }
};

const results = [];
const face = (name, pass, detail) => {
  results.push({ name, pass: !!pass, detail: detail ?? null });
  console.log(`${pass ? 'PASS' : 'DEFECT'} ${name}${detail !== undefined && detail !== null ? ` — ${JSON.stringify(detail)}` : ''}`);
};

// dead endpoint: TCP RST immediately (nothing listens) — connect_fail face
const DEAD_WS = 'ws://127.0.0.1:9';
const DEAD_RPC = 'http://127.0.0.1:9';

// ---- the live sink: real RFC 6455 upgrade, signature walk over plain HTTP.
// The upgraded socket is held open past the run window so the run exits at
// its own deadline, never through a disconnect it would journal. ----
function startSink(signatures) {
  const GUID = '258EAFA5-E914-47DA-95CA-C5AB0DC85B11';
  const server = net.createServer((s) => {
    const chunks = [];
    let acc = Buffer.alloc(0);
    s.on('data', (d) => {
      acc = Buffer.concat([acc, d]);
      const p = acc.indexOf('\r\n\r\n');
      if (p < 0) return;
      const head = acc.subarray(0, p).toString();
      const lower = head.toLowerCase();
      if (/^upgrade:\s*websocket/m.test(lower)) {
        const key = (head.match(/sec-websocket-key:\s*(.+)/i) || [])[1]?.trim();
        if (!key) { s.destroy(); return; }
        const accept = crypto.createHash('sha1').update(key + GUID).digest('base64');
        s.write(`HTTP/1.1 101 Switching Protocols\r\nupgrade: websocket\r\nconnection: upgrade\r\nsec-websocket-accept: ${accept}\r\n\r\n`);
        acc = Buffer.alloc(0); // hold the socket; frames are not our business
        return;
      }
      const cl = Number((head.match(/content-length:\s*(\d+)/i) || [])[1] || 0);
      if (acc.length < p + 4 + cl) return;
      const body = acc.subarray(p + 4, p + 4 + cl).toString();
      acc = Buffer.alloc(0);
      let resp = { jsonrpc: '2.0', id: 1, result: [] };
      try {
        const v = JSON.parse(body);
        if (v.method === 'getSignaturesForAddress') {
          resp.result = signatures.map((sig) => ({ signature: sig, slot: 1, err: null }));
        }
      } catch { s.destroy(); return; }
      const payload = JSON.stringify(resp);
      s.write(`HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: ${Buffer.byteLength(payload)}\r\nconnection: close\r\n\r\n${payload}`);
      s.end();
    });
    s.on('error', () => {});
  });
  return new Promise((resolve) => server.listen(0, '127.0.0.1', () => resolve({ server, port: server.address().port })));
}

const episode = () => {
  // The daemon runs against the DEAD endpoint; we SIGKILL it once the
  // ledger holds ATTEMPTS connect attempts (rows pre-fix, detail post-fix).
  const child = spawn(BIN, [
    'start', '--db', DB, '--program', PROGRAM,
    '--seconds', '3600', // we kill it ourselves after the attempts are recorded
    '--rpc', DEAD_RPC, '--ws', DEAD_WS,
    '--repair-every', '0', '--seal-every', '0', '--reorg-every', '0', '--webhook-every', '0',
  ], { stdio: ['ignore', 'pipe', 'pipe'] });
  let stderr = '';
  child.stderr.on('data', (d) => { stderr += d.toString(); });
  return new Promise((resolve) => {
    const t0 = Date.now();
    const iv = setInterval(() => {
      const c = counts();
      if (c.attempts >= ATTEMPTS || Date.now() - t0 > 30000) {
        clearInterval(iv);
        child.kill('SIGKILL');
        child.on('exit', () => resolve({ ...c, stderr }));
        setTimeout(() => { try { child.kill('SIGKILL'); } catch {} resolve({ ...c, stderr }); }, 2000).unref?.();
      }
    }, 250);
  });
};

const heal = async () => {
  // The stream is back: a real sink, a clean exit at the run's own deadline.
  // The sink lives in THIS process, so the daemon must run async — a
  // spawnSync here would freeze the event loop and with it the sink.
  const { server, port } = await startSink(['sig1', 'sig2']);
  const out = await new Promise((resolve) => {
    const c = spawn(BIN, [
      'start', '--db', DB, '--program', PROGRAM,
      '--seconds', '4',
      '--rpc', `http://127.0.0.1:${port}`, '--ws', `ws://127.0.0.1:${port}`,
      '--repair-every', '0', '--seal-every', '0', '--reorg-every', '0', '--webhook-every', '0',
    ], { stdio: ['ignore', 'pipe', 'pipe'] });
    let stdout = '';
    c.stdout.on('data', (d) => { stdout += d.toString(); });
    c.on('exit', (code, signal) => resolve({ status: code, signal, stdout }));
  });
  server.close();
  return out;
};

const snap = () => { const c = counts(); return { ...c, st: status() }; };

// EPISODE 1: the endpoint refuses everything.
const ep1 = await episode();
const s1 = snap();
face('F1 one episode = exactly ONE open connect_fail row', s1.open === 1 && s1.openByKind.includes('connect_fail=1'),
  { open: s1.open, openByKind: s1.openByKind });
face(`F2 the episode row carries every attempt (>= ${ATTEMPTS})`, s1.attempts >= ATTEMPTS,
  { attempts: s1.attempts, openByKind: s1.openByKind });
face('F3 status gaps_open == 1 while the endpoint is down (loud)', s1.st.gaps_open === 1,
  { gaps_open: s1.st.gaps_open, gaps: s1.st.gaps });

// The heal: the stream returns, the episode becomes history.
const h1 = await heal();
if (h1.status !== 0) die(`heal run failed: status=${h1.status} signal=${h1.signal}`);
const s2 = snap();
face('F4 after the heal: ZERO open rows', s2.open === 0, { open: s2.open, openByKind: s2.openByKind });
face('F5 after the heal: the episode stays as ONE resolved row', s2.resolved === 1 && s2.total === 1,
  { resolved: s2.resolved, total: s2.total, byKind: s2.byKind });
face('F6 after the heal: status gaps_open == 0', s2.st.gaps_open === 0,
  { gaps_open: s2.st.gaps_open, gaps: s2.st.gaps });

// EPISODE 2: a new outage writes its OWN row; its heal resolves it too.
await episode();
const s3 = snap();
face('F7a a second episode adds exactly one open row', s3.total === 2 && s3.open === 1,
  { total: s3.total, open: s3.open, byKind: s3.byKind });
const h2 = await heal();
if (h2.status !== 0) die(`heal run 2 failed: status=${h2.status} signal=${h2.signal}`);
const s4 = snap();
face('F7b both episodes stay as resolved history, nothing open', s4.resolved === 2 && s4.open === 0 && s4.st.gaps_open === 0,
  { resolved: s4.resolved, open: s4.open, gaps_open: s4.st.gaps_open, total: s4.total });

const defects = results.filter((x) => !x.pass).length;
const verdict = defects === 0 ? 'gap-episode-dedup-held' : 'gap-episode-rows-accumulate';
const report = {
  verdict,
  defects,
  // Identity of the measured binary WITHOUT the host path (committed
  // artifacts must not carry a machine path — tests/research_artifacts.rs).
  bin: path.basename(BIN),
  binSha256: BIN_SHA256,
  binVersion: (spawnSync(BIN, ['--version'], { encoding: 'utf8' }).stdout || '').trim(),
  label: LABEL,
  at: new Date().toISOString(),
  dir: '<tmp>/',
  attemptsPerEpisode: ATTEMPTS,
  results,
  stderr_sample: (ep1.stderr || '').split('\n').slice(0, 3),
};
const out = opt('--out', `research/gap-episode-dedup-${LABEL}-${new Date().toISOString().slice(0, 10)}.json`);
fs.mkdirSync(path.dirname(out), { recursive: true });
fs.writeFileSync(out, JSON.stringify(report, null, 2));
console.log(`verdict ${verdict} (${defects} defect faces) -> ${out}`);
process.exit(defects === 0 ? 0 : 1);
