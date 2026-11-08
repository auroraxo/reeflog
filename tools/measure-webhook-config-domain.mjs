#!/usr/bin/env node
// The webhook CONFIG value domain, measured against REAL binaries.
//
// The defect (measured 2026-11-08 on the published v0.7.4 tree, HEAD
// ce8e4f1): the webhook config file is the value source one layer ABOVE the
// flag parser, and it was checked less carefully than the flag parser:
//
//   "timeout_ms": 18446744073709551615  -> ACCEPTED, and the delivery pass
//     NEVER returned against a receiver that accepts the connection and
//     never answers (killed at 25 s; the deadline is ~584 million years
//     out). The key exists so "a webhook whose endpoint hangs must not hang
//     the delivery pass" — the key could hang the delivery pass.
//   "timeout_ms": 9223372036854775808   -> same class, same kill: `as_u64`
//     accepts it, `Duration::from_millis` schedules it, the deadline never
//     arrives.
//   "timeout_ms": 0                     -> ACCEPTED, and the poster silently
//     uses `max(1)` = 1 ms: the file says 0, the delivery behaves as 1 ms,
//     and nothing says so.
//   "timeout_ms": 1.5 / -1 / "5000"     -> refused, but with "must be a
//     number" — the message does not name the offending VALUE, so -1 and
//     1.5, which ARE numbers, are told they are not.
//   "dead_letter_after": -1 / 9223372036854775808 -> the count is compared
//     against an i64 refusal counter; a value outside that space can never
//     match and was accepted anyway.
//
// The tool measures both trees on the same cases and writes ONE artifact:
//
//   BASELINE  the published v0.7.4 binary (git archive of the tag)
//   FIXED     the binary built from this tree
//
// Per case: exit code, wall time, whether the run had to be KILLED (a hang
// is a failure, not a slow pass), delivered/failed/dead_lettered, and
// whether the refusal named the offending value. Guards pin the
// false-positive side: the default timeout must still bound a stalled
// delivery, a configured timeout must be honoured as written, a value at the
// ceiling must still be accepted, and the dead-letter valve must still
// quarantine (and still be disableable with 0).
//
// Three measurement traps this tool had to be fixed for (they are the
// reason a first version of this file reported 8/10 refusals and 2/5 guards
// on a FIXED tree that actually behaves correctly):
//
//   T1  JS numbers are f64. `18446744073709551615` in a JS literal is
//       already 18446744073709552000 before it is ever written, and
//       `JSON.stringify` would emit a value the operator never typed. The
//       config is therefore written as RAW JSON TEXT (the case carries the
//       literal as a string), never through JSON.stringify.
//   T2  One journal for every case lets the state of one case bleed into
//       the next: the refusal counter, the dead-letter queue and the
//       per-target cursor all persist, so a case that ran before changed
//       what the next case measured. Every case gets its OWN journal.
//   T3  A guard can be judged by the wrong signal. "A value at the ceiling
//       is accepted" is about the CONFIG being accepted, not about a
//       timeout firing — against a server that drops the connection the
//       delivery fails for its own reason. The guard therefore checks that
//       the run produced a REPORT (i.e. the config was accepted), not that
//       it failed with a timeout.
//
// Usage:
//   node tools/measure-webhook-config-domain.mjs
//   node tools/measure-webhook-config-domain.mjs --baseline <bin> --fixed <bin>
//
// Exit: 0 all cases behave as specified on the FIXED tree, 1 a case failed,
// 2 setup failed.
import { spawn, spawnSync, execFileSync } from 'node:child_process';
import net from 'node:net';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { DatabaseSync } from 'node:sqlite';

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const ROOT = path.resolve(__dirname, '..');
const OUT = path.resolve(
  (
    process.env.OUT ||
    (process.argv.includes('--out')
      ? process.argv[process.argv.indexOf('--out') + 1]
      : path.join(ROOT, 'research', 'webhook-config-domain-2026-11-08.json'))
  ).replace(/^file:/, ''),
);

const PROGRAM = 'whirLbMiicVdio4qvUfM5KAg6Ct8VwpYzGff3uctyCc';
/** How many events every fresh journal holds. The valve guard is derived
 * from this: with `dead_letter_after: 1` EACH event is quarantined on its
 * first refusal, so the expected letter count is the journal size — an
 * expectation of "1" would be counting events, not refusals. */
const SEEDED = 3;
/** A stalled delivery must be over well inside this; the pre-fix behaviour
 * for an out-of-domain timeout is "never", so anything finite with margin is
 * the gate. */
const KILL_MS = 12000;

function argOf(flag) {
  const i = process.argv.indexOf(flag);
  return i >= 0 ? process.argv[i + 1] : null;
}

// --- the trees ------------------------------------------------------------

/** The published v0.7.4 tree, taken from the TAG and not from the checkout:
 * a baseline that is "the current tree with the fix removed" would prove
 * nothing about what a stranger installs. */
function baselineBinary() {
  const given = argOf('--baseline');
  if (given) return { bin: path.resolve(given), source: 'given' };
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'reeflog-whcfg-baseline-'));
  const tag = process.env.BASELINE_TAG || 'v0.7.4';
  const tar = path.join(dir, 'baseline.tar');
  execFileSync('git', ['archive', '-o', tar, tag], { cwd: ROOT, stdio: 'inherit' });
  const src = path.join(dir, 'src');
  fs.mkdirSync(src);
  execFileSync('tar', ['-xf', tar, '-C', src], { stdio: 'inherit' });
  execFileSync('cargo', ['build', '--release', '--locked'], { cwd: src, stdio: 'inherit' });
  return { bin: path.join(src, 'target', 'release', 'reeflog'), source: `git archive ${tag}` };
}

function fixedBinary() {
  const given = argOf('--fixed');
  if (given) return { bin: path.resolve(given), source: 'given' };
  execFileSync('cargo', ['build', '--release', '--locked'], { cwd: ROOT, stdio: 'inherit' });
  return { bin: path.join(ROOT, 'target', 'release', 'reeflog'), source: 'this tree' };
}

// --- the receivers --------------------------------------------------------

/** Accepts the connection, reads the request and NEVER answers: the shape of
 * a consumer that hung mid-delivery. The socket is held open, so the sender
 * really waits for its timeout instead of seeing a connection reset. */
function stallingServer() {
  const server = net.createServer((c) => {
    c.on('error', () => {});
    c.on('data', () => {});
  });
  return new Promise((resolve) => {
    server.listen(0, '127.0.0.1', () => resolve(server));
  });
}

/** Answers every request with a status — a verdict, not an outage. This is
 * the input `dead_letter_after` counts. */
function httpServer(status) {
  const server = net.createServer((c) => {
    c.on('error', () => {});
    c.on('data', () => {
      const body = '{"ok":false}';
      const resp =
        `HTTP/1.1 ${status} ${status === 403 ? 'Forbidden' : 'OK'}\r\n` +
        `content-type: application/json\r\ncontent-length: ${body.length}\r\n\r\n${body}`;
      c.write(resp);
    });
  });
  return new Promise((resolve) => {
    server.listen(0, '127.0.0.1', () => resolve(server));
  });
}

// --- the journal ----------------------------------------------------------

function seedJournal(bin, db) {
  const r = spawnSync(bin, ['init', '--db', db, '--program', PROGRAM], { encoding: 'utf8' });
  if (r.status !== 0) throw new Error(`init failed: ${r.stderr}`);
  const c = new DatabaseSync(db);
  for (let i = 0; i < 3; i++) {
    c.exec(
      `INSERT OR IGNORE INTO events (signature, slot, err, logs, source, first_seen, seal_status)
       VALUES ('CFG${String(i).padStart(58, '0')}', ${44_000_000 + i}, NULL, '[]', 'ws', 1700000000000, 'sealed')`,
    );
  }
  c.close();
}

// --- running one case -----------------------------------------------------

function runCase(bin, server, extra) {
  return new Promise((resolve) => {
    const port = server.address().port;
    // T2: a fresh journal per case. The refusal counter, the dead-letter
    // queue and the per-target cursor are all persistent state — a shared
    // journal let one case change what the next one measured.
    const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'reeflog-whcfg-case-'));
    const db = path.join(dir, 'reeflog.db');
    seedJournal(bin, db);
    const cfg = path.join(dir, 'wh.json');
    // T1: the config is written as RAW JSON TEXT. The case carries the
    // value as a string that is spliced in verbatim, so an integer above
    // 2^53 reaches the file exactly as the operator wrote it — a JS number
    // would have been rounded before it was ever serialised.
    const extras = Object.entries(extra)
      .map(([k, v]) => `"${k}": ${v}`)
      .join(', ');
    const body =
      `{"targets":[{"id":"t1","url":"http://127.0.0.1:${port}/hook","secret":"s"` +
      (extras ? `, ${extras}` : '') +
      `}]}`;
    fs.writeFileSync(cfg, body);
    const t0 = Date.now();
    const c = spawn(bin, ['webhook', 'push', '--db', db, '--config', cfg], { cwd: dir });
    let out = '';
    let err = '';
    c.stdout.on('data', (d) => { out += d; });
    c.stderr.on('data', (d) => { err += d; });
    let killed = false;
    const killer = setTimeout(() => { killed = true; c.kill('SIGKILL'); }, KILL_MS);
    c.on('close', (code, signal) => {
      clearTimeout(killer);
      let parsed;
      try { parsed = JSON.parse(out); } catch { parsed = undefined; /* refused before the report */ }
      const report = parsed === undefined ? null : parsed;
      const t = report?.targets?.[0] ?? null;
      resolve({
        // Distinguishes "the config was refused before anything ran" (no
        // report at all) from "the config was accepted and the delivery
        // failed for its own reason" (a report without a target error).
        reported: t !== null,
        exit: code,
        signal,
        killed,
        ms: Date.now() - t0,
        delivered: t?.delivered ?? null,
        failed: t?.failed ?? null,
        dead_lettered: t?.dead_lettered ?? null,
        blocked: t?.blocked ?? null,
        error: t?.error ?? null,
        stderr: err.trim().split('\n')[0] ?? '',
      });
    });
  });
}

// --- the cases ------------------------------------------------------------

/**
 * kind:
 *   'refusal' — the value is outside the domain and must be refused:
 *               a non-zero exit, the key AND the value named, no report,
 *               and the run must FINISH (a hang is the defect).
 *   'bounded' — a stalled delivery must be bounded by the timeout.
 *   'accepted'— the config must be ACCEPTED: a report is produced, whatever
 *               the delivery outcome was. This is the false-positive guard
 *               for the ceiling: a checker that refuses a config the
 *               product documents as valid is worse than the defect.
 *   'valve'   — the dead-letter valve must count and quarantine.
 *
 * The value is a RAW JSON fragment, so it reaches the file exactly as
 * written (T1). `value` is the string the refusal must echo.
 */
const CASES = [
  { id: 'timeout-zero', kind: 'refusal', mode: 'stall', extra: { timeout_ms: '0' }, value: '0' },
  { id: 'timeout-u64-max', kind: 'refusal', mode: 'stall', extra: { timeout_ms: '18446744073709551615' }, value: '18446744073709551615' },
  { id: 'timeout-i64-max-plus-one', kind: 'refusal', mode: 'stall', extra: { timeout_ms: '9223372036854775808' }, value: '9223372036854775808' },
  { id: 'timeout-above-ceiling', kind: 'refusal', mode: 'stall', extra: { timeout_ms: '3600001' }, value: '3600001' },
  { id: 'timeout-fractional', kind: 'refusal', mode: 'stall', extra: { timeout_ms: '1.5' }, value: '1.5' },
  { id: 'timeout-negative', kind: 'refusal', mode: 'stall', extra: { timeout_ms: '-1' }, value: '-1' },
  { id: 'timeout-string', kind: 'refusal', mode: 'stall', extra: { timeout_ms: '"5000"' }, value: '5000' },
  { id: 'timeout-beyond-u64', kind: 'refusal', mode: 'stall', extra: { timeout_ms: '18446744073709551616' }, value: '18446744073709551616' },
  { id: 'dead-letter-negative', kind: 'refusal', mode: 'refuse', extra: { dead_letter_after: '-1' }, value: '-1' },
  { id: 'dead-letter-out-of-i64', kind: 'refusal', mode: 'refuse', extra: { dead_letter_after: '9223372036854775808' }, value: '9223372036854775808' },
  { id: 'dead-letter-fractional', kind: 'refusal', mode: 'refuse', extra: { dead_letter_after: '1.5' }, value: '1.5' },
  { id: 'guard-default-timeout-bounds', kind: 'bounded', mode: 'stall', extra: {} },
  { id: 'guard-configured-timeout-bounds', kind: 'bounded', mode: 'stall', extra: { timeout_ms: '1000' } },
  { id: 'guard-ceiling-accepted', kind: 'accepted', mode: 'drop', extra: { timeout_ms: '3600000' } },
  { id: 'guard-dead-letter-one-quarantines', kind: 'valve', mode: 'refuse', extra: { dead_letter_after: '1' } },
  { id: 'guard-dead-letter-zero-disables', kind: 'valve', mode: 'refuse', extra: { dead_letter_after: '0' } },
];

async function serverFor(mode) {
  if (mode === 'stall') return stallingServer();
  if (mode === 'refuse') return httpServer(403);
  if (mode === 'drop') {
    const server = net.createServer((c) => { c.on('error', () => {}); c.destroy(); });
    return new Promise((resolve) => server.listen(0, '127.0.0.1', () => resolve(server)));
  }
  throw new Error(`unknown mode ${mode}`);
}

// --- the verdict ----------------------------------------------------------

function judge(caseDef, r) {
  if (caseDef.kind === 'refusal') {
    if (r.killed) {
      return { ok: false, why: 'the run had to be KILLED — it waited for a deadline that never comes' };
    }
    if (r.exit === 0) return { ok: false, why: 'the value was accepted (exit 0)' };
    if (r.error !== null) return { ok: false, why: 'a refused config must print no report' };
    const err = r.stderr.toLowerCase();
    if (!err.includes('timeout_ms') && !err.includes('dead_letter_after')) {
      return { ok: false, why: 'the refusal does not name the key' };
    }
    if (!err.includes(String(caseDef.value).toLowerCase())) {
      return { ok: false, why: 'the refusal does not name the offending value' };
    }
    return { ok: true, why: 'refused promptly, key and value named' };
  }
  if (caseDef.kind === 'bounded') {
    if (r.killed) return { ok: false, why: 'the run had to be KILLED — no timeout fired' };
    if (r.exit !== 3) return { ok: false, why: `expected exit 3 (blocked queue), got ${r.exit}` };
    if (!String(r.error ?? '').includes('timeout')) {
      return { ok: false, why: `expected a timeout failure, got ${r.error}` };
    }
    return { ok: true, why: `bounded at ${r.ms} ms` };
  }
  if (caseDef.kind === 'accepted') {
    // T3: the guard is about the CONFIG being accepted, not about a timeout
    // firing. A report exists exactly when the config was accepted.
    if (r.killed) return { ok: false, why: 'the run had to be KILLED' };
    if (!r.reported) {
      return { ok: false, why: 'the config was refused — a valid config must still be accepted' };
    }
    return { ok: true, why: `accepted, the delivery failed for its own reason (${r.error ?? 'no error'})` };
  }
  // valve
  if (r.killed) return { ok: false, why: 'the run had to be KILLED' };
  if (caseDef.id.includes('one-quarantines')) {
    // threshold 1 = every seeded event is quarantined on its first refusal;
    // the queue must have MOVED past them instead of blocking.
    if (r.dead_lettered !== SEEDED) {
      return { ok: false, why: `expected ${SEEDED} dead letters (one per event, each on its first refusal), got ${r.dead_lettered}` };
    }
    if (r.blocked === true) return { ok: false, why: 'quarantine moves the queue; it must not block it' };
    return { ok: true, why: `all ${SEEDED} quarantined on the first refusal, queue moved` };
  }
  if (r.dead_lettered !== 0) return { ok: false, why: `expected 0 dead letters, got ${r.dead_lettered}` };
  if (r.blocked !== true) return { ok: false, why: 'the refusal must block the queue' };
  return { ok: true, why: 'the valve stays disabled' };
}

// --- main -----------------------------------------------------------------

const baseline = baselineBinary();
const fixed = fixedBinary();
console.error(`baseline: ${baseline.bin} (${baseline.source})`);
console.error(`fixed:    ${fixed.bin} (${fixed.source})`);

const trees = { baseline, fixed };
const results = {};
for (const [name, tree] of Object.entries(trees)) {
  const rows = [];
  for (const caseDef of CASES) {
    const server = await serverFor(caseDef.mode);
    const r = await runCase(tree.bin, server, caseDef.extra);
    server.close();
    const verdict = judge(caseDef, r);
    rows.push({
      id: caseDef.id,
      kind: caseDef.kind,
      value: caseDef.value ?? null,
      extra: caseDef.extra,
      ...r,
      ok: verdict.ok,
      verdict: verdict.why,
    });
    console.error(`${name}: ${verdict.ok ? 'PASS' : 'FAIL'} ${caseDef.id} — ${verdict.why}`);
  }
  results[name] = {
    source: tree.source,
    cases: rows,
    refusals: rows.filter((r) => r.kind === 'refusal' && r.ok).length,
    refusalCases: rows.filter((r) => r.kind === 'refusal').length,
    guards: rows.filter((r) => r.kind !== 'refusal' && r.ok).length,
    guardCases: rows.filter((r) => r.kind !== 'refusal').length,
  };
}

const artifact = {
  measuredAt: '2026-11-08',
  what: 'the webhook CONFIG value domain (timeout_ms, dead_letter_after), measured against real binaries',
  defect:
    'the published v0.7.4 tree accepted a timeout_ms that made the delivery pass wait for a deadline that never comes, silently re-read a timeout_ms of 0 as 1 ms, and refused a value without naming it',
  measurementTraps: {
    T1: 'the config is written as raw JSON text — a JS number would round any integer above 2^53 before it reached the file',
    T2: 'every case gets its own journal — the refusal counter, the dead-letter queue and the per-target cursor are persistent state',
    T3: 'the ceiling guard is judged by the report existing (the config was accepted), not by a timeout firing',
  },
  trees: results,
};
fs.mkdirSync(path.dirname(OUT), { recursive: true });
fs.writeFileSync(OUT, JSON.stringify(artifact, null, 2));
console.error(`wrote ${OUT}`);

const ok =
  results.fixed.refusals === results.fixed.refusalCases &&
  results.fixed.guards === results.fixed.guardCases;
if (!ok) {
  console.error(
    `FIXED TREE FAILS: ${results.fixed.refusals}/${results.fixed.refusalCases} refusals, ` +
      `${results.fixed.guards}/${results.fixed.guardCases} guards`,
  );
  process.exit(1);
}
console.error(
  `fixed tree: ${results.fixed.refusals}/${results.fixed.refusalCases} refusals, ` +
    `${results.fixed.guards}/${results.fixed.guardCases} guards ` +
    `(baseline: ${results.baseline.refusals}/${results.baseline.refusalCases} refusals, ` +
    `${results.baseline.guards}/${results.baseline.guardCases} guards)`,
);
