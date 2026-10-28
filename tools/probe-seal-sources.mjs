// Probe: is a NULL from getSignatureStatuses(searchTransactionHistory) alone a
// safe basis for claiming "the chain does not know this signature"?
//
// Take signatures from an OLD block (far outside the recent status cache),
// then ask both sources independently:
//   A) getSignatureStatuses([sig], {searchTransactionHistory: true})
//   B) getTransaction(sig)
// If A says null while B returns the transaction, one source alone would have
// claimed a revert for a transaction that plainly exists — the exact
// false-positive a reorg marking must never publish.
//
// Two distinctions this probe must keep straight, because conflating either
// one produced a wrong reading on the first attempt:
//   * the JSON-RPC envelope is {context, value:[...]}, so the status lives at
//     a.value.value[0] — reading a.value[0] yields undefined, which looks
//     exactly like "the chain does not know it";
//   * a JSON-RPC *error* (rate limit, bad params) is not a null verdict. It is
//     counted separately below and never folded into "refused".

const RPC = "https://api.mainnet-beta.solana.com";
let calls = 0;

async function rpc(method, params) {
  calls++;
  for (let attempt = 0; attempt < 6; attempt++) {
    const res = await fetch(RPC, {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ jsonrpc: "2.0", id: 1, method, params }),
    });
    if (res.status === 429 || res.status >= 500) {
      await new Promise((r) => setTimeout(r, 800 * (attempt + 1)));
      continue;
    }
    const body = await res.json();
    if (body.error) return { error: body.error };
    return { value: body.result };
  }
  return { error: { message: "retries exhausted" } };
}

const cur = await rpc("getSlot", [{ commitment: "finalized" }]);
if (cur.error) { console.log("getSlot failed", cur.error); process.exit(1); }
const nowSlot = cur.value;
// Walk back to an old block: start ~8 months out and shrink until one exists.
let slot = nowSlot - 50_000_000;
let block = null;
for (let i = 0; i < 12 && !block; i++) {
  const b = await rpc("getBlock", [slot, { transactionDetails: "signatures", rewards: false, commitment: "finalized" }]);
  if (b.value) block = b.value;
  else slot -= 4_000_000;
}
if (!block) { console.log("no old block found"); process.exit(1); }

const sigs = (block.signatures || []).slice(0, 6);
console.log(JSON.stringify({ nowSlot, probedSlot: slot, txsInBlock: (block.signatures || []).length, sample: sigs.length, calls }));

const out = [];
for (const sig of sigs) {
  const a = await rpc("getSignatureStatuses", [[sig], { searchTransactionHistory: true }]);
  // The envelope is {context, value:[...]}; an ERROR is not a null verdict.
  let aVerdict = "error";
  if (!a.error) {
    const arr = a.value && a.value.value;
    const st = Array.isArray(arr) ? arr[0] : undefined;
    aVerdict = st === undefined ? "no-value" : (st === null ? "refused" : (st.confirmationStatus || "present"));
  }
  const b = await rpc("getTransaction", [sig, { encoding: "json", maxSupportedTransactionVersion: 0, commitment: "finalized" }]);
  const bVerdict = b.error ? "error" : (b.value === null ? "refused" : "found");

  out.push({
    sig: sig.slice(0, 12) + "…",
    status: aVerdict,
    tx: bVerdict,
    // A single-source trap is: A refuses while B serves the transaction.
    trap: aVerdict === "refused" && bVerdict === "found",
    // An RPC error counted as a refusal would be a second, silent trap.
    error_as_null: a.error && aVerdict === "refused",
  });
}
console.log(JSON.stringify(out, null, 1));
const traps = out.filter((o) => o.trap).length;
const errors = out.filter((o) => o.status === "error" || o.tx === "error").length;
const bothKnown = out.filter((o) => o.status !== "refused" && o.tx === "found").length;
console.log(JSON.stringify({
  sampled: out.length,
  single_source_traps: traps,
  rpc_errors_kept_separate: errors,
  status_still_answers_for_old_blocks: bothKnown,
  rpcCalls: calls,
}));
