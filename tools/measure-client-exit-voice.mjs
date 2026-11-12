#!/usr/bin/env node
// The EXIT-CODE VOICE measurement (PULS W, 2026-11-12) — MEASURED, not
// inferred.
//
// The defect: the Node client's `run()` knew only the codes the product's
// contract defines (0 delivered, 1 refused, 3 held) and treated every other
// close as SUCCESS. Measured against the PUBLISHED v0.7.19 tree:
//
//   F1 crash-with-rows — a tail that prints its rows and then dies with the
//      binary's journal-write failure code (src/main.rs exits 2 there)
//      resolved as a delivered batch: `held: null` is documented as "the
//      batch is fully delivered";
//   F1b same code on status — the JSON command resolved with a REPORT THAT
//      IS NOT A REPORT (the row the fake printed);
//   F2 usage-skew      — an old binary that does not know the invocation
//      exits 64 (usage): `tail()` resolved as a clean zero-event batch;
//   F2b same on status — JSON.parse ate the empty body and threw a RAW
//      SyntaxError out of the client's promise;
//   F3 panic-report    — a scan that printed its report and then panicked
//      (101) before its own verdict resolved as a CLEAN scan;
//   F4 killed          — a child killed by a signal leaves close code null,
//      and `code ?? 1` minted a forged "exit 1" refusal naming no signal.
//
// The guards (they must hold on BOTH trees):
//   G1 healthy exit 0 resolves;
//   G2 exit 1 keeps the binary's words;
//   G3a tail exit 3 keeps the hold shape;
//   G3b verify exit 3 throws INCOMPLETE.
//
// The DEFECT is what the baseline shows; the FIX is what this tree shows.
// Each face has a predicate for BOTH sides, evaluated here — a face whose
// predicate is not decisive on the baseline would certify nothing.
//
//   node tools/measure-client-exit-voice.mjs
//   TREE=/path/to/tree OUT=/tmp/x.json TAG_LABEL=v0.7.19 node tools/measure-client-exit-voice.mjs
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { pathToFileURL } from 'node:url';

const TREE = process.env.TREE || path.resolve(import.meta.dirname, '..');
const OUT = process.env.OUT || null;
const TAG_LABEL = process.env.TAG_LABEL || 'this-tree';

const { reeflog, ReeflogError } = await import(pathToFileURL(path.join(TREE, 'index.js')).href);

const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'reeflog-exit-voice-meas-'));
const db = path.join(dir, 'j.db');

function fakeBin(name, body) {
  const p = path.join(dir, name);
  fs.writeFileSync(p, `#!/bin/sh\n${body}\n`, { mode: 0o755 });
  return p;
}

const TAIL_ROW = '{"seq":1,"signature":"sigA","slot":1,"err":null,"logs":["a"]}';

const BINS = {
  f1: fakeBin('f1.sh', `echo '${TAIL_ROW}'\necho "reeflog: journal write failed: disk i/o error" >&2\nexit 2`),
  f2: fakeBin('f2.sh', `echo "reeflog v0 - durable local mailbox" >&2\necho "A flag this command does not know is refused" >&2\nexit 64`),
  f3: fakeBin('f3.sh', `echo '{"verified":true,"missing":[]}'\necho "thread 'main' panicked at src/journal.rs:42" >&2\nexit 101`),
  f4: fakeBin('f4.sh', `echo '{"db":"x"}'\nkill -TERM $$\nsleep 5`),
  g1: fakeBin('g1.sh', `echo '{"db":"x","events":0}'\nexit 0`),
  g2: fakeBin('g2.sh', `echo 'reeflog: error: no journal at ${db} - run reeflog init first' >&2\nexit 1`),
  g3t: fakeBin('g3t.sh', `echo '${TAIL_ROW}'\necho "reeflog: HELD at seq 2 sigB: the account envelope is still owed" >&2\nexit 3`),
  g3v: fakeBin('g3v.sh', `echo "reeflog: scan INCOMPLETE" >&2\nexit 3`),
};

/** Run one face and describe the OUTCOME, never the exception type alone:
 *  a raw engine crash (SyntaxError etc.) is a DIFFERENT defect class from a
 *  refusal in the product's voice, and the two must not be lumped. */
async function face(call) {
  try {
    const value = await call();
    return {
      outcome: 'resolved',
      detail: JSON.stringify(value).slice(0, 200).replaceAll(dir, '<tmpdir>'),
      exitCode: value && typeof value.exitCode === 'number' ? value.exitCode : null,
    };
  } catch (e) {
    const isProduct = e instanceof ReeflogError;
    const firstLine = String(e.message).split('\n').find((l) => l.trim()) || '';
    return {
      outcome: isProduct ? 'refused' : 'raw-crash',
      message: firstLine.slice(0, 240).replaceAll(dir, '<tmpdir>'),
      exitCode: isProduct && typeof e.exitCode === 'number' ? e.exitCode : null,
    };
  }
}

const faces = [];
async function record(mode, call) {
  faces.push({ mode, ...(await face(call)) });
}

const box = (bin) => reeflog({ db, bin });

await record('F1 tail crash-after-rows (exit 2)', () => box(BINS.f1).tail({ limit: 10 }));
await record('F1b status crash-after-rows (exit 2)', () => box(BINS.f1).status());
await record('F2 tail usage-skew (exit 64)', () => box(BINS.f2).tail({ limit: 10 }));
await record('F2b status usage-skew (exit 64)', () => box(BINS.f2).status());
await record('F3 verify panic-after-report (exit 101)', () => box(BINS.f3).verify({}));
await record('F4 status killed-by-signal', () => box(BINS.f4).status());
await record('G1 healthy exit 0', () => box(BINS.g1).status());
await record('G2 exit 1 refusal', () => box(BINS.g2).status());
await record('G3a tail exit 3 hold', () => box(BINS.g3t).tail({ limit: 10 }));
await record('G3b verify exit 3 incomplete', () => box(BINS.g3v).verify({}));

const byMode = Object.fromEntries(faces.map((f) => [f.mode, f]));

/** For every face: the predicate for the BASELINE (the defect is visible)
 *  and the predicate for the FIX (the defect is gone). A face whose
 *  baseline predicate is vacuous is a measurement bug, not a catch. */
const FACES = [
  {
    mode: 'F1 tail crash-after-rows (exit 2)',
    defect: (f) => f.outcome === 'resolved' && f.exitCode === 2,
    fixed: (f) => f.outcome === 'refused' && f.exitCode === 2 && /journal write failed/.test(f.message || ''),
  },
  {
    mode: 'F1b status crash-after-rows (exit 2)',
    defect: (f) => f.outcome === 'resolved',
    fixed: (f) => f.outcome === 'refused' && f.exitCode === 2,
  },
  {
    mode: 'F2 tail usage-skew (exit 64)',
    defect: (f) => f.outcome === 'resolved' && f.exitCode === 64,
    fixed: (f) => f.outcome === 'refused' && f.exitCode === 64 && /version skew|does not know/.test(f.message || ''),
  },
  {
    mode: 'F2b status usage-skew (exit 64)',
    defect: (f) => f.outcome === 'raw-crash',
    fixed: (f) => f.outcome === 'refused' && f.exitCode === 64,
  },
  {
    mode: 'F3 verify panic-after-report (exit 101)',
    defect: (f) => f.outcome === 'resolved',
    fixed: (f) => f.outcome === 'refused' && f.exitCode === 101 && /panicked/.test(f.message || ''),
  },
  {
    // The baseline FORGED a refusal here ("exit 1") — a refusal that names
    // no signal and discards stderr. The defect is a refusal without the
    // truth, so the fixed side must name the signal.
    mode: 'F4 status killed-by-signal',
    defect: (f) => f.outcome === 'refused' && !/signal/i.test(f.message || ''),
    fixed: (f) => f.outcome === 'refused' && /SIGTERM/.test(f.message || ''),
  },
];

const GUARDS = [
  { mode: 'G1 healthy exit 0', holds: (f) => f.outcome === 'resolved' },
  { mode: 'G2 exit 1 refusal', holds: (f) => f.outcome === 'refused' && f.exitCode === 1 && /no journal at/.test(f.message || '') },
  { mode: 'G3a tail exit 3 hold', holds: (f) => f.outcome === 'resolved' && f.exitCode === 3 },
  { mode: 'G3b verify exit 3 incomplete', holds: (f) => f.outcome === 'refused' && f.exitCode === 3 },
];

const faceResults = FACES.map((spec) => {
  const f = byMode[spec.mode];
  return {
    mode: spec.mode,
    outcome: f ? f.outcome : null,
    exitCode: f ? f.exitCode : null,
    message: f ? (f.message || '') : '',
    detail: f ? (f.detail || '') : '',
  };
});

const guardResults = GUARDS.map((spec) => {
  const f = byMode[spec.mode];
  return {
    mode: spec.mode,
    outcome: f ? f.outcome : null,
    exitCode: f ? f.exitCode : null,
    message: f ? (f.message || '') : '',
    holds: Boolean(f) && spec.holds(f),
  };
});

const report = {
  measuredAt: new Date().toISOString(),
  tag: TAG_LABEL,
  faces,
  faceResults,
  guardResults,
  guardsHold: guardResults.every((g) => g.holds),
};
fs.rmSync(dir, { recursive: true, force: true });
if (OUT) fs.writeFileSync(OUT, `${JSON.stringify(report, null, 2)}\n`);
console.log(JSON.stringify(report, null, 2));
