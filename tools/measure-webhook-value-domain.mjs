#!/usr/bin/env node
// The BEFORE/AFTER measurement for the webhook value domain: the SAME
// values, run against the PUBLISHED v0.7.8 binary and against the FIXED
// build, with a receiver that records what actually arrives.
//
//   node research/measure-webhook-value-domain.mjs <baseline-bin> <fixed-bin> <out.json>
//
// The baseline is the published v0.7.8 asset (the release archive's sha256
// equals the digest GitHub Releases publishes). The fixed binary is the
// tree's own build.
//
// What it records per case: whether the CONFIG was accepted, and whether
// the delivery actually reached the receiver, with the transport's own
// error message. A case the baseline accepted AND the baseline failed to
// deliver is a defect; the fixed build must REFUSE it up front.
import { spawnSync, spawn } from 'node:child_process';
import fs from 'node:fs';
import path from 'node:path';

const BASELINE = process.argv[2];
const FIXED = process.argv[3];
const OUT = process.argv[4];
if (!BASELINE || !FIXED || !OUT) {
  console.error('usage: measure-webhook-value-domain.mjs <baseline-bin> <fixed-bin> <out.json>');
  process.exit(2);
}

const PROGRAM = 'whirLbMiicVdio4qvUfM5KAg6Ct8VwpYzGff3uctyCc';
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

async function runCase(bin, target, { deliver }) {
  const dir = fs.mkdtempSync('/tmp/whvd-');
  const db = path.join(dir, 'j.db');
  spawnSync(bin, ['init', '--db', db, '--program', PROGRAM], { cwd: dir, encoding: 'utf8' });
  spawnSync('node', ['-e', `
    const {DatabaseSync}=require('node:sqlite');
    const d=new DatabaseSync(process.argv[1]);
    d.exec("INSERT INTO events (signature, slot, err, logs, source, first_seen, seal_status) VALUES ('SIG${'x'.repeat(60)}', 44000000, NULL, '[]', 'ws', 1700000000000, 'sealed')");
  `, db]);
  const seenFile = path.join(dir, 'seen.json');
  let recv = null;
  let port = null;
  if (deliver) {
    recv = spawn('node', [path.join(path.dirname(new URL(import.meta.url).pathname), 'raw-receiver.mjs'), seenFile], { stdio: 'ignore' });
    for (let i = 0; i < 60 && !fs.existsSync(seenFile + '.port'); i++) await sleep(50);
    port = fs.readFileSync(seenFile + '.port', 'utf8').trim();
  }
  const cfg = path.join(dir, 'cfg.json');
  const t = { ...target, timeout_ms: 5000 };
  if (port && t.url) t.url = t.url.replace('PORT', port);
  fs.writeFileSync(cfg, JSON.stringify({ targets: [t] }));
  const st = spawnSync(bin, ['webhook', 'status', '--db', db, '--config', cfg], { cwd: dir, encoding: 'utf8' });
  const rec = {
    configAccepted: st.status === 0,
    configError: st.status === 0 ? null : (st.stderr || '').trim().split('\n').pop(),
    delivered: null,
    transportError: null,
  };
  if (st.status === 0 && deliver) {
    spawnSync(bin, ['webhook', 'push', '--db', db, '--config', cfg], { cwd: dir, encoding: 'utf8', timeout: 60000 });
    const rep = spawnSync(bin, ['webhook', 'status', '--db', db, '--config', cfg], { cwd: dir, encoding: 'utf8' });
    let j = {};
    try { j = JSON.parse(rep.stdout); } catch {}
    const t0 = (j.targets || [])[0] || {};
    rec.delivered = t0.delivered ?? null;
    rec.transportError = t0.last_error ?? null;
  }
  if (recv) recv.kill();
  fs.rmSync(dir, { recursive: true, force: true });
  return rec;
}

// The cases: every value the two trees must AGREE on, in both directions.
const cases = [
  { case: 'id-plain', target: { id: 't1', url: 'http://127.0.0.1:PORT/hook', secret: 's' }, deliver: true },
  { case: 'id-leading-space', target: { id: ' t1', url: 'http://127.0.0.1:PORT/hook', secret: 's' }, deliver: true },
  { case: 'id-trailing-space', target: { id: 't1 ', url: 'http://127.0.0.1:PORT/hook', secret: 's' }, deliver: true },
  { case: 'id-internal-space', target: { id: 'a b', url: 'http://127.0.0.1:PORT/hook', secret: 's' }, deliver: true },
  { case: 'id-internal-tab', target: { id: 'a\tb', url: 'http://127.0.0.1:PORT/hook', secret: 's' }, deliver: true },
  { case: 'id-carriage-return', target: { id: 'a\rb', url: 'http://127.0.0.1:PORT/hook', secret: 's' }, deliver: true },
  { case: 'id-nul', target: { id: 'a\u0000b', url: 'http://127.0.0.1:PORT/hook', secret: 's' }, deliver: true },
  { case: 'id-del', target: { id: 'a\u007fb', url: 'http://127.0.0.1:PORT/hook', secret: 's' }, deliver: true },
  { case: 'id-empty', target: { id: '', url: 'http://127.0.0.1:PORT/hook', secret: 's' }, deliver: false },
  { case: 'secret-empty', target: { id: 't1', url: 'http://127.0.0.1:PORT/hook', secret: '' }, deliver: false },
  { case: 'secret-newline', target: { id: 't1', url: 'http://127.0.0.1:PORT/hook', secret: 'se\ncr\net' }, deliver: true },
  { case: 'secret-nonascii', target: { id: 't1', url: 'http://127.0.0.1:PORT/hook', secret: 'sécret-ß' }, deliver: true },
  { case: 'url-space-in-path', target: { id: 't1', url: 'http://127.0.0.1:PORT/a b', secret: 's' }, deliver: true },
  { case: 'url-angle-in-path', target: { id: 't1', url: 'http://127.0.0.1:PORT/a<b', secret: 's' }, deliver: true },
  { case: 'url-backtick-in-path', target: { id: 't1', url: 'http://127.0.0.1:PORT/a`b', secret: 's' }, deliver: true },
  { case: 'url-quote-in-host', target: { id: 't1', url: 'http://ho"st:PORT/hook', secret: 's' }, deliver: true },
  { case: 'url-brace-in-path', target: { id: 't1', url: 'http://127.0.0.1:PORT/a{b', secret: 's' }, deliver: true },
  { case: 'url-backtick-in-query', target: { id: 't1', url: 'http://127.0.0.1:PORT/hook?k=`v`', secret: 's' }, deliver: true },
  { case: 'url-nonascii-in-path', target: { id: 't1', url: 'http://127.0.0.1:PORT/atüb', secret: 's' }, deliver: true },
  { case: 'url-nonascii-in-host', target: { id: 't1', url: 'http://tüb.example:PORT/hook', secret: 's' }, deliver: true },
  { case: 'url-nonnumeric-port', target: { id: 't1', url: 'http://127.0.0.1:abc/hook', secret: 's' }, deliver: false },
  { case: 'url-port-too-large', target: { id: 't1', url: 'http://127.0.0.1:99999/hook', secret: 's' }, deliver: false },
  { case: 'url-no-host', target: { id: 't1', url: 'http://', secret: 's' }, deliver: false },
  { case: 'url-uppercase-scheme', target: { id: 't1', url: 'HTTP://127.0.0.1:PORT/hook', secret: 's' }, deliver: true },
];

const rows = [];
for (const c of cases) {
  const before = await runCase(BASELINE, c.target, { deliver: c.deliver });
  const after = await runCase(FIXED, c.target, { deliver: c.deliver });
  rows.push({ case: c.case, before, after });
  const verdict =
    before.configAccepted && before.delivered !== 1
      ? 'DEFECT (baseline accepted, delivery died)'
      : before.configAccepted
        ? 'accepted-and-delivered'
        : 'refused by baseline';
  console.log(
    `${c.case.padEnd(26)} baseline: ${verdict.padEnd(40)} fixed: ${
      after.configAccepted ? 'accepted' : 'refused'
    }`
  );
}

const out = {
  artifact: 'webhook-value-domain-before-after',
  measuredAt: new Date().toISOString(),
  baseline: { binary: 'published v0.7.8 asset', version: '0.7.8' },
  fixed: { binary: 'tree build', version: '0.7.8' },
  method:
    'the same config values against both binaries, with a receiver process that records ' +
    'what arrives; the baseline is the PUBLISHED v0.7.8 asset',
  cases: rows,
  summary: {
    total: rows.length,
    baselineAcceptedButFailed: rows.filter(
      (r) => r.before.configAccepted && r.before.delivered !== 1
    ).length,
    baselineRefused: rows.filter((r) => !r.before.configAccepted).length,
    fixedRefused: rows.filter((r) => !r.after.configAccepted).length,
    fixedAcceptedAndDelivered: rows.filter(
      (r) => r.after.configAccepted && r.after.delivered === 1
    ).length,
  },
};
fs.writeFileSync(OUT, JSON.stringify(out, null, 2) + '\n');
console.log(JSON.stringify(out.summary));
console.log('wrote', OUT);
