#!/usr/bin/env node
// probe-reorg-refusal-voice.mjs — measure the RETRACTION channel's refusal
// voice on one redirecting webhook endpoint, baseline binary vs fixed
// binary, one live sink probe.
//
// THE QUESTION: when the endpoint answers a 3xx redirect, does the
// RETRACTION queue's standing reason name the mechanism and the repair —
// exactly like the EVENT channel's reason does (v0.7.36 rule: one mint, so
// a 3xx never borrows the flat 4xx wording)?
//
// Faces (aligned with tests/webhook_reorg_refusal_voice.rs):
//   D1 a redirecting endpoint's RETRACTION refusal names the redirect in
//      the recorded attempt (defect: flat "refused with http 302")
//   D2 a redirecting endpoint's RETRACTION refusal names the redirect in
//      the report row's `reorg_error` (defect: flat wording, no repair)
//   D3 a redirecting endpoint's RETRACTION refusal names the redirect in
//      the operator's blocked line on stderr (defect: flat wording)
//   D4 the EVENT channel's reason is unchanged by the fix (same mint, both
//      channels, one wording — must match across trees)
// Guards (green on BOTH sides):
//   G1 a plain 200 sink delivers the retraction normally (exit 0, receipt,
//      cursor advanced)
//   G2 a 404 sink keeps the flat 4xx wording on the retraction channel
//
//   node tools/probe-reorg-refusal-voice.mjs --baseline <bin> --fixed <bin> [--out f.json]
import { spawn, spawnSync } from 'node:child_process';
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
const OUT = opt('--out', `research/reorg-refusal-voice-${new Date().toISOString().slice(0, 10)}.json`);
const die = (m) => { console.error(m); process.exit(2); };
if (!BASELINE || !FIXED) die('usage: probe-reorg-refusal-voice.mjs --baseline <bin> --fixed <bin> [--out f.json]');
const BASELINE_BIN = path.resolve(BASELINE);
const FIXED_BIN = path.resolve(FIXED);

const PROGRAM = 'whirLbMiicVdio4qvUfM5KAg6Ct8VwpYzGff3uctyCc';
const versionOf = (bin) => (spawnSync(bin, ['--version'], { encoding: 'utf8' }).stdout || '').trim();
const sha256 = (bin) => crypto.createHash('sha256').update(fs.readFileSync(bin)).digest('hex');
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

// A recorder that answers a SCRIPTED status for every request and records
// every request it sees. Records BEFORE answering.
function recorder(answer, location) {
  const hits = [];
  const state = { answer, location };
  const server = net.createServer((s) => {
    let acc = Buffer.alloc(0);
    s.on('data', (d) => {
      acc = Buffer.concat([acc, d]);
      const p = acc.indexOf('\r\n\r\n');
      if (p < 0) return;
      const head = acc.subarray(0, p).toString();
      const cl = Number((head.match(/content-length:\s*(\d+)/i) || [])[1] || 0);
      if (acc.length < p + 4 + cl) return;
      const line = head.split('\r\n')[0];
      hits.push({
        request_line: line,
        method: line.split(' ')[0],
        signature_header: /x-reeflog-signature:/i.test(head),
      });
      const code = state.answer;
      let resp;
      if (code === 200) {
        resp = 'HTTP/1.1 200 OK\r\ncontent-length: 2\r\nconnection: close\r\n\r\nok';
      } else if (code >= 300 && code < 400 && state.location) {
        resp = `HTTP/1.1 ${code} Moved\r\nlocation: ${state.location}\r\ncontent-length: 0\r\nconnection: close\r\n\r\n`;
      } else {
        resp = `HTTP/1.1 ${code} Gone\r\ncontent-length: 0\r\nconnection: close\r\n\r\n`;
      }
      s.write(resp);
      s.end();
    });
    s.on('error', () => {});
  });
  return new Promise((resolve) => {
    server.listen(0, '127.0.0.1', () => {
      resolve({
        server,
        port: server.address().port,
        url: `http://127.0.0.1:${server.address().port}/hook`,
        hits: () => hits.slice(),
        setAnswer: (c) => { state.answer = c; },
        setLocation: (l) => { state.location = l; },
        postCount: () => hits.filter((h) => h.method === 'POST').length,
      });
    });
  });
}

function mkTag(tag) {
  const nanos = process.hrtime.bigint().toString();
  const dir = path.join(os.tmpdir(), `reeflog-reorg-voice-${tag}-${process.pid}-${nanos}`);
  fs.mkdirSync(dir, { recursive: true });
  return dir;
}

function sh(bin, args, cwd) {
  return new Promise((resolve) => {
    const child = spawn(bin, args, { cwd });
    let stdout = '';
    let stderr = '';
    child.stdout.on('data', (d) => { stdout += d; });
    child.stderr.on('data', (d) => { stderr += d; });
    child.on('close', (code) => resolve({ code, stdout, stderr }));
  });
}

function seed(db) {
  const out = spawnSync('node', ['-e', `
const { DatabaseSync } = require('node:sqlite');
const d = new DatabaseSync(process.argv[1]);
d.exec("INSERT INTO reorgs(signature, slot, detected_at, status_source, tx_source, detail) VALUES('sigRET1', 201, 1700000000000, 'slot', 'tx', 'the chain unmade it')");
d.exec("INSERT INTO events(signature, slot, err, logs, source, first_seen, seal_status, payload_state) VALUES('sigEV1', 101, NULL, NULL, 'ws', 1700000000000, 'sealed', 'held')");
`, db], { encoding: 'utf8' });
  if (out.status !== 0) die(`seeding failed: ${out.stderr}`);
}

function jsonStdout(s) {
  try { return JSON.parse(s); } catch { return null; }
}

// Read the reorg refusal surfaces out of the journal: the recorded attempt
// (channel='reorg'), the standing reorg error, and the cursor.
function journalFacts(db) {
  const out = spawnSync('node', ['-e', `
const { DatabaseSync } = require('node:sqlite');
const d = new DatabaseSync(process.argv[1]);
const attempt = d.prepare("SELECT status, error FROM webhook_attempts WHERE kind='reorg' ORDER BY id").all();
const evAttempt = d.prepare("SELECT status, error FROM webhook_attempts WHERE kind='event' ORDER BY id").all();
const state = d.prepare("SELECT next_reorg_id FROM webhook_state").all();
const receipts = d.prepare("SELECT COUNT(*) AS n FROM webhook_receipts WHERE kind='reorg'").all();
console.log(JSON.stringify({ attempt, evAttempt, state, receipts }));
`, db], { encoding: 'utf8' });
  return JSON.parse(out.stdout || '{}');
}

async function runOne(bin, label, answer, location, tag) {
  const dir = mkTag(tag);
  const db = path.join(dir, 'reeflog.db');
  let init = await sh(bin, ['init', '--db', db, '--program', PROGRAM], dir);
  if (init.code !== 0) die(`init failed on ${label}: ${init.stderr}`);
  seed(db);
  const sink = await recorder(answer, location);
  const cfg = path.join(dir, 'cfg.json');
  fs.writeFileSync(cfg, JSON.stringify({
    targets: [{ id: 't1', url: sink.url, secret: 's-t1', timeout_ms: 2000, dead_letter_after: 0 }],
  }));
  const push = await sh(bin, ['webhook', 'push', '--db', db, '--config', cfg], dir);
  const rep = jsonStdout(push.stdout);
  const jf = journalFacts(db);
  // The status surface too: the polled reason the operator reads.
  const status = await sh(bin, ['webhook', 'status', '--db', db, '--config', cfg], dir);
  const srep = jsonStdout(status.stdout);
  const facts = {
    label,
    binary_version: versionOf(bin),
    exit_code: push.code,
    report_error: rep?.targets?.[0]?.error ?? null,
    report_reorg_error: rep?.targets?.[0]?.reorg_error ?? null,
    report_blocked: rep?.targets?.[0]?.blocked ?? null,
    report_blocked_reorg_id: rep?.targets?.[0]?.blocked_reorg_id ?? null,
    report_reorg_delivered: rep?.targets?.[0]?.reorg_delivered ?? null,
    attempt: jf.attempt ?? null,
    event_attempt: jf.evAttempt ?? null,
    next_reorg_id: jf.state?.[0]?.next_reorg_id ?? null,
    reorg_receipts: jf.receipts?.[0]?.n ?? null,
    status_reorg_error: srep?.targets?.[0]?.reorg_error ?? null,
    stderr_blocked_line: push.stderr.split('\n').find((l) => l.includes('reorg')) ?? null,
    sink_post_count: sink.postCount(),
    sink_hit_count: sink.hits().length,
  };
  sink.server.close();
  fs.rmSync(dir, { recursive: true, force: true });
  return facts;
}

const baseline = await runOne(BASELINE_BIN, 'baseline', 302, 'http://127.0.0.1:9/elsewhere', 'base302');
const fixed = await runOne(FIXED_BIN, 'fixed', 302, 'http://127.0.0.1:9/elsewhere', 'fix302');

// G2: a 404 keeps the flat wording — measured on both sides.
const baseline404 = await runOne(BASELINE_BIN, 'baseline-404', 404, null, 'base404');
const fixed404 = await runOne(FIXED_BIN, 'fixed-404', 404, null, 'fix404');

// G1: a 200 sink delivers the retraction — measured on both sides.
const baseline200 = await runOne(BASELINE_BIN, 'baseline-200', 200, null, 'base200');
const fixed200 = await runOne(FIXED_BIN, 'fixed-200', 200, null, 'fix200');

const name = (f) => (f || '').toLowerCase().includes('redirect');
const flat = (f, s) => f === `refused with http ${s}`;

const faces = [
  {
    id: 'D1',
    question: 'the retraction attempt record names the redirect mechanism',
    baseline: { defect: !name(baseline.attempt?.[0]?.error), value: baseline.attempt?.[0]?.error ?? null },
    fixed: { defect: !name(fixed.attempt?.[0]?.error), value: fixed.attempt?.[0]?.error ?? null },
  },
  {
    id: 'D2',
    question: 'the report row names the redirect mechanism in reorg_error',
    baseline: { defect: !name(baseline.report_reorg_error), value: baseline.report_reorg_error },
    fixed: { defect: !name(fixed.report_reorg_error), value: fixed.report_reorg_error },
  },
  {
    id: 'D3',
    question: 'the operator blocked line on stderr names the redirect mechanism',
    baseline: { defect: !name(baseline.stderr_blocked_line), value: baseline.stderr_blocked_line },
    fixed: { defect: !name(fixed.stderr_blocked_line), value: fixed.stderr_blocked_line },
  },
  {
    id: 'D4',
    question: 'the event channel reason names the redirect and is byte-identical across trees (one mint)',
    baseline: { defect: !name(baseline.report_error), value: baseline.report_error },
    fixed: { defect: !name(fixed.report_error), value: fixed.report_error },
    consistent: baseline.report_error === fixed.report_error,
  },
];

faces.push({
  id: 'D5',
  question: 'the polled `webhook status` surface names the redirect on the retraction queue',
  baseline: { defect: !name(baseline.status_reorg_error), value: baseline.status_reorg_error },
  fixed: { defect: !name(fixed.status_reorg_error), value: fixed.status_reorg_error },
});

const guards = [
  {
    id: 'G1',
    question: 'a 200 sink still delivers the retraction (exit 0, receipt, cursor)',
    baseline: baseline200.exit_code === 0 && baseline200.reorg_receipts === 1 && baseline200.next_reorg_id === 1,
    fixed: fixed200.exit_code === 0 && fixed200.reorg_receipts === 1 && fixed200.next_reorg_id === 1,
    baseline_value: baseline200,
    fixed_value: fixed200,
  },
  {
    id: 'G2',
    question: 'a 404 refusal keeps the flat wording on the retraction channel',
    baseline: flat(baseline404.report_reorg_error, 404),
    fixed: flat(fixed404.report_reorg_error, 404),
    baseline_value: baseline404.report_reorg_error,
    fixed_value: fixed404.report_reorg_error,
  },
  {
    id: 'G3',
    question: 'the redirect refusal blocks the retraction queue with the owed id (both trees)',
    baseline: baseline.report_blocked_reorg_id === 1 && baseline.exit_code === 3,
    fixed: fixed.report_blocked_reorg_id === 1 && fixed.exit_code === 3,
    baseline_value: { exit: baseline.exit_code, blocked_reorg_id: baseline.report_blocked_reorg_id },
    fixed_value: { exit: fixed.exit_code, blocked_reorg_id: fixed.report_blocked_reorg_id },
  },
];

const result = {
  tool: 'probe-reorg-refusal-voice.mjs',
  date: new Date().toISOString().slice(0, 10),
  verdict: faces.every((f) => !f.baseline.defect) && faces.every((f) => !f.fixed.defect)
    ? 'reorg-refusal-voice-held'
    : faces.some((f) => f.baseline.defect) && faces.every((f) => !f.fixed.defect)
      ? 'reorg-refusal-voice-borrowed-flat-wording'
      : 'reorg-refusal-voice-unexpected',
  baseline: { binary_version: baseline.binary_version, sha256: sha256(BASELINE_BIN) },
  fixed: { binary_version: fixed.binary_version, sha256: sha256(FIXED_BIN) },
  faces,
  guards,
  measured: { baseline, fixed, baseline404, fixed404, baseline200, fixed200 },
};

fs.mkdirSync(path.dirname(OUT), { recursive: true });
fs.writeFileSync(OUT, JSON.stringify(result, null, 1) + '\n');
console.log(JSON.stringify({ verdict: result.verdict, faces: faces.map((f) => ({ id: f.id, base: f.baseline.defect, fixed: f.fixed.defect })), guards: guards.map((g) => ({ id: g.id, base: g.baseline, fixed: g.fixed })), out: OUT }, null, 1));
process.exit(result.verdict === 'reorg-refusal-voice-borrowed-flat-wording' ? 0 : 1);
