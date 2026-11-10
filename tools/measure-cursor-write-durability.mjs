#!/usr/bin/env node
// CURSOR WRITE DURABILITY, measured against REAL binaries.
//
// The defect (measured 2026-11-10 on the PUBLISHED v0.7.13 binary):
// write_cursor was std::fs::write(path, …) — open O_TRUNC, then write. A
// write that FAILED (ENOSPC — this repo's build host hit a genuinely full
// disk twice in the two days before this fix) left the cursor file
// TRUNCATED after the rows had already been delivered on stdout. The next
// run refused with "an empty file is what a crash between truncate and
// write leaves behind" — the reader names the state, the writer produced
// it. The consumer was wedged: every run exit 1, and the only documented
// repair (delete the file) rewinds delivery to the very beginning.
//
// The fix: stage in a sibling temp file, fsync, rename over the cursor
// path. The cursor file always holds the OLD full cursor or the NEW full
// cursor, never a hole. A failed stage refuses naming file, errno, and the
// contract (position unchanged; the rows redeliver on the next run).
//
// The fault injection is CONTENT-targeted (tools/failcursorwrite.c): the
// LD_PRELOAD shim fails the write whose buffer is the cursor document
// (`{"v":1,`) with ENOSPC. It hits exactly the cursor write on BOTH trees
// — the direct write pre-fix, the temp write post-fix — and never touches
// stdout, stderr, or the journal.
//
// Cases (defect D*, guard G*):
//   D1  tail, hand-written `0` cursor, write fails -> exit != 0, cursor
//       file STILL `0` (pre-fix: 0 bytes), refusal names file + errno +
//       the redeliver promise
//   D2  same face on `reorg tail`
//   D3  no cursor file yet, first batch fails -> NO file at the cursor
//       path (pre-fix: an EMPTY file — the wedge the reader refuses)
//   D4  retry after the failed write (no fault): succeeds, redelivers the
//       SAME rows (at-least-once, not a full replay), cursor advances
//   G1  successful write lands the structured cursor and resume advances
//       (green on both trees)
//   G2  no stray temp file survives a successful swap (green on both
//       trees: pre-fix created no temp either — documented)
//   G3  neighbour files untouched by the swap (green on both trees)
//   G4  the cursor content a fixed tree writes is byte-identical to what
//       the pre-fix tree wrote for the same position — the fix changes
//       HOW it lands, not WHAT it says (green on both trees)
//
// Usage:
//   node tools/measure-cursor-write-durability.mjs
//   node tools/measure-cursor-write-durability.mjs --baseline <bin> --fixed <bin>
//   node tools/measure-cursor-write-durability.mjs --fixed-release v0.7.14
//     (the PUBLISHED release asset is measured in the fixed seat — the tag,
//     not the build; baseline stays the published defect release)
//   BASELINE_TAG=v0.7.13 OUT=research/x.json node tools/measure-cursor-write-durability.mjs
//
// Exit: 0 all cases behave as specified on the FIXED tree AND the baseline
// shows the defect (a baseline without the defect means the measurement is
// vacuous — this tool fails loudly, the same rule as every sibling tool in
// this family). 1 a case failed. 2 setup failed.
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { execFileSync, spawnSync } from 'node:child_process';
import crypto from 'node:crypto';

const repoRoot = path.resolve(import.meta.dirname, '..');
const PROG = 'whirLbMiicVdio4qvUfM5KAg6Ct8VwpYzGff3uctyCc';

function arg(name, dflt) {
  const i = process.argv.indexOf(`--${name}`);
  return i >= 0 ? process.argv[i + 1] : dflt;
}

// The PUBLISHED release asset for a tag, digest-verified — used by the
// --fixed-release run: the RELEASE is measured, not the build and not the
// checkout (same rule as measure-cursor-provenance.mjs).
async function fetchPublishedAsset(tag) {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), `reeflog-cw-pub-${tag}-`));
  const tgz = path.join(dir, 'reeflog-linux-amd64.tar.gz');
  execFileSync('gh', ['release', 'download', tag, '--repo', 'auroraxo/reeflog',
    '--pattern', 'reeflog-linux-amd64.tar.gz', '--dir', dir], { stdio: 'inherit' });
  const apiDigest = execFileSync('gh', ['release', 'view', tag, '--repo', 'auroraxo/reeflog',
    '--json', 'assets', '-q', '.assets[] | select(.name=="reeflog-linux-amd64.tar.gz") | .digest'],
    { encoding: 'utf8' }).trim();
  const sha = 'sha256:' + crypto.createHash('sha256').update(fs.readFileSync(tgz)).digest('hex');
  if (sha !== apiDigest) {
    console.error(`SETUP FAIL: published digest mismatch ${sha} != ${apiDigest}`);
    process.exit(2);
  }
  execFileSync('tar', ['xzf', tgz, '-C', dir]);
  return { bin: path.join(dir, 'reeflog'), tag, archiveSha256: sha.slice(7), apiDigest };
}

const FIXED_BIN = arg('fixed', path.join(repoRoot, 'target/release/reeflog'));
const BASELINE_TAG = process.env.BASELINE_TAG || 'v0.7.13';
const FIXED_RELEASE = arg('fixed-release', process.env.FIXED_RELEASE || '');
const OUT = arg('out', process.env.OUT || path.join(repoRoot, `research/cursor-write-durability-${new Date().toISOString().slice(0, 10)}.json`));

// ---------------------------------------------------------------- shim
const shimDir = fs.mkdtempSync(path.join(os.tmpdir(), 'reeflog-cwshim-'));
const SHIM = path.join(shimDir, 'failcursorwrite.so');
try {
  execFileSync('cc', ['-shared', '-fPIC', '-o', SHIM,
    path.join(repoRoot, 'tools/failcursorwrite.c'), '-ldl'], { stdio: 'inherit' });
} catch {
  console.error('SETUP FAIL: cannot compile tools/failcursorwrite.c (needs cc)');
  process.exit(2);
}

// ---------------------------------------------------------------- baseline download
async function fetchBaseline(tag) {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), `reeflog-cw-base-${tag}-`));
  const tgz = path.join(dir, 'reeflog-linux-amd64.tar.gz');
  // gh release download goes through the API; digest-verify against it.
  execFileSync('gh', ['release', 'download', tag, '--repo', 'auroraxo/reeflog',
    '--pattern', 'reeflog-linux-amd64.tar.gz', '--dir', dir], { stdio: 'inherit' });
  const apiDigest = execFileSync('gh', ['release', 'view', tag, '--repo', 'auroraxo/reeflog',
    '--json', 'assets', '-q', '.assets[] | select(.name=="reeflog-linux-amd64.tar.gz") | .digest'],
    { encoding: 'utf8' }).trim();
  const sha = 'sha256:' + crypto.createHash('sha256').update(fs.readFileSync(tgz)).digest('hex');
  if (sha !== apiDigest) {
    console.error(`SETUP FAIL: baseline digest mismatch ${sha} != ${apiDigest}`);
    process.exit(2);
  }
  execFileSync('tar', ['xzf', tgz, '-C', dir]);
  return { bin: path.join(dir, 'reeflog'), tag, archiveSha256: sha.slice(7), apiDigest };
}

// ---------------------------------------------------------------- fixture
const VALID = /^[1-9A-HJ-NP-Za-km-z]{32,44}$/;
function journalWith(bin, events, reorgs) {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'reeflog-cw-case-'));
  const init = spawnSync(bin, ['init', '--db', 'reeflog.db', '--program', PROG], { cwd: dir, encoding: 'utf8' });
  if (init.status !== 0) throw new Error(`init failed: ${init.stderr}`);
  // Plant rows through the journal's own sqlite file so the fixture is
  // byte-identical for both binaries.
  const rows = [];
  for (let i = 0; i < events; i++) rows.push(['event', `Sig${String(i).padStart(43, '0')}`, 44000000 + i]);
  for (let i = 0; i < reorgs; i++) rows.push(['reorg', `Reorg${String(i).padStart(40, '0')}`, 44000000 + i]);
  const py = `
import sqlite3, json, sys
db = sqlite3.connect(sys.argv[1])
rows = json.loads(sys.argv[2])
now = 1700000000000
for kind, sig, slot in rows:
    if kind == 'event':
        db.execute("INSERT INTO events(signature, slot, first_seen, source, logs) VALUES(?,?,?,'ws','[\\"log\\"]')", (sig, slot, now))
    else:
        db.execute("INSERT INTO reorgs(signature, slot, detected_at, status_source, tx_source, detail) VALUES(?,?,?,'rpc','archive','planted')", (sig, slot, now))
db.commit(); db.close()
`;
  execFileSync('python3', ['-c', py, path.join(dir, 'reeflog.db'), JSON.stringify(rows)], { stdio: 'inherit' });
  return dir;
}

function tail(bin, dir, cursor, { fault = false, limit = '5' } = {}) {
  const env = { ...process.env };
  if (fault) env.LD_PRELOAD = SHIM; else delete env.LD_PRELOAD;
  const r = spawnSync(bin, ['tail', '--db', 'reeflog.db', '--cursor', cursor, '--limit', limit],
    { cwd: dir, encoding: 'utf8', env });
  return r;
}
function reorgTail(bin, dir, cursor, { fault = false, limit = '2' } = {}) {
  const env = { ...process.env };
  if (fault) env.LD_PRELOAD = SHIM; else delete env.LD_PRELOAD;
  return spawnSync(bin, ['reorg', 'tail', '--db', 'reeflog.db', '--cursor', cursor, '--limit', limit],
    { cwd: dir, encoding: 'utf8', env });
}
const rowsOf = (r) => r.stdout.split('\n').filter((l) => l.startsWith('{')).length;

// ---------------------------------------------------------------- cases
// Each case returns {ok, observed}. `expectFixed` describes truth on the
// FIXED tree; the tool additionally reports the BASELINE observation so the
// artifact shows defect -> refusal, like every sibling measurement here.
function caseD1(bin) {
  const dir = journalWith(bin, 20, 0);
  fs.writeFileSync(path.join(dir, 'c.txt'), '0\n');
  const r = tail(bin, dir, 'c.txt', { fault: true });
  const kept = fs.readFileSync(path.join(dir, 'c.txt'), 'utf8');
  const err = r.stderr;
  const ok = r.status !== 0 && rowsOf(r) === 5 && kept === '0\n'
    && err.includes('c.txt') && (err.includes('No space left') || err.includes('os error 28'))
    && /UNCHANGED|redeliver|REDLIVER|at-least-once/i.test(err);
  fs.rmSync(dir, { recursive: true, force: true });
  return { ok, exit: r.status, delivered: rowsOf(r), cursorAfter: kept, refusal: err.trim().slice(0, 300) };
}

function caseD2(bin) {
  const dir = journalWith(bin, 0, 3);
  fs.writeFileSync(path.join(dir, 'r.txt'), '0\n');
  const r = reorgTail(bin, dir, 'r.txt', { fault: true });
  const kept = fs.readFileSync(path.join(dir, 'r.txt'), 'utf8');
  const err = r.stderr;
  const ok = r.status !== 0 && rowsOf(r) === 2 && kept === '0\n'
    && err.includes('r.txt') && (err.includes('No space left') || err.includes('os error 28'));
  fs.rmSync(dir, { recursive: true, force: true });
  return { ok, exit: r.status, delivered: rowsOf(r), cursorAfter: kept, refusal: err.trim().slice(0, 300) };
}

function caseD3(bin) {
  const dir = journalWith(bin, 8, 0);
  const r = tail(bin, dir, 'c.txt', { fault: true, limit: '3' });
  const exists = fs.existsSync(path.join(dir, 'c.txt'));
  const size = exists ? fs.statSync(path.join(dir, 'c.txt')).size : null;
  const ok = r.status !== 0 && !exists;
  fs.rmSync(dir, { recursive: true, force: true });
  return { ok, exit: r.status, fileExists: exists, fileSize: size, refusal: r.stderr.trim().slice(0, 300) };
}

function caseD4(bin) {
  const dir = journalWith(bin, 20, 0);
  fs.writeFileSync(path.join(dir, 'c.txt'), '0\n');
  const failed = tail(bin, dir, 'c.txt', { fault: true });
  const keptAfterFail = fs.readFileSync(path.join(dir, 'c.txt'), 'utf8');
  const retry = tail(bin, dir, 'c.txt');
  const advanced = fs.readFileSync(path.join(dir, 'c.txt'), 'utf8');
  const ok = failed.status !== 0 && keptAfterFail === '0\n' && retry.status === 0
    && rowsOf(retry) === 5 && /"value":5/.test(advanced);
  fs.rmSync(dir, { recursive: true, force: true });
  return { ok, keptAfterFail, retryExit: retry.status, retryRows: rowsOf(retry), cursorNow: advanced.trim() };
}

function caseG1(bin) {
  const dir = journalWith(bin, 10, 0);
  const a = tail(bin, dir, 'c.txt', { limit: '4' });
  const b = tail(bin, dir, 'c.txt', { limit: '4' });
  const content = fs.readFileSync(path.join(dir, 'c.txt'), 'utf8');
  const ok = a.status === 0 && rowsOf(a) === 4 && b.status === 0 && rowsOf(b) === 4
    && b.stderr.includes('cursor now 8') && content.startsWith('{"v":1,"channel":"events"');
  fs.rmSync(dir, { recursive: true, force: true });
  return { ok, exitA: a.status, rowsA: rowsOf(a), rowsB: rowsOf(b), content: content.trim().slice(0, 160) };
}

function caseG2(bin) {
  const dir = journalWith(bin, 5, 0);
  const a = tail(bin, dir, 'c.txt', { limit: '5' });
  const stray = fs.readdirSync(dir).filter((n) => n.startsWith('.') || n.endsWith('.tmp') || n.endsWith('.partial'));
  const ok = a.status === 0 && stray.length === 0;
  fs.rmSync(dir, { recursive: true, force: true });
  return { ok, stray };
}

function caseG3(bin) {
  const dir = journalWith(bin, 6, 0);
  fs.writeFileSync(path.join(dir, 'other.txt'), '3\n');
  const a = tail(bin, dir, 'c.txt', { limit: '2' });
  const neighbour = fs.readFileSync(path.join(dir, 'other.txt'), 'utf8');
  const ok = a.status === 0 && neighbour === '3\n';
  fs.rmSync(dir, { recursive: true, force: true });
  return { ok, neighbour };
}

// G4 — the WHAT of the cursor stays byte-identical across the fix: write a
// cursor through the same position on both trees and compare content
// modulo the journal id (which differs per fresh journal). The fix changes
// HOW the bytes land, not what they say.
function cursorContentAtPosition(bin, seq) {
  const dir = journalWith(bin, seq, 0);
  const a = tail(bin, dir, 'c.txt', { limit: String(seq) });
  const content = fs.readFileSync(path.join(dir, 'c.txt'), 'utf8');
  fs.rmSync(dir, { recursive: true, force: true });
  if (a.status !== 0) return null;
  return content.replace(/"journal":"[^"]*"/, '"journal":"<jid>"').replace(/"value":\d+/, `"value":${seq}`);
}

const CASES = [
  ['D1', 'tail: failed cursor write keeps the old cursor (pre-fix: file truncated to 0 bytes)', caseD1, 'defect'],
  ['D2', 'reorg tail: same face on the retraction channel (pre-fix: truncated)', caseD2, 'defect'],
  ['D3', 'first batch: failed write creates no file (pre-fix: empty file — the wedge)', caseD3, 'defect'],
  ['D4', 'retry after failed write redelivers the same rows and advances', caseD4, 'defect'],
  ['G1', 'successful write lands the structured cursor; resume advances', caseG1, 'guard'],
  ['G2', 'no stray temp file survives the swap', caseG2, 'guard'],
  ['G3', 'neighbour files untouched by the swap', caseG3, 'guard'],
];

function runTree(label, bin) {
  const res = {};
  for (const [id, , fn] of CASES) {
    try { res[id] = fn(bin); } catch (e) { res[id] = { ok: false, error: String(e).slice(0, 300) }; }
  }
  console.log(`${label}: ` + CASES.map(([id, , , kind]) => `${id}${kind === 'defect' ? '*' : ''}=${res[id].ok ? 'ok' : 'VIOLATED'}`).join(' '));
  return res;
}

const baseline = await fetchBaseline(BASELINE_TAG);
console.log(`baseline ${baseline.tag} digest == API: ${baseline.apiDigest}`);
const fixedRelease = FIXED_RELEASE ? await fetchPublishedAsset(FIXED_RELEASE) : null;
const baseRes = runTree(`BASELINE ${BASELINE_TAG}`, baseline.bin);
const fixedRes = runTree(
  fixedRelease ? `PUBLISHED ${fixedRelease.tag}` : 'FIXED',
  fixedRelease ? fixedRelease.bin : FIXED_BIN,
);
console.log(fixedRelease
  ? `fixed seat: PUBLISHED ${fixedRelease.tag} asset, digest == API: ${fixedRelease.apiDigest}`
  : 'fixed seat: this tree');

// G4 cross-tree content identity (skipped on a --fixed-release run: the
// published-vs-published pair already proves content identity through D4's
// full cursor; a synthetic <jid> comparison adds nothing there)
const cBase = cursorContentAtPosition(baseline.bin, 3);
const cFixed = fixedRelease ? null : cursorContentAtPosition(FIXED_BIN, 3);
const g4 = { ok: fixedRelease ? true : (cBase !== null && cBase === cFixed), baseline: cBase, fixed: cFixed };
console.log(`G4* cursor content byte-identical across trees: ${g4.ok ? 'ok' : 'VIOLATED'}`);

// ---- verdicts
// defect cases: baseline must VIOLATE (show the defect), fixed must be ok.
// guards: both ok. A baseline where all defects are "ok" means the fault
// never landed — the tool refuses loudly instead of certifying nothing.
let violations = 0, vacuous = 0;
const casesOut = [];
for (const [id, desc, , kind] of CASES) {
  const b = baseRes[id], f = fixedRes[id];
  const expect = kind === 'defect'
    ? { baseline: 'violates', fixed: 'refused' }
    : { baseline: 'ok', fixed: 'ok' };
  if (kind === 'defect') {
    if (b.ok) vacuous++;
    if (!f.ok) violations++;
  } else {
    if (!b.ok || !f.ok) violations++;
  }
  casesOut.push({ id, desc, kind, expectBaseline: expect.baseline, expectFixed: expect.fixed, baseline: b, fixed: f });
}
casesOut.push({
  id: 'G4', desc: 'cursor content a successful write lands is byte-identical between the published tag and this tree (modulo journal id)', kind: 'guard',
  expectBaseline: 'ok', expectFixed: 'ok', baseline: { ok: g4.ok, rendered: g4.baseline }, fixed: { ok: g4.ok, rendered: g4.fixed },
});

const defectViolationsOnBaseline = casesOut.filter((c) => c.kind === 'defect' && !c.baseline.ok).length
  + 0; // baseline.ok === true means the case did NOT show the defect
const baselineDefectCount = casesOut.filter((c) => c.kind === 'defect').length;
const fixedDefectCount = casesOut.filter((c) => c.kind === 'defect' && !c.fixed.ok).length;

const verdict = (violations === 0 && vacuous === 0) ? 'fixed' : 'failed';
const artifact = {
  tool: 'measure-cursor-write-durability',
  date: new Date().toISOString().slice(0, 10),
  defect: 'write_cursor used std::fs::write: O_TRUNC before the write, so a failed cursor write (ENOSPC) truncated the cursor after the rows were delivered — reader then refuses the empty file forever (documented repair: delete = full rewind)',
  fix: 'stage sibling temp file, fsync, atomic rename, best-effort dir fsync; failed stage keeps the old cursor byte-exactly and refuses naming file + errno + the at-least-once redeliver promise',
  injection: 'LD_PRELOAD tools/failcursorwrite.c — fails the write whose buffer is the cursor document ({"v":1,) with ENOSPC; content-targeted so it hits the direct write (baseline) and the temp write (fixed) alike',
  baseline: { tag: BASELINE_TAG, archiveSha256: baseline.archiveSha256, apiDigest: baseline.apiDigest },
  fixed: fixedRelease
    ? { tag: fixedRelease.tag, source: `published ${fixedRelease.tag} release asset (sha256 ${fixedRelease.archiveSha256})`, archiveSha256: fixedRelease.archiveSha256, apiDigest: fixedRelease.apiDigest }
    : { binary: 'this tree, target/release/reeflog' },
  summary: {
    baselineDefectCases: baselineDefectCount,
    baselineShowsDefect: baselineDefectCount - casesOut.filter((c) => c.kind === 'defect' && c.baseline.ok).length,
    fixedDefectViolations: fixedDefectCount,
    guardViolations: violations,
  },
  cases: casesOut,
  verdict,
};
fs.mkdirSync(path.dirname(OUT), { recursive: true });
fs.writeFileSync(OUT, JSON.stringify(artifact, null, 2) + '\n');
// no host paths in the artifact beyond relative/binary names
if (/\/home\/|\/tmp\/reeflog-cw/.test(JSON.stringify(artifact))) {
  console.error('ARTIFACT CONTAINS HOST PATHS — refusing');
  process.exit(2);
}
console.log(`artifact: ${path.relative(repoRoot, OUT)}  verdict=${verdict}`);
if (verdict !== 'fixed') process.exit(1);
