//! What a transaction did — to ACCOUNTS and to the PROGRAM IT RAN — extracted
//! from the response reeflog already pays for.
//!
//! Why this exists
//! ---------------
//! reeflog's hydration pass already fetches every row it recovered with
//! `getTransaction` and stores `meta.logMessages` — and throws the rest of
//! the response away. Measured 2026-10-31 on live mainnet (Raydium AMM v4,
//! `tools/probe-accounts.mjs`): that discarded response averages 11.9 KB per
//! transaction and carries, per transaction, ~33.5 account keys, ~5.5
//! accounts with a lamport balance change and ~3.5 changed SPL token
//! balances. The information is already paid for; it was being thrown away.
//!
//! What this module claims
//! -----------------------
//! It extracts the ACCOUNT CHANGES a transaction made — the accounts whose
//! lamport balance moved, and the token balances that changed — plus the
//! transaction's INSTRUCTION BYTES (outer instructions and the CPIs they
//! spawned), from the transaction's own metadata. The instruction bytes are
//! what a decoder downstream needs to turn an event into meaning; they were
//! being fetched a second time by every consumer that wanted them. It does
//! NOT recover account state: the bytes an account held BEFORE the event are
//! a different problem, and reeflog does not solve it (see the README's
//! limits).
//!
//! The hard part: account indexing
//! ------------------------------
//! `meta.preBalances` / `meta.postBalances` are indexed over ALL accounts
//! the transaction touched — static keys FIRST, then the lookup table's
//! writable accounts, then its readonly accounts. `message.accountKeys` on a
//! legacy (v0) transaction lists ONLY the static keys. So on any transaction
//! that uses a lookup table, indexing the balance arrays by
//! `message.accountKeys` maps every balance to the WRONG address, silently:
//! no error, plausible numbers, wrong accounts.
//!
//! That is why the account list is rebuilt in the canonical order
//! (static ++ loadedWritable ++ loadedReadonly) instead of trusting one field
//! to be complete. A v0 transaction with a lookup table therefore keeps its
//! balances attached to the right addresses.

use serde_json::{json, Value};

/// One changed token balance, keyed by (account index, mint, owner): the
/// before/after amounts as exact decimal strings.
type TokenEntry = (Option<String>, Option<String>);

/// The account-change envelope stored alongside a row's logs.
///
/// Shape (stable):
/// ```json
/// {
///   "version": "legacy" | 0,
///   "slot": 123,
///   "program_id": "...",
///   "err": null | {...},
///   "accounts_total": 33,
///   "accounts_static": 33,
///   "accounts_loaded": 0,
///   "instructions": [
///     {"inner": false, "outer": 0, "program_id": "...",
///      "accounts": ["...", null], "data": "<base58>"},
///     {"inner": true, "outer": 0, "program_id": "...", "accounts": [],
///      "data": "<base58>"}
///   ],
///   "lamport_changes": [
///     {"index": 2, "address": "...", "before": 1, "after": 2, "delta": 1}
///   ],
///   "token_changes": [
///     {"index": 5, "address": "...", "mint": "...", "owner": "...",
///      "before": "0", "after": "100", "delta": "100"}
///   ]
/// }
/// ```
/// Amounts are strings: a u64 token amount does not fit in a JSON number
/// without losing digits, and a balance that silently loses its last digits
/// is worse than a balance that is obviously a string.
///
/// `instructions` is ordered: every outer instruction in execution order
/// first, then `meta.innerInstructions` groups in response order. `outer` on
/// an outer entry is its own index; on an inner entry it is the top-level
/// instruction that spawned it. `data` is the base58 string exactly as the
/// RPC returned it — reeflog never re-encodes bytes it did not produce. An
/// account index that does not resolve becomes an explicit `null` at its own
/// position: position is information, and a shifted list would put an
/// argument under the wrong name downstream. `err` is `meta.err` verbatim
/// (`null` = the transaction succeeded), so a consumer never has to guess
/// whether a failed transaction's logs describe an effect that happened.
#[derive(Debug, Clone, PartialEq)]
pub struct AccountEnvelope {
    pub value: Value,
}

/// Extract the account-change envelope from a `getTransaction` result.
///
/// Returns `None` for a response that carries no usable metadata: a
/// transaction without a `meta` block says nothing about accounts, and
/// inventing an empty envelope would claim "nothing changed" for a
/// transaction we simply did not understand.
pub fn extract(tx: &Value) -> Option<AccountEnvelope> {
    let meta = tx.get("meta")?;
    let msg = tx.get("transaction")?.get("message")?;

    // The account list, in the order the balance arrays use:
    // static keys, then the lookup table's writable, then its readonly.
    let mut addresses: Vec<String> = Vec::new();
    let mut static_count = 0usize;
    for k in msg.get("accountKeys")?.as_array()? {
        let a = match k {
            // Legacy encoding: the key is a bare string.
            Value::String(s) => s.clone(),
            // Versioned encoding: the key is an object with a `pubkey`.
            Value::Object(_) => k.get("pubkey")?.as_str()?.to_string(),
            _ => return None,
        };
        addresses.push(a);
        static_count += 1;
    }
    let loaded = meta.get("loadedAddresses");
    let mut loaded_count = 0usize;
    for group in ["writable", "readonly"] {
        if let Some(list) = loaded.and_then(|l| l.get(group)).and_then(|v| v.as_array()) {
            for a in list {
                let s = match a {
                    Value::String(s) => s.clone(),
                    _ => return None,
                };
                addresses.push(s);
                loaded_count += 1;
            }
        }
    }

    let pre = meta.get("preBalances").and_then(|v| v.as_array());
    let post = meta.get("postBalances").and_then(|v| v.as_array());
    let pre_tok = meta.get("preTokenBalances").and_then(|v| v.as_array());
    let post_tok = meta.get("postTokenBalances").and_then(|v| v.as_array());

    // The balances may be shorter than the account list (a truncated or
    // partial response). Every balance is then matched to the address at the
    // same index — and an index beyond the balance arrays is simply not
    // reported, rather than reported against a guessed address.
    let address_at = |i: usize| -> Option<String> { addresses.get(i).cloned() };

    // Lamport balance changes.
    let mut lamport_changes: Vec<Value> = Vec::new();
    let n_balances = pre.map(|p| p.len()).unwrap_or(0).max(post.map(|p| p.len()).unwrap_or(0));
    for i in 0..n_balances {
        let b = pre.and_then(|p| p.get(i)).and_then(|v| v.as_i64()).unwrap_or(0);
        let a = post.and_then(|p| p.get(i)).and_then(|v| v.as_i64()).unwrap_or(0);
        if b == a {
            continue;
        }
        let Some(address) = address_at(i) else { continue };
        lamport_changes.push(json!({
            "index": i,
            "address": address,
            "before": b,
            "after": a,
            "delta": a - b,
        }));
    }

    // Token balance changes, keyed by (account index, mint, owner).
    let mut tok: std::collections::BTreeMap<(usize, String, String), TokenEntry> =
        std::collections::BTreeMap::new();
    for t in pre_tok.into_iter().flatten() {
        let Some(idx) = t.get("accountIndex").and_then(|v| v.as_u64()) else { continue };
        let mint = t.get("mint").and_then(|v| v.as_str()).unwrap_or("").to_string();
        let owner = t.get("owner").and_then(|v| v.as_str()).unwrap_or("").to_string();
        let amt = t
            .get("uiTokenAmount")
            .and_then(|v| v.get("amount"))
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        tok.entry((idx as usize, mint, owner)).or_insert((amt, None));
    }
    for t in post_tok.into_iter().flatten() {
        let Some(idx) = t.get("accountIndex").and_then(|v| v.as_u64()) else { continue };
        let mint = t.get("mint").and_then(|v| v.as_str()).unwrap_or("").to_string();
        let owner = t.get("owner").and_then(|v| v.as_str()).unwrap_or("").to_string();
        let amt = t
            .get("uiTokenAmount")
            .and_then(|v| v.get("amount"))
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        let e = tok
            .entry((idx as usize, mint, owner))
            .or_insert((None, None));
        e.1 = amt;
    }

    let mut token_changes: Vec<Value> = Vec::new();
    for ((idx, mint, owner), (before, after)) in tok {
        // Absent from one side = zero tokens on that side. An account absent
        // from `postTokenBalances` was closed (its balance went to 0 and the
        // token state with it); one absent from `preTokenBalances` did not
        // exist as a token account yet. The RPC confirms this reading: it
        // DOES list genuinely-zero balances (measured on mainnet — a row with
        // amount "0" appears in both arrays), so absence is not a silent 0,
        // it is no token state at all, whose balance is 0 by definition.
        //
        // The missing side used to be encoded as an EMPTY STRING delta — an
        // encoding hole the live cross-check in `tools/e2e-accounts.mjs`
        // caught (one closed-account transaction out of four). An empty
        // delta is neither exact nor honest; a closed account's delta is
        // `-before`.
        let before_eff = before.clone().unwrap_or_else(|| "0".to_string());
        let after_eff = after.clone().unwrap_or_else(|| "0".to_string());
        if before_eff == after_eff {
            continue;
        }
        let Some(address) = address_at(idx) else { continue };
        // The delta is computed on the exact decimal strings, so a u64
        // amount never round-trips through a double.
        let delta = decimal_sub(&after_eff, &before_eff);
        token_changes.push(json!({
            "index": idx,
            "address": address,
            "mint": mint,
            "owner": owner,
            "before": before_eff,
            "after": after_eff,
            "delta": delta,
        }));
    }

    // The invoked program: the outer instruction's program id. On a legacy
    // transaction that is an index into the account list; on a versioned one
    // it is an address.
    let program_id = msg
        .get("instructions")
        .and_then(|v| v.as_array())
        .and_then(|a| a.first())
        .and_then(|ix| ix.get("programIdIndex"))
        .and_then(|v| v.as_u64())
        .and_then(|i| address_at(i as usize))
        .or_else(|| {
            msg.get("instructions")
                .and_then(|v| v.as_array())
                .and_then(|a| a.first())
                .and_then(|ix| ix.get("programId"))
                .and_then(|v| v.as_str())
                .map(|s| s.to_string())
        });

    // The instruction bytes, resolved through the SAME canonical address list
    // the balances use: a v0 transaction with a lookup table keeps its
    // instruction accounts on the right addresses (the index shift this
    // module exists to prevent would corrupt arguments silently, exactly the
    // way it corrupted balances).
    let resolve_ix = |ix: &Value, outer: usize, inner: bool| -> Value {
        let program_id = ix
            .get("programIdIndex")
            .and_then(|v| v.as_u64())
            .and_then(|i| addresses.get(i as usize).cloned())
            .or_else(|| {
                ix.get("programId")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string())
            });
        let accounts: Vec<Value> = ix
            .get("accounts")
            .and_then(|v| v.as_array())
            .map(|list| {
                list.iter()
                    .map(|i| {
                        i.as_u64()
                            .and_then(|n| addresses.get(n as usize).cloned())
                            .map(Value::String)
                            // An index that does not resolve keeps its SLOT:
                            // position is information. Dropping it or
                            // shifting the list would put an argument under
                            // the wrong name downstream.
                            .unwrap_or(Value::Null)
                    })
                    .collect()
            })
            .unwrap_or_default();
        json!({
            "inner": inner,
            "outer": outer,
            "program_id": program_id,
            "accounts": accounts,
            "data": ix.get("data").cloned().unwrap_or(Value::Null),
        })
    };
    let mut instructions: Vec<Value> = Vec::new();
    if let Some(list) = msg.get("instructions").and_then(|v| v.as_array()) {
        for (i, ix) in list.iter().enumerate() {
            instructions.push(resolve_ix(ix, i, false));
        }
    }
    if let Some(groups) = meta.get("innerInstructions").and_then(|v| v.as_array()) {
        for g in groups {
            let outer = g.get("index").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
            if let Some(list) = g.get("instructions").and_then(|v| v.as_array()) {
                for (pos, ix) in list.iter().enumerate() {
                    let mut v = resolve_ix(ix, outer, true);
                    // Position within the parent's CPI list: a decoder reads
                    // the Nth CPI of an instruction as its own fact, and a
                    // lost position is a lost fact.
                    v["inner_index"] = json!(pos);
                    instructions.push(v);
                }
            }
        }
    }

    let version = match msg.get("version") {
        Some(Value::String(s)) => Value::String(s.clone()),
        Some(v) if v.is_number() => v.clone(),
        _ => Value::String("legacy".into()),
    };

    Some(AccountEnvelope {
        value: json!({
            "version": version,
            "slot": tx.get("slot").cloned().unwrap_or(Value::Null),
            "program_id": program_id,
            "err": meta.get("err").cloned().unwrap_or(Value::Null),
            "accounts_total": addresses.len(),
            "accounts_static": static_count,
            "accounts_loaded": loaded_count,
            "instructions": instructions,
            "lamport_changes": lamport_changes,
            "token_changes": token_changes,
        }),
    })
}

/// Drop leading zeros, keeping at least one digit.
fn strip_leading_zeros(v: &[u8]) -> &[u8] {
    let mut i = 0;
    while i + 1 < v.len() && v[i] == b'0' {
        i += 1;
    }
    &v[i..]
}

/// Subtract two non-negative decimal integers given as strings.
///
/// Implemented on the digit strings rather than on u64/i64 so that a token
/// amount at the top of the u64 range is exact. Both sides are non-negative
/// decimals; the result may be negative (a balance can shrink).
fn decimal_sub(a: &str, b: &str) -> String {
    let av: Vec<u8> = a.bytes().filter(|c| c.is_ascii_digit()).collect();
    let bv: Vec<u8> = b.bytes().filter(|c| c.is_ascii_digit()).collect();
    if av.is_empty() && bv.is_empty() {
        return "0".into();
    }
    // Which operand is larger, compared by stripped length then digits.
    let (sa, sb) = (strip_leading_zeros(&av), strip_leading_zeros(&bv));
    let a_bigger = match sa.len().cmp(&sb.len()) {
        std::cmp::Ordering::Greater => true,
        std::cmp::Ordering::Less => false,
        std::cmp::Ordering::Equal => sa >= sb,
    };
    let (big, small, neg) = if a_bigger { (&av, &bv, false) } else { (&bv, &av, true) };
    let mut ia = big.len() as i64 - 1;
    let mut ib = small.len() as i64 - 1;
    let mut borrow = 0i64;
    let mut out: Vec<u8> = Vec::new();
    while ia >= 0 || ib >= 0 {
        let da = if ia >= 0 { big[ia as usize] - b'0' } else { 0 } as i64;
        let db = if ib >= 0 { small[ib as usize] - b'0' } else { 0 } as i64;
        let mut d = da - db - borrow;
        if d < 0 {
            d += 10;
            borrow = 1;
        } else {
            borrow = 0;
        }
        out.push(b'0' + d as u8);
        ia -= 1;
        ib -= 1;
    }
    // big >= small was established above, so the final borrow is 0 by
    // construction; nothing to terminate here.
    while out.len() > 1 && *out.last().unwrap() == b'0' {
        out.pop();
    }
    out.reverse();
    let s = String::from_utf8(out).unwrap_or_else(|_| "0".into());
    if s == "0" {
        return "0".into();
    }
    if neg {
        format!("-{s}")
    } else {
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn legacy_tx() -> Value {
        json!({
            "slot": 100,
            "transaction": {
                "message": {
                    "accountKeys": ["A", "B", "C"],
                    "instructions": [{"programIdIndex": 2, "accounts": [0, 1], "data": "x"}]
                }
            },
            "meta": {
                "preBalances": [100, 200, 300],
                "postBalances": [100, 250, 300],
                "preTokenBalances": [
                    {"accountIndex": 1, "mint": "M1", "owner": "O", "uiTokenAmount": {"amount": "10"}}
                ],
                "postTokenBalances": [
                    {"accountIndex": 1, "mint": "M1", "owner": "O", "uiTokenAmount": {"amount": "25"}}
                ]
            }
        })
    }

    #[test]
    fn a_lamport_change_is_reported_with_its_address() {
        let env = extract(&legacy_tx()).expect("envelope");
        assert_eq!(env.value["accounts_total"].as_i64(), Some(3));
        let changes = env.value["lamport_changes"].as_array().unwrap();
        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0]["address"], "B");
        assert_eq!(changes[0]["index"], 1);
        assert_eq!(changes[0]["before"], 200);
        assert_eq!(changes[0]["after"], 250);
        assert_eq!(changes[0]["delta"], 50);
    }

    #[test]
    fn a_token_change_keeps_its_exact_amounts_as_strings() {
        let env = extract(&legacy_tx()).expect("envelope");
        let t = &env.value["token_changes"].as_array().unwrap()[0];
        assert_eq!(t["before"], "10");
        assert_eq!(t["after"], "25");
        assert_eq!(t["delta"], "15");
        assert_eq!(t["mint"], "M1");
        assert_eq!(t["address"], "B");
    }

    #[test]
    fn a_lookup_table_does_not_shift_the_balances_onto_the_wrong_addresses() {
        // The trap: a v0 transaction with a lookup table has 3 static keys but
        // 5 accounts. The balance arrays are indexed over ALL FIVE, in the
        // order static ++ loadedWritable ++ loadedReadonly. Indexing them by
        // message.accountKeys would attach account 3's balance to address "C".
        let tx = json!({
            "slot": 101,
            "transaction": {
                "message": {
                    "accountKeys": ["A", "B", "C"],
                    "instructions": [{"programIdIndex": 2}]
                }
            },
            "meta": {
                "loadedAddresses": {"writable": ["W"], "readonly": ["R"]},
                "preBalances": [1, 2, 3, 40, 50],
                "postBalances": [1, 2, 3, 45, 50]
            }
        });
        let env = extract(&tx).expect("envelope");
        assert_eq!(env.value["accounts_total"].as_i64(), Some(5));
        assert_eq!(env.value["accounts_static"], 3);
        assert_eq!(env.value["accounts_loaded"], 2);
        let changes = env.value["lamport_changes"].as_array().unwrap();
        assert_eq!(changes.len(), 1);
        assert_eq!(
            changes[0]["address"], "W",
            "the changed balance belongs to the loaded writable account, not to the third static key"
        );
        assert_eq!(changes[0]["index"], 3);
    }

    #[test]
    fn a_versioned_transaction_with_object_keys_is_read() {
        let tx = json!({
            "slot": 102,
            "transaction": {
                "message": {
                    "version": 0,
                    "accountKeys": [
                        {"pubkey": "A", "signer": true, "writable": true},
                        {"pubkey": "B", "signer": false, "writable": true}
                    ],
                    "instructions": [{"programId": "P"}]
                }
            },
            "meta": {"preBalances": [5, 5], "postBalances": [5, 9]}
        });
        let env = extract(&tx).expect("envelope");
        assert_eq!(env.value["version"], 0);
        assert_eq!(env.value["program_id"], "P");
        let changes = env.value["lamport_changes"].as_array().unwrap();
        assert_eq!(changes[0]["address"], "B");
    }

    #[test]
    fn the_envelope_carries_the_instruction_bytes_of_the_response_it_was_paid_for() {
        let env = extract(&legacy_tx()).expect("envelope");
        let ixs = env.value["instructions"].as_array().unwrap();
        assert_eq!(ixs.len(), 1);
        assert_eq!(ixs[0]["inner"], false);
        assert_eq!(ixs[0]["outer"], 0);
        assert_eq!(ixs[0]["program_id"], "C", "programIdIndex resolves through the canonical list");
        assert_eq!(ixs[0]["accounts"], json!(["A", "B"]));
        assert_eq!(ixs[0]["data"], "x");
        assert_eq!(env.value["err"], Value::Null, "a successful transaction says null, not nothing");
    }

    #[test]
    fn meta_err_is_carried_verbatim_so_a_consumer_never_guesses() {
        let mut tx = legacy_tx();
        tx["meta"]["err"] = json!({"InstructionError": [0, {"Custom": 1}]});
        let env = extract(&tx).expect("envelope");
        assert_eq!(env.value["err"], json!({"InstructionError": [0, {"Custom": 1}]}));
    }

    #[test]
    fn inner_instructions_are_tagged_with_the_top_level_instruction_that_spawned_them() {
        let tx = json!({
            "transaction": {
                "message": {
                    "accountKeys": ["A", "B", "C"],
                    "instructions": [
                        {"programIdIndex": 2, "accounts": [0, 1], "data": "outer1"},
                        {"programIdIndex": 1, "accounts": [0], "data": "outer2"}
                    ]
                }
            },
            "meta": {
                "preBalances": [1, 2, 3],
                "postBalances": [1, 2, 3],
                "innerInstructions": [
                    {"index": 0, "instructions": [
                        {"programIdIndex": 1, "accounts": [1], "data": "cpi-a"},
                        {"programIdIndex": 2, "accounts": [0], "data": "cpi-b"}
                    ]},
                    {"index": 1, "instructions": [
                        {"programIdIndex": 0, "accounts": [2], "data": "cpi-c"}
                    ]}
                ]
            }
        });
        let env = extract(&tx).expect("envelope");
        let ixs = env.value["instructions"].as_array().unwrap();
        assert_eq!(ixs.len(), 5, "2 outer + 3 inner");
        // Outer first, in execution order.
        assert_eq!(ixs[0]["inner"], false);
        assert_eq!(ixs[0]["outer"], 0);
        assert_eq!(ixs[1]["outer"], 1);
        // Inner entries carry the index of the instruction that spawned them,
        // so a consumer can attribute a CPI to the call that made it.
        assert_eq!(ixs[2]["inner"], true);
        assert_eq!(ixs[2]["outer"], 0);
        assert_eq!(ixs[2]["data"], "cpi-a");
        assert_eq!(ixs[3]["outer"], 0);
        assert_eq!(ixs[4]["inner"], true);
        assert_eq!(ixs[4]["outer"], 1);
    }

    #[test]
    fn an_unresolvable_account_index_stays_null_in_its_own_position() {
        // Position is information. A list that silently drops an unknown
        // index shifts every later argument one slot left — the decoder then
        // reads the wrong bytes under the right names.
        let tx = json!({
            "transaction": {
                "message": {"accountKeys": ["A"], "instructions": [
                    {"programIdIndex": 0, "accounts": [0, 9, 0], "data": "d"}
                ]}
            },
            "meta": {"preBalances": [1], "postBalances": [1]}
        });
        let env = extract(&tx).expect("envelope");
        let ixs = env.value["instructions"].as_array().unwrap();
        assert_eq!(ixs[0]["accounts"], json!(["A", null, "A"]));
    }

    #[test]
    fn an_instruction_without_data_says_so_instead_of_inventing_bytes() {
        let tx = json!({
            "transaction": {
                "message": {"accountKeys": ["A"], "instructions": [{"programIdIndex": 0}]}
            },
            "meta": {"preBalances": [1], "postBalances": [1]}
        });
        let env = extract(&tx).expect("envelope");
        let ixs = env.value["instructions"].as_array().unwrap();
        assert_eq!(ixs[0]["data"], Value::Null);
        assert_eq!(ixs[0]["accounts"], json!([]));
    }

    #[test]
    fn a_response_without_metadata_yields_no_envelope() {
        assert!(extract(&json!({})).is_none());
        assert!(extract(&json!({"transaction": {"message": {"accountKeys": []}}})).is_none());
    }

    #[test]
    fn a_balance_without_a_matching_account_is_not_reported_against_a_guess() {
        let tx = json!({
            "transaction": {
                "message": {"accountKeys": ["A"], "instructions": []}
            },
            "meta": {"preBalances": [1, 2, 3], "postBalances": [1, 5, 3]}
        });
        let env = extract(&tx).expect("envelope");
        assert_eq!(
            env.value["lamport_changes"].as_array().unwrap().len(),
            0,
            "a balance with no address must be dropped, never attributed"
        );
    }

    #[test]
    fn a_closed_token_account_reports_the_full_negative_delta() {
        // Found by the live cross-check (tools/e2e-accounts.mjs): an account
        // present in preTokenBalances but absent from postTokenBalances had
        // its delta encoded as an EMPTY STRING — neither exact nor honest.
        // Absence on one side means a zero balance on that side, so the delta
        // is -before.
        let tx = json!({
            "transaction": {
                "message": {"accountKeys": ["A", "T"], "instructions": []}
            },
            "meta": {
                "preTokenBalances": [
                    {"accountIndex": 1, "mint": "M", "owner": "O", "uiTokenAmount": {"amount": "1331874357100"}}
                ],
                "postTokenBalances": []
            }
        });
        let env = extract(&tx).expect("envelope");
        let t = &env.value["token_changes"].as_array().unwrap()[0];
        assert_eq!(t["before"], "1331874357100");
        assert_eq!(t["after"], "0");
        assert_eq!(t["delta"], "-1331874357100", "a closed account loses exactly its balance");
    }

    #[test]
    fn a_token_account_that_did_not_exist_yet_reports_the_positive_delta() {
        let tx = json!({
            "transaction": {
                "message": {"accountKeys": ["A", "T"], "instructions": []}
            },
            "meta": {
                "preTokenBalances": [],
                "postTokenBalances": [
                    {"accountIndex": 1, "mint": "M", "owner": "O", "uiTokenAmount": {"amount": "500"}}
                ]
            }
        });
        let env = extract(&tx).expect("envelope");
        let t = &env.value["token_changes"].as_array().unwrap()[0];
        assert_eq!(t["before"], "0");
        assert_eq!(t["after"], "500");
        assert_eq!(t["delta"], "500");
    }

    #[test]
    fn a_genuinely_zero_balance_that_the_rpc_does_report_is_not_a_change() {
        // The RPC lists real zero balances (amount "0" in both arrays), so a
        // 0 -> 0 row is a genuine no-change, not an absence.
        let tx = json!({
            "transaction": {
                "message": {"accountKeys": ["A", "T"], "instructions": []}
            },
            "meta": {
                "preTokenBalances": [
                    {"accountIndex": 1, "mint": "M", "owner": "O", "uiTokenAmount": {"amount": "0"}}
                ],
                "postTokenBalances": [
                    {"accountIndex": 1, "mint": "M", "owner": "O", "uiTokenAmount": {"amount": "0"}}
                ]
            }
        });
        let env = extract(&tx).expect("envelope");
        assert_eq!(env.value["token_changes"].as_array().unwrap().len(), 0);
    }

    #[test]
    fn decimal_subtraction_is_exact_at_the_top_of_the_u64_range() {
        assert_eq!(decimal_sub("18446744073709551615", "1"), "18446744073709551614");
        assert_eq!(decimal_sub("100", "250"), "-150");
        assert_eq!(decimal_sub("0", "0"), "0");
        assert_eq!(decimal_sub("1000", "1"), "999");
    }
}
