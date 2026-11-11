// Probe: the SECRET DOMAIN face PULS S planned — "zwei Targets, ein secret"
// (two webhook targets, one shared secret), measured against the PUBLISHED
// v0.7.15 asset.
//
//   node tools/measure-secret-domain.mjs <reeflog-bin> <out.json> [source-label]
//
// The source label names WHAT was measured ("published v0.7.15 release asset
// ...") so the artifact cannot quietly claim the build while measuring /tmp.
//
// Why this face exists: a webhook body is signed with the target's secret
// (`X-Reeflog-Signature: sha256=HMAC(secret, body)`), but the body itself is
// canonical per EVENT — it carries no target id, no endpoint, no delivery id
// outside the headers. The headers are NOT signed. So the signature attests
// "someone who knows the secret signed THIS BODY", never "signed FOR ME".
//
// Faces (each observed from the RECEIVER's own record of what it accepted):
//   F1  two targets, one secret, same body -> receiver A's (sig, body) pair
//       is byte-valid as a receiver-B delivery. HMAC identity across targets.
//   F2  the delivery-id HEADER (which names the target) is transport
//       metadata outside the signature: rewritten freely, receiver 200s it.
//   F3  replay across targets: the exact bytes A received (body + headers)
//       replayed at B answer 200 again (dedup only saves B's own cursor —
//       the forged copy still lands with a valid signature).
//   F4  the seq header lies independently of the signature: seq 2 arrives
//       wearing the signature of seq 1's body, receiver 200s it.
//
// A signature bound to the TARGET (id/url inside the signed body) refuses
// these at the receiver: the forged pair fails B's own HMAC check.
// A receiver-side contract cannot fix what the producer never attested —
// the measurement states the producer-side fact: one secret = one identity
// space across every endpoint that shares it.
import { spawnSync, spawn } from 'node:child_process';
import { createHash, createHmac } from 'node:crypto';
import fs, { readFileSync } from 'node:fs';
import path from 'node:path';

const BIN = path.resolve(process.argv[2]);
const OUT = process.argv[3];
const SOURCE = process.argv[4] || 'tree build';
const DIR = fs.mkdtempSync('/tmp/reeflog-secret-');
const PROGRAM = 'whirLbMiicVdio4qvUfM5KAg6Ct8VwpYzGff3uctyCc';
const SECRET = 'whsec_shared_between_A_and_B';
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

// A receiver that VERIFIES with its own secret (the README's documented
// rule: want = "sha256=" + HMAC(secret, body)) and records the FULL
// request (body + headers) plus what its own rule decided. 200 = its own
// check passed; 401 = its own check refused. The status code IS the
// receiver's verdict, not a dumb echo.
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

function hmac(secret, body) {
  return 'sha256=' + createHmac('sha256', secret).update(body).digest('hex');
}

// One raw POST with full control over every header; resolves with the
// status code, or null on transport error. The body travels on argv so the
// helper stays a plain CJS script — no require() inside this ESM scope.
function rawPost(port, headers, body) {
  const child = spawnSync('node', [
    path.join(path.dirname(new URL(import.meta.url).pathname), 'secret-domain-post.cjs'),
    String(port), JSON.stringify(headers), body,
  ], { encoding: 'utf8' });
  const code = (child.stdout || '').trim();
  return code === 'ERR' || code === '' ? null : Number(code);
}

// --- setup: journal with 3 events, receivers A and B, both targets sharing
// --- ONE secret (the README says every non-empty secret is in domain).
const db = path.join(DIR, 'j.db');
const init = spawnSync(BIN, ['init', '--db', db, '--program', PROGRAM], { encoding: 'utf8' });
if (init.status !== 0) throw new Error('init failed: ' + init.stderr);
spawnSync('node', [
  path.join(path.dirname(new URL(import.meta.url).pathname), 'endpoint-binding-seed.cjs'), db, '3',
], { encoding: 'utf8' });

const A = fullReceiver('A', SECRET);
const portA = await A.port();
const B = fullReceiver('B', SECRET);
const portB = await B.port();

const cfg = path.join(DIR, 'cfg.json');
fs.writeFileSync(cfg, JSON.stringify({
  targets: [
    { id: 'ta', url: `http://127.0.0.1:${portA}/hook`, secret: SECRET },
    { id: 'tb', url: `http://127.0.0.1:${portB}/hook`, secret: SECRET },
  ],
}));

const push = spawnSync(BIN, ['webhook', 'push', '--db', db, '--config', cfg], { encoding: 'utf8' });
// A tree that REFUSES the shared-secret config up front never delivers:
// every face below is then unmeasurable BECAUSE the product closed the
// door — that is the fixed verdict, not a crash.
const refusedUpFront =
  push.status !== 0 && /share one secret/.test(push.stderr || '');
if (!refusedUpFront && push.status !== 0)
  throw new Error('push failed for an unknown reason: ' + (push.stderr || push.stdout));

const aSeen = refusedUpFront ? [] : A.seen();
const bSeen = refusedUpFront ? [] : B.seen();

// --- F1: one shared secret -> the (secret, body) HMAC is target-agnostic:
// --- A's recorded pairs pass A's OWN verification rule 3/3, and the very
// --- same pair would pass B's (identical secret, identical rule).
const f1 = aSeen.map((r) => r.verified === true && hmac(SECRET, r.body) === r.sig);
const f1violated = f1.length === 3 && f1.every(Boolean);
// --- F2: the delivery-id header names the target, but is outside the
// --- signature: replay A's body at B under a forged tb: event-id header
// --- with the EXACT signature A received — B's OWN rule says yes (200)
// --- and records the forged delivery id as its own. The header lies; the
// --- signature cannot tell it did.
const f2status = refusedUpFront
  ? null
  : rawPost(portB, {
  'content-type': 'application/json',
  'x-reeflog-signature': aSeen[0].sig,
  'x-reeflog-seq': aSeen[0].seq,
  'x-reeflog-event-id': aSeen[0].eventId.replace(/^ta:/, 'tb:'),
}, aSeen[0].body);
const f2violated =
  !refusedUpFront &&
  f2status === 200 &&
  B.seen().some((r) => r.eventId === aSeen[0].eventId.replace(/^ta:/, 'tb:') && r.verified === true);

// --- F3: cross-target FORWARD: a body B received (signed under the shared
// --- secret) replays at A and A's OWN rule accepts it (200) — one secret
// --- = one identity space in BOTH directions.
const f3status = refusedUpFront
  ? null
  : rawPost(portA, {
  'content-type': 'application/json',
  'x-reeflog-signature': bSeen[0].sig,
  'x-reeflog-seq': bSeen[0].seq,
  'x-reeflog-event-id': bSeen[0].eventId,
}, bSeen[0].body);
const f3violated = !refusedUpFront && f3status === 200;

// --- F4: the seq/event-id headers lie independently: A's body, forged
// --- headers, signature under the shared secret — B's own rule says 200.
// --- (The body carries its own seq; a receiver that reads body.seq is
// --- immune to the header lie — that limit is part of the report.)
const f4status = refusedUpFront
  ? null
  : rawPost(portB, {
  'content-type': 'application/json',
  'x-reeflog-signature': hmac(SECRET, aSeen[0].body),
  'x-reeflog-seq': '999',
  'x-reeflog-event-id': 'tb:event:999',
}, aSeen[0].body);
const f4violated = !refusedUpFront && f4status === 200;

const violations = refusedUpFront ? 0 : [f1violated, f2violated, f3violated, f4violated].filter(Boolean).length;

const out = {
  artifact: 'secret-domain',
  source: SOURCE,
  method:
    'two VERIFYING live receivers (ta, tb — each answers 200 only when its OWN documented rule HMAC(secret, body) == the signature header passes, else 401) sharing ONE non-empty secret, inside the documented domain: every face is the receivers own verdict on a forged pair',
  measuredAt: new Date().toISOString(),
  binaryVersion: (spawnSync(BIN, ['--version'], { encoding: 'utf8' }).stdout || '').trim(),
  binarySha256: createHash('sha256').update(readFileSync(BIN)).digest('hex'),
  assetSha256: SOURCE.includes('published') ? createHash('sha256').update(readFileSync(path.join(path.dirname(BIN), 'reeflog-linux-amd64.tar.gz'))).digest('hex') : undefined,
  push: { exit: push.status, stdout: push.stdout.trim(), stderr: (push.stderr || '').trim() },
  refusedUpFront,
  deliveredA: aSeen.length,
  deliveredB: bSeen.length,
  faces: {
    F1_sharedSecretHmacValidatesAtBothTargets: { violated: f1violated, pairsChecked: aSeen.length },
    F2_deliveryIdHeaderOutsideSignature: { violated: f2violated, status: f2status },
    F3_crossTargetReplayAccepted200: { violated: f3violated, status: f3status },
    F4_seqHeaderLiesUnderValidSignature: { violated: f4violated, status: f4status },
  },
  violations,
  verdict: refusedUpFront
    ? 'REFUSED-UP-FRONT'
    : violations >= 3
      ? 'SIGNATURE-TARGET-AGNOSTIC'
      : 'other',
};
fs.writeFileSync(OUT, JSON.stringify(out, null, 2));
A.child.kill();
B.child.kill();
console.log(JSON.stringify(out, null, 2));
