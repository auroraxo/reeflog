#!/usr/bin/env node
// Measure `reeflog start --seconds N`: does the run END near its deadline,
// and does its EXIT CODE tell the truth about what it left owed?
//
// WHY (measured 2026-11-20, on PUBLISHED bytes): the live user-zero harness
// (`tools/e2e-registry-install-live.mjs`) demanded `exit === 0` from the
// daemon. On the published v0.7.44 asset the run ended with exit 3. Two
// readings were possible and both had to be separated by measurement:
//   (a) honest — the run ends on time and NAMES the debt it could not pay
//       (exit 3 = "reported and held", the repo's own vocabulary, documented
//       since v0.7.31: "the daemon exits 3 at shutdown while the payload
//       hydrate is incomplete"), and the harness expectation is stale;
//   (b) broken — the new shutdown budget broke the run.
// The answer is (a), and it is worth pinning because the SAME harness
// expectation passed on v0.7.43 only because that binary OVERRAN its own
// deadline: measured live, v0.7.43 took 223 s for a 120 s run and exited 0
// — the exit code was honest about its state only because the walk was
// allowed to keep paying the debt past the deadline the caller asked for.
//
// So this instrument measures FOUR things on the SAME command, on both
// published assets, against the SAME mainnet-beta program:
//
//   D1 the deadline: the fixed side ends inside its own budget
//      (seconds + the shutdown budget the fix grants), the baseline does
//      not — the overrun the v0.7.44 fix ships;
//   D2 the exit VOICE: the fixed side never exits 0 while a payload debt
//      is owed, and when it exits 3 it names the repair on stderr
//      ("payload hydrate INCOMPLETE ... run `reeflog hydrate`");
//   G1 the FALSE-POSITIVE GUARD: on a quiet program the fixed side ends
//      near its deadline AND exits 0 with no debt — the fix must not turn
//      every run into exit 3;
//   G2 the baseline's silence: the baseline reports health (exit 0) only
//      AFTER spending far more than the caller asked for — measured in
//      BOTH forms now: a late exit 0 (207 s / 223 s wall for a 120 s run)
//      and, at a 240 s ceiling, no return at all (killed by the
//      instrument — the ceiling's SIGTERM, not a product verdict). The
//      defect is the deadline; the exit code only decides which form the
//      silence takes.
//
// Usage:
//   BASELINE=<reeflog> BIN=<reeflog> OUT=<json> SECONDS=120 \
//     node tools/measure-start-deadline-exit.mjs
//
// Exit: 0 every face holds, 1 a defect face is violated, 2 no verdict.

import { spawn, spawnSync } from 'node:child_process';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import crypto from 'node:crypto';

const BIN = process.env.BIN || path.resolve(import.meta.dirname, '..', 'target', 'release', 'reeflog');
const BASELINE = process.env.BASELINE ? path.resolve(process.env.BASELINE) : null;
const OUT = process.env.OUT || null;
const SECONDS = Number(process.env.SECONDS || 120);
const QUIET_SECONDS = Number(process.env.QUIET_SECONDS || 60);
const BUSY_PROGRAM = 'whirLbMiicVdio4qvUfM5KAg6Ct8VwpYzGff3uctyCc';
const QUIET_PROGRAM = 'metaqbxxUerdq28cj1RbAWkYQm3ybzjb6a8bt518x1s';
// The shutdown budget the v0.7.44 fix grants the final passes: seconds/5,
// clamped to [2, 30] (see `shutdown_repair_budget` in src/main.rs).
const shutdownBudgetMs = (s) => Math.min(Math.max(Math.floor(s / 5), 2), 30) * 1000;
// Grace for process spawn, the WS connect and the final `status` read —
// the same bound the published pin uses (`tests/shutdown_overrun_published_v0743.rs`:
// "ms < (seconds + 15) * 1000"). The contract is "ends near N", not
// "ends inside N + budget to the millisecond".
const GRACE_MS = 15000;

if (!fs.existsSync(BIN)) {
  console.error(`no reeflog binary at ${BIN}`);
  process.exit(2);
}

function sha256(file) {
  return crypto.createHash('sha256').update(fs.readFileSync(file)).digest('hex');
}

function versionOf(bin) {
  try {
    return spawnSync(bin, ['--version'], { encoding: 'utf8' }).stdout.trim();
  } catch {
    return null;
  }
}

// One real run against mainnet-beta: the wall-clock, the exit code, the
// journal's end-state debt and the stderr verdict.
//
// CEILING: a run that never returns is itself the defect, so the instrument
// bounds every run. The ceiling is the requested seconds plus a generous
// margin — far beyond what the fixed side needs (it ends inside
// seconds + its shutdown budget) and far below the baseline's real overrun
// (measured live: 223 s for a 120 s run; 28:11 in the first live
// measurement). A run that hits the ceiling did not return on its own: it
// is recorded as `killed`, and its exit code is the one the SIGTERM
// produced, NOT the product's verdict — the artifact says so explicitly.
async function runOnce(bin, { label, seconds, program, ceilingMs }) {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), `reeflog-deadline-${label}-`));
  const db = path.join(dir, 'j.db');
  spawnSync(bin, ['init', '--db', db, '--program', program], { stdio: 'ignore' });
  const started = Date.now();
  const p = spawn(bin, ['start', '--db', db, '--seconds', String(seconds)],
    { stdio: ['ignore', 'pipe', 'pipe'] });
  let stdout = '';
  let stderr = '';
  p.stdout.on('data', d => { stdout += d; });
  p.stderr.on('data', d => { stderr += d; });
  let killed = false;
  const killTimer = setTimeout(() => { killed = true; p.kill('SIGTERM'); }, ceilingMs);
  const exitCode = await new Promise(r => p.on('close', c => r(c)));
  clearTimeout(killTimer);
  const wallMs = Date.now() - started;

  // The journal's end state, read through the product's own health report.
  const st = spawnSync(bin, ['status', '--db', db], { encoding: 'utf8' });
  let status = null;
  try { status = JSON.parse(st.stdout); } catch { status = null; }
  const statusExit = st.status;

  const namesDebt = /payload hydrate INCOMPLETE/.test(stderr);
  const namesRepair = /run `reeflog hydrate`/.test(stderr);
  fs.rmSync(dir, { recursive: true, force: true });
  return {
    label,
    binVersion: versionOf(bin),
    binSha256: sha256(bin),
    requestedSeconds: seconds,
    ceilingMs,
    wallMs,
    exitCode,
    // A killed run has no verdict of its own: the exit code belongs to the
    // SIGTERM, not to the product.
    killed,
    // Did the run END on its own? (killed = it never came back)
    returnedOnItsOwn: !killed,
    payloadPending: status ? status.payload_pending : null,
    statusExit,
    statusComplete: status ? status.complete : null,
    namesDebt,
    namesRepair,
    // The debt the run left behind, as the product itself reports it.
    endStateDebt: status ? (status.payload_pending || 0) > 0 : null,
  };
}

const faces = [];
const record = (name, ok, detail) => { faces.push({ name, ok, detail }); };

const CEILING_MS = Number(process.env.CEILING_MS || (SECONDS + 120) * 1000);
const QUIET_CEILING_MS = Number(process.env.QUIET_CEILING_MS || (QUIET_SECONDS + 120) * 1000);

const fixedBusy = await runOnce(BIN, { label: 'fixed-busy', seconds: SECONDS, program: BUSY_PROGRAM, ceilingMs: CEILING_MS });
const fixedQuiet = await runOnce(BIN, { label: 'fixed-quiet', seconds: QUIET_SECONDS, program: QUIET_PROGRAM, ceilingMs: QUIET_CEILING_MS });
const baselineBusy = BASELINE
  ? await runOnce(BASELINE, { label: 'baseline-busy', seconds: SECONDS, program: BUSY_PROGRAM, ceilingMs: CEILING_MS })
  : null;

// D1 — the deadline. The fixed side ends inside its own budget; the
// baseline (the published v0.7.43 asset) does not.
{
  const budget = shutdownBudgetMs(SECONDS);
  const allowed = SECONDS * 1000 + budget + GRACE_MS;
  record('D1_fixed_ends_inside_its_deadline_budget',
    fixedBusy.returnedOnItsOwn && fixedBusy.wallMs <= allowed,
    `fixed ${fixedBusy.wallMs}ms for a ${SECONDS}s run (budget ${budget}ms, allowed ${allowed}ms)`);
  if (baselineBusy) {
    record('D1_baseline_overruns_the_deadline',
      !baselineBusy.returnedOnItsOwn || baselineBusy.wallMs > allowed,
      `baseline ${baselineBusy.wallMs}ms vs allowed ${allowed}ms (overshoot ${baselineBusy.wallMs - SECONDS * 1000}ms${baselineBusy.killed ? ', KILLED at the ceiling: it never returned on its own' : ''})`);
  }
}

// D2 — the exit VOICE. A run that leaves a payload debt owed must not exit
// 0; it must exit 3 AND name the repair.
{
  if (fixedBusy.endStateDebt) {
    record('D2_fixed_does_not_exit_zero_while_a_debt_is_owed',
      fixedBusy.exitCode === 3,
      `exit=${fixedBusy.exitCode} with ${fixedBusy.payloadPending} payload row(s) owed`);
    record('D2_fixed_names_the_repair_on_stderr',
      fixedBusy.namesDebt && fixedBusy.namesRepair,
      `namesDebt=${fixedBusy.namesDebt} namesRepair=${fixedBusy.namesRepair}`);
    record('D2_status_agrees_with_the_exit_code',
      fixedBusy.statusExit === fixedBusy.exitCode,
      `status exit=${fixedBusy.statusExit} vs daemon exit=${fixedBusy.exitCode}`);
  } else {
    record('D2_fixed_does_not_exit_zero_while_a_debt_is_owed',
      fixedBusy.exitCode === 0,
      `no debt owed: exit=${fixedBusy.exitCode} (the debt face is not exercised on this run)`);
  }
}

// G1 — the false-positive guard: a quiet program ends near its deadline and
// exits 0. The fix must not turn every run into exit 3.
{
  const allowed = QUIET_SECONDS * 1000 + shutdownBudgetMs(QUIET_SECONDS) + GRACE_MS;
  record('G1_quiet_program_ends_near_its_deadline_and_exits_zero',
    fixedQuiet.exitCode === 0 && fixedQuiet.wallMs <= allowed,
    `exit=${fixedQuiet.exitCode} wall=${fixedQuiet.wallMs}ms allowed=${allowed}ms debt=${fixedQuiet.payloadPending}`);
}

// G2 — the baseline's silence: its health verdict only arrived behind the
// overrun, or it never arrived at all.
if (baselineBusy) {
  const overran = !baselineBusy.returnedOnItsOwn
    || baselineBusy.wallMs > SECONDS * 1000 + shutdownBudgetMs(SECONDS);
  const cleanExitBehindTheOverrun = baselineBusy.returnedOnItsOwn && baselineBusy.exitCode === 0;
  const neverReturned = baselineBusy.killed;
  record('G2_baseline_reports_health_only_behind_the_overrun',
    overran && (cleanExitBehindTheOverrun || neverReturned),
    neverReturned
      ? `baseline never returned on its own inside the ${Math.round(CEILING_MS / 1000)}s ceiling for a ${SECONDS}s run — killed by the instrument (SIGTERM, not a product verdict)`
      : `baseline exit=${baselineBusy.exitCode} wall=${baselineBusy.wallMs}ms for a ${SECONDS}s run — health only after the overrun`);
}

const defectFaces = faces.filter(f => !f.ok);
const verdict = defectFaces.length === 0
  ? 'start-deadline-and-exit-voice-hold'
  : 'start-deadline-or-exit-voice-defect-present';

const artifact = {
  tool: 'tools/measure-start-deadline-exit.mjs',
  question: 'does `reeflog start --seconds N` end near N and tell the truth in its exit code about what it left owed?',
  confound: 'the busy runs share the public mainnet-beta endpoint with whatever else this host is doing: when the endpoint is contended the journal ends with rows still owing their payload (exit 3, the repair named); when it is quiet the daemon pays the debt inside the run and exits 0. BOTH are honest under the contract — the debt face is load-conditional, so D2 records whether it was exercised this run.',
  defect: 'the live user-zero harness demanded exit 0; the published v0.7.44 asset ends with exit 3 on a busy program. Either the run broke, or the run ends on time and names the debt it could not pay (exit 3 = "reported and held", documented since v0.7.31). The same expectation passed on v0.7.43 only because that binary overran its own deadline (measured: 223 s for a 120 s run) and so had time to pay the debt.',
  measuredAt: new Date().toISOString(),
  verdict,
  seconds: SECONDS,
  quietSeconds: QUIET_SECONDS,
  busyProgram: BUSY_PROGRAM,
  quietProgram: QUIET_PROGRAM,
  shutdownBudgetMs: shutdownBudgetMs(SECONDS),
  faces,
  runs: { fixed: fixedBusy, fixedQuiet, ...(baselineBusy ? { baseline: baselineBusy } : {}) },
};

if (OUT) {
  fs.mkdirSync(path.dirname(OUT), { recursive: true });
  fs.writeFileSync(OUT, JSON.stringify(artifact, null, 2) + '\n');
  console.log(`wrote ${OUT}`);
}
console.log(JSON.stringify({ verdict, faces, runs: artifact.runs }, null, 2));
process.exit(defectFaces.length === 0 ? 0 : 1);
