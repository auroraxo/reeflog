// Does the account-data harness actually catch a broken byte map?
//
// A green harness is only evidence if it CAN go red. This mutates the
// product (not the harness) three ways, each of which must be caught:
//
//   1. GUESS-THE-MISSING — an address the chain does not have is written as
//      an empty `held` entry instead of landing in `missing`. The single
//      defect the design exists to prevent: absence that looks like data.
//   2. TRUNCATION-DENIAL — an account over the cap is stored with
//      `truncated: false` (the bytes still cut at the cap), so a consumer
//      believes it holds the whole account.
//   3. ADDRESS-SHIFT — bytes attached to the WRONG address (every address
//      gets its neighbour's bytes while the row stays COMPLETE): the
//      classic index corruption this repo has been bitten by twice
//      (lookup-table balances, account list). Completeness is preserved on
//      purpose — a shift that merely broke the pass would be a vacuous
//      catch; this one must be caught by the content checks.
//
// Each mutant is judged by TWO gates:
//   - the offline unit tests (`cargo test`), which are deterministic, and
//   - the live e2e harness against mainnet.
// A mutant must be caught by at least one gate; both results are reported
// per mutant, because "caught by the unit test" and "caught by the live
// harness" are different claims. The unmutated tree must pass BOTH —
// otherwise the gates are broken, not the product.
import { spawn, execFileSync } from 'node:child_process';
import fs from 'node:fs';
import path from 'node:path';

const SRC = path.resolve('./');
const HARNESS = path.resolve('./tools/e2e-account-data.mjs');
const WORK = '/tmp/reeflog-account-data-mutants';

const MUTATIONS = [
  {
    name: 'guess-the-missing',
    file: 'src/account_data.rs',
    find: '            if entry.is_null() {\n                // The chain does not have this account. A fact, recorded.\n                entries.insert(\n                    addr.clone(),\n                    Entry {\n                        data: None,\n                        bytes: None,\n                        executable: false,\n                        missing: true,',
    replace: '            if entry.is_null() {\n                // MUTANT: pretend absence is an empty account instead of recording it.\n                entries.insert(\n                    addr.clone(),\n                    Entry {\n                        data: Some(String::new()),\n                        bytes: Some(0),\n                        executable: false,\n                        missing: false,',
  },
  {
    name: 'truncation-denial',
    file: 'src/account_data.rs',
    find: '            let truncated = space.map(|s| s > ACCOUNT_DATA_CAP as u64).unwrap_or(false)\n                || bytes.map(|b| b > ACCOUNT_DATA_CAP as u64).unwrap_or(false);',
    replace: '            let truncated = false; // MUTANT: deny truncation even when the account is longer than the cap',
  },
  {
    name: 'address-shift',
    file: 'src/account_data.rs',
    find: '        for (i, entry) in value.iter().enumerate() {\n            let Some(addr) = chunk.get(i) else { break };\n            if entry.is_null() {\n                // The chain does not have this account. A fact, recorded.',
    replace: '        let vlen = value.len();\n        for i in 0..vlen {\n            let entry = &value[(i + 1) % vlen]; // MUTANT: bytes of address i+1 stored under address i (row stays complete)\n            let Some(addr) = chunk.get(i) else { break };\n            if entry.is_null() {\n                // The chain does not have this account. A fact, recorded.',
  },
];

function run(cmd, args, opts = {}) {
  return execFileSync(cmd, args, { encoding: 'utf8', stdio: ['ignore', 'pipe', 'pipe'], ...opts });
}

let caught = 0;

// Gate 1 (deterministic): the offline unit tests on the unmutated tree.
console.log('baseline: unit tests on the unmutated tree...');
try {
  run('cargo', ['test', '--release'], { cwd: SRC });
} catch (err) {
  console.log('BASELINE UNIT TESTS FAILED — the deterministic gate is broken');
  console.log(String(err.stdout || err.stderr || err).split('\n').slice(-10).join('\n'));
  process.exit(1);
}
console.log('baseline unit tests PASSED\n');

// Baseline live: the unmutated tree must pass the same harness.
console.log('baseline: building unmutated binary...');
run('cargo', ['build', '--release'], { cwd: SRC });
const baseBin = path.join(SRC, 'target/release/reeflog');
const base = await runHarness(baseBin, SRC);
if (base.code !== 0) {
  console.log(`BASELINE FAILED (exit=${base.code}) — the harness cannot prove anything on a working tree`);
  console.log(base.out.trim().split('\n').slice(-6).join('\n'));
  process.exit(1);
}
console.log('baseline PASSED — the harness is green on the working product\n');

async function runHarness(bin, cwd) {
  return new Promise((resolve) => {
    const c = spawn('node', [HARNESS], { cwd, env: { ...process.env, BIN: bin, DIR: `/tmp/e2e-account-data-${path.basename(path.dirname(bin))}-${Date.now()}` }, stdio: ['ignore', 'pipe', 'pipe'] });
    let o = '', e = '';
    c.stdout.on('data', (d) => { o += d; });
    c.stderr.on('data', (d) => { e += d; });
    c.on('close', (code) => resolve({ code, out: o, err: e }));
    c.on('error', (err) => resolve({ code: null, out: o, err: `${e}\n${err}` }));
  });
}

for (const m of MUTATIONS) {
  const dir = path.join(WORK, m.name);
  fs.rmSync(dir, { recursive: true, force: true });
  fs.mkdirSync(dir, { recursive: true });
  // Copy the tree without the build output and the VCS metadata.
  run('bash', ['-c', `tar -C ${SRC} --exclude=target --exclude=.git --exclude=node_modules -cf - . | tar -C ${dir} -xf -`]);
  const file = path.join(dir, m.file);
  const before = fs.readFileSync(file, 'utf8');
  if (!before.includes(m.find)) {
    console.log(`FAIL ${m.name}: mutation anchor not found in ${m.file}`);
    continue;
  }
  fs.writeFileSync(file, before.replace(m.find, m.replace));

  // Gate A: deterministic unit tests.
  let unitCaught = false;
  try {
    run('cargo', ['test', '--release'], { cwd: dir });
  } catch (err) {
    unitCaught = true;
    const tail = String(err.stdout || err.stderr || err).split('\n').filter((l) => l.includes('FAILED') || l.includes('panicked')).slice(0, 3).join(' | ');
    console.log(`mutant ${m.name}: CAUGHT by unit tests — ${tail}`);
  }
  if (!unitCaught) console.log(`mutant ${m.name}: unit tests PASSED (not caught by the deterministic gate) — trying the live harness...`);

  console.log(`mutant ${m.name}: building...`);
  try {
    run('cargo', ['build', '--release'], { cwd: dir });
  } catch (err) {
    console.log(`FAIL ${m.name}: mutant does not compile (a mutant that cannot build proves nothing)`);
    console.log(String(err.stderr || err.stdout || err).split('\n').slice(-8).join('\n'));
    continue;
  }
  const bin = path.join(dir, 'target/release/reeflog');
  let res = await runHarness(bin, dir);
  // Exit 2 is setup (rate limit / empty window), not a red check: retry once,
  // then call it inconclusive — a probe that cannot tell an RPC flake from a
  // red check proves nothing either way.
  if (res.code === 2) {
    console.log('  setup failed (exit=2), retrying once...');
    await new Promise((r) => setTimeout(r, 20000));
    res = await runHarness(bin, dir);
  }
  const last = res.out.trim().split('\n').slice(-4).join(' | ');
  const liveCaught = res.code === 1;
  if (liveCaught) console.log(`  live harness CAUGHT: exit=1 — ${last}`);
  else if (res.code === 0) console.log(`  live harness: green (window may not exercise the mutant) — ${last}`);
  else console.log(`  live harness: exit=${res.code} (not a red check) — ${last}`);

  if (unitCaught || liveCaught) {
    caught++;
    console.log(`  RESULT: caught by ${[unitCaught ? 'unit tests' : null, liveCaught ? 'live harness' : null].filter(Boolean).join(' + ')}`);
  } else {
    console.log(`  NOT CAUGHT by either gate (unit=pass, live=exit ${res.code})`);
  }
}

console.log(`\n${caught}/${MUTATIONS.length} mutants caught`);
if (caught !== MUTATIONS.length) {
  console.log('MUTATION PROBE FAILED — a green gate that cannot go red proves nothing');
  process.exit(1);
}
console.log('MUTATION PROBE PASSED');
