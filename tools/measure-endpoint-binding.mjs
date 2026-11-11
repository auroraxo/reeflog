#!/usr/bin/env node
// Probe: does reeflog's per-target delivery state follow the ENDPOINT or
// only the target ID?
//
//   node tools/measure-endpoint-binding.mjs <reeflog-bin> <out.json> [source-label]
//
// The source label names WHAT was measured ("published v0.7.14 release
// asset ..." / "tree build") so the artifact cannot quietly claim the build
// while measuring a branch, or the release while measuring /tmp — the pin
// test reads this field verbatim.
//
// The probe:
//   1. target `t1` -> receiver A. Push 3 events. A must receive 3.
//   2. SAME target id `t1`, SAME secret, DIFFERENT url -> receiver B.
//      Push again.
//   3. Report: what B received, what reeflog's report says, and its exit
//      code.
//
// A delivery state that is bound to the ENDPOINT cannot tell B from A and
// must refuse or reset. A state bound only to the id delivers nothing to B
// and still reports success.
import { spawnSync, spawn } from 'node:child_process';
import { createHash } from 'node:crypto';
import fs, { readFileSync } from 'node:fs';
import path from 'node:path';

const BIN = path.resolve(process.argv[2]);
const OUT = process.argv[3];
const SOURCE = process.argv[4] || 'tree build';
const DIR = fs.mkdtempSync('/tmp/reeflog-endpoint-');
// The helpers travel WITH the probe: the artifact must be reproducible from
// the file alone, not from files left lying in a scratch directory.
for (const helper of ['endpoint-binding-receiver.mjs', 'endpoint-binding-seed.cjs']) {
  fs.copyFileSync(path.join(path.dirname(new URL(import.meta.url).pathname), helper), path.join(DIR, helper));
}
const PROGRAM = 'whirLbMiicVdio4qvUfM5KAg6Ct8VwpYzGff3uctyCc';
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

function receiver(tag) {
  const seenFile = path.join(DIR, `seen-${tag}.json`);
  const portFile = seenFile + '.port';
  const child = spawn('node', [path.join(DIR, 'endpoint-binding-receiver.mjs'), seenFile], {
    stdio: 'ignore',
  });
  return {
    child,
    async port() {
      for (let i = 0; i < 200 && !fs.existsSync(portFile); i++) await sleep(25);
      return fs.readFileSync(portFile, 'utf8').trim();
    },
    seen() {
      try {
        return JSON.parse(fs.readFileSync(seenFile, 'utf8'));
      } catch {
        return [];
      }
    },
  };
}

const db = path.join(DIR, 'j.db');
const init = spawnSync(BIN, ['init', '--db', db, '--program', PROGRAM], { encoding: 'utf8' });
if (init.status !== 0) throw new Error('init failed: ' + init.stderr);
spawnSync('node', [path.join(DIR, 'endpoint-binding-seed.cjs'), db, '3'], { encoding: 'utf8' });

const A = receiver('A');
const portA = await A.port();
const B = receiver('B');
const portB = await B.port();

const cfgA = path.join(DIR, 'cfgA.json');
const cfgB = path.join(DIR, 'cfgB.json');
fs.writeFileSync(
  cfgA,
  JSON.stringify({ targets: [{ id: 't1', url: `http://127.0.0.1:${portA}/hook`, secret: 's' }] })
);
fs.writeFileSync(
  cfgB,
  JSON.stringify({ targets: [{ id: 't1', url: `http://127.0.0.1:${portB}/hook`, secret: 's' }] })
);

const push1 = spawnSync(BIN, ['webhook', 'push', '--db', db, '--config', cfgA], { encoding: 'utf8' });
const push2 = spawnSync(BIN, ['webhook', 'push', '--db', db, '--config', cfgB], { encoding: 'utf8' });
const status2 = spawnSync(BIN, ['webhook', 'status', '--db', db, '--config', cfgB], { encoding: 'utf8' });

const out = {
  artifact: 'endpoint-binding',
  source: SOURCE,
  method:
    "two live 200-OK receivers and the binary own report: t1 -> A (3 events), then the SAME id t1 repointed to B; verdict STARVED means A holds all three, B holds none, and the repointed push still exited 0",
  measuredAt: new Date().toISOString(),
  binaryVersion: (spawnSync(BIN, ['--version'], { encoding: 'utf8' }).stdout || '').trim(),
  binarySha256: createHash('sha256').update(readFileSync(BIN)).digest('hex'),
  push1: { exit: push1.status, stderr: (push1.stderr || '').trim() },
  push2: { exit: push2.status, stderr: (push2.stderr || '').trim(), stdout: push2.stdout },
  status2: { exit: status2.status, stdout: status2.stdout, stderr: (status2.stderr || '').trim() },
  receiverA: A.seen(),
  receiverB: B.seen(),
  receiverACount: A.seen().length,
  receiverBCount: B.seen().length,
};
out.verdict = out.receiverACount === 3 && out.receiverBCount === 0 ? 'STARVED' : 'other';
fs.writeFileSync(OUT, JSON.stringify(out, null, 2));

A.child.kill();
B.child.kill();
console.log(JSON.stringify({ ...out, receiverA: undefined, receiverB: undefined }, null, 2));
