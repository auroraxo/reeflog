// Does the owners harness actually catch a broken owner map?
//
// A green harness is only evidence if it CAN go red. This mutates the
// product (not the harness) three ways, each of which must make the live
// e2e fail:
//
//   1. GUESS-THE-MISSING — an address the chain does not have is written
//      with a plausible owner (the System Program) instead of landing in
//      `missing`. This is the single defect the whole design exists to
//      prevent: a guessed authority is indistinguishable from a fetched one
//      to a consumer.
//   2. ADDRESS-SHIFT — owners attached to the WRONG address (off-by-one in
//      the batch mapping): the classic index corruption this repo has been
//      bitten by twice (lookup-table balances, account list).
//   3. FREE-AUTHORITY-DROP — the tx-time token program copied from the tx
//      response is dropped, so changed token accounts name no program.
//
// Each mutation is applied to a COPY of the source tree, the binary is
// rebuilt, and the e2e must fail. The unmutated tree must pass — otherwise
// the harness is broken, not the product.
import { spawn, execFileSync } from 'node:child_process';
import fs from 'node:fs';
import path from 'node:path';

const SRC = path.resolve('./');
const HARNESS = path.resolve('./tools/e2e-owners.mjs');
const WORK = '/tmp/reeflog-owner-mutants';

const MUTATIONS = [
  {
    name: 'guess-the-missing',
    file: 'src/owners.rs',
    find: '            if entry.is_null() {\n                // The chain does not have this account. A fact, recorded.\n                gone.insert(addr.clone());',
    replace: '            if entry.is_null() {\n                // MUTANT: guess a plausible owner instead of recording absence.\n                owned.insert(addr.clone(), "11111111111111111111111111111111".to_string());',
  },
  {
    name: 'address-shift',
    file: 'src/owners.rs',
    find: '            } else if let Some(o) = entry.get("owner").and_then(|v| v.as_str()) {\n                owned.insert(addr.clone(), o.to_string());',
    replace: '            } else if let Some(o) = value.get(i.wrapping_sub(1)).and_then(|e| e.get("owner")).and_then(|v| v.as_str()) {\n                owned.insert(addr.clone(), o.to_string());',
  },
  {
    name: 'free-authority-drop',
    file: 'src/accounts.rs',
    find: '        if let Some(p) = tx_time_program.get(&idx) {\n            token[\"token_program"] = json!(p);\n        }',
    replace: '        if false {\n            token["token_program"] = json!(tx_time_program.get(&idx));\n        }',
  },
];

function run(cmd, args, opts = {}) {
  return execFileSync(cmd, args, { encoding: 'utf8', stdio: ['ignore', 'pipe', 'pipe'], ...opts });
}

let caught = 0;

// Baseline first: the unmutated tree must pass the same harness.
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
    const c = spawn('node', [HARNESS], { cwd, env: { ...process.env, BIN: bin, DIR: `/tmp/e2e-owners-${path.basename(path.dirname(bin))}-${Date.now()}` }, stdio: ['ignore', 'pipe', 'pipe'] });
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
  // then call it inconclusive and fail — a probe that cannot tell an RPC
  // flake from a red check proves nothing either way.
  if (res.code === 2) {
    console.log(`  setup failed (exit=2), retrying once...`);
    await new Promise((r) => setTimeout(r, 20000));
    res = await runHarness(bin, dir);
  }
  const last = res.out.trim().split('\n').slice(-4).join(' | ');
  if (res.code === 1) {
    caught++;
    console.log(`  CAUGHT: exit=1 — ${last}`);
  } else if (res.code === 0) {
    console.log(`  NOT CAUGHT: the harness passed a broken product — ${last}`);
  } else {
    console.log(`  INCONCLUSIVE: exit=${res.code} — ${last}`);
  }
}

console.log(`\n${caught}/${MUTATIONS.length} mutants caught`);
if (caught !== MUTATIONS.length) {
  console.log('MUTATION PROBE FAILED — a green harness that cannot go red proves nothing');
  process.exit(1);
}
console.log('MUTATION PROBE PASSED');
