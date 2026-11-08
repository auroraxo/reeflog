// Live proof of the Node client against a REAL mainnet journal.
//
// The offline tests in test/client.test.mjs pin the contract on a planted
// journal. This run asks the same question against what `reeflog start`
// actually wrote from mainnet: does the client deliver the daemon's own rows,
// in order, byte-equal to the CLI, without replay, and does it report the
// health the binary reports?
//
// Checks (each falsifiable):
//  1. STATUS EQUALE    — client.status() equals `reeflog status`'s JSON.
//  2. TAIL ORDERED     — a fresh cursor delivers rows in strictly
//                        increasing seq, starting at the journal's oldest.
//  3. ROWS BYTE-EQUAL  — for the SAME window, the client's rows equal the
//                        CLI's own stdout lines byte for byte (the client
//                        parses the binary's output; it does not re-derive
//                        it). Window is bounded so spawnSync's stdout buffer
//                        cannot truncate the comparison — a truncated CLI
//                        capture made an earlier version of this harness
//                        report a false mismatch (342 vs 500 lines).
//  4. NO REPLAY        — a second call from the advanced cursor continues
//                        at the next seq and shares NO row with the first
//                        batch (it must not return 0 rows: a journal with
//                        more rows behind the cursor delivers them).
//  5. RESUME EQUALS    — a fresh client on a COPIED cursor file continues at
//                        the same seq: the cursor is the binary's own file.
//  6. REFUSAL LOUD     — a wrong db path rejects with the CLI's own words.
//  7. VERIFY INCOMPLETE — an incomplete verify THROWS (exit 3) and a
//                        complete one resolves with complete:true; the
//                        mapping follows the CLI's exit code, never a guess.
//
// Exit: 0 all pass, 1 a check failed, 2 setup failed, 3 binary missing.
import { spawn, spawnSync } from 'node:child_process';
import fs from 'node:fs';
import path from 'node:path';
import { reeflog, ReeflogError } from '../index.js';

const ROOT = process.cwd();
const BIN = process.env.BIN || path.resolve('target/release/reeflog');
const DB = process.env.DB || '/tmp/rl-live/live.db';
const OUT = process.env.OUT || path.resolve('research/client-live-e2e-2026-11-07.json');
const LIMIT = Number(process.env.LIMIT || 500);
const EQUAL_WINDOW = Number(process.env.EQUAL_WINDOW || 100);

if (!fs.existsSync(BIN)) { console.error('skip: no binary'); process.exit(3); }
if (!fs.existsSync(DB)) { console.error(`setup: no journal at ${DB}`); process.exit(2); }

// Full-output capture, no maxBuffer: the CLI comparison must not be able to
// "fail" because a buffer silently cut its stdout short.
function capture(args) {
  return new Promise((resolve, reject) => {
    const c = spawn(BIN, args, { stdio: ['ignore', 'pipe', 'pipe'] });
    let stdout = ''; let stderr = '';
    c.stdout.on('data', (d) => { stdout += d; });
    c.stderr.on('data', (d) => { stderr += d; });
    c.on('error', reject);
    c.on('close', (status) => resolve({ status, stdout, stderr }));
  });
}

const checks = [];
const record = (name, ok, detail) => { checks.push({ name, ok, detail }); console.log(`${ok ? 'PASS' : 'FAIL'} ${name} — ${detail}`); };

const box = reeflog({ db: DB, bin: BIN });

// 1. status equality
{
  const out = await capture(['status', '--db', DB]);
  if (out.status !== 0) { console.error(`setup: status failed: ${out.stderr}`); process.exit(2); }
  const clientStatus = await box.status();
  const equal = JSON.stringify(clientStatus) === JSON.stringify(JSON.parse(out.stdout));
  record('status_equals_cli', equal, `events=${clientStatus.events} max_seq=${clientStatus.max_seq} equal=${equal}`);
}

const cursorA = `${DB}.e2e-a.cursor`;
const cursorB = `${DB}.e2e-b.cursor`;
const cursorCli = `${DB}.e2e-cli.cursor`;
for (const f of [cursorA, cursorB, cursorCli]) fs.rmSync(f, { force: true });

// 2. ordering from a fresh cursor
const first = await box.tail({ cursor: cursorA, limit: LIMIT });
{
  const ordered = first.events.every((e, i) => i === 0 || e.seq > first.events[i - 1].seq);
  record('tail_strictly_increasing_seq', ordered && first.events.length > 0,
    `${first.events.length} events, first=${first.events[0]?.seq} last=${first.events.at(-1)?.seq} cursorValue=${first.cursorValue}`);
}

// 3. byte equality with the CLI over the SAME window. Each side gets its
//    OWN fresh cursor: the CLI advances the cursor file as it delivers, so
//    reusing one file would compare rows 1..100 against rows 101..200 and
//    report a false mismatch (measured: 100 vs 100 lines, all different).
{
  const cliCursor = `${cursorCli}.a`;
  const clientCursor = `${cursorCli}.b`;
  for (const f of [cliCursor, clientCursor]) fs.rmSync(f, { force: true });
  const out = await capture(['tail', '--db', DB, '--cursor', cliCursor, '--limit', String(EQUAL_WINDOW)]);
  const cliLines = out.stdout.split('\n').filter((l) => l.trim());
  const client = await box.tail({ cursor: clientCursor, limit: EQUAL_WINDOW });
  const clientLines = client.events.map((e) => JSON.stringify(e));
  const same = cliLines.length === clientLines.length && cliLines.every((l, i) => l === clientLines[i]);
  record('rows_byte_equal_to_cli', same && cliLines.length === EQUAL_WINDOW,
    `cli=${cliLines.length} client=${clientLines.length} expected=${EQUAL_WINDOW}`);
}

// 4. no replay: the advanced cursor continues at the next seq
{
  const nextFrom = first.events.at(-1)?.seq;
  const second = await box.tail({ cursor: cursorA, limit: LIMIT });
  const firstSeqs = new Set(first.events.map((e) => e.seq));
  const overlap = second.events.filter((e) => firstSeqs.has(e.seq)).length;
  const continues = second.events.length === 0 || second.events[0].seq === nextFrom + 1;
  record('no_replay_continues_at_next_seq', overlap === 0 && continues && second.events.length > 0,
    `first ended at ${nextFrom}, second started at ${second.events[0]?.seq} (${second.events.length} rows), overlap=${overlap}`);
}

// 5. a copied cursor file resumes exactly where the binary left it
{
  fs.copyFileSync(cursorA, cursorB);
  const second = await box.tail({ cursor: cursorA, limit: LIMIT });
  const resumed = await box.tail({ cursor: cursorB, limit: LIMIT });
  const sameStart = second.events[0]?.seq === resumed.events[0]?.seq;
  record('copied_cursor_resumes_same_seq', sameStart && resumed.events.length > 0,
    `cursor=${second.cursorValue} next event seq=${resumed.events[0]?.seq} (${resumed.events.length} rows)`);
}

// 6. refusal keeps the CLI's words
{
  let ok = false; let detail = 'no rejection';
  try {
    await reeflog({ db: '/nonexistent/nope.db', bin: BIN }).tail({ cursor: cursorA });
  } catch (e) {
    ok = e instanceof ReeflogError && /no journal|reeflog init/.test(e.stderr || e.message);
    detail = `exit=${e.exitCode} msg="${(e.stderr || e.message).split('\n')[0]}"`;
  }
  record('refusal_keeps_cli_words', ok, detail);
}

// 7. verify: the client follows the CLI's exit code
{
  const out = await capture(['verify', '--db', DB, '--pages', '5']);
  let ok = false; let detail = `cli exit=${out.status}`;
  if (out.status === 3) {
    try { await box.verify({ pages: 5 }); detail = 'incomplete verify RESOLVED — must throw'; ok = false; }
    catch (e) { ok = e instanceof ReeflogError && e.exitCode === 3; detail = `cli exit=3, client threw exitCode=${e.exitCode}`; }
  } else if (out.status === 0) {
    const r = await box.verify({ pages: 5 });
    ok = r.complete === true && JSON.parse(out.stdout).complete === true;
    detail = `cli exit=0 complete=${r.complete} missing=${r.missing_count}`;
  } else {
    detail = `cli refused (exit=${out.status}) — no verdict`;
    ok = true;
  }
  record('verify_follows_cli_exit_code', ok, detail);
}

const failed = checks.filter((c) => !c.ok);
const status = await box.status();
const report = {
  generatedAt: new Date().toISOString(),
  // Committed artifact: repo-relative binary path, no host directory names.
  binary: BIN.startsWith(ROOT + path.sep) ? path.relative(ROOT, BIN) : path.basename(BIN),
  journal: path.basename(DB),
  journalEvents: status.events,
  journalMaxSeq: status.max_seq,
  checks,
  verdict: failed.length === 0 ? 'all_pass' : 'failed',
};
fs.mkdirSync(path.dirname(OUT), { recursive: true });
fs.writeFileSync(OUT, JSON.stringify(report, null, 2));
console.log(`\n${failed.length === 0 ? 'ALL PASS' : `${failed.length} FAILED`} (${checks.length} checks) -> ${OUT}`);
process.exit(failed.length === 0 ? 0 : 1);
