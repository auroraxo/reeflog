#!/usr/bin/env node
// The stranger path, measured against the REAL published release.
//
// The offline suite (test/bootstrap.test.mjs) proves the contract against a
// local server that plays GitHub. This run asks the real question against
// the real published release (default: the package's own version): does a
// consumer with NO Rust toolchain get a
// working `reeflog` binary in one step, and does the client then deliver
// REAL rows out of a REAL mainnet journal?
//
// Checks (each falsifiable):
//  1. BOOTSTRAP      — bootstrap() installs from the published release,
//                      verifies the sha256 the release API stores, and the
//                      installed binary reports the release version.
//  2. CACHED        — the second call resolves from disk, no download.
//  3. CLIENT WORKS  — the client, given THAT binary, delivers rows from a
//                      real journal, in order, without replay.
//  4. REFUSAL       — a wrong journal path rejects with the binary's words.
//
// Exit: 0 all pass, 1 a check failed, 2 setup failed.
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { execFileSync } from 'node:child_process';
import { bootstrap, reeflog } from '../index.js';

const pkg = JSON.parse(fs.readFileSync(new URL('../package.json', import.meta.url), 'utf8'));
const VERSION = process.env.VERSION || `v${pkg.version}`;
const DB = process.env.DB || '/tmp/rl-live/live.db';
const OUT = process.env.OUT || path.resolve('research/bootstrap-2026-11-07.json');
const LIMIT = Number(process.env.LIMIT || 5);

const checks = [];
const record = (name, ok, detail) => {
  checks.push({ name, ok, detail });
  console.log(`${ok ? 'PASS' : 'FAIL'} ${name} — ${detail}`);
};

const dest = fs.mkdtempSync(path.join(os.tmpdir(), 'reeflog-bootstrap-live-'));
const result = { generatedAt: new Date().toISOString(), version: VERSION, checks: [] };

try {
  // 1. bootstrap
  const t0 = Date.now();
  const first = await bootstrap({ version: VERSION, dest });
  const ms = Date.now() - t0;
  const reported = execFileSync(first.bin, ['--version'], { encoding: 'utf8' }).trim();
  const okVersion = reported === `reeflog ${VERSION.replace(/^v/, '')}`;
  const okExec = (fs.statSync(first.bin).mode & 0o111) !== 0;
  record('bootstrap_installs_from_the_published_release',
    okVersion && okExec,
    `asset=${first.asset} digest=${first.digest.slice(0, 16)}… version="${reported}" executable=${okExec} ms=${ms}`);
  result.first = { asset: first.asset, digest: first.digest, reported, executable: okExec, ms };

  // 2. cached
  const t1 = Date.now();
  const second = await bootstrap({ version: VERSION, dest });
  record('second_call_resolves_from_disk', second.cached === true,
    `cached=${second.cached} ms=${Date.now() - t1}`);
  result.second = { cached: second.cached, ms: Date.now() - t1 };

  // 3. the client, on THAT binary, against a REAL journal
  if (!fs.existsSync(DB)) {
    console.error(`setup: no journal at ${DB} — run a live ingest first`);
    process.exit(2);
  }
  const box = reeflog({ db: DB, bin: first.bin });
  const status = await box.status();
  const a = await box.tail({ limit: LIMIT, consumer: 'bootstrap-live' });
  const b = await box.tail({ limit: LIMIT, consumer: 'bootstrap-live' });
  const ordered = a.events.every((e, i) => i === 0 || e.seq > a.events[i - 1].seq);
  const noReplay = b.events.length === 0 || b.events[0].seq > a.events[a.events.length - 1].seq;
  record('client_delivers_real_rows_through_the_bootstrapped_binary',
    a.events.length > 0 && ordered && noReplay && status.events > 0,
    `events=${status.events} delivered=${a.events.length} ordered=${ordered} noReplay=${noReplay}`);
  result.client = {
    events: status.events,
    delivered: a.events.length,
    firstSeq: a.events[0]?.seq ?? null,
    lastSeq: a.events[a.events.length - 1]?.seq ?? null,
    ordered,
    noReplay,
  };

  // 4. refusal keeps the binary's words
  let refused = '';
  try {
    await reeflog({ db: '/nonexistent/journal.db', bin: first.bin }).status();
  } catch (e) { refused = e.message; }
  record('a_missing_journal_is_refused_with_the_binaries_own_words',
    /journal/i.test(refused) && /init/i.test(refused), refused.slice(0, 120));
  result.refusal = refused;
} catch (e) {
  record('bootstrap', false, String(e && e.message ? e.message : e));
}

result.checks = checks;
result.ok = checks.every((c) => c.ok);
result.limits = [
  'one host, one network, one platform (linux-x64); the macos-arm64 asset is mapped but was not installed here',
  'the trust anchor is github.com/auroraxo/reeflog over TLS — the same anchor `cargo install --git` uses; it does not defend against whoever can push tags and releases to that repository',
  'the sha256 comes from the release API\'s stored digest for the asset, i.e. from the same account that published the bytes — it protects against a corrupted or substituted download, not against a malicious release',
  'the journal used here was ingested by an earlier run; the bootstrap path itself was measured from an empty directory',
  'no third party has used this yet',
];

fs.mkdirSync(path.dirname(OUT), { recursive: true });
fs.writeFileSync(OUT, `${JSON.stringify(result, null, 2)}\n`);
fs.rmSync(dest, { recursive: true, force: true });
console.log(`\n${result.ok ? 'ALL CHECKS PASSED' : 'CHECKS FAILED'} → ${OUT}`);
process.exit(result.ok ? 0 : 1);
