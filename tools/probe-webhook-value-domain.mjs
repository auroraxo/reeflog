#!/usr/bin/env node
// The probe layer behind research/webhook-value-domain-*.json: it measures
// what a REAL consumer does with the webhook CONFIG values `id`, `secret`
// and `url`, against whichever binary you point it at.
//
//   node tools/probe-webhook-value-domain.mjs <binary> <out-dir>
//
// It writes the raw sweeps the artifact maker consumes:
//
//   id-bytes.json         which id bytes survive into the HTTP field value,
//                         and whether the consumer's dedup key still equals
//                         the journal's
//   url-pos.json          is the refusal set position-dependent? (same char
//                         in HOST / PATH / QUERY / FRAGMENT)
//   query-fragment.json   dense QUERY and FRAGMENT charset sweeps
//   authority-edge.json   ports, %, userinfo, brackets, scheme edges
//   dense.json            dense PATH (printable ASCII) and id (C0+DEL) sweeps
//   secret.json           does an odd-byte secret still deliver and verify?
//
// The receiver answers 200 and records the RAW X-Reeflog-Event-Id bytes;
// for the secret cases it computes its own HMAC over the received body the
// way a consumer would, so `verified` is a measurement, not an assertion.
//
// WHY THIS FILE EXISTS (2026-11-10): the committed artifact was produced by
// probe scripts that lived only in /tmp — the artifact claimed a
// measurement nobody could re-run from the repo. This is that probe,
// committed. Regenerate with:
//
//   node tools/probe-webhook-value-domain.mjs <baseline-bin> /tmp/out
//   node tools/make-webhook-value-domain-artifact.mjs /tmp/out research/webhook-value-domain-<date>.json
//
// NOTE: run it against the PUBLISHED baseline (the release asset, whose
// sha256 equals the Release-API digest), not against the tree build — the
// artifact's rule statements describe the consumer's behaviour, which is
// the same in both, but the config refusals are not.
import { spawnSync, spawn } from 'node:child_process';
import crypto from 'node:crypto';
import fs from 'node:fs';

import os from 'node:os';
import path from 'node:path';

const BIN = process.argv[2];
const OUT = process.argv[3];
// Optional third argument: run only one sweep (id | url | query |
// authority | dense | secret), so a single section can be re-measured
// without re-running the whole suite.
const ONLY = process.argv[4] || null;
if (!BIN || !OUT) {
  console.error('usage: probe-webhook-value-domain.mjs <binary> <out-dir> [id|url|query|authority|dense|secret]');
  process.exit(2);
}
const want = (name) => !ONLY || ONLY === name;
const absBin = path.resolve(BIN);
const PROGRAM = 'whirLbMiicVdio4qvUfM5KAg6Ct8VwpYzGff3uctyCc';
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
fs.mkdirSync(OUT, { recursive: true });

// --- the receiver: raw header bytes + consumer-side HMAC -------------
// Run as a CHILD PROCESS (tools/raw-receiver.mjs): this probe drives
// reeflog with `spawnSync`, which blocks THIS process's event loop, so an
// in-process server could never answer and every delivery would time out
// with `timeout: global` instead of the transport error being measured.
const RECEIVER = path.join(path.dirname(new URL(import.meta.url).pathname), 'raw-receiver.mjs');

async function startReceiver(tmpDir) {
  const seenFile = path.join(tmpDir, 'seen.json');
  const child = spawn('node', [RECEIVER, seenFile], { stdio: 'ignore' });
  for (let i = 0; i < 100 && !fs.existsSync(seenFile + '.port'); i++) await sleep(50);
  if (!fs.existsSync(seenFile + '.port')) throw new Error('receiver did not become ready');
  const port = fs.readFileSync(seenFile + '.port', 'utf8').trim();
  return { child, seenFile, port };
}
function readSeen(seenFile) {
  try {
    return JSON.parse(fs.readFileSync(seenFile, 'utf8'));
  } catch {
    return [];
  }
}

// --- one config case against the binary ------------------------------
async function runCase({ id = 't1', url, secret = 's', deliver = true }) {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'whprobe-'));
  const db = path.join(dir, 'j.db');
  spawnSync(absBin, ['init', '--db', db, '--program', PROGRAM], { cwd: dir, encoding: 'utf8' });
  spawnSync('node', ['-e', `
    const {DatabaseSync}=require('node:sqlite');
    const d=new DatabaseSync(process.argv[1]);
    d.exec("INSERT INTO events (signature, slot, err, logs, source, first_seen, seal_status) VALUES ('SIG${'x'.repeat(60)}', 44000000, NULL, '[]', 'ws', 1700000000000, 'sealed')");
  `, db]);

  let recv = null;
  let realUrl = url;
  if (deliver) {
    recv = await startReceiver(dir);
    realUrl = url.replaceAll('PORT', String(recv.port)).replaceAll('LOCALHOST', `127.0.0.1:${recv.port}`);
  }
  const cfg = path.join(dir, 'cfg.json');
  fs.writeFileSync(cfg, JSON.stringify({ targets: [{ id, url: realUrl, secret, timeout_ms: 5000 }] }));

  const st = spawnSync(absBin, ['webhook', 'status', '--db', db, '--config', cfg], { cwd: dir, encoding: 'utf8' });
  const rec = {
    configExit: st.status,
    configErr: st.status === 0 ? null : (st.stderr || '').trim().split('\n').pop(),
    delivered: null,
    err: null,
    rawEventIdBytes: null,
    hmacVerified: null,
  };
  if (st.status === 0 && deliver) {
    spawnSync(absBin, ['webhook', 'push', '--db', db, '--config', cfg], { cwd: dir, encoding: 'utf8', timeout: 60000 });
    await sleep(300);
    const rep = spawnSync(absBin, ['webhook', 'status', '--db', db, '--config', cfg], { cwd: dir, encoding: 'utf8' });
    let j = {};
    try { j = JSON.parse(rep.stdout); } catch {}
    const t0 = (j.targets || [])[0] || {};
    rec.delivered = t0.delivered ?? null;
    rec.err = t0.last_error ?? null;
    const seen = recv ? readSeen(recv.seenFile) : [];
    const hit = seen.find((s) => s.method === 'POST') || null;
    if (hit) {
      rec.rawEventIdBytes = hit.rawEventIdBytes;
      // The consumer's own check, run HERE: HMAC-SHA256 over the exact
      // received bytes, compared to the X-Reeflog-Signature header the
      // sender put on the wire. `verified` is a measurement, not a claim.
      if (hit.signatureHeader) {
        // The secret is hashed as UTF-8 bytes (reeflog signs with
        // `secret.as_bytes()`), and the body is hashed as the exact bytes
        // that arrived. Using latin1 for the key silently truncated a
        // multi-byte secret — that is what made the first run report
        // `verified=false` for a secret the product handles correctly.
        const mac = 'sha256=' + crypto
          .createHmac('sha256', Buffer.from(secret, 'utf8'))
          .update(Buffer.from(hit.body, 'latin1'))
          .digest('hex');
        rec.hmacVerified = mac === hit.signatureHeader;
      } else {
        rec.hmacVerified = false;
      }
    }
  }
  if (recv) {
    recv.child.kill();
    await sleep(50);
  }
  fs.rmSync(dir, { recursive: true, force: true });
  return rec;
}

// =========================================================================
// 1. id-bytes: which bytes of the target id reach the HTTP field value?
// =========================================================================
if (want('id')) {
  const cases = [
    { name: 'plain', id: 't1' },
    { name: 'leading-space', id: ' t1' },
    { name: 'trailing-space', id: 't1 ' },
    { name: 'internal-space', id: 'a b' },
    { name: 'trailing-tab', id: 't1\t' },
    { name: 'internal-tab', id: 'a\tb' },
    { name: 'non-ascii-umlaut', id: 'tü1' },
    { name: 'non-ascii-emoji', id: 't1🙂' },
    { name: 'control-cr', id: 'a\rb' },
  ];
  const out = [];
  for (const c of cases) {
    const r = await runCase({ id: c.id, url: 'http://127.0.0.1:PORT/hook' });
    const journalId = null; // the config is the journal's view here
    const raw = r.rawEventIdBytes;
    out.push({
      id: c.name,
      configuredId: c.id,
      journalId: r.configExit === 0 ? c.id : journalId,
      rawHeaderBytes: raw,
      rawHex: raw === null ? null : Buffer.from(raw, 'latin1').toString('hex'),
      delivered: r.delivered,
      lastError: r.err,
      consumerKeyMatchesJournal: raw === null ? null : raw === `${c.id}:event:1`,
    });
    console.log(
      `id/${c.name.padEnd(18)} delivered=${r.delivered} raw=${JSON.stringify(raw)} err=${JSON.stringify(r.err)}`
    );
  }
  fs.writeFileSync(path.join(OUT, 'id-bytes.json'), JSON.stringify(out, null, 2) + '\n');
  console.log('wrote id-bytes.json');
}

// =========================================================================
// 2. url-pos: is the refusal set position-dependent?
// =========================================================================
if (want('url')) {
  const hostSet = '"<>\\^`{|}';
  const probes = [...hostSet, '<', '>', '`', ' ', 'ü', '€'];
  const results = { when: new Date().toISOString(), positions: {} };
  for (const ch of probes) {
    for (const pos of ['HOST', 'PATH', 'QUERY', 'FRAGMENT']) {
      const url =
        pos === 'HOST' ? `http://h${ch}o.st:PORT/hook`
        : pos === 'PATH' ? `http://127.0.0.1:PORT/a${ch}b`
        : pos === 'QUERY' ? `http://127.0.0.1:PORT/hook?k=${ch}v`
        : `http://127.0.0.1:PORT/hook#${ch}`;
      const r = await runCase({ url });
      if (!results.positions[pos]) results.positions[pos] = [];
      results.positions[pos].push({
        char: ch,
        cp: ch.codePointAt(0),
        ok: r.delivered === 1,
        err: r.err,
        configErr: r.configExit === 0 ? null : r.configErr,
      });
    }
  }
  for (const [pos, rows] of Object.entries(results.positions)) {
    const refused = rows.filter((r) => !r.ok);
    console.log(`${pos}: ${rows.length - refused.length}/${rows.length} delivered`);
  }
  fs.writeFileSync(path.join(OUT, 'url-pos.json'), JSON.stringify(results, null, 2));
  console.log('wrote url-pos.json');
}

// =========================================================================
// 3. query-fragment: dense QUERY and FRAGMENT charset sweeps
// =========================================================================
if (want('query')) {
  const chars = [];
  for (let c = 0x20; c <= 0x7e; c++) chars.push(String.fromCharCode(c));
  for (let c = 0x00; c <= 0x1f; c++) chars.push(String.fromCharCode(c));
  chars.push('\u007f', 'ü');
  const out = { when: new Date().toISOString(), query: [], fragment: [] };
  for (const ch of chars) {
    const r = await runCase({ url: `http://127.0.0.1:PORT/hook?k=${ch}v` });
    out.query.push({ ch: ch.codePointAt(0), ok: r.delivered === 1, err: r.err, configErr: r.configErr ?? undefined });
  }
  for (const ch of chars) {
    const r = await runCase({ url: `http://127.0.0.1:PORT/hook#a${ch}b` });
    out.fragment.push({ ch: ch.codePointAt(0), ok: r.delivered === 1, err: r.err, configErr: r.configErr ?? undefined });
  }
  const qRef = out.query.filter((r) => !r.ok).length;
  const fRef = out.fragment.filter((r) => !r.ok).length;
  console.log(`QUERY: ${out.query.length - qRef}/${out.query.length} delivered; FRAGMENT: ${out.fragment.length - fRef}/${out.fragment.length} delivered`);
  fs.writeFileSync(path.join(OUT, 'query-fragment.json'), JSON.stringify(out, null, 2));
  console.log('wrote query-fragment.json');
}

// =========================================================================
// 4. authority-edge: ports, %, userinfo, brackets, scheme edges
// =========================================================================
if (want('authority')) {
  const cases = [
    ['bad port letters', 'http://127.0.0.1:abc/hook'],
    ['empty port', 'http://127.0.0.1:/hook'],
    ['port > 65535', 'http://127.0.0.1:99999/hook'],
    ['port 0', 'http://127.0.0.1:0/hook'],
    ['userinfo', 'http://u:***@LOCALHOST/hook'],
    ['userinfo %', 'http://us%40er@LOCALHOST/hook'],
    ['% in host', 'http://ho%73t:PORT/hook'],
    ['% alone in host', 'http://x%yz:PORT/hook'],
    ['ipv6', 'http://[::1]:PORT/hook'],
    ['bracket no port', 'http://[::1]:PORT/hook'],
    ['orphan bracket', 'http://[::1:PORT/hook'],
    ['double @', 'http://a@b@LOCALHOST/hook'],
    ['double colon host', 'http://a::b:PORT/hook'],
    ['question in authority', 'http://ho?st:PORT/hook'],
    ['space before scheme', ' http://LOCALHOST/hook'],
    ['trailing space', 'http://LOCALHOST/hook '],
    ['uppercase host', 'http://127.0.0.1:PORT/HOOK'],
    ['dot segments', 'http://LOCALHOST/a/../b'],
    ['star path', 'http://LOCALHOST/*'],
    ['percent-encoded path', 'http://LOCALHOST/caf%C3%A9'],
    ['percent-bad escape', 'http://LOCALHOST/%zz'],
    ['percent-encoded space path', 'http://LOCALHOST/a%20b'],
    ['query percent-bad', 'http://LOCALHOST/hook?k=%zz'],
    ['fragment then junk', 'http://LOCALHOST/hook#a b'],
    ['two fragments', 'http://LOCALHOST/hook#a#b'],
    ['query with hash value', 'http://LOCALHOST/hook?k=#frag'],
    ['no scheme', 'LOCALHOST/hook'],
    ['scheme only', 'http:LOCALHOST/hook'],
    ['backslash as separator', 'http:\\LOCALHOST\\hook'],
    ['file scheme', 'file:///etc/passwd'],
    ['ws scheme in config', 'ws://LOCALHOST/hook'],
  ];
  const out = { when: new Date().toISOString(), cases: [] };
  for (const [name, url] of cases) {
    const r = await runCase({ url });
    const verdict =
      r.configExit !== 0 ? 'CONFIG-REFUSED'
      : r.delivered === 1 ? 'delivered'
      : `not-delivered(${r.err || '?'})`;
    out.cases.push({ name, url, verdict, configErr: r.configErr ?? undefined, err: r.err ?? undefined });
    console.log(`${name.padEnd(28)} ${verdict}`);
  }
  fs.writeFileSync(path.join(OUT, 'authority-edge.json'), JSON.stringify(out, null, 2));
  console.log('wrote authority-edge.json');
}

// =========================================================================
// 5. dense: PATH printable ASCII + id C0 controls and DEL
// =========================================================================
if (want('dense')) {
  const urlDelivered = [], urlRefused = [];
  for (let c = 0x21; c <= 0x7e; c++) {
    const ch = String.fromCharCode(c);
    const r = await runCase({ url: `http://127.0.0.1:PORT/${ch}x` });
    const e = { cp: c, char: ch, configExit: r.configExit, delivered: r.delivered, err: r.err };
    (r.delivered === 1 ? urlDelivered : urlRefused).push(e);
  }
  const idDelivered = [], idRefused = [];
  for (let c = 0x00; c <= 0x1f; c++) {
    const ch = String.fromCharCode(c);
    const r = await runCase({ id: `a${ch}b`, url: 'http://127.0.0.1:PORT/hook' });
    const e = { cp: c, configExit: r.configExit, delivered: r.delivered, err: r.err };
    (r.delivered === 1 ? idDelivered : idRefused).push(e);
  }
  {
    const r = await runCase({ id: 'a\u007fb', url: 'http://127.0.0.1:PORT/hook' });
    const e = { cp: 0x7f, configExit: r.configExit, delivered: r.delivered, err: r.err };
    (r.delivered === 1 ? idDelivered : idRefused).push(e);
  }
  console.log(`dense path: ${urlDelivered.length} delivered / ${urlRefused.length} refused`);
  console.log(`dense id:   ${idDelivered.length} delivered / ${idRefused.length} refused`);
  fs.writeFileSync(
    path.join(OUT, 'dense.json'),
    JSON.stringify({ urlRefused, urlDelivered, idRefused, idDelivered }, null, 2) + '\n'
  );
  console.log('wrote dense.json');
}

// =========================================================================
// 6. secret: odd-byte secrets still deliver (the secret never hits the wire)
// =========================================================================
if (want('secret')) {
  const cases = [
    { secret: 's' },
    { secret: 'se\ncr\net' },
    { secret: 'a\u0001b' },
    { secret: 'sécret-ß' },
    { secret: '' },
  ];
  const out = [];
  for (const c of cases) {
    const r = await runCase({ url: 'http://127.0.0.1:PORT/hook', secret: c.secret });
    out.push({
      secret: c.secret,
      delivered: r.configExit === 0 ? r.delivered : null,
      verified: r.delivered === 1 && r.hmacVerified === true,
      note: c.secret === '' ? 'out of domain: the empty key authenticates nothing' : undefined,
    });
    console.log(
      `secret/${JSON.stringify(c.secret).padEnd(14)} delivered=${out[out.length - 1].delivered} hmacVerified=${r.hmacVerified} verified=${out[out.length - 1].verified}`
    );
  }
  fs.writeFileSync(path.join(OUT, 'secret.json'), JSON.stringify(out, null, 2) + '\n');
  console.log('wrote secret.json');
}

console.log(`\nprobe complete -> ${OUT}`);
