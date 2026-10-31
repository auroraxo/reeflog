// Does the account harness actually catch a broken extractor?
//
// A green harness is only evidence if it CAN go red. This mutates the
// product (not the harness) four ways, each of which must make the live
// cross-check fail:
//
//   1. ADDRESS SHIFT   — balance indices shifted by one (the lookup-table
//                        trap: attach a balance to the wrong address).
//   2. DELTA DROP      — a changed token balance silently omitted.
//   3. EMPTY DELTA     — the closed-account encoding hole the first live run
//                        found, reintroduced.
//   4. WRONG MINT      — a token row's mint replaced by another row's.
//
// Each mutation is applied to a COPY of the source tree, the binary is
// rebuilt, and the e2e must fail. The unmutated tree must pass — otherwise
// the harness is broken, not the product.
import { spawn, execFileSync } from 'node:child_process';
import fs from 'node:fs';
import path from 'node:path';

const SRC = path.resolve('./');
const HARNESS = path.resolve('./tools/e2e-accounts.mjs');
const WORK = '/tmp/reeflog-mutants';

const MUTATIONS = [
  {
    name: 'address-shift',
    find: '        let Some(address) = address_at(idx) else { continue };\n        // The delta is computed on the exact decimal strings, so a u64\n        // amount never round-trips through a double.\n        let delta = decimal_sub(&after_eff, &before_eff);',
    replace: '        let Some(address) = address_at(idx + 1) else { continue };\n        let delta = decimal_sub(&after_eff, &before_eff);',
  },
  {
    name: 'delta-drop',
    find: '        let Some(address) = address_at(idx) else { continue };\n        // The delta is computed on the exact decimal strings, so a u64\n        // amount never round-trips through a double.\n        let delta = decimal_sub(&after_eff, &before_eff);',
    replace: '        let Some(address) = address_at(idx) else { continue };\n        let delta = decimal_sub(&after_eff, &before_eff);\n        if idx % 2 == 1 { continue; }',
  },
  {
    name: 'empty-delta',
    find: '        let delta = decimal_sub(&after_eff, &before_eff);',
    replace: '        let delta = if after.is_none() { String::new() } else { decimal_sub(&after_eff, &before_eff) };',
  },
  {
    name: 'wrong-mint',
    find: '            "mint": mint,',
    replace: '            "mint": format!("{}x", mint),',
  },
];

function run(cmd, args, opts = {}) {
  return execFileSync(cmd, args, { encoding: 'utf8', stdio: ['ignore', 'pipe', 'pipe'], ...opts });
}

function e2e(bin) {
  const r = spawn('node', [HARNESS], {
    env: { ...process.env, BIN: bin },
    stdio: ['ignore', 'pipe', 'pipe'],
  });
  let o = '', e = '';
  r.stdout.on('data', (d) => { o += d; });
  r.stderr.on('data', (d) => { e += d; });
  return new Promise((res) => {
    r.on('close', (code) => res({ code, out: o, err: e }));
  });
}

fs.rmSync(WORK, { recursive: true, force: true });
fs.mkdirSync(WORK, { recursive: true });

// Baseline: the unmutated tree must PASS, or every result below is void.
// Retried once: this harness hammers the public RPC endpoint, and a 429 that
// exhausts its retries is a transient, not a verdict on the tree.
console.log('baseline (unmutated tree):');
let base = await e2e(path.join(SRC, 'target/release/reeflog'));
if (base.code !== 0) {
  console.log(`  first attempt exit=${base.code}; stderr tail: ${base.err.trim().split('\n').slice(-2).join(' | ')}`);
  console.log('  retrying once (public endpoint rate limits):');
  await new Promise((r) => setTimeout(r, 15000));
  base = await e2e(path.join(SRC, 'target/release/reeflog'));
}
console.log(`  exit=${base.code} ${base.out.trim().split('\n').slice(-2).join(' | ')}`);
if (base.code !== 0) {
  console.log('BASELINE FAILED — the harness must pass on the unmutated tree first');
  console.log(`stderr tail: ${base.err.trim().split('\n').slice(-3).join(' | ')}`);
  process.exit(2);
}

let caught = 0;
for (const m of MUTATIONS) {
  const dir = path.join(WORK, m.name);
  fs.mkdirSync(dir, { recursive: true });
  // Copy the tree without the build output and the VCS metadata.
  run('bash', ['-c', `tar -C ${SRC} --exclude=target --exclude=.git --exclude=node_modules -cf - . | tar -C ${dir} -xf -`]);
  const file = path.join(dir, 'src/accounts.rs');
  const before = fs.readFileSync(file, 'utf8');
  if (!before.includes(m.find)) {
    console.log(`FAIL ${m.name}: mutation anchor not found in src/accounts.rs`);
    continue;
  }
  fs.writeFileSync(file, before.replace(m.find, m.replace));
  console.log(`\nmutant ${m.name}: building...`);
  try {
    run('cargo', ['build', '--release'], { cwd: dir });
  } catch (err) {
    console.log(`FAIL ${m.name}: mutant does not compile (a mutant that cannot build proves nothing)`);
    continue;
  }
  const bin = path.join(dir, 'target/release/reeflog');
  const res = await e2e(bin);
  const last = res.out.trim().split('\n').slice(-3).join(' | ');
  if (res.code !== 0) {
    caught++;
    console.log(`  CAUGHT: exit=${res.code} — ${last}`);
  } else {
    console.log(`  NOT CAUGHT: the harness passed a broken product — ${last}`);
  }
}

console.log(`\n${caught}/${MUTATIONS.length} mutants caught`);
if (caught !== MUTATIONS.length) {
  console.log('MUTATION PROBE FAILED — a green harness that cannot go red proves nothing');
  process.exit(1);
}
console.log('MUTATION PROBE PASSED');
