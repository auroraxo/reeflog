// PULS W — the EXIT-CODE VOICE of the client (2026-11-12).
//
// The product's whole contract is the exit code (README: exit 0 delivered,
// exit 3 held/incomplete, exit 1 refused). The client's `run()` knew only
// 0/1/3 — and treated EVERYTHING ELSE as success. Measured against the
// PUBLISHED v0.7.19 tree (research/client-exit-voice-2026-11-12.json):
//
//   F1 crash-with-rows  — a tail that prints rows and then dies with the
//      binary's own journal-write failure code (src/main.rs exits 2 there)
//      RESOLVED as a delivered batch: held:null is documented as "the batch
//      is fully delivered" — a lie carried by the client itself;
//   F2 usage-skew       — an older `reeflog` on PATH that does not know the
//      subcommand exits 64 (usage). `status()` crashed out with a RAW
//      SyntaxError from JSON.parse(''), and `tail()` resolved as a clean
//      zero-event batch — the stranger's version skew reported health;
//   F3 crash-after-rows — verify printed its JSON report and died on a
//      panic (exit 101) BEFORE its own exit-3 verdict; baseline resolved
//      the scan as clean;
//   F4 killed-by-signal — a SIGKILL'd run leaves `close` with code=null,
//      and `code ?? 1` minted a FAKE refusal "exit 1": the message claimed
//      a refusal the binary never made, named no signal, and discarded
//      stderr. A dead process dressed up as a refusal is the same class
//      the daemon side refuses ("an incomplete pass is never a success").
//
// The fix class is narrow and the guards pin what must NOT change:
//   G1 healthy exit 0 still resolves;
//   G2 exit 1 keeps the binary's words verbatim;
//   G3 exit 3 keeps tail-hold / verify-throw semantics;
//   G4 raw() stays the documented UNCLASSIFIED escape hatch (any code
//      comes back as { exitCode, ... } — the escape hatch is the point).
import test from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { ReeflogError, reeflog } from '../index.js';

const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'reeflog-exit-voice-'));
const db = path.join(dir, 'j.db');

function fakeBin(name, body) {
  const p = path.join(dir, name);
  fs.writeFileSync(p, `#!/bin/sh\n${body}\n`, { mode: 0o755 });
  return p;
}

const TAIL_ROW = '{"seq":1,"signature":"sigA","slot":1,"err":null,"logs":["a"]}';

const BIN = {
  // F1: prints a delivered row, then dies with the journal-write code.
  f1: fakeBin('f1.sh', `echo '${TAIL_ROW}'\necho "reeflog: journal write failed: disk i/o error" >&2\nexit 2`),
  // F2: the usage-skew face — refuses an unknown subcommand with exit 64.
  f2: fakeBin('f2.sh', `echo "reeflog v0 — durable local mailbox" >&2\necho "A flag this command does not know is refused" >&2\nexit 64`),
  // F3: prints the report, then panics before its own verdict.
  f3: fakeBin('f3.sh', `echo '{"verified":true,"missing":[]}'\necho "thread 'main' panicked at src/journal.rs:42" >&2\nexit 101`),
  // G1: healthy.
  g1: fakeBin('g1.sh', `echo '{"db":"x","events":0}'\nexit 0`),
  // G2: a real refusal in the binary's own voice. (Single quotes: the
  // message carries backticks, and sh would run them as command
  // substitution inside double quotes.)
  g2: fakeBin('g2.sh', `echo 'reeflog: error: no journal at ${db} — run \`reeflog init\` first' >&2\nexit 1`),
};

test('F1: a run that dies with exit 2 AFTER printing rows is REFUSED, never delivered', async () => {
  const box = reeflog({ db, bin: BIN.f1 });
  await assert.rejects(
    () => box.tail({ limit: 10 }),
    (e) => {
      assert.ok(e instanceof ReeflogError, `must be a ReeflogError, got ${e.constructor.name}`);
      assert.equal(e.exitCode, 2, 'the verdict must carry the REAL code, not 0');
      assert.match(e.message, /exited with code 2/, 'the words must name the same code the object carries');
      assert.match(e.message, /journal write failed/, 'the binary\'s last words must reach the caller');
      return true;
    },
  );
  // status() on the same shape must not JSON.parse crash either.
  const err = await box.status().then(
    () => { throw new Error('F1b: status RESOLVED on a crashed run'); },
    (e) => e,
  );
  assert.ok(err instanceof ReeflogError, `F1b: raw ${err.constructor.name} escaped status()`);
  assert.equal(err.exitCode, 2);
});

test('F2: the usage-skew exit 64 is REFUSED BY NAME, not a clean empty batch', async () => {
  const box = reeflog({ db, bin: BIN.f2 });
  // On the baseline `tail` resolved with exitCode 64 and held:null —
  // the documented shape of "fully delivered".
  const err = await box.tail({ limit: 10 }).then(
    () => { throw new Error('F2a: exit 64 RESOLVED as a delivered batch'); },
    (e) => e,
  );
  assert.ok(err instanceof ReeflogError);
  assert.equal(err.exitCode, 64, 'the skew must be named with its real code');
  assert.match(err.message, /exited with code 64/, 'the words must name the same code the object carries');
  assert.match(err.message, /durable local mailbox/, 'the usage text is the proof of version skew');
  const err2 = await box.status().then(() => { throw new Error('F2b: raw crash escaped'); }, (e) => e);
  assert.ok(err2 instanceof ReeflogError, `F2b: raw ${err2.constructor.name} from JSON.parse`);
});

test('F3: a panic (exit 101) after a printed report is REFUSED, never a clean scan', async () => {
  const box = reeflog({ db, bin: BIN.f3 });
  const err = await box.verify({}).then(
    () => { throw new Error('F3: a panicked verify RESOLVED as clean'); },
    (e) => e,
  );
  assert.ok(err instanceof ReeflogError);
  assert.equal(err.exitCode, 101);
  assert.match(err.message, /panicked/, 'the panic line must reach the verdict');
});

test('F4: a run killed by a signal names the signal — never a forged "exit 1" refusal', async () => {
  // The child kills ITSELF by signal: `close` then carries code=null and
  // signal='SIGTERM'. (Killing the parent would take the test runner with
  // it — the parent stays alive on purpose.)
  const killer = fakeBin('f4.sh', `echo '{"db":"x"}'\nkill -TERM $$\nsleep 5`);
  const box = reeflog({ db, bin: killer });
  const err = await box.status().then(() => { throw new Error('F4: a killed run RESOLVED'); }, (e) => e);
  assert.ok(err instanceof ReeflogError);
  assert.match(err.message, /SIG|signal/, `the verdict must name the signal, got: ${err.message}`);
  assert.doesNotMatch(err.message, /^exit 1$/, 'the forgery class: a bare "exit 1" for a death that was never a refusal');
  assert.equal(err.exitCode, null, 'there was no exit code — say so');
});

test('G1: a healthy exit 0 still resolves', async () => {
  const s = await reeflog({ db, bin: BIN.g1 }).status();
  assert.equal(s.events, 0);
});

test('G2: exit 1 keeps the binary\'s words verbatim', async () => {
  await assert.rejects(
    () => reeflog({ db, bin: BIN.g2 }).status(),
    (e) => {
      assert.ok(e instanceof ReeflogError);
      assert.equal(e.exitCode, 1);
      assert.match(e.message, /no journal at .* run `reeflog init` first/);
      return true;
    },
  );
});

test('G3: exit 3 keeps the documented shapes — tail holds, verify throws INCOMPLETE', async () => {
  const heldBin = fakeBin('g3-tail.sh', `echo '${TAIL_ROW}'\necho "reeflog: HELD at seq 2 sigB: the account envelope is still owed" >&2\nexit 3`);
  const r = await reeflog({ db, bin: heldBin }).tail({ limit: 10 });
  assert.equal(r.exitCode, 3);
  assert.equal(r.events.length, 1);
  assert.equal(r.held.signature, 'sigB');
  const verifyBin = fakeBin('g3-verify.sh', `echo "reeflog: scan INCOMPLETE" >&2\nexit 3`);
  await assert.rejects(
    () => reeflog({ db, bin: verifyBin }).verify({}),
    (e) => e instanceof ReeflogError && e.exitCode === 3 && /INCOMPLETE/.test(e.message),
  );
});

test('G4: raw() stays the unclassified escape hatch', async () => {
  const r = await reeflog({ db, bin: BIN.f2 }).raw(['bogus', '--db', db]);
  assert.equal(r.exitCode, 64, 'raw hands the code back — it is not a reeflog command');
});
