// reeflog user-zero: a Raydium AMM v4 fill watcher that consumes the reeflog
// mailbox through `reeflog tail --cursor`.
//
// Why this exists
// ---------------
// reeflog's contract is: a cursor-addressed, durable event mailbox. Until now
// that contract was only tested against reeflog's own harness. This is the
// first consumer that is not reeflog: it reads events the way a real bot
// would, decodes them, and derives a business fact (a fill).
//
// The decode is not guesswork. The Raydium AMM v4 program emits a base64
// `ray_log:` line that is a bincode-serialized struct (see raydium-amm
// program/src/log.rs). The layout used here was verified against the chain:
//   - `pool_coin` (field 4) + `amount_in` (field 0) == the coin vault balance
//     after the swap,
//   - `pool_pc` (field 5) - `out_amount` (field 6) == the pc vault balance
//     after the swap,
// both read from the transaction's own pre/post token balances.
//
// The watcher is deliberately strict: a line it cannot decode is reported,
// never guessed.
import { spawn } from 'node:child_process';
import fs from 'node:fs';
import path from 'node:path';

const RAYDIUM_AMM_V4 = '675kPX9MHTjS2zt1qfr1NYHuzeLXfQM9H24wFSUt1Mp8';
const RAYDIUM_AUTHORITY = '5Q544fKrFoe6tsEbD7S8EmxGTJYAKtTVhAW5Q5pge4j1';

// LogType discriminants (raydium-amm program/src/log.rs, LogType::from_u8).
const LOG_TYPE = { INIT: 0, DEPOSIT: 1, WITHDRAW: 2, SWAP_BASE_IN: 3, SWAP_BASE_OUT: 4 };
// SwapDirection (raydium-amm program/src/math.rs). u64 values; the decoder
// reads them as BigInt, so the constants must be BigInt too.
const DIRECTION = { PC2COIN: 1n, COIN2PC: 2n };

const u64 = (b, o) => b.readBigUInt64LE(o);

// direction arrives as BigInt (u64); compare against BigInt constants.
const isPcToCoin = (d) => d === DIRECTION.PC2COIN;

// Decode one base64 `ray_log:` payload into a typed event.
export function decodeRayLog(b64) {
  const raw = Buffer.from(b64, 'base64');
  if (raw.length < 1) throw new Error('empty ray_log');
  const logType = raw[0];
  if (logType !== LOG_TYPE.SWAP_BASE_IN && logType !== LOG_TYPE.SWAP_BASE_OUT) {
    return { kind: 'other', log_type: logType };
  }
  // bincode: u8 discriminant, then the struct fields in declaration order,
  // each u64 little-endian. 7 u64 after the tag => 57 bytes.
  if (raw.length !== 57) throw new Error(`unexpected ray_log length ${raw.length}`);
  const f = [];
  for (let i = 0; i < 7; i++) f.push(u64(raw, 1 + i * 8));
  const [amount_in, minimum_out, direction, user_source, pool_coin, pool_pc, out_amount] = f;

  if (logType === LOG_TYPE.SWAP_BASE_IN) {
    return {
      kind: 'swap_base_in',
      log_type: logType,
      amount_in,
      minimum_out,
      direction,
      user_source,
      pool_coin,
      pool_pc,
      out_amount,
      // direction 1 = PC2Coin: input is the "pc" (quote) side.
      input_side: isPcToCoin(direction) ? 'pc' : 'coin',
      output_side: isPcToCoin(direction) ? 'coin' : 'pc',
    };
  }
  return {
    kind: 'swap_base_out',
    log_type: logType,
    max_in: amount_in,
    amount_out: minimum_out,
    direction,
    user_source,
    pool_coin,
    pool_pc,
    deduct_in: out_amount,
    input_side: isPcToCoin(direction) ? 'pc' : 'coin',
    output_side: isPcToCoin(direction) ? 'coin' : 'pc',
  };
}

// Pull every `ray_log:` line out of a stored log array.
export function rayLogs(logs) {
  if (!Array.isArray(logs)) return [];
  const out = [];
  for (const line of logs) {
    if (typeof line !== 'string') continue;
    const i = line.indexOf('ray_log: ');
    if (i < 0) continue;
    out.push(line.slice(i + 'ray_log: '.length).trim());
  }
  return out;
}

// Turn one reeflog row into a fill, or null if the row is not a swap.
export function fillFromRow(row) {
  const blobs = rayLogs(row.logs);
  if (!blobs.length) return null;
  const events = [];
  for (const b of blobs) {
    let ev;
    try {
      ev = decodeRayLog(b);
    } catch (e) {
      events.push({ kind: 'undecodable', error: String(e.message || e) });
      continue;
    }
    if (ev.kind === 'swap_base_in' || ev.kind === 'swap_base_out') events.push(ev);
  }
  if (!events.length) return null;
  return {
    signature: row.signature,
    slot: row.slot,
    err: row.err ?? null,
    source: row.source,
    seal: row.seal,
    legs: events,
  };
}

function cli(args) {
  return new Promise((resolve) => {
    const c = spawn(args[0], args.slice(1), { stdio: ['ignore', 'pipe', 'pipe'] });
    let o = '', e = '';
    c.stdout.on('data', (d) => { o += d; });
    c.stderr.on('data', (d) => { e += d; });
    c.on('exit', (code) => resolve({ code, out: o, err: e }));
  });
}

// ---------------------------------------------------------------------------
// The ledger: what this consumer has already acted on.
//
// Why it exists: a retraction is only meaningful against a fill someone read.
// reeflog's reorg channel can deliver a retraction for a signature this
// watcher never decoded (another consumer's event, a row it never delivered).
// Joining the retraction against a local ledger is the only way to answer
// "which of MY fills must be undone" — and to answer honestly when the answer
// is "none of them".
//
// BigInt-safe: u64 amounts exceed Number range, and a ledger that silently
// loses digits would make every later comparison meaningless.
const bigintReplacer = (_k, v) => (typeof v === 'bigint' ? v.toString() : v);

export function loadLedger(file) {
  try {
    const raw = JSON.parse(fs.readFileSync(file, 'utf8'));
    return { fills: raw.fills || {}, retracted: raw.retracted || {} };
  } catch {
    return { fills: {}, retracted: {} };
  }
}

// NB: the parameter must not be named `path` — it would shadow the imported
// `path` module inside this file and break `path.dirname`.
export function saveLedger(file, ledger) {
  fs.mkdirSync(path.dirname(file), { recursive: true });
  fs.writeFileSync(file, JSON.stringify(ledger, bigintReplacer, 2));
}

export async function watch({ db, cursor, bin = './target/release/reeflog', limit = 1000, onFill, ledgerPath }) {
  const t = await cli([bin, 'tail', '--db', db, '--cursor', cursor, '--limit', String(limit)]);
  const lines = t.out.split('\n').filter(Boolean);
  const fills = [];
  const ledger = ledgerPath ? loadLedger(ledgerPath) : null;
  for (const line of lines) {
    let row;
    try { row = JSON.parse(line); } catch { continue; }
    const f = fillFromRow(row);
    if (f) {
      fills.push(f);
      if (ledger) ledger.fills[f.signature] = { ...f, seq: row.seq, read_at: Date.now() };
      if (onFill) onFill(f);
    }
  }
  if (ledger && fills.length) saveLedger(ledgerPath, ledger);
  return { rows: lines.length, fills, stderr: t.err, exit: t.code };
}

// Read the retraction channel and join it against the ledger.
//
// Two outcomes, kept apart on purpose:
//   undoable  — this consumer read that fill; it must be reversed, and the
//               reversal carries the fill's own data so the consumer can undo
//               exactly what it did (amounts included).
//   unknown   — the chain no longer knows this signature, but this consumer
//               never acted on it. Reported, never silently dropped, and
//               never turned into an unfill of some other fill.
//
// The cursor is strict: a second read delivers nothing, so an undo is never
// applied twice.
export async function watchRetractions({
  db,
  cursor,
  bin = './target/release/reeflog',
  limit = 1000,
  ledgerPath,
  onRetract,
}) {
  const t = await cli([bin, 'reorg', 'tail', '--db', db, '--cursor', cursor, '--limit', String(limit)]);
  const lines = t.out.split('\n').filter(Boolean);
  const ledger = ledgerPath ? loadLedger(ledgerPath) : null;
  const retractions = [];
  for (const line of lines) {
    let row;
    try { row = JSON.parse(line); } catch { continue; }
    const fill = ledger ? ledger.fills[row.signature] : undefined;
    const rec = {
      ...row,
      undoable: !!fill,
      fill: fill ?? null,
    };
    retractions.push(rec);
    if (ledger && fill) {
      delete ledger.fills[row.signature];
      ledger.retracted[row.signature] = { ...row, undone_at: Date.now() };
    }
    if (onRetract) onRetract(rec);
  }
  if (ledger && retractions.length) saveLedger(ledgerPath, ledger);
  return { rows: lines.length, retractions, stderr: t.err, exit: t.code };
}

// CLI: node tools/fill-watcher.mjs --db reeflog.db --cursor c.txt [--ledger l.json] [--reorg-cursor r.txt]
if (process.argv[1] && process.argv[1].endsWith('fill-watcher.mjs')) {
  const arg = (k, d) => {
    const i = process.argv.indexOf(k);
    return i >= 0 ? process.argv[i + 1] : d;
  };
  const db = arg('--db', 'reeflog.db');
  const cursor = arg('--cursor', path.join('/tmp', 'fill-watcher-cursor.txt'));
  const bin = arg('--bin', './target/release/reeflog');
  const ledgerPath = arg('--ledger', null);
  const r = await watch({ db, cursor, bin, ledgerPath, limit: Number(arg('--limit', '1000')) });
  const rc = arg('--reorg-cursor', null);
  const retr = rc
    ? await watchRetractions({
        db,
        cursor: rc,
        bin,
        ledgerPath,
        limit: Number(arg('--reorg-limit', '1000')),
      })
    : null;
  const ledger = ledgerPath ? loadLedger(ledgerPath) : null;
  console.log(JSON.stringify({
    rows: r.rows,
    fills: r.fills.length,
    retractions: retr ? retr.retractions.length : null,
    undoable: retr ? retr.retractions.filter((x) => x.undoable).length : null,
    open_fills: ledger ? Object.keys(ledger.fills).length : null,
    undone_fills: ledger ? Object.keys(ledger.retracted).length : null,
  }, null, 2));
  // BigInt-safe: the previous CLI used a bare JSON.stringify, which threw on
  // the very first fill it printed (u64 amounts are BigInt).
  for (const f of r.fills.slice(0, 5)) console.log(JSON.stringify(f, bigintReplacer));
  for (const x of retr ? retr.retractions : []) console.log(JSON.stringify(x, bigintReplacer));
  if (r.stderr) console.error(r.stderr.trim());
}

