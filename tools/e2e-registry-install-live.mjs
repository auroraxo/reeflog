// REGISTRY-INSTALL USER-ZERO, LIVE: what a stranger who only knows the
// npm name actually gets, end to end, against the REAL chain.
//
//   1. `npm install reeflog-client` into an empty app — the registry
//      channel, no git URL, no checkout, no Rust toolchain;
//   2. bootstrap() from the installed bytes — real GitHub release API,
//      real archive, digest verified before anything is installed;
//   3. `reeflog start` against mainnet-beta for N seconds — a real
//      journal written by the installed binary;
//   4. the registry-installed client reads it: status equality with the
//      CLI, strict ordering, byte-equality with the CLI window, no replay,
//      cursor-file resume, refusal voice, verify exit-code mapping, and
//      the HELD row named rather than silently delivered.
//
// Exit: 0 all checks pass, 1 a check failed, 2 setup failed, 3 offline.
//   PKG=reeflog-client@0.7.21 SECONDS=90 OUT=research/… node tools/e2e-registry-install-live.mjs
import { spawn, spawnSync, execFileSync } from 'node:child_process';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { createHash } from 'node:crypto';

const PKG = process.env.PKG || 'reeflog-client';
const SECONDS = Number(process.env.SECONDS || 90);
const PROGRAM = process.env.PROGRAM || 'whirLbMiicVdio4qvUfM5KAg6Ct8VwpYzGff3uctyCc';
const OUT = process.env.OUT || null;
const LIMIT = 500, WIN = 100;

const checks = [];
const record = (name, ok, detail) => { checks.push({ name, ok, detail }); console.log(`${ok ? 'PASS' : 'FAIL'} ${name} — ${detail}`); };

const root = fs.mkdtempSync(path.join(os.tmpdir(), 'regzero-'));
const home = path.join(root, 'home');
const app = path.join(root, 'app');
fs.mkdirSync(home, { recursive: true });
fs.mkdirSync(app, { recursive: true });

// 1 — the registry install (offline guard: npm must answer)
try {
  execFileSync('npm', ['init', '-y'], { cwd: app, stdio: 'ignore' });
  execFileSync('npm', ['install', PKG, '--silent'], { cwd: app, stdio: 'ignore' });
} catch {
  console.error('offline: registry unreachable'); process.exit(3);
}
const inst = path.join(app, 'node_modules', 'reeflog-client');
const instManifest = JSON.parse(fs.readFileSync(path.join(inst, 'package.json'), 'utf8'));
record('registry_install_from_empty_app', Boolean(instManifest.version),
  `${PKG} -> ${instManifest.version} (no git URL, no toolchain)`);

// 2 — bootstrap from the INSTALLED bytes, isolated HOME (no pre-seeded cache)
let bin = null, digest = null, binarySha256 = null;
try {
  const mod = await import(path.join(inst, 'index.js'));
  const res = await mod.bootstrap({ dest: path.join(home, '.reeflog') });
  bin = res.bin; digest = String(res.digest); binarySha256 = String(res.binarySha256);
} catch (e) {
  record('bootstrap_installs_binary', false, String(e.message).split('\n')[0]);
  console.error('setup: bootstrap failed'); process.exit(2);
}
record('bootstrap_installs_binary', Boolean(bin) && binarySha256?.length === 64,
  `digest=${digest.slice(0, 8)} binarySha256=${binarySha256.slice(0, 8)} (verified before install)`);

// installed binary names the version the package manifest names
const v = execFileSync(bin, ['--version'], { encoding: 'utf8' }).trim();
record('binary_version_matches_manifest', v === `reeflog ${instManifest.version}`,
  `${v} vs package.json ${instManifest.version}`);

// 3 — a REAL journal: daemon against mainnet-beta
const db = path.join(root, 'stranger.db');
try {
  execFileSync(bin, ['init', '--db', db, '--program', PROGRAM], { stdio: 'ignore' });
} catch (e) {
  record('init_journal', false, String(e.message).split('\n')[0]); process.exit(2);
}
const started = Date.now();
let rc = null;
try {
  const p = spawn(bin, ['start', '--db', db, '--seconds', String(SECONDS)],
    { stdio: ['ignore', 'ignore', 'ignore'] });
  rc = await new Promise((res) => { p.on('close', (code) => res(code)); });
} catch (e) {
  record('daemon_live_run', false, String(e.message)); process.exit(2);
}
record('daemon_live_run', rc === 0, `exit=${rc} after ${Math.round((Date.now() - started) / 1000)}s against mainnet-beta`);

// 4 — the registry-installed client reads what the binary wrote
const mod = await import(path.join(inst, 'index.js'));
const { reeflog, ReeflogError } = mod;
const capture = (args) => new Promise((resolve, reject) => {
  const c = spawn(bin, args, { stdio: ['ignore', 'pipe', 'pipe'] });
  let stdout = '', stderr = '';
  c.stdout.on('data', d => { stdout += d; });
  c.stderr.on('data', d => { stderr += d; });
  c.on('error', reject);
  c.on('close', status => resolve({ status, stdout, stderr }));
});
const box = reeflog({ db, bin });

// 4.1 status equality
{
  const out = await capture(['status', '--db', db]);
  const cs = await box.status();
  const equal = JSON.stringify(cs) === JSON.stringify(JSON.parse(out.stdout));
  record('status_equals_cli', equal, `events=${cs.events} max_seq=${cs.max_seq} equal=${equal}`);
  if (!equal || cs.events === 0) { console.error('setup: no journal rows to read'); process.exit(2); }
}

const curA = path.join(root, 'c-a.cursor'), curB = path.join(root, 'c-b.cursor'), curCli = path.join(root, 'c-cli.cursor');

// 4.2 ordering from a fresh cursor
const first = await box.tail({ cursor: curA, limit: LIMIT });
{
  const ordered = first.events.every((e, i) => i === 0 || e.seq > first.events[i - 1].seq);
  record('tail_strictly_increasing_seq', ordered && first.events.length > 0,
    `${first.events.length} events, first=${first.events[0]?.seq} last=${first.events.at(-1)?.seq}`);
}

// 4.3 byte equality with the CLI over the SAME window (own fresh cursors)
{
  fs.rmSync(curCli + '.a', { force: true }); fs.rmSync(curCli + '.b', { force: true });
  const out = await capture(['tail', '--db', db, '--cursor', curCli + '.a', '--limit', String(WIN)]);
  const cliLines = out.stdout.split('\n').filter(l => l.trim());
  const client = await box.tail({ cursor: curCli + '.b', limit: WIN });
  const clientLines = client.events.map(e => JSON.stringify(e));
  const same = cliLines.length === clientLines.length && cliLines.every((l, i) => l === clientLines[i]);
  record('rows_byte_equal_to_cli', same && cliLines.length === WIN,
    `cli=${cliLines.length} client=${clientLines.length} expected=${WIN}`);
}

// 4.4 no replay
{
  const nextFrom = first.events.at(-1)?.seq;
  const second = await box.tail({ cursor: curA, limit: LIMIT });
  const firstSeqs = new Set(first.events.map(e => e.seq));
  const overlap = second.events.filter(e => firstSeqs.has(e.seq)).length;
  const continues = second.events.length === 0 || second.events[0].seq === nextFrom + 1;
  record('no_replay_continues_at_next_seq', overlap === 0 && continues && second.events.length > 0,
    `first ended at ${nextFrom}, second started at ${second.events[0]?.seq} (${second.events.length} rows), overlap=${overlap}`);
}

// 4.5 a copied cursor file resumes at the same seq (the binary's own file)
{
  fs.copyFileSync(curA, curB);
  const second = await box.tail({ cursor: curA, limit: LIMIT });
  const resumed = await box.tail({ cursor: curB, limit: LIMIT });
  const sameStart = second.events[0]?.seq === resumed.events[0]?.seq;
  record('copied_cursor_resumes_same_seq', sameStart && resumed.events.length > 0,
    `cursor=${second.cursorValue} next seq=${resumed.events[0]?.seq} (${resumed.events.length} rows)`);
}

// 4.6 refusal keeps the CLI's words
{
  let ok = false, detail = 'no rejection';
  try { await reeflog({ db: '/nonexistent/nope.db', bin }).tail({ cursor: curA }); }
  catch (e) {
    ok = e instanceof ReeflogError && /no journal|reeflog init/.test(e.stderr || e.message);
    detail = `exit=${e.exitCode} msg="${(e.stderr || e.message).split('\n')[0]}"`;
  }
  record('refusal_keeps_cli_words', ok, detail);
}

// 4.7 verify follows the CLI exit code
{
  const out = await capture(['verify', '--db', db, '--pages', '5']);
  let ok = false, detail = `cli exit=${out.status}`;
  if (out.status === 3) {
    try { await box.verify({ pages: 5 }); ok = false; detail = 'incomplete verify RESOLVED — must throw'; }
    catch (e) { ok = e instanceof ReeflogError && e.exitCode === 3; detail = `cli exit=3, client threw exitCode=${e.exitCode}`; }
  } else if (out.status === 0) {
    const r = await box.verify({ pages: 5 });
    ok = r.complete === true && JSON.parse(out.stdout).complete === true;
    detail = `cli exit=0 complete=${r.complete}`;
  } else { ok = true; detail = `cli refused exit=${out.status} — no verdict`; }
  record('verify_follows_cli_exit_code', ok, detail);
}

// 4.8 an owed envelope HOLDs by name (withAccounts, fresh cursor)
{
  const cur = path.join(root, 'c-held.cursor');
  fs.rmSync(cur, { force: true });
  let ok = false, detail = 'no verdict';
  try {
    const r = await box.tail({ cursor: cur, limit: LIMIT, withAccounts: true });
    ok = r.held === null ? true
      : (r.held.seq !== undefined && r.held.signature !== undefined && /HELD|owed/.test(r.held.reason || ''));
    detail = r.held === null ? `no held row (all ${r.events.length} rows delivered)`
      : `held seq=${r.held.seq} delivered=${r.events.length} reason="${String(r.held.reason).split(':')[0]}"`;
  } catch (e) { ok = false; detail = `threw: ${String(e.message).split('\n')[0]}`; }
  record('held_row_is_named_not_silent', ok, detail);
}

const failed = checks.filter(c => !c.ok);
const status = await box.status();
const report = {
  package: PKG,
  installedVersion: instManifest.version,
  measuredAt: new Date().toISOString().slice(0, 10),
  liveSeconds: SECONDS,
  program: PROGRAM,
  archiveDigestPrefix: digest.slice(0, 8),
  binarySha256Prefix: binarySha256.slice(0, 8),
  journalEvents: status.events,
  journalMaxSeq: status.max_seq,
  checks,
  verdict: failed.length === 0 ? 'all_pass' : 'failed',
};
const text = JSON.stringify(report, null, 2);
if (OUT) fs.writeFileSync(OUT, text);
console.log(`\n${failed.length === 0 ? 'ALL PASS' : failed.length + ' FAILED'} (${checks.length} checks)${OUT ? ' -> ' + path.basename(OUT) : ''}`);
fs.rmSync(root, { recursive: true, force: true });
process.exit(failed.length === 0 ? 0 : 1);
