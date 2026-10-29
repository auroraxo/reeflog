// Falsification of the dead-letter-queue harness itself.
//
// A green harness proves nothing unless it CAN go red. This script mutates
// the sender in ways that would each break one of the dead-letter
// guarantees, and asserts that tools/e2e-dlq.mjs catches every one of them.
//
// The mutations are applied to a COPY of the repository — the real tree is
// never touched — and each mutation is reverted before the next.
//
//   D1  the dead-letter queue is disabled (the threshold is never reached)
//       -> the refused event never lands in the DLQ and the queue dies
//   D2  a quarantined event is also written as a delivery receipt
//       -> the harness must see a delivery that never happened
//   D3  a 5xx is classified as a refusal (a transient outage becomes a
//       verdict) -> the harness must see a dead letter that must not exist
//   D4  the replay sends different bytes than the ones quarantined
//       -> the harness must see bytes that do not match
import { spawnSync } from 'node:child_process';
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

// One reusable pristine copy: the FIRST build is the expensive one (all
// dependencies), and every later mutation is an incremental rebuild of a
// single file. The real tree is never written to.
const DIR = fs.mkdtempSync(path.join(os.tmpdir(), 'reeflog-dlq-mut-'));
spawnSync('bash', ['-c',
  `tar -C ${JSON.stringify(REPO)} --exclude=./target --exclude=./.git --exclude=./node_modules -cf - . | tar -C ${JSON.stringify(DIR)} -xf -`]);

function build() {
  const r = spawnSync('cargo', ['build', '--release'], { cwd: DIR, encoding: 'utf8' });
  if (r.status !== 0) {
    console.error(r.stdout?.slice(-3000), r.stderr?.slice(-3000));
    throw new Error('build failed');
  }
}

function runHarness() {
  const r = spawnSync('node', ['tools/e2e-dlq.mjs'], {
    cwd: DIR,
    encoding: 'utf8',
    timeout: 600000,
    env: { ...process.env, REEFLOG_BIN: path.join(DIR, BIN_REL) },
  });
  const out = `${r.stdout || ''}${r.stderr || ''}`;
  const failedChecks = [...out.matchAll(/^FAIL (.+?) —/gm)].map((m) => m[1]);
  const passedChecks = [...out.matchAll(/^PASS (.+?) —/gm)].map((m) => m[1]);
  return { code: r.status, out, failedChecks, passedChecks };
}

function file(rel) { return path.join(DIR, rel); }
function read(rel) { return fs.readFileSync(file(rel), 'utf8'); }
function write(rel, s) { fs.writeFileSync(file(rel), s); }

// Apply a mutation, or throw if the anchor moved (a silent no-op mutation
// would make every "caught" result meaningless).
function mutate(rel, from, to, tag) {
  const s = read(rel);
  const out = s.split(from).join(to);
  if (out === s) throw new Error(`${tag} did not apply`);
  write(rel, out);
}

// --- D1: disable the dead-letter queue ------------------------------------
function mutationDlqDisabled() {
  mutate('src/webhook.rs',
    `if target.dead_letter_after > 0 && refusals >= target.dead_letter_after {`,
    `if false {`,
    'D1');
}

// --- D2: a quarantined event is also a delivery ---------------------------
function mutationDeadLetterCountsAsDelivery() {
  mutate('src/webhook.rs',
    `                    rep.dead_lettered += 1;`,
    `                    j.webhook_record_delivered(&target.id, "event", r.seq, &body)?;
                    rep.dead_lettered += 1;`,
    'D2');
}

// --- D3: a 5xx becomes a verdict ------------------------------------------
function mutationTransientIsRefusal() {
  mutate('src/webhook.rs',
    `    } else {
        Attempt::Transient(format!("http {status}"))
    }
}`,
    `    } else {
        Attempt::Refused(status)
    }
}`,
    'D3');
}

// --- D4: the replay sends different bytes ---------------------------------
function mutationReplayChangesBytes() {
  mutate('src/main.rs',
    `let outcome = match poster.post(&t.url, &r.body, &headers) {`,
    `let outcome = match poster.post(&t.url, &format!("{} ", r.body), &headers) {`,
    'D4');
}

const mutations = [
  ['D1_dlq_disabled', mutationDlqDisabled,
    'the_refused_event_lands_in_the_dead_letter_queue'],
  ['D2_dead_letter_counts_as_delivery', mutationDeadLetterCountsAsDelivery,
    'the_refused_event_is_never_counted_as_delivered'],
  ['D3_transient_outage_becomes_a_verdict', mutationTransientIsRefusal,
    'a_503_is_retried_not_quarantined'],
  ['D4_replay_sends_other_bytes', mutationReplayChangesBytes,
    'the_replay_sends_byte_identical_bytes'],
].filter(([name]) => !process.env.ONLY || name.startsWith(process.env.ONLY));

for (const [name, mutateFn, expectedFailingCheck] of mutations) {
  console.log(`\n=== ${name} ===`);
  try {
    mutateFn();
    build();
    const r = runHarness();
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
    // Restore the pristine tree for the next mutation.
    spawnSync('bash', ['-c',
      `tar -C ${JSON.stringify(REPO)} --exclude=./target --exclude=./.git --exclude=./node_modules -cf - . | tar -C ${JSON.stringify(DIR)} -xf -`]);
  }
}

fs.rmSync(DIR, { recursive: true, force: true });
console.log(`\n${results.filter(Boolean).length}/${results.length} mutations caught`);
process.exit(results.every(Boolean) ? 0 : 1);
