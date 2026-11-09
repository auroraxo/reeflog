#!/usr/bin/env node
// The --ws / --rpc URL value domain, measured against REAL binaries.
//
// The defect (measured 2026-11-09 on the published v0.7.5 tree): the two
// URL flags are the only value flags with NO domain in the parser.
//
//   BASELINE behaviour, published v0.7.5 binary (git archive of the tag):
//
//     start --ws ftp://x/y        -> exit 0, journal CREATED, two
//                                    connect_fail gaps written, a report
//                                    that looks like a normal run;
//     start --ws ''               -> exit 0, the daemon just ran;
//     start --ws https://<live>   -> full duration, gap "URL error: URL
//                                    scheme not supported" — tungstenite
//                                    can never speak https;
//     verify --rpc ftp://x/y      -> exit 3, but only AFTER the journal was
//                                    opened and the call attempted:
//                                    "bad uri: unknown scheme: ftp" —
//                                    ureq's words, the flag's never;
//     verify --rpc not-a-url      -> exit 3, "bad uri: not-a-url is missing
//                                    scheme";
//     verify --rpc https://       -> exit 3, "bad uri: http: empty string".
//
//   The consumers' domains are their source code, not an opinion:
//   ureq resolves only http/https (ureq-proto SchemeExt::default_port:
//   None for anything else -> resolver: "unknown scheme"); tungstenite's
//   uri_mode accepts only ws/wss (_ => Err(UnsupportedUrlScheme)).
//
// The tool measures BOTH trees on the SAME cases and writes ONE artifact:
//
//   BASELINE  the published v0.7.5 binary (git archive of the tag)
//   FIXED     the binary built from this tree
//
// Per case: exit code, wall time, whether a journal was CREATED, whether a
// REPORT was printed (start always prints one), and — the verdict — whether
// the PARSER refused: non-zero exit, stderr naming the flag, stderr naming
// the value, and none of the transport's own words ("bad uri",
// "unknown scheme"). Refusing a run for some OTHER reason (missing journal)
// is not a URL refusal and must not be counted as one.
//
// Guards pin the false-positive side: everything the consumers accept must
// still reach them — http and https for --rpc, ws and wss for --ws, a URL
// with port/path/query, an UPPERCASE scheme (MEASURED on v0.7.5:
// `--ws WS://127.0.0.1:9/x` reached TCP connect, so the consumer accepts it
// and a stricter checker would reject a working value).
//
// Usage:
//   node tools/measure-url-flag-domain.mjs
//   node tools/measure-url-flag-domain.mjs --baseline <bin> --fixed <bin>
//
// Exit: 0 all cases behave as specified on the FIXED tree, 1 a case failed,
// 2 setup failed.
import { spawn, spawnSync, execFileSync } from 'node:child_process';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const ROOT = path.resolve(__dirname, '..');
const OUT = path.resolve(
  (
    process.env.OUT ||
    (process.argv.includes('--out')
      ? process.argv[process.argv.indexOf('--out') + 1]
      : path.join(ROOT, 'research', 'url-flag-domain-2026-11-09.json'))
  ).replace(/^file:/, ''),
);

const PROGRAM = '675kPX9MHTjS2zt1qfr1NYHuzeLXfQM9H24wFSUt1Mp8';
/** A daemon run must end well inside this. The baseline for a bad --ws
 * scheme is "runs its full --seconds and exits 0", never a hang — so this
 * is a safety net, not the verdict. */
const KILL_MS = 30000;

function argOf(flag) {
  const i = process.argv.indexOf(flag);
  return i >= 0 ? process.argv[i + 1] : null;
}

// --- the trees -------------------------------------------------------------

function baselineBinary() {
  const given = argOf('--baseline');
  if (given) return { bin: path.resolve(given), source: 'given' };
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'reeflog-urlflag-baseline-'));
  const tag = process.env.BASELINE_TAG || 'v0.7.5';
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

// --- running one case ------------------------------------------------------

/** Paths must never reach the artifact (tests/research_artifacts.rs scans
 * research/ for /home/, /tmp/, /Users/, C:\ byte-exactly). The runner works
 * in mkdtemp directories, so every path-shaped token in captured output is
 * collapsed before it is stored. */
function scrub(text) {
  return String(text)
    .replace(/(?:\/[\w.@+=-]+){2,}/g, '<path>')
    .replace(/([A-Za-z]:\\[\\\w.]+)+/g, '<path>');
}

function runCase(bin, c) {
  return new Promise((resolve) => {
    // Fresh directory per case: for `start`, a journal left by a previous
    // case would change what this one creates.
    const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'reeflog-urlflag-case-'));
    // The journal the command under test will use. For `start` it is the
    // probe journal (the daemon may create it); for the rpc cases it is a
    // journal SEEDED FIRST, so the baseline reaches its transport instead of
    // refusing for "no journal at ..." — a refusal that says nothing about
    // the URL domain and must not be counted either way.
    const journalName = c.seed ? 'reeflog.db' : 'probe.db';
    const journalPath = path.join(dir, journalName);
    let setupFailed = false;
    let setupStderr = '';
    if (c.seed) {
      const init = spawnSync(bin, ['init', '--db', 'reeflog.db', '--program', PROGRAM], {
        cwd: dir,
        encoding: 'utf8',
      });
      if (init.status !== 0) {
        setupFailed = true;
        setupStderr = init.stderr || '';
      }
    }
    if (setupFailed) {
      fs.rmSync(dir, { recursive: true, force: true });
      resolve({ id: c.id, kind: c.kind, setupFailed: true, stderr: scrub(setupStderr) });
      return;
    }
    const existedBefore = fs.existsSync(journalPath);

    const args = c.args.map((a) => (a === '{db}' ? 'reeflog.db' : a === '{probe}' ? 'probe.db' : a));
    const t0 = Date.now();
    const child = spawn(bin, args, { cwd: dir });
    let stdout = '';
    let stderr = '';
    child.stdout.on('data', (d) => { stdout += d; });
    child.stderr.on('data', (d) => { stderr += d; });
    let killed = false;
    const killer = setTimeout(() => { killed = true; child.kill('SIGKILL'); }, KILL_MS);
    child.on('close', (code, signal) => {
      clearTimeout(killer);
      // "created" means the RUN created it — a journal that was seeded
      // before the run existed already, so it says nothing about the run.
      const created = fs.existsSync(journalPath) && !existedBefore;
      // The transport's own words can land on stdout (the JSON report's
      // "error" field) as well as on stderr — both are searched, because
      // "the transport said unknown scheme" is the defect, wherever it is
      // printed.
      const low = (stderr + '\n' + stdout).toLowerCase();
      // A URL refusal: the parser spoke. The flag is named, and no
      // transport's words appear (they mean the value got past the parser).
      const namesFlag = low.includes('--ws') || low.includes('--rpc');
      const transportWords = low.includes('bad uri:') || low.includes('unknown scheme:')
        || low.includes('url scheme not supported') || low.includes('http format error');
      const refused = code !== 0 && !killed && namesFlag && !transportWords;
      const value = c.value ?? '';
      const namesValue = value === '' ? true : low.includes(value.toLowerCase());
      fs.rmSync(dir, { recursive: true, force: true });
      resolve({
        id: c.id,
        kind: c.kind,
        exit: code,
        signal: signal ?? null,
        killed,
        ms: Date.now() - t0,
        journalCreated: created,
        seeded: c.seed === true,
        report: stdout.trim().length > 0 && stdout.trim().startsWith('{'),
        refused,
        namesFlag: namesFlag && code !== 0,
        namesValue: namesFlag && code !== 0 && namesValue,
        transportWords,
        stderr: scrub((stderr.trim().split('\n')[0] ?? '').slice(0, 300)),
      });
    });
  });
}

// --- the cases -------------------------------------------------------------

/**
 * kind:
 *   'refusal' — out of the consumer's domain: refused by the PARSER, before
 *               anything runs, flag and value named, no journal created.
 *   'guard'   — inside the consumer's domain: NOT refused for the URL.
 */
const CASES = [
  // --- refusals: the value is outside what the consumer can speak ---
  { id: 'ws-scheme-ftp', kind: 'refusal', args: ['start', '--db', '{probe}', '--program', PROGRAM, '--seconds', '1', '--ws', 'ftp://127.0.0.1:9/x'], value: 'ftp://127.0.0.1:9/x' },
  { id: 'ws-scheme-https', kind: 'refusal', args: ['start', '--db', '{probe}', '--program', PROGRAM, '--seconds', '1', '--ws', 'https://127.0.0.1:18099/x'], value: 'https://127.0.0.1:18099/x' },
  { id: 'ws-empty', kind: 'refusal', args: ['start', '--db', '{probe}', '--program', PROGRAM, '--seconds', '1', '--ws', ''], value: '' },
  { id: 'ws-single-slash', kind: 'refusal', args: ['start', '--db', '{probe}', '--program', PROGRAM, '--seconds', '1', '--ws', 'ws:/127.0.0.1:9'], value: 'ws:/127.0.0.1:9' },
  { id: 'ws-no-host', kind: 'refusal', args: ['start', '--db', '{probe}', '--program', PROGRAM, '--seconds', '1', '--ws', 'wss://'], value: 'wss://' },
  { id: 'ws-no-scheme', kind: 'refusal', args: ['start', '--db', '{probe}', '--program', PROGRAM, '--seconds', '1', '--ws', '127.0.0.1:9'], value: '127.0.0.1:9' },
  { id: 'rpc-scheme-ftp', kind: 'refusal', seed: true, args: ['verify', '--db', '{db}', '--rpc', 'ftp://127.0.0.1:9/x'], value: 'ftp://127.0.0.1:9/x' },
  { id: 'rpc-scheme-ws', kind: 'refusal', seed: true, args: ['verify', '--db', '{db}', '--rpc', 'ws://127.0.0.1:9/x'], value: 'ws://127.0.0.1:9/x' },
  { id: 'rpc-no-scheme', kind: 'refusal', seed: true, args: ['verify', '--db', '{db}', '--rpc', 'not-a-url'], value: 'not-a-url' },
  { id: 'rpc-empty', kind: 'refusal', seed: true, args: ['verify', '--db', '{db}', '--rpc', ''], value: '' },
  { id: 'rpc-no-host', kind: 'refusal', seed: true, args: ['verify', '--db', '{db}', '--rpc', 'https://'], value: 'https://' },

  // --- guards: everything the consumers DO accept ---
  { id: 'guard-rpc-http', kind: 'guard', seed: true, args: ['verify', '--db', '{db}', '--rpc', 'http://127.0.0.1:9'] },
  { id: 'guard-rpc-https', kind: 'guard', seed: true, args: ['verify', '--db', '{db}', '--rpc', 'https://127.0.0.1:9/x'] },
  { id: 'guard-rpc-port-path-query', kind: 'guard', seed: true, args: ['verify', '--db', '{db}', '--rpc', 'http://127.0.0.1:18899/rpc?commitment=x'] },
  { id: 'guard-ws-plain', kind: 'guard', args: ['start', '--db', '{probe}', '--program', PROGRAM, '--seconds', '0', '--ws', 'ws://127.0.0.1:9', '--rpc', 'http://127.0.0.1:9'] },
  { id: 'guard-ws-wss-default', kind: 'guard', args: ['start', '--db', '{probe}', '--program', PROGRAM, '--seconds', '0', '--ws', 'wss://api.mainnet-beta.solana.com'] },
  { id: 'guard-ws-uppercase-scheme', kind: 'guard', args: ['start', '--db', '{probe}', '--program', PROGRAM, '--seconds', '0', '--ws', 'WS://127.0.0.1:9'] },
];

// --- the verdict -----------------------------------------------------------

function judge(c, r) {
  if (r.setupFailed) return { ok: false, why: `setup failed: ${r.stderr}` };
  if (r.killed) return { ok: false, why: 'the run had to be KILLED' };
  if (c.kind === 'refusal') {
    if (!r.refused) {
      if (r.exit === 0) return { ok: false, why: 'accepted (exit 0) — the parser refused nothing' };
      if (r.transportWords) return { ok: false, why: `refused by the TRANSPORT, not the parser: ${r.stderr}` };
      if (!r.namesFlag) return { ok: false, why: `refused, but the flag is not named: ${r.stderr}` };
      return { ok: false, why: `not a parser refusal: ${r.stderr}` };
    }
    if (!r.namesValue) return { ok: false, why: `the refusal does not name the value: ${r.stderr}` };
    if (r.journalCreated) return { ok: false, why: 'the refusal left a journal behind' };
    return { ok: true, why: `refused by the parser in ${r.ms} ms, flag and value named` };
  }
  // guard: must NOT be refused for the URL, on either tree
  if (r.refused || r.namesFlag) {
    return { ok: false, why: `a value inside the consumer's domain was refused: ${r.stderr}` };
  }
  if (r.exit === 0 || r.report || r.transportWords) {
    return { ok: true, why: `not refused (${r.exit === 0 ? 'ran' : 'reached the consumer'})` };
  }
  return { ok: true, why: `not refused (exit ${r.exit})` };
}

// --- run both trees --------------------------------------------------------

async function measure(tree) {
  const rows = [];
  for (const c of CASES) {
    const r = await runCase(tree.bin, c);
    const v = judge(c, r);
    rows.push({ ...r, ok: v.ok, why: v.why });
  }
  const refusals = rows.filter((r) => r.kind === 'refusal');
  const guards = rows.filter((r) => r.kind === 'guard');
  return {
    source: tree.source,
    refusals: refusals.filter((r) => r.ok).length,
    refusalCases: refusals.length,
    guards: guards.filter((r) => r.ok).length,
    guardCases: guards.length,
    cases: rows.map((r) => ({
      id: r.id,
      ok: r.ok,
      exit: r.exit ?? null,
      killed: r.killed ?? false,
      journalCreated: r.journalCreated ?? false,
      refused: r.refused ?? false,
      ms: r.ms ?? null,
      why: scrub(r.why ?? ''),
      stderr: r.stderr ?? '',
    })),
  };
}

const baseline = baselineBinary();
const fixed = fixedBinary();
console.log(`baseline: ${baseline.source}\nfixed:    ${fixed.source}`);

const b = await measure(baseline);
const f = await measure(fixed);

const artifact = {
  what: 'The --ws / --rpc URL value domain: a URL the CONSUMER cannot speak must be refused by the PARSER, with the flag and the value named, before anything is opened or created.',
  date: '2026-11-09',
  baselineBinary: b.source,
  fixedBinary: f.source,
  consumers: {
    '--rpc': 'ureq resolves http/https only (ureq-proto SchemeExt::default_port -> None for anything else -> "unknown scheme")',
    '--ws': 'tokio-tungstenite uri_mode accepts ws/wss only (_ => Err(UnsupportedUrlScheme))',
  },
  baseline: b,
  fixed: f,
};

fs.mkdirSync(path.dirname(OUT), { recursive: true });
fs.writeFileSync(OUT, JSON.stringify(artifact, null, 2) + '\n');
console.log(`wrote ${path.relative(ROOT, OUT)}`);
console.log(
  `baseline: ${b.refusals}/${b.refusalCases} refused, ${b.guards}/${b.guardCases} guards green`,
);
console.log(`fixed:    ${f.refusals}/${f.refusalCases} refused, ${f.guards}/${f.guardCases} guards green`);

const badFixed = f.cases.filter((c) => !c.ok);
console.log('--- baseline (the defect, as measured on the published binary) ---');
for (const c of b.cases.filter((c) => !c.ok)) {
  console.log(`  baseline ${c.id}: ${c.why}`);
}
if (badFixed.length) {
  console.log('--- fixed tree (must be green) ---');
  for (const c of badFixed) console.log(`FAIL ${c.id}: ${c.why}`);
}
process.exit(badFixed.length === 0 ? 0 : 1);
