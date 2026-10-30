// End-to-end probe of ARCHIVE-ANCHORED gap fill on live mainnet.
//
// WHY (measured 2026-10-30): Raydium AMM v4 trades so heavily that 40 pages
// x 1000 signatures = 40,000 signatures only covers ~17,490 slots (~2.4h).
// A journal whose window sits DEEPER than that can never be verified or
// repaired from the tip: the walk burns its budget crossing history the
// journal never claimed, reports `truncated: true` and exits 3 forever.
//
// CONTRACT UNDER TEST:
//   1. Pull one page of real signatures from DEEP history (24h back, ~216k slots).
//   2. Build a journal holding only those rows, with planted holes.
//   3. `verify` (tip-started) MUST FAIL to cover the window (incomplete, exit 3).
//   4. `verify --anchor-slot` MUST REACH the window, see the holes, and exit 0.
//   5. `repair --anchor-slot` MUST FILL the planted holes.
//   6. `verify --anchor-slot` AGAIN MUST be clean and complete (exit 0).
//   7. `verify --anchor-slot <bad>` MUST refuse to fake completeness (exit 3).

import { spawn } from 'node:child_process';
import fs from 'node:fs';
import path from 'node:path';
import { DatabaseSync } from 'node:sqlite';

const RPC = 'https://api.mainnet-beta.solana.com';
const PROGRAM = '675kPX9MHTjS2zt1qfr1NYHuzeLXfQM9H24wFSUt1Mp8'; // Raydium AMM v4
const BIN = path.resolve('./target/release/reeflog');
const DB_DIR = '/tmp/reeflog-e2e-archive';
const DB = `${DB_DIR}/reeflog.db`;

async function rpc(method, params, tries = 8) {
  let last;
  for (let i = 0; i < tries; i++) {
    if (i) await new Promise((r) => setTimeout(r, 700 * i));
    try {
      const r = await fetch(RPC, {
        method: 'POST',
        headers: { 'content-type': 'application/json' },
        body: JSON.stringify({ jsonrpc: '2.0', id: 1, method, params }),
      });
      if (r.status === 429) {
        last = new Error('429 rate limited');
        continue;
      }
      const j = await r.json();
      if (j.error) throw new Error(JSON.stringify(j.error));
      return j.result;
    } catch (e) {
      last = e;
    }
  }
  throw last;
}

function cli(args) {
  return new Promise((resolve) => {
    const c = spawn(BIN, args, { stdio: ['ignore', 'pipe', 'pipe'] });
    let o = '', e = '';
    c.stdout.on('data', (d) => { o += d; });
    c.stderr.on('data', (d) => { e += d; });
    c.on('close', (code) => resolve({ code, out: o, err: e }));
    c.on('error', (err) => resolve({ code: null, out: o, err: `${e}\n${err}` }));
  });
}
const json = (s) => {
  try {
    return JSON.parse(s);
  } catch {
    return { raw: s };
  }
};

const tip = await rpc('getSlot', [{ commitment: 'finalized' }]);
// 24 hours back ≈ 216,000 slots. A tip-started walk covers ~17.5k slots,
// so 216k slots back is unambiguously out of reach of the tip-walk budget.
const targetSlot = tip - 216000;
console.log(`tip ${tip}, deep target ${targetSlot} (24h back, ${tip - targetSlot} slots deep)`);

// 1) Find the anchor at that depth: the last signature of a finalized block.
let anchor = null;
for (let s = targetSlot; s < targetSlot + 12; s++) {
  const b = await rpc('getBlock', [
    s,
    {
      encoding: 'base64',
      transactionDetails: 'signatures',
      rewards: false,
      commitment: 'finalized',
      maxSupportedTransactionVersion: 1,
    },
  ]);
  const sigs = b.signatures || [];
  if (sigs.length) {
    anchor = { sig: sigs[sigs.length - 1], slot: s };
    break;
  }
}
if (!anchor) {
  console.error('FATAL: could not resolve an anchor block at target depth');
  process.exit(2);
}
console.log(`archive anchor: slot ${anchor.slot}, sig ${anchor.sig.slice(0, 16)}...`);

// 2) Harvest one page of real program signatures walking backward from the anchor.
const harvestRows = await rpc('getSignaturesForAddress', [
  PROGRAM,
  { limit: 200, commitment: 'finalized', before: anchor.sig },
]);
if (harvestRows.length < 50) {
  console.error('FATAL: deep harvest returned too few rows:', harvestRows.length);
  process.exit(2);
}
// Discard the oldest partial slot (same clean-floor rule as e2e-repair.mjs).
const cutSlot = harvestRows[harvestRows.length - 1].slot;
const rows = harvestRows.filter((r) => r.slot > cutSlot);
console.log(
  `harvested ${harvestRows.length} raw, ${rows.length} clean, slots ${rows[rows.length - 1].slot}..${rows[0].slot}`
);

fs.rmSync(DB_DIR, { recursive: true, force: true });
fs.mkdirSync(DB_DIR, { recursive: true });

let init = await cli(['init', '--db', DB, '--program', PROGRAM]);
if (init.code !== 0) {
  console.error('INIT FAILED', init);
  process.exit(2);
}

// 3) Seed the journal with the harvested rows, planting holes every 5th row.
const db = new DatabaseSync(DB);
const insert = db.prepare(
  'INSERT OR IGNORE INTO events(signature,slot,err,logs,source,seal_status,first_seen) VALUES(?,?,?,?,?,?,?)'
);
const planted = [];
let kept = 0;
for (let i = 0; i < rows.length; i++) {
  const r = rows[i];
  if (i % 5 === 4) {
    planted.push(r);
    continue;
  }
  const err = r.err == null ? null : JSON.stringify(r.err);
  insert.run(r.signature, r.slot, err, '[]', 'ws', 'pending', Date.now());
  kept++;
}
db.close();
console.log(`planted: ${kept} kept rows, ${planted.length} holes in deep window`);

// --- CHECK 1: tip-started verify MUST FAIL to cover the deep window ---
// 2 pages from tip can only reach ~2,000 rows below the tip, which is
// ~200,000 slots above our window floor.
let tipRun = await cli(['verify', '--db', DB, '--pages', '2']);
const tipJ = json(tipRun.out);
const pass1 = tipRun.code === 3 && tipJ.complete !== true;
console.log(
  `${pass1 ? 'PASS' : 'FAIL'} tip_started_verify_refuses_to_claim_deep_window (exit=${tipRun.code}, complete=${tipJ.complete}, truncated=${tipJ.truncated})`
);

// --- CHECK 2: anchored verify reaches the window and SEES the holes ---
let v1 = await cli(['verify', '--db', DB, '--pages', '5', '--anchor-slot', String(anchor.slot)]);
const v1j = json(v1.out);
const pass2 =
  v1.code === 0 &&
  v1j.complete === true &&
  v1j.missing_count === planted.length &&
  v1j.anchor?.slot === anchor.slot;
console.log(
  `${pass2 ? 'PASS' : 'FAIL'} anchored_verify_sees_deep_holes (exit=${v1.code}, complete=${v1j.complete}, missing=${v1j.missing_count} expected=${planted.length})`
);

// --- CHECK 3: anchored repair fills the holes -------------------------
let p1 = await cli(['repair', '--db', DB, '--pages', '5', '--anchor-slot', String(anchor.slot)]);
const p1j = json(p1.out);
const pass3 =
  p1.code === 0 &&
  p1j.complete === true &&
  p1j.filled === planted.length;
console.log(
  `${pass3 ? 'PASS' : 'FAIL'} anchored_repair_fills_deep_holes (exit=${p1.code}, filled=${p1j.filled} expected=${planted.length})`
);

// --- CHECK 4: second anchored verify is clean and complete ------------
let v2 = await cli(['verify', '--db', DB, '--pages', '5', '--anchor-slot', String(anchor.slot)]);
const v2j = json(v2.out);
const pass4 =
  v2.code === 0 &&
  v2j.complete === true &&
  v2j.missing_count === 0;
console.log(
  `${pass4 ? 'PASS' : 'FAIL'} second_anchored_verify_is_clean (exit=${v2.code}, complete=${v2j.complete}, missing=${v2j.missing_count})`
);

// --- CHECK 5: anchor at/above ceiling via bare `--anchor-slot` works ---
// The journal's own ceiling is rows[0].slot; `--anchor-slot` without value
// targets ceiling+1, which resolves to an anchor above the window.
let vBare = await cli(['verify', '--db', DB, '--pages', '5', '--anchor-slot']);
const vBareJ = json(vBare.out);
const pass5 =
  vBare.code === 0 &&
  vBareJ.complete === true &&
  vBareJ.missing_count === 0 &&
  vBareJ.anchor?.slot >= rows[0].slot;
console.log(
  `${pass5 ? 'PASS' : 'FAIL'} bare_anchor_slot_anchors_at_journal_ceiling (exit=${vBare.code}, complete=${vBareJ.complete}, anchorSlot=${vBareJ.anchor?.slot} >= ceil=${rows[0].slot})`
);

// --- CHECK 6: unresolvable anchor reports failure, never fakes clean ---
let vBad = await cli(['verify', '--db', DB, '--pages', '5', '--anchor-slot', '999999999', '--anchor-tries', '2']);
const vBadJ = json(vBad.out);
const pass6 = vBad.code === 3 && vBadJ.complete !== true;
console.log(
  `${pass6 ? 'PASS' : 'FAIL'} unresolvable_anchor_fails_closed (exit=${vBad.code}, complete=${vBadJ.complete})`
);

const verdict = pass1 && pass2 && pass3 && pass4 && pass5 && pass6;
console.log(verdict ? 'E2E ARCHIVE: ALL PASS' : 'E2E ARCHIVE: FAILURE');
process.exit(verdict ? 0 : 1);
