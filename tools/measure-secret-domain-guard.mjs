// Guard: the same four faces with DISTINCT secrets must NOT be violable by
// cross-target forgery — a receiver with its own secret rejects the foreign
// pair. This is the false-positive guard for the secret-domain fix: whatever
// the fix changes, it must not break the world where every target has its
// own secret (the README's own example: "whsec_…" per target).
//
//   node measure-secret-domain-guard.mjs <reeflog-bin> <out.json> [source]
import { spawnSync, spawn } from 'node:child_process';
import { createHash, createHmac } from 'node:crypto';
import fs, { readFileSync } from 'node:fs';
import path from 'node:path';

const BIN = path.resolve(process.argv[2]);
const OUT = process.argv[3];
const SOURCE = process.argv[4] || 'tree build';
const HERE = path.dirname(new URL(import.meta.url).pathname);
const DIR = fs.mkdtempSync('/tmp/reeflog-secret-guard-');
const PROGRAM = 'whirLbMiicVdio4qvUfM5KAg6Ct8VwpYzGff3uctyCc';
const SECRET_A = 'whsec_only_for_A';
const SECRET_B = 'whsec_only_for_B';
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

// A receiver that VERIFIES with its own secret (200 only when HMAC passes,
// else 401) and records the FULL request plus its own verdict.
function fullReceiver(tag, secret) {
  const seenFile = path.join(DIR, `seen-${tag}.json`);
  const portFile = seenFile + '.port';
  const child = spawn('node', ['-e', `
    const http = require('node:http');
    const fs = require('node:fs');
    const crypto = require('node:crypto');
    const secret = process.env.RECV_SECRET;
    const seen = [];
    const srv = http.createServer((req, res) => {
      let body = '';
      req.on('data', (c) => { body += c; });
      req.on('end', () => {
        const want = 'sha256=' + crypto.createHmac('sha256', secret).update(body).digest('hex');
        const got = req.headers['x-reeflog-signature'] ?? null;
        const verified = want === got;
        seen.push({
          eventId: req.headers['x-reeflog-event-id'] ?? null,
          seq: req.headers['x-reeflog-seq'] ?? null,
          sig: got,
          verified,
          body,
        });
        fs.writeFileSync(${JSON.stringify(seenFile)}, JSON.stringify(seen, null, 2));
        res.statusCode = verified ? 200 : 401;
        res.end(verified ? 'ok' : 'bad signature');
      });
    });
    srv.listen(0, () => fs.writeFileSync(${JSON.stringify(portFile)}, String(srv.address().port)));
  `], { stdio: 'ignore', env: { ...process.env, RECV_SECRET: secret } });
  return {
    child,
    async port() {
      for (let i = 0; i < 200 && !fs.existsSync(portFile); i++) await sleep(25);
      return fs.readFileSync(portFile, 'utf8').trim();
    },
    seen() {
      try { return JSON.parse(fs.readFileSync(seenFile, 'utf8')); } catch { return []; }
    },
  };
}

function rawPost(port, headers, body) {
  const child = spawnSync('node', [
    path.join(HERE, 'secret-domain-post.cjs'), String(port), JSON.stringify(headers), body,
  ], { encoding: 'utf8' });
  const code = (child.stdout || '').trim();
  return code === 'ERR' || code === '' ? null : Number(code);
}

const db = path.join(DIR, 'j.db');
spawnSync(BIN, ['init', '--db', db, '--program', PROGRAM], { encoding: 'utf8' });
spawnSync('node', [path.join(HERE, 'endpoint-binding-seed.cjs'), db, '3'], { encoding: 'utf8' });

const A = fullReceiver('A', SECRET_A);
const portA = await A.port();
const B = fullReceiver('B', SECRET_B);
const portB = await B.port();

const cfg = path.join(DIR, 'cfg.json');
fs.writeFileSync(cfg, JSON.stringify({
  targets: [
    { id: 'ta', url: `http://127.0.0.1:${portA}/hook`, secret: SECRET_A },
    { id: 'tb', url: `http://127.0.0.1:${portB}/hook`, secret: SECRET_B },
  ],
}));
const push = spawnSync(BIN, ['webhook', 'push', '--db', db, '--config', cfg], { encoding: 'utf8' });
if (push.status !== 0) throw new Error('push failed');

const aSeen = A.seen();
const bSeen = B.seen();

// Cross-target forgeries with each side's own signature-receiver rule:
const f1 = aSeen.map((r) => createHmac('sha256', SECRET_A).update(r.body).digest('hex'));
const f1ok = aSeen.every((r, i) => 'sha256=' + f1[i] === r.sig);
const f2status = rawPost(portB, {
  'content-type': 'application/json',
  'x-reeflog-signature': aSeen[0].sig,
  'x-reeflog-seq': aSeen[0].seq,
  'x-reeflog-event-id': aSeen[0].eventId.replace(/^ta:/, 'tb:'),
}, aSeen[0].body);
const f3status = rawPost(portA, {
  'content-type': 'application/json',
  'x-reeflog-signature': bSeen[0].sig,
  'x-reeflog-seq': bSeen[0].seq,
  'x-reeflog-event-id': bSeen[0].eventId,
}, bSeen[0].body);
const f4status = rawPost(portB, {
  'content-type': 'application/json',
  'x-reeflog-signature': createHmac('sha256', SECRET_B).update(aSeen[0].body).digest('hex'),
  'x-reeflog-seq': '999',
  'x-reeflog-event-id': 'tb:event:999',
}, aSeen[0].body);

const out = {
  artifact: 'secret-domain-guard',
  source: SOURCE,
  method:
    'the same four faces with DISTINCT secrets: a receiver with its own secret must reject every foreign pair — this is the false-positive guard the fix must keep green',
  measuredAt: new Date().toISOString(),
  binaryVersion: (spawnSync(BIN, ['--version'], { encoding: 'utf8' }).stdout || '').trim(),
  binarySha256: createHash('sha256').update(readFileSync(BIN)).digest('hex'),
  pushExit: push.status,
  deliveredA: aSeen.length,
  deliveredB: bSeen.length,
  faces: {
    G1_ownSecretValidatesOwnTarget: { ok: f1ok },
    G2_foreignSigRejectedAtB: { status: f2status, rejected: f2status !== 200 },
    G3_foreignSigRejectedAtA: { status: f3status, rejected: f3status !== 200 },
    G4_forgedSeqStillRejectedBySig: { status: f4status, rejected: f4status !== 200 },
  },
};
fs.writeFileSync(OUT, JSON.stringify(out, null, 2));
A.child.kill();
B.child.kill();
console.log(JSON.stringify(out, null, 2));
