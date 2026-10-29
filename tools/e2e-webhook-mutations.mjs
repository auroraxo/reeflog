// Falsification of the webhook harness itself.
//
// A green harness proves nothing unless it CAN go red. This script mutates
// the sender in ways that would each break one of the guarantees, and
// asserts that the e2e harness catches every one of them.
//
// The mutations are applied to a COPY of the repository — the real tree is
// never touched — and each mutation is reverted before the next.
//
//   M1  the signature is computed over the wrong bytes (body mutated after
//       signing)  -> the receiver must reject every delivery
//   M2  a refused (403) endpoint is counted as a delivery -> the harness must
//       see a delivery it should not have
//   M3  the receipt check is removed (crash window re-sends) -> the harness
//       must see a duplicate
//   M4  the retraction channel is gated behind the event queue -> the harness
//       must see a retraction that never arrives
import { spawn, spawnSync } from 'node:child_process';
import fs from 'node:fs';
import path from 'node:path';
import os from 'node:os';

const REPO = path.resolve('.');
const BIN_REL = 'target/release/reeflog';

const results = [];
const check = (name, pass, detail) => {
  results.push(pass);
  console.log(`${pass ? 'PASS' : 'FAIL'} ${name}${detail ? ` — ${detail}` : ''}`);
};

// A pristine copy of the tree (without target/ and .git) that we mutate.
function makeCopy(tag) {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), `reeflog-mut-${tag}-`));
  spawnSync('bash', ['-c',
    `tar -C ${JSON.stringify(REPO)} --exclude=./target --exclude=./.git --exclude=./node_modules -cf - . | tar -C ${JSON.stringify(dir)} -xf -`]);
  return dir;
}

function build(dir) {
  const r = spawnSync('cargo', ['build', '--release'], { cwd: dir, encoding: 'utf8' });
  if (r.status !== 0) {
    console.error(r.stdout?.slice(-3000), r.stderr?.slice(-3000));
    throw new Error('build failed');
  }
}

function runHarness(dir) {
  const r = spawnSync('node', ['tools/e2e-webhook.mjs'], {
    cwd: dir,
    encoding: 'utf8',
    timeout: 600000,
    env: { ...process.env, REEFLOG_BIN: path.join(dir, BIN_REL) },
  });
  const out = `${r.stdout || ''}${r.stderr || ''}`;
  const failedChecks = [...out.matchAll(/^FAIL (.+?) —/gm)].map((m) => m[1]);
  const passedChecks = [...out.matchAll(/^PASS (.+?) —/gm)].map((m) => m[1]);
  return { code: r.status, out, failedChecks, passedChecks };
}

// The harness resolves the binary relative to the repo root; make sure the
// copy is what gets exercised.
function patchBinaryPath(dir) {
  const p = path.join(dir, 'tools', 'e2e-webhook.mjs');
  const s = fs.readFileSync(p, 'utf8');
  const patched = s.replace(
    "const BIN = path.resolve('./target/release/reeflog');",
    "const BIN = process.env.REEFLOG_BIN || path.resolve('./target/release/reeflog');");
  fs.writeFileSync(p, patched);
}

// --- M1: sign the wrong bytes --------------------------------------------
async function mutationWrongBytes(dir) {
  const p = path.join(dir, 'src', 'webhook.rs');
  const s = fs.readFileSync(p, 'utf8');
  const out = s.replace(
    'let sig = signature_header(&target.secret, &body);',
    'let sig = signature_header(&target.secret, &format!("{body} "));');
  if (out === s) throw new Error('M1 did not apply');
  fs.writeFileSync(p, out);
}

// --- M2: count a refusal as a delivery -----------------------------------
async function mutationRefusalIsDelivery(dir) {
  const p = path.join(dir, 'src', 'webhook.rs');
  const s = fs.readFileSync(p, 'utf8');
  const out = s.replace(
    `    if (200..300).contains(&status) {
        Attempt::Delivered(status)`,
    `    if (true) {
        Attempt::Delivered(status)`);
  if (out === s) throw new Error('M2 did not apply');
  fs.writeFileSync(p, out);
}

// --- M3: drop the receipt guard (re-send after a crash) -------------------
async function mutationNoReceiptGuard(dir) {
  const p = path.join(dir, 'src', 'webhook.rs');
  const s = fs.readFileSync(p, 'utf8');
  const out = s.replace(
    `        if j.webhook_delivered(&target.id, "event", r.seq)? {`,
    `        if false {`);
  if (out === s) throw new Error('M3 did not apply');
  fs.writeFileSync(p, out);
}

// --- M4: gate the retraction channel behind the blocked event queue -------
async function mutationReorgBehindEvents(dir) {
  const p = path.join(dir, 'src', 'webhook.rs');
  const s = fs.readFileSync(p, 'utf8');
  const out = s.replace(
    `    let rows = j.reorgs_after(next_reorg_id, limit.max(1))?;`,
    `    // MUTATION: a retraction waits behind a blocked event queue
    if j.webhook_summary(&target.id)?.last_error.is_some() {
        return Ok(rep);
    }
    let rows = j.reorgs_after(next_reorg_id, limit.max(1))?;`);
  if (out === s) throw new Error('M4 did not apply');
  fs.writeFileSync(p, out);
}

const mutations = [
  ['M1_signs_the_wrong_bytes', mutationWrongBytes,
    'every_delivered_body_verifies_against_the_shared_secret'],
  ['M2_counts_a_refusal_as_a_delivery', mutationRefusalIsDelivery,
    'a_403_endpoint_is_recorded_as_a_failure_not_a_delivery'],
  ['M3_resends_after_a_crash', mutationNoReceiptGuard,
    'the_owed_event_is_never_sent_twice_after_a_crash'],
  ['M4_hides_the_retraction_behind_the_blocked_queue', mutationReorgBehindEvents,
    'a_retraction_reaches_the_consumer_while_events_are_blocked'],
].filter(([name]) => !process.env.ONLY || name.startsWith(process.env.ONLY));

for (const [name, mutate, expectedFailingCheck] of mutations) {
  console.log(`\n=== ${name} ===`);
  const dir = makeCopy(name.slice(0, 3).toLowerCase());
  patchBinaryPath(dir);
  try {
    await mutate(dir);
    build(dir);
    const r = runHarness(dir);
    console.log(`harness exit=${r.code}`);
    for (const f of r.failedChecks) console.log(`  caught: ${f}`);
    check(
      `${name}_is_caught_by_the_harness`,
      r.code !== 0 && r.failedChecks.includes(expectedFailingCheck),
      `expected "${expectedFailingCheck}" to fail; failed checks: ${JSON.stringify(r.failedChecks)}`,
    );
  } catch (e) {
    check(`${name}_is_caught_by_the_harness`, false, `mutation error: ${e.message}`);
  } finally {
    fs.rmSync(dir, { recursive: true, force: true });
  }
}

console.log(`\n${results.filter(Boolean).length}/${results.length} mutations caught`);
process.exit(results.every(Boolean) ? 0 : 1);
