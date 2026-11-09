#!/usr/bin/env node
// The webhook config URL domain, measured against REAL binaries.
//
// The defect (measured 2026-11-09 on the published v0.7.7 binary, the
// sha256 of whose release archive equals the digest published by GitHub
// Releases):
//
//   `"url": "HTTP://127.0.0.1:8794/hook"`
//     -> REFUSED with `must be http:// or https://` — about a URL that IS
//        http. The consumer (ureq 3.5.0) lowercases the scheme: measured,
//        it answers 200 to `HTTP://` and `HtTp://`. The checker refused a
//        URL the product CAN deliver to.
//   `"url": "http://"`
//     -> ACCEPTED (exit 0, report), then every delivery dies in the
//        transport with `http: empty string`. A dead-on-arrival target was
//        configured as healthy.
//   `"url": "http:///x"`
//     -> ACCEPTED, then dies with `http: invalid format`.
//   `"url": "https://"`
//     -> ACCEPTED, then dies with `http: empty string`.
//
// The tool measures BOTH trees on the SAME cases and writes ONE artifact:
//
//   BASELINE  the published v0.7.7 binary (gh release download, verified
//             against the release API digest)
//   FIXED     the binary built from this tree
//
// Cases cover both defects and four guards (lowercase URL, non-http scheme,
// typo key, duplicate id).
//
// Usage:
//   node tools/measure-webhook-url-domain.mjs
//   node tools/measure-webhook-url-domain.mjs --baseline <bin> --fixed <bin>
//
// Exit: 0 all cases behave as specified on the FIXED tree, 1 a case failed,
// 2 setup failed.
import { spawnSync, execFileSync } from 'node:child_process';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const OUT = path.resolve(
  (
    process.env.OUT ||
    (process.argv.includes('--out')
      ? process.argv[process.argv.indexOf('--out') + 1]
      : path.join(ROOT, 'research', 'webhook-url-domain-2026-11-09.json'))
  ).replace(/^file:/, ''),
);

const PROGRAM = 'whirLbMiicVdio4qvUfM5KAg6Ct8VwpYzGff3uctyCc';

function argOf(flag) {
  const i = process.argv.indexOf(flag);
  return i >= 0 ? process.argv[i + 1] : null;
}

// --- the trees -------------------------------------------------------------

function baselineBinary() {
  const given = argOf('--baseline');
  if (given) return { bin: path.resolve(given), source: 'given' };
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'reeflog-whurl-baseline-'));
  const tar = path.join(dir, 'baseline.tar.gz');
  execFileSync(
    'gh',
    ['release', 'download', 'v0.7.7', '--repo', 'auroraxo/reeflog',
     '-p', 'reeflog-linux-amd64.tar.gz', '-O', tar],
    { cwd: ROOT, stdio: 'inherit' },
  );
  const digest = execFileSync(
    'gh',
    ['api', 'repos/auroraxo/reeflog/releases/tags/v0.7.7', '--jq',
     '.assets[] | select(.name=="reeflog-linux-amd64.tar.gz") | .digest'],
    { cwd: ROOT, encoding: 'utf8' },
  ).trim().replace(/^sha256:/, '');
  const local = execFileSync('sha256sum', [tar], { encoding: 'utf8' }).split(/\s+/)[0];
  if (digest && local !== digest) {
    throw new Error(`the downloaded archive does not match the release digest (${local} != ${digest})`);
  }
  execFileSync('tar', ['-xzf', tar, '-C', dir], { stdio: 'inherit' });
  return { bin: path.join(dir, 'reeflog'), source: 'gh release download v0.7.7 (digest verified)' };
}

function fixedBinary() {
  const given = argOf('--fixed');
  if (given) return { bin: path.resolve(given), source: 'given' };
  execFileSync('cargo', ['build', '--release', '--locked'], { cwd: ROOT, stdio: 'inherit' });
  return { bin: path.join(ROOT, 'target', 'release', 'reeflog'), source: 'this tree (cargo build --release)' };
}

// --- the cases -------------------------------------------------------------

/**
 * kind:
 *   'refusal' — the URL must be REFUSED: exit != 0, no report on stdout,
 *               key `url` named, offending value named.
 *   'accept'  — the URL must be ACCEPTED by the config loader: exit 0,
 *               report on stdout containing the target id.
 *   'guard_refusal' — must be refused (exit != 0), old message acceptable.
 */
const CASES = [
  { id: 'nohost-http', kind: 'refusal', url: 'http://' },
  { id: 'nohost-https', kind: 'refusal', url: 'https://' },
  { id: 'empty-auth', kind: 'refusal', url: 'http:///x' },
  { id: 'uppercase-scheme', kind: 'accept', url: 'HTTP://127.0.0.1:9/hook' },
  { id: 'mixed-case-scheme', kind: 'accept', url: 'HtTp://127.0.0.1:9/hook' },
  { id: 'guard-lowercase-http', kind: 'accept', url: 'http://127.0.0.1:9/hook' },
  { id: 'guard-lowercase-https', kind: 'accept', url: 'https://127.0.0.1:9/hook' },
  { id: 'guard-non-http-scheme', kind: 'guard_refusal', url: 'ftp://x/y' },
];

function runCase(bin, c) {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'reeflog-whurl-case-'));
  const db = path.join(dir, 'reeflog.db');
  const init = spawnSync(bin, ['init', '--db', db, '--program', PROGRAM], { cwd: dir, encoding: 'utf8' });
  if (init.status !== 0) throw new Error(`init failed: ${init.stderr}`);

  const cfg = path.join(dir, 'wh.json');
  fs.writeFileSync(
    cfg,
    JSON.stringify({ targets: [{ id: 't1', url: c.url, secret: 's', timeout_ms: 2000 }] }),
  );

  const res = spawnSync(bin, ['webhook', 'status', '--db', db, '--config', cfg], { cwd: dir, encoding: 'utf8' });
  const stdout = res.stdout ?? '';
  const stderr = res.stderr ?? '';
  const exit = res.status;

  let violated = false;
  let reason = 'as_specified';

  if (c.kind === 'refusal') {
    // Expected: exit != 0, "url" in stderr, url value in stderr, no report
    if (exit === 0) {
      violated = true;
      reason = 'accepted_when_it_should_be_refused';
    } else if (!stderr.toLowerCase().includes('url')) {
      violated = true;
      reason = 'refusal_missing_url_key';
    } else if (!stderr.toLowerCase().includes(c.url.toLowerCase())) {
      violated = true;
      reason = 'refusal_missing_value';
    }
  } else if (c.kind === 'accept') {
    if (exit !== 0) {
      violated = true;
      reason = `refused_when_it_should_be_accepted: ${stderr.trim()}`;
    } else if (!stdout.includes('t1')) {
      violated = true;
      reason = 'report_missing_target';
    }
  } else if (c.kind === 'guard_refusal') {
    if (exit === 0) {
      violated = true;
      reason = 'guard_refusal_accepted';
    }
  }

  // Scrub any temp dir / host paths
  const scrubbedStderr = stderr.split('\n')[0].replace(dir, '<dir>');
  fs.rmSync(dir, { recursive: true, force: true });

  return {
    id: c.id,
    kind: c.kind,
    url: c.url,
    exit,
    violated,
    reason,
    stderr: scrubbedStderr,
  };
}

function main() {
  const base = baselineBinary();
  const fixed = fixedBinary();

  const baselineResults = CASES.map((c) => runCase(base.bin, c));
  const fixedResults = CASES.map((c) => runCase(fixed.bin, c));

  const baselineViolations = baselineResults.filter((r) => r.violated).length;
  const fixedViolations = fixedResults.filter((r) => r.violated).length;

  const defectCases = CASES.filter((c) => c.kind !== 'guard_refusal' && !c.id.startsWith('guard-'));
  const guardCases = CASES.filter((c) => c.kind === 'guard_refusal' || c.id.startsWith('guard-'));

  const payload = {
    measuredAt: new Date().toISOString(),
    baselineSource: base.source,
    fixedSource: fixed.source,
    summary: {
      totalCases: CASES.length,
      defectCases: defectCases.length,
      guardCases: guardCases.length,
      baselineViolations,
      fixedViolations,
      verdict: fixedViolations === 0 && baselineViolations > 0 ? 'fixed' : 'failed',
    },
    baseline: baselineResults,
    fixed: fixedResults,
  };

  fs.mkdirSync(path.dirname(OUT), { recursive: true });
  fs.writeFileSync(OUT, JSON.stringify(payload, null, 2) + '\n');
  console.log(`wrote ${OUT}`);
  console.log(`verdict: ${payload.summary.verdict} (baseline violations: ${baselineViolations}, fixed: ${fixedViolations})`);

  if (payload.summary.verdict !== 'fixed') {
    process.exit(1);
  }
}

main();
