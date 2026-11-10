#!/usr/bin/env node
// The probe layer behind research/error-voice-<date>.json: it measures what
// reeflog's REFUSAL VOICE does with a value the operator supplied.
//
//   node tools/probe-error-voice.mjs <binary> <out-dir>
//
// It writes the raw sweeps the artifact maker consumes:
//
//   error-voice.json   per case: the exit code, the raw stderr bytes, how
//                      many lines of it look like reeflog's own, and whether
//                      a control byte reached the terminal
//
// THE DEFECT (found 2026-11-10, measured on the PUBLISHED v0.7.9 asset):
// every refusal ends by naming the offending value — and it names it RAW.
// A value carrying a NEWLINE therefore splits the refusal into two lines,
// and the second one starts with `reeflog: error: ` like a real message:
// the operator's own input can forge a log line. The same value's ESC
// bytes reach the terminal unchanged, and a DEL or a C0 byte lands in the
// log stream as a control character.
//
// The refusal is still a refusal (exit 1, the value IS named) — what is
// wrong is that the voice is not ONE line and not free of control bytes.
//
// WHY THIS FILE EXISTS (2026-11-10): the same reason as
// tools/probe-webhook-value-domain.mjs — a measurement nobody can re-run
// from the repo is not a measurement. Regenerate with:
//
//   node tools/probe-error-voice.mjs <binary> /tmp/out
//   node tools/make-error-voice-artifact.mjs /tmp/out research/error-voice-<date>.json
import { spawnSync } from 'node:child_process';
import fs from 'node:fs';
import path from 'node:path';

const BIN = path.resolve(process.argv[2]);
const OUT = process.argv[3];
if (!BIN || !OUT) {
  console.error('usage: probe-error-voice.mjs <binary> <out-dir>');
  process.exit(2);
}
fs.mkdirSync(OUT, { recursive: true });

const PROGRAM = 'whirLbMiicVdio4qvUfM5KAg6Ct8VwpYzGff3uctyCc';

function run(args, cwd) {
  const r = spawnSync(BIN, args, { cwd, encoding: 'buffer' });
  return {
    exitCode: r.status,
    stdout: r.stdout ? r.stdout.toString('utf8') : '',
    stderrRaw: r.stderr ? Buffer.from(r.stderr) : Buffer.alloc(0),
  };
}

// The bytes that must not reach a terminal or a log line raw: every C0
// control byte except HTAB, plus DEL. HTAB is a legal character in a line
// of text, so it is not part of this class.
//
// The ONE trailing newline the message itself ends with is the line
// terminator, not part of it — so it is excluded. Everything else counts,
// including a newline that arrives from INSIDE the message (which is exactly
// what a forged log line looks like).
function controlBytes(buf) {
  let body = buf;
  if (body.length && body[body.length - 1] === 0x0a) body = body.slice(0, -1);
  const found = [];
  for (const b of body) {
    if ((b < 0x20 && b !== 0x09) || b === 0x7f) found.push(b);
  }
  return found;
}

// How many lines of the stderr look like reeflog's OWN message voice? The
// first one is the real refusal; every further one was forged by a value.
function reeflogLines(buf) {
  const text = buf.toString('utf8');
  return text.split('\n').filter((l) => l.startsWith('reeflog: ')).length;
}

function cases() {
  return [
    {
      id: 'config-id-newline',
      how: 'the config value `id` carries a newline; the loader refuses it and names it',
      // A config the loader must refuse — the refusal is where the value is
      // echoed, so the echo is what is measured.
      setup: (dir) => {
        fs.writeFileSync(
          path.join(dir, 'cfg.json'),
          JSON.stringify({
            targets: [{ id: 't1\nreeflog: error: forged', url: 'http://127.0.0.1:9/hook', secret: 's' }],
          }),
        );
      },
      args: ['webhook', 'status', '--db', 'j.db', '--config', 'cfg.json'],
    },
    {
      id: 'config-url-newline',
      how: 'the config value `url` carries a newline',
      setup: (dir) => {
        fs.writeFileSync(
          path.join(dir, 'cfg.json'),
          JSON.stringify({
            targets: [{ id: 't1', url: 'http://127.0.0.1:9/a\nreeflog: error: forged', secret: 's' }],
          }),
        );
      },
      args: ['webhook', 'status', '--db', 'j.db', '--config', 'cfg.json'],
    },
    {
      id: 'flag-rpc-newline',
      how: 'the flag value `--rpc` carries a newline',
      setup: () => {},
      args: ['start', '--db', 'j.db', '--program', PROGRAM, '--rpc', 'http://h/a\nreeflog: error: forged'],
    },
    {
      id: 'flag-numeric-newline',
      how: 'the flag value `--seconds` carries a newline',
      setup: () => {},
      args: ['start', '--db', 'j.db', '--program', PROGRAM, '--seconds', '5\nreeflog: error: forged'],
    },
    {
      id: 'flag-target-newline',
      how: 'the flag value `--target` carries a newline',
      setup: (dir) => {
        fs.writeFileSync(
          path.join(dir, 'cfg.json'),
          JSON.stringify({ targets: [{ id: 'alpha', url: 'http://127.0.0.1:9/hook', secret: 's' }] }),
        );
      },
      args: ['webhook', 'dlq', 'list', '--db', 'j.db', '--config', 'cfg.json', '--target', 'a\nreeflog: error: forged'],
    },
    {
      id: 'config-id-esc',
      how: 'the config value `id` carries an ESC sequence',
      setup: (dir) => {
        fs.writeFileSync(
          path.join(dir, 'cfg.json'),
          JSON.stringify({
            targets: [{ id: 't1\u001b[31mRED', url: 'http://127.0.0.1:9/hook', secret: 's' }],
          }),
        );
      },
      args: ['webhook', 'status', '--db', 'j.db', '--config', 'cfg.json'],
    },
    {
      id: 'flag-target-esc',
      how: 'the flag value `--target` carries an ESC sequence',
      setup: (dir) => {
        fs.writeFileSync(
          path.join(dir, 'cfg.json'),
          JSON.stringify({ targets: [{ id: 'alpha', url: 'http://127.0.0.1:9/hook', secret: 's' }] }),
        );
      },
      args: ['webhook', 'dlq', 'list', '--db', 'j.db', '--config', 'cfg.json', '--target', 'a\u001b[31mRED'],
    },
  ];
}

const results = [];
for (const c of cases()) {
  const dir = fs.mkdtempSync('/tmp/reeflog-error-voice-');
  const db = path.join(dir, 'j.db');
  run(['init', '--db', db, '--program', PROGRAM], dir);
  c.setup(dir);
  const out = run(c.args, dir);
  const ctrl = controlBytes(out.stderrRaw);
  results.push({
    id: c.id,
    how: c.how,
    exitCode: out.exitCode,
    refused: out.exitCode !== 0,
    stdoutEmpty: out.stdout.trim() === '',
    reeflogLines: reeflogLines(out.stderrRaw),
    forgedLines: Math.max(0, reeflogLines(out.stderrRaw) - 1),
    controlBytes: ctrl.length,
    controlByteList: ctrl,
    stderrText: out.stderrRaw.toString('utf8'),
  });
  fs.rmSync(dir, { recursive: true, force: true });
}

const summary = {
  total: results.length,
  refused: results.filter((r) => r.refused).length,
  forgedLines: results.reduce((n, r) => n + r.forgedLines, 0),
  controlBytes: results.reduce((n, r) => n + r.controlBytes, 0),
};
fs.writeFileSync(path.join(OUT, 'error-voice.json'), JSON.stringify({ summary, cases: results }, null, 2));
console.log(JSON.stringify(summary, null, 2));
