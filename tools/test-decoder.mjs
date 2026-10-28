// Offline self-test for the Raydium AMM v4 ray_log decoder.
//
// The three fixtures below were taken from real mainnet transactions
// (2026-10-28, slots ~462482142) and their field meanings were verified
// against the chain: `pool_coin + amount_in` equals the pool's coin-vault
// balance after the swap, `pool_pc - out_amount` equals its pc-vault balance,
// both read from the transaction's own pre/post token balances.
//
// This test needs no network: it proves the decoder keeps reading those
// layouts correctly and refuses anything it does not recognise.
import { decodeRayLog, rayLogs, fillFromRow } from './fill-watcher.mjs';

const fixtures = [
  {
    name: 'swap_base_in, direction=1 (PC2Coin)',
    blob: 'A2m72aIBAAAAW2/tNgAAAAABAAAAAAAAADX+hUWLBAAAC07UKCQIAADmAuRJ6z0AAA2Q7TYAAAAA',
    expect: {
      kind: 'swap_base_in', log_type: 3,
      amount_in: 7027145577n, minimum_out: 921530203n, direction: 1n,
      user_source: 4996213374517n, pool_coin: 8951396847115n,
      pool_pc: 68080766288614n, out_amount: 921538573n,
      input_side: 'pc', output_side: 'coin',
    },
  },
  {
    name: 'swap_base_in, direction=1 (PC2Coin), large out',
    blob: 'A1I7YUAFAAAAoWY5sAAAAAABAAAAAAAAABhTHh7QAgAA/r3m8SMIAABPvr3s7D0AANmsObAAAAAA',
    expect: {
      kind: 'swap_base_in', log_type: 3,
      amount_in: 22554950482n, minimum_out: 2956551841n, direction: 1n,
      user_source: 3092881756952n, pool_coin: 8950475308542n,
      pool_pc: 68087793434191n, out_amount: 2956569817n,
      input_side: 'pc', output_side: 'coin',
    },
  },
  {
    name: 'swap_base_in, direction=2 (Coin2PC)',
    blob: 'A9IxhAAAAAAAjxnnFQAAAAACAAAAAAAAANIxhAAAAAAA2DzlJuoAAADa2/A3SScAAI+8HxYAAAAA',
    expect: {
      kind: 'swap_base_in', log_type: 3,
      amount_in: 8663506n, minimum_out: 367466895n, direction: 2n,
      user_source: 8663506n, pool_coin: 1005674904792n,
      pool_pc: 43195424627674n, out_amount: 371178639n,
      input_side: 'coin', output_side: 'pc',
    },
  },
];

const checks = [];
const push = (name, pass, detail) => {
  checks.push({ name, pass, detail });
  console.log(`${pass ? 'PASS' : 'FAIL'} ${name}: ${detail}`);
};

for (const f of fixtures) {
  const got = decodeRayLog(f.blob);
  const diff = [];
  for (const [k, v] of Object.entries(f.expect)) {
    if (got[k] !== v) diff.push(`${k}: got ${String(got[k])}, want ${String(v)}`);
  }
  push(`decode_${f.name.replace(/[^a-z0-9]+/gi, '_')}`, diff.length === 0,
    diff.length ? diff.join('; ') : `all ${Object.keys(f.expect).length} fields match`);
}

// wrong length must be refused, never guessed at
{
  let threw = false;
  try { decodeRayLog(Buffer.from([3, 1, 2, 3]).toString('base64')); }
  catch { threw = true; }
  push('rejects_short_ray_log', threw, '57-byte rule enforced by exception');
}

// a non-swap log type is reported as `other`, not decoded as a fill
{
  const init = Buffer.alloc(57);
  init[0] = 0; // LogType::Init
  const ev = decodeRayLog(init.toString('base64'));
  push('non_swap_log_type_is_other', ev.kind === 'other' && ev.log_type === 0,
    `kind=${ev.kind} log_type=${ev.log_type}`);
}

// rayLogs picks the payload out of a stored log array
{
  const lines = [
    'Program log: Program ID: 675kPX9MHTjS2zt1qfr1NYHuzeLXfQM9H24wFSUt1Mp8',
    `Program log: ray_log: ${fixtures[0].blob}`,
    'Program 675kPX9MHTjS2zt1qfr1NYHuzeLXfQM9H24wFSUt1Mp8 success',
  ];
  const got = rayLogs(lines);
  push('extracts_ray_log_from_log_array', got.length === 1 && got[0] === fixtures[0].blob,
    `extracted ${got.length} payload(s)`);
}

// fillFromRow turns a row with a swap payload into a fill, and a row without
// one into null (not a fake fill)
{
  const row = {
    signature: 'x'.repeat(88), slot: 462482142, err: null, source: 'ws', seal: 'confirmed',
    logs: [`Program log: ray_log: ${fixtures[0].blob}`],
  };
  const f = fillFromRow(row);
  push('row_with_swap_becomes_fill',
    f && f.legs.length === 1 && f.legs[0].kind === 'swap_base_in' && f.slot === 462482142,
    `legs=${f?.legs.length} kind=${f?.legs[0]?.kind}`);

  const noSwap = fillFromRow({ ...row, logs: ['Program log: Instruction: Transfer'] });
  push('row_without_swap_is_not_a_fill', noSwap === null, `returned ${noSwap}`);
}

const verdict = checks.every((c) => c.pass);
console.log(verdict ? 'DECODER SELFTEST: ALL PASS' : 'DECODER SELFTEST: FAILURE');
process.exit(verdict ? 0 : 1);
