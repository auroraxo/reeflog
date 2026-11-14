#!/usr/bin/env node
// The GAP-LEDGER measurement (PULS AJ, 2026-11-14) — MEASURED with the
// REAL binary, not inferred from the source.
//
// The defect (v0.7.26, published): the gap ledger counted SCANS, not
// EPISODES.
//
//   `verify` INSERTED a fresh `hole` row every time it found a hole, so two
//   scans of the SAME hole left two open rows (gaps: 2, gaps_open: 2).
//   `repair` closed only the OLDEST open hole (`gap_find_open("hole")` ->
//   one id), so the duplicates stayed open FOREVER: after the hole was
//   filled and the chain called the journal whole (`missing_count: 0`,
//   `complete: true`), `reeflog status` still reported `gaps_open: 1`.
//
// The class is the one this repo keeps closing: a report that names a
// problem that does not exist. `gaps_open` is what an operator and every
// monitoring caller reads to decide whether the mailbox is whole. A
// permanent false alarm on a healed hole trains the reader to ignore the
// field — which is how the NEXT real hole goes unnoticed.
//
// The faces (each one a predicate for BOTH trees):
//   F1 one hole + two `verify` runs -> ONE gap row (fix);
//   F2 after `repair` fills the hole -> `gaps_open: 0` (fix);
//   F3 a clean `verify` after the heal -> the ledger stays quiet (fix);
//   G1 the healed episode stays in the HISTORY (`gaps` keeps counting it)
//      (guard, both trees);
//   G2 the open row accumulates the detail of every scan (guard, both trees);
//   G3 a journal that never had a hole keeps `gaps: 0, gaps_open: 0`
//      (guard, both trees).
//
//   node tools/measure-gap-ledger.mjs
//   BIN=/path/to/reeflog OUT=/tmp/x.json LABEL=v0.7.26 node tools/measure-gap-ledger.mjs
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import http from 'node:http';
import crypto from 'node:crypto';
import { execFile, execFileSync } from 'node:child_process';
import { DatabaseSync } from 'node:sqlite';

const BIN = process.env.BIN || path.resolve(import.meta.dirname, '..', 'target/release/reeflog');
const OUT = process.env.OUT || null;
const LABEL = process.env.LABEL || 'this-tree';
const PROGRAM = 'whirLbMiicVdio4qvUfM5KAg6Ct8VwpYzGff3uctyCc';

if (!fs.existsSync(BIN)) {
  console.error(`no reeflog binary at ${BIN} — refusing to measure without one`);
  process.exit(2);
}

const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'reeflog-gap-ledger-'));
const db = path.join(dir, 'j.db');

/** The chain: the journal's rows plus one signature the journal lacks. */
const chainWithHole = [
  { signature: 'sig6', slot: 6 },
  { signature: 'sig5', slot: 5 },
  { signature: 'sig4', slot: 4 },
  { signature: 'sigX', slot: 3 },
  { signature: 'sig3', slot: 3 },
  { signature: 'sig2', slot: 2 },
  { signature: 'sig1', slot: 1 },
];
/** The same chain WITHOUT the hole — the journal and the chain agree. */
const chainClean = chainWithHole.filter((r) => r.signature !== 'sigX');

function startChain(chain) {
  const srv = http.createServer((req, res) => {
    const chunks = [];
    req.on('data', (c) => chunks.push(c));
    req.on('end', () => {
      let body;
      try {
        body = JSON.parse(Buffer.concat(chunks).toString('utf8'));
      } catch {
        res.writeHead(400).end('{}');
        return;
      }
      if (body.method !== 'getSignaturesForAddress') {
        res.writeHead(200, { 'content-type': 'application/json' });
        res.end(JSON.stringify({ jsonrpc: '2.0', id: body.id, error: { message: 'unexpected method ' + body.method } }));
        return;
      }
      const before = body.params?.[1]?.before;
      let rows = chain;
      if (before) {
        const i = chain.findIndex((r) => r.signature === before);
        rows = i >= 0 ? chain.slice(i + 1) : [];
      }
      res.writeHead(200, { 'content-type': 'application/json' });
      res.end(JSON.stringify({
        jsonrpc: '2.0',
        id: body.id,
        result: rows.map((r) => ({ signature: r.signature, slot: r.slot, err: null })),
      }));
    });
  });
  return new Promise((resolve) => {
    srv.listen(0, '127.0.0.1', () => {
      resolve({ url: `http://127.0.0.1:${srv.address().port}`, srv });
    });
  });
}

function run(args) {
  return new Promise((resolve) => {
    execFile(BIN, args, { cwd: dir, maxBuffer: 8 * 1024 * 1024 }, (error, stdout, stderr) => {
      resolve({
        code: error ? (error.code ?? 1) : 0,
        stdout: stdout || '',
        stderr: stderr || '',
        report: (() => {
          try { return JSON.parse((stdout || '').trim()); } catch { return null; }
        })(),
      });
    });
  });
}

function plant(rows) {
  const conn = new DatabaseSync(db);
  const now = Date.now();
  for (const r of rows) {
    conn.prepare(
      `INSERT INTO events(signature, slot, err, logs, source, first_seen, payload_state)
       VALUES (?, ?, NULL, '["log"]', 'ws', ?, 'held')`,
    ).run(r.sig, r.slot, now);
  }
  conn.close();
}
function gapRows() {
  const conn = new DatabaseSync(db);
  const rows = conn
    .prepare('SELECT id, kind, resolved_at IS NULL AS open, detail FROM gaps ORDER BY id')
    .all();
  conn.close();
  return rows.map((r) => ({ id: r.id, kind: r.kind, open: !!r.open, detail: r.detail }));
}
function reset(rows) {
  for (const f of [db, `${db}-wal`, `${db}-shm`]) fs.rmSync(f, { force: true });
  execFileSync(BIN, ['init', '--db', db, '--program', PROGRAM], { cwd: dir, stdio: 'ignore' });
  plant(rows);
}

const JOURNAL_ROWS = [
  { sig: 'sig1', slot: 1 },
  { sig: 'sig2', slot: 2 },
  { sig: 'sig3', slot: 3 },
  { sig: 'sig4', slot: 4 },
  { sig: 'sig5', slot: 5 },
];

const faces = [];
const face = (id, kind, detail) => faces.push({ id, kind, ...detail });

// ------------------------------------------------------------ F1
// One hole, TWO `verify` runs: one episode, ONE gap row.
{
  const { url, srv } = await startChain(chainWithHole);
  reset(JOURNAL_ROWS);
  const v1 = await run(['verify', '--db', db, '--rpc', url]);
  const g1 = gapRows();
  const v2 = await run(['verify', '--db', db, '--rpc', url]);
  const g2 = gapRows();
  face('F1', 'two verifies over one hole leave ONE gap row', {
    firstMissing: v1.report?.missing_count ?? 'missing',
    secondMissing: v2.report?.missing_count ?? 'missing',
    rowsAfterFirst: g1.length,
    rowsAfterSecond: g2.length,
    openAfterSecond: g2.filter((g) => g.open).length,
    detail: g2[0]?.detail ?? '',
    oneRow: g2.length === 1,
  });
  srv.close();
}

// ------------------------------------------------------------ F2
// `repair` fills the hole -> the ledger must be quiet.
{
  const { url, srv } = await startChain(chainWithHole);
  reset(JOURNAL_ROWS);
  await run(['verify', '--db', db, '--rpc', url]);
  await run(['verify', '--db', db, '--rpc', url]);
  const rep = await run(['repair', '--db', db, '--rpc', url]);
  const g = gapRows();
  const st = (await run(['status', '--db', db])).report;
  face('F2', 'a repaired hole leaves no OPEN gap', {
    filled: rep.report?.filled ?? 'missing',
    repairComplete: rep.report?.complete ?? 'missing',
    rows: g.length,
    open: g.filter((x) => x.open).length,
    statusGaps: st?.gaps ?? 'missing',
    statusGapsOpen: st?.gaps_open ?? 'missing',
    statusBackfilled: st?.backfilled ?? 'missing',
    quiet: (st?.gaps_open ?? 99) === 0,
  });
  srv.close();
}

// ------------------------------------------------------------ F3
// A clean `verify` after the heal -> the ledger stays quiet.
{
  const { url, srv } = await startChain(chainWithHole);
  reset(JOURNAL_ROWS);
  await run(['verify', '--db', db, '--rpc', url]);
  await run(['verify', '--db', db, '--rpc', url]);
  await run(['repair', '--db', db, '--rpc', url]);
  const v = await run(['verify', '--db', db, '--rpc', url]);
  const g = gapRows();
  const st = (await run(['status', '--db', db])).report;
  face('F3', 'a clean verify after the heal leaves the ledger quiet', {
    missing: v.report?.missing_count ?? 'missing',
    complete: v.report?.complete ?? 'missing',
    open: g.filter((x) => x.open).length,
    statusGapsOpen: st?.gaps_open ?? 'missing',
    quiet: (st?.gaps_open ?? 99) === 0 && g.filter((x) => x.open).length === 0,
  });
  srv.close();
}

// ------------------------------------------------------------ G1
// The healed episode stays in the HISTORY.
{
  const { url, srv } = await startChain(chainWithHole);
  reset(JOURNAL_ROWS);
  await run(['verify', '--db', db, '--rpc', url]);
  await run(['repair', '--db', db, '--rpc', url]);
  const st = (await run(['status', '--db', db])).report;
  face('G1', 'the healed episode stays in the gap history', {
    statusGaps: st?.gaps ?? 'missing',
    statusGapsOpen: st?.gaps_open ?? 'missing',
    statusBackfilled: st?.backfilled ?? 'missing',
    historyKept: (st?.gaps ?? 0) >= 1 && (st?.gaps_open ?? 99) === 0,
  });
  srv.close();
}

// ------------------------------------------------------------ G2
// The open row accumulates the detail of every scan.
{
  const { url, srv } = await startChain(chainWithHole);
  reset(JOURNAL_ROWS);
  await run(['verify', '--db', db, '--rpc', url]);
  const g1 = gapRows();
  await run(['verify', '--db', db, '--rpc', url]);
  const g2 = gapRows();
  face('G2', 'the open row accumulates the detail of every scan', {
    rowsAfterSecond: g2.length,
    firstDetailLength: (g1[0]?.detail ?? '').length,
    secondDetailLength: (g2[0]?.detail ?? '').length,
    accumulated: g2.length === 1 && (g2[0]?.detail ?? '').length >= (g1[0]?.detail ?? '').length,
  });
  srv.close();
}

// ------------------------------------------------------------ G3
// A journal that never had a hole keeps no gaps.
{
  const { url, srv } = await startChain(chainClean);
  reset(JOURNAL_ROWS);
  const v = await run(['verify', '--db', db, '--rpc', url]);
  const st = (await run(['status', '--db', db])).report;
  face('G3', 'a healthy journal keeps no gaps', {
    missing: v.report?.missing_count ?? 'missing',
    statusGaps: st?.gaps ?? 'missing',
    statusGapsOpen: st?.gaps_open ?? 'missing',
    noGaps: (st?.gaps ?? 99) === 0 && (st?.gaps_open ?? 99) === 0,
  });
  srv.close();
}

// The published v0.7.26 tree carries the defect; this tree must not.
const defectPresent = faces.find((f) => f.id === 'F1')?.oneRow === false;
const fixed = !defectPresent && faces.every((f) => {
  if (f.id === 'F1') return f.oneRow === true;
  if (f.id === 'F2') return f.quiet === true && f.filled === 1;
  if (f.id === 'F3') return f.quiet === true && f.missing === 0;
  if (f.id === 'G1') return f.historyKept === true;
  if (f.id === 'G2') return f.accumulated === true;
  if (f.id === 'G3') return f.noGaps === true;
  return false;
});

const binarySha256 = crypto.createHash('sha256').update(fs.readFileSync(BIN)).digest('hex');
const version = (await run(['--version'])).stdout.trim();

const report = {
  kind: 'gap-ledger-episode-accounting',
  label: LABEL,
  measuredAt: new Date().toISOString().slice(0, 10),
  binary: path.basename(BIN),
  binarySha256,
  binaryVersion: version,
  verdict: fixed ? 'fixed-and-guards-held' : 'defect-present',
  faces,
};

if (OUT) {
  fs.writeFileSync(OUT, JSON.stringify(report, null, 2) + '\n');
  console.log(`wrote ${OUT}`);
}
console.log(JSON.stringify(report, null, 2));

fs.rmSync(dir, { recursive: true, force: true });
