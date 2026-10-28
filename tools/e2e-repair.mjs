// End-to-end probe of the detect -> repair -> prove cycle on live mainnet.
//
// 1. Pull a real window of signatures for the program from the chain tip.
// 2. Build a journal holding only EVERY OTHER one -> planted holes.
// 3. `verify` must see them (non-zero missing, complete=true, exit 0).
// 4. `repair` must fill exactly those.
// 5. `verify` again must come back clean and complete.
// 6. `verify --pages 1` must refuse to claim completeness (exit 3).
import { spawn } from 'node:child_process';
import fs from 'node:fs';
import path from 'node:path';

const RPC = 'https://api.mainnet-beta.solana.com';
const PROGRAM = '675kPX9MHTjS2zt1qfr1NYHuzeLXfQM9H24wFSUt1Mp8';
const BIN = path.resolve('./target/release/reeflog');
const DB = '/tmp/reeflog-e2e/reeflog.db';

async function rpc(method, params, tries = 6) {
  let last;
  for (let i = 0; i < tries; i++) {
    if (i) await new Promise((r) => setTimeout(r, 700 * i));
    try {
      const r = await fetch(RPC, {
        method: 'POST',
        headers: { 'content-type': 'application/json' },
        body: JSON.stringify({ jsonrpc: '2.0', id: 1, method, params }),
      });
      if (r.status === 429) { last = new Error('429'); continue; }
      const j = await r.json();
      if (j.error) throw new Error(JSON.stringify(j.error));
      return j.result;
    } catch (e) { last = e; }
  }
  throw last;
}

function cli(args) {
  return new Promise((resolve) => {
    const c = spawn(BIN, args, { stdio: ['ignore', 'pipe', 'pipe'] });
    let o = '', e = '';
    c.stdout.on('data', (d) => { o += d; });
    c.stderr.on('data', (d) => { e += d; });
    c.on('exit', (code) => resolve({ code, out: o, err: e }));
  });
}
const json = (s) => { try { return JSON.parse(s); } catch { return { raw: s }; } };

// One page of real signatures from the tip (limit max 1000).
let rawRows = await rpc('getSignaturesForAddress', [PROGRAM, { limit: 1000, commitment: 'finalized' }]);
// A 1000-row page cuts off mid-slot: the oldest slot on the page usually has
// more signatures that were not returned. `verify` scans down to `min_slot`
// and reads the ENTIRE slot, so the remainder of the cut-off slot would be
// flagged as a hole. Discard the partial oldest slot so `min_slot` is a slot
// whose signatures the harvest held completely.
const cutSlot = rawRows[rawRows.length - 1].slot;
const rows = rawRows.filter((r) => r.slot > cutSlot);
console.log(`chain page: ${rawRows.length} raw, ${rows.length} clean (discarded partial slot ${cutSlot}), slots ${rows[rows.length - 1].slot}..${rows[0].slot}`);

fs.rmSync('/tmp/reeflog-e2e', { recursive: true, force: true });
fs.mkdirSync('/tmp/reeflog-e2e', { recursive: true });

let r = await cli(['init', '--db', DB, '--program', PROGRAM]);
if (r.code !== 0) { console.error('INIT FAILED', r); process.exit(2); }

// Load every row, but SKIP every third one -> planted holes in the window.
// The top two slots are excluded from the journal: the ceiling slot is still
// being written while the harness runs, so a transaction finalising in it
// after the harvest would be flagged as a hole nobody planted. Planting only
// below the tip keeps the expected count deterministic.
const ceilSlot = rows[0].slot;
const planted = [];
let kept = 0;
for (const row of rows) {
  if (row.slot >= ceilSlot - 1) continue; // tip race: leave the newest slots out
  if (kept % 3 === 2) { planted.push(row); kept++; continue; }
  const err = row.err == null ? null : JSON.stringify(row.err);
  // reuse the tail command's input path: append via `start` would need WS,
  // so write through sqlite directly? No — the journal is the product under
  // test; use its own append path through a tiny `repair` seed instead:
  // we instead let repair fill EVERYTHING and compare. For planting we need
  // append; the binary exposes it only via WS ingest, so plant with sqlite.
  const { DatabaseSync } = await import('node:sqlite');
  const db = new DatabaseSync(DB);
  db.prepare('INSERT OR IGNORE INTO events(signature,slot,err,logs,source,seal_status,first_seen) VALUES(?,?,?,?,?,?,?)')
    .run(row.signature, row.slot, err, '[]', 'ws', 'pending', Date.now());
  db.close();
  kept++;
}
console.log(`planted ${kept} kept, ${planted.length} holes (window ceiling ${ceilSlot})`);

// --- 1. verify must SEE the holes -------------------------------------
let v1 = await cli(['verify', '--db', DB, '--pages', '50']);
const v1j = json(v1.out);
console.log(`VERIFY#1 exit=${v1.code} complete=${v1j.complete} missing=${v1j.missing_count} pages=${v1j.pages} scanned_from=${v1j.scanned_from_slot}`);
const pass1 = v1.code === 0 && v1j.complete === true && v1j.missing_count === planted.length;
console.log(`${pass1 ? 'PASS' : 'FAIL'} verify_sees_exactly_the_planted_holes (expected ${planted.length})`);

// --- 2. repair must fill them ------------------------------------------
let p1 = await cli(['repair', '--db', DB, '--pages', '50']);
const p1j = json(p1.out);
console.log(`REPAIR exit=${p1.code} complete=${p1j.complete} found=${p1j.found_missing} filled=${p1j.filled}`);
const pass2 = p1.code === 0 && p1j.complete === true && p1j.filled === planted.length;
console.log(`${pass2 ? 'PASS' : 'FAIL'} repair_fills_exactly_the_planted_holes (expected ${planted.length})`);

// --- 3. verify again must be clean --------------------------------------
let v2 = await cli(['verify', '--db', DB, '--pages', '50']);
const v2j = json(v2.out);
console.log(`VERIFY#2 exit=${v2.code} complete=${v2j.complete} missing=${v2j.missing_count}`);
const pass3 = v2.code === 0 && v2j.complete === true && v2j.missing_count === 0;
console.log(`${pass3 ? 'PASS' : 'FAIL'} second_verify_is_clean_and_complete`);

// --- 4. an unfinished scan must refuse to exit 0 ------------------------
let v3 = await cli(['verify', '--db', DB, '--pages', '1']);
const v3j = json(v3.out);
console.log(`VERIFY#3(pages=1) exit=${v3.code} complete=${v3j.complete} truncated=${v3j.truncated} missing=${v3j.missing_count}`);
const pass4 = v3.code !== 0 && v3j.complete !== true;
console.log(`${pass4 ? 'PASS' : 'FAIL'} incomplete_scan_never_exits_zero`);

const verdict = pass1 && pass2 && pass3 && pass4;
console.log(verdict ? 'E2E: ALL PASS' : 'E2E: FAILURE');
process.exit(verdict ? 0 : 1);
