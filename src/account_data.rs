//! What an account HELD, in bytes — resolved in batches, never guessed.
//!
//! Why this exists
//! ---------------
//! The owners pass (src/owners.rs) resolves WHO owns each enumerated
//! address; the named open item it leaves is WHAT the account held. The
//! same `getMultipleAccounts` call the owners pass already pays for returns
//! the account's data bytes — the owners pass throws them away on purpose
//! (`dataSlice {0,1}`). This module is the second half of that answer, as
//! its own explicitly-budgeted pass: `hydrate --account-data`.
//!
//! What this module claims
//! -----------------------
//! For every row it stores, `account_data` inside the envelope maps every
//! address the row enumerates to one of:
//!   - `held`: the account's data bytes (base64), truncated at the cap
//!     (16384 B — measured defense against a 1 MB slab) with the actual
//!     size recorded (`bytes`, `truncated`),
//!   - `missing`: the chain does not have the account (the same rule the
//!     owners pass established: absence is a fact, recorded not guessed),
//!   - `skipped_executable`: the account is program code (`executable:
//!     true`). Measured 2026-11-02 (tools/probe-account-data.mjs, 14 Raydium
//!     AMM v4 txs, 238 distinct addresses): the largest accounts in that
//!     window are program code (105 KB, 74 KB). Journalling them as event
//!     data would be wrong in a different way than truncating state is.
//!     They are still delivered — flagged, byte-free — so the row stays
//!     complete.
//!
//! Every address is accounted for before the row is marked; a row whose
//! addresses span a failed batch stays owed, never half-written.
//!
//! What it does NOT claim
//! ----------------------
//! These bytes are read AT FETCH TIME, not at event time. An account can be
//! closed or rewritten between the transaction and the fetch (measured on
//! the same window: 40 of 238 addresses no longer existed). The envelope's
//! slot and the `source` tag say when the facts were true; nothing here is
//! historical state. A consumer who needs the bytes AS OF the event still
//! does not have them — this shrinks the gap, it does not close it.
//!
//! The at-cap ambiguity
//! --------------------
//! An account sitting exactly at the cap with more bytes beyond it would be
//! indistinguishable from one that ends exactly there. The pass pays one
//! extra batched call for exactly those addresses (offset=cap, length=1):
//! the account appearing again means more data beyond the cap
//! (`truncated: true`, `truncation_verified: true`); `null` means the
//! account is gone OR ends exactly at the cap — genuinely ambiguous at
//! fetch time, so this module does not invent the difference: the row says
//! `truncation_verified: false` instead of a guess.

use crate::backfill::Fetcher;
use crate::journal::Journal;
use base64::Engine;
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};

/// Measured 2026-11-02 (tools/probe-account-data.mjs, live mainnet,
/// Raydium AMM v4): p50 165 B, p90 752 B, p95 4097 B, and a 1,048,588 B
/// non-executable Serum/OpenBook slab. This cap stores the observed
/// accounts whole (198 observed, 6 over the cap) while bounding the worst
/// case per address.
pub const ACCOUNT_DATA_CAP: usize = 16384;

/// One `hydrate --account-data` pass, in the same shape the owners pass
/// reports: considered / stored / cost / known-missing / failed / left.
#[derive(Debug, Default, Clone)]
pub struct AccountDataReport {
    /// Rows the envelope predicate put in front of this pass.
    pub considered: i64,
    /// Rows whose addresses were fully accounted for and whose envelope now
    /// carries `account_data`.
    pub stored: i64,
    /// Addresses whose bytes were stored (state accounts, ≤ cap or capped).
    pub addresses_held: i64,
    /// Addresses the chain did not have at fetch time — recorded as facts.
    pub addresses_missing: i64,
    /// Program-code accounts delivered flagged, byte-free.
    pub addresses_skipped_executable: i64,
    /// `getMultipleAccounts` calls actually made (bytes batch + probe batch).
    pub calls: i64,
    /// Batches the endpoint refused or broke on. Rows containing those
    /// addresses stay owed — never half-written.
    pub failed_batches: i64,
    pub error: Option<String>,
    /// Rows still owed after this pass (limit reached or a batch failed).
    pub left: i64,
    pub stored_sample: Vec<String>,
}

impl AccountDataReport {
    /// Complete = no batch failed and nothing is left owed. A known-missing
    /// address is a RESOLVED fact and never makes a pass incomplete.
    pub fn complete(&self) -> bool {
        self.error.is_none() && self.failed_batches == 0 && self.left == 0
    }
}

/// One address's answer.
struct Entry {
    /// base64 of the (possibly truncated) data bytes.
    data: Option<String>,
    /// The account's real size in bytes, when known.
    bytes: Option<u64>,
    /// The account is program code: flagged, byte-free.
    executable: bool,
    /// The chain does not have this account.
    missing: bool,
    /// The stored bytes were cut at the cap.
    truncated: bool,
    /// Whether "truncated" was CONFIRMED by an at-cap probe. False can mean
    /// "not probed" or "probe ambiguous" — both mean: do not claim the
    /// account ends here.
    truncation_verified: bool,
}

/// Extract the address list a stored envelope enumerates.
///
/// Same rule as the owners pass: only `accounts[]` counts (the
/// flag-verified canonical list). An envelope whose flags were unavailable
/// carries `accounts: null` and names no enumerable set — such rows are
/// excluded by the journal predicate, and this function returns None as
/// defense in depth.
fn envelope_addresses(accounts_json: &str) -> Option<Vec<String>> {
    let v: Value = serde_json::from_str(accounts_json).ok()?;
    let list = v.get("accounts")?.as_array()?;
    let mut out = Vec::with_capacity(list.len());
    for entry in list {
        // A null address slot is a position the resolution refused; there is
        // nothing to look up, and nothing to guess in its place.
        if let Some(a) = entry.get("address").and_then(|x| x.as_str()) {
            if !out.iter().any(|x: &String| x == a) {
                out.push(a.to_string());
            }
        }
    }
    Some(out)
}

/// Decode the base64 data an RPC account entry carries.
///
/// The encoding is base64 with the padding the endpoint sent; a value that
/// does not decode is not a fact about the account, it is a fact about the
/// response — it must not be stored as bytes.
fn decode_data(entry: &Value) -> Option<Vec<u8>> {
    let arr = entry.get("data")?.as_array()?;
    let b64 = arr.first()?.as_str()?;
    base64::engine::general_purpose::STANDARD
        .decode(b64.as_bytes())
        .ok()
}

/// Resolve account data for rows the journal still owes, in batches of
/// `batch` addresses per `getMultipleAccounts` call.
///
/// Order of operations is the cost claim: addresses are collected ACROSS all
/// considered rows first, deduplicated, then fetched. Writing happens last
/// and only for rows whose every address came back with an answer.
pub fn hydrate_account_data<F: Fetcher>(
    j: &Journal,
    f: &mut F,
    limit: i64,
    batch: usize,
) -> Result<AccountDataReport, String> {
    let mut r = AccountDataReport::default();
    let rows = j.events_missing_account_data(limit + 1)?; // +1: detect "more remain"
    if rows.is_empty() {
        r.left = j.account_data_missing_count()?;
        return Ok(r);
    }
    let take = limit.max(0).min(rows.len() as i64) as usize;
    let batch_rows = &rows[..take];
    r.considered = batch_rows.len() as i64;

    // (signature, addresses) for each considered row, in journal order.
    let mut per_row: Vec<(String, Vec<String>)> = Vec::with_capacity(batch_rows.len());
    let mut all: BTreeSet<String> = BTreeSet::new();
    for (_seq, signature, accounts_json) in batch_rows {
        let Some(addrs) = envelope_addresses(accounts_json) else {
            // Predicate said enumerable, parser disagrees: no row is written
            // from a list this function refused to read. Counted as
            // considered, stays owed, and the next pass sees it again.
            continue;
        };
        for a in &addrs {
            all.insert(a.clone());
        }
        per_row.push((signature.clone(), addrs));
    }
    if per_row.is_empty() {
        r.left = j.account_data_missing_count()?;
        return Ok(r);
    }

    // Fetch: deduplicated addresses, in batches, all rows together.
    //
    // The slice is the cap: at most ACCOUNT_DATA_CAP bytes per account.
    // The endpoint reports the account's REAL size in `space`, so the row
    // can say how much was left out without a second fetch.
    let addresses: Vec<String> = all.into_iter().collect();
    let batch = batch.max(1);
    let mut entries: BTreeMap<String, Entry> = BTreeMap::new();
    let chunks: Vec<&[String]> = addresses.chunks(batch).collect();
    for chunk in chunks {
        let result = match f.call(
            "getMultipleAccounts",
            json!([
                chunk,
                {
                    "encoding": "base64",
                    // Bounded: the cap, not the whole account. `space` still
                    // reports the real size, so truncation stays visible.
                    "dataSlice": {"offset": 0, "length": ACCOUNT_DATA_CAP},
                    "commitment": "finalized"
                }
            ]),
        ) {
            Ok(v) => v,
            Err(e) => {
                r.failed_batches += 1;
                if r.error.is_none() {
                    r.error = Some(e);
                }
                break;
            }
        };
        r.calls += 1;
        let Some(value) = result.get("value").and_then(|v| v.as_array()) else {
            // A response without the array carries no answers: the batch
            // failed as a fact about the response, not about the accounts.
            r.failed_batches += 1;
            if r.error.is_none() {
                r.error = Some("getMultipleAccounts returned no value array".into());
            }
            break;
        };
        for (i, entry) in value.iter().enumerate() {
            let Some(addr) = chunk.get(i) else { break };
            if entry.is_null() {
                // The chain does not have this account. A fact, recorded.
                entries.insert(
                    addr.clone(),
                    Entry {
                        data: None,
                        bytes: None,
                        executable: false,
                        missing: true,
                        truncated: false,
                        truncation_verified: false,
                    },
                );
                continue;
            }
            let executable = entry.get("executable").and_then(|v| v.as_bool()).unwrap_or(false);
            // The account's real size, when the endpoint states it.
            let space = entry.get("space").and_then(|v| v.as_u64());
            let decoded = decode_data(entry);
            let bytes = decoded.as_ref().map(|d| d.len() as u64);
            // Truncated = the endpoint's own size is larger than what we
            // were given. Both signals must agree: a size the endpoint
            // states above the cap, or more bytes than the cap decoded.
            let truncated = space.map(|s| s > ACCOUNT_DATA_CAP as u64).unwrap_or(false)
                || bytes.map(|b| b > ACCOUNT_DATA_CAP as u64).unwrap_or(false);
            entries.insert(
                addr.clone(),
                Entry {
                    data: decoded.map(|d| {
                        base64::engine::general_purpose::STANDARD.encode(&d[..d.len().min(ACCOUNT_DATA_CAP)])
                    }),
                    bytes: space.or(bytes),
                    executable,
                    missing: false,
                    truncated,
                    truncation_verified: false,
                },
            );
        }
    }

    // The at-cap probe: for every address that came back TRUNCATED, ask the
    // chain whether there is more beyond the cap. One extra batched call for
    // exactly those addresses.
    //
    // Without it, "exactly at the cap" and "longer than the cap" are the
    // same bytes — and a consumer told "truncated" cannot tell whether the
    // rest exists. With it, the account appearing again proves more data
    // exists; `null` means it is gone OR ends exactly at the cap, which is
    // ambiguous at fetch time and is reported as unverified rather than
    // guessed.
    let at_cap: Vec<String> = entries
        .iter()
        .filter(|(_, e)| e.truncated && !e.missing)
        .map(|(a, _)| a.clone())
        .collect();
    if !at_cap.is_empty() {
        let chunks: Vec<&[String]> = at_cap.chunks(batch).collect();
        for chunk in chunks {
            let result = match f.call(
                "getMultipleAccounts",
                json!([
                    chunk,
                    {
                        "encoding": "base64",
                        // One byte AT the cap: present → more data beyond.
                        "dataSlice": {"offset": ACCOUNT_DATA_CAP, "length": 1},
                        "commitment": "finalized"
                    }
                ]),
            ) {
                Ok(v) => v,
                Err(e) => {
                    r.failed_batches += 1;
                    if r.error.is_none() {
                        r.error = Some(e);
                    }
                    break;
                }
            };
            r.calls += 1;
            let Some(value) = result.get("value").and_then(|v| v.as_array()) else {
                r.failed_batches += 1;
                if r.error.is_none() {
                    r.error = Some("at-cap probe returned no value array".into());
                }
                break;
            };
            for (i, entry) in value.iter().enumerate() {
                let Some(addr) = chunk.get(i) else { break };
                if let Some(e) = entries.get_mut(addr) {
                    // Present again → there IS data beyond the cap.
                    e.truncation_verified = !entry.is_null();
                }
            }
        }
    }

    // Write: only rows where EVERY address has an answer.
    // A row containing an address from a failed batch stays owed — the
    // half-written-row defect the payload pass already paid for once.
    for (signature, addrs) in &per_row {
        let mut data = serde_json::Map::new();
        let mut missing: Vec<String> = Vec::new();
        let mut skipped_executable: Vec<String> = Vec::new();
        let mut complete = true;
        for a in addrs {
            let Some(e) = entries.get(a) else {
                complete = false;
                break;
            };
            if e.missing {
                missing.push(a.clone());
                continue;
            }
            if e.executable {
                // Program code: delivered as a fact, without bytes.
                skipped_executable.push(a.clone());
                data.insert(
                    a.clone(),
                    json!({
                        "state": "skipped_executable",
                        "bytes": e.bytes,
                    }),
                );
                continue;
            }
            let Some(b64) = e.data.clone() else {
                // Exists but no data field: no answer, so no write for the
                // rows that need it. Leaves them owed instead of inventing.
                complete = false;
                break;
            };
            data.insert(
                a.clone(),
                json!({
                    "state": "held",
                    "encoding": "base64",
                    "data": b64,
                    "bytes": e.bytes,
                    "truncated": e.truncated,
                    "truncation_verified": e.truncation_verified,
                }),
            );
        }
        if !complete {
            continue;
        }
        let data_json = json!({
            // "fetch time" is the honest tag: these bytes were read when this
            // pass ran, not when the event happened. The envelope's own slot
            // and the row's first_seen bracket it.
            "source": "getMultipleAccounts",
            "cap_bytes": ACCOUNT_DATA_CAP,
            "data": data,
            "missing": missing,
            "skipped_executable": skipped_executable,
        });
        if j.set_account_data(signature, &data_json.to_string())? {
            r.stored += 1;
            if r.stored_sample.len() < 5 {
                r.stored_sample.push(signature.clone());
            }
        }
    }
    r.addresses_held = entries
        .values()
        .filter(|e| !e.missing && !e.executable)
        .count() as i64;
    r.addresses_missing = entries.values().filter(|e| e.missing).count() as i64;
    r.addresses_skipped_executable = entries.values().filter(|e| e.executable).count() as i64;
    r.left = j.account_data_missing_count()?;
    Ok(r)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backfill::Fetcher;
    use serde_json::json;
    use std::collections::BTreeMap as Map;

    fn tmp(tag: &str) -> Journal {
        use std::sync::atomic::{AtomicU64, Ordering};
        static N: AtomicU64 = AtomicU64::new(0);
        let n = N.fetch_add(1, Ordering::SeqCst);
        let p = std::env::temp_dir().join(format!(
            "reeflog-account-data-test-{}-{}-{}.db",
            std::process::id(),
            tag,
            n
        ));
        let _ = std::fs::remove_file(&p);
        Journal::open(&p).expect("open journal")
    }

    /// A row with logs and an envelope whose `accounts` list enumerates the
    /// given addresses.
    fn plant(j: &Journal, sig: &str, addresses: &[&str]) {
        let list: Vec<Value> = addresses
            .iter()
            .enumerate()
            .map(|(i, a)| {
                json!({"index": i, "address": a, "signer": i == 0, "writable": true, "source": "static"})
            })
            .collect();
        let env = json!({
            "version": "legacy", "slot": 1, "program_id": "P", "err": Value::Null,
            "accounts_total": addresses.len(), "accounts_static": addresses.len(),
            "accounts_loaded": 0, "accounts_flags_source": "header",
            "accounts": list, "instructions": [], "lamport_changes": [], "token_changes": [],
        });
        j.insert_for_test(sig, 1, Some("[\"log\"]"), None).unwrap();
        assert!(j.set_accounts(sig, &env.to_string()).unwrap(), "plant envelope for {sig}");
    }

    /// Stub: answers getMultipleAccounts from a fixed table.
    /// `held`: address -> (bytes, executable, extra_beyond_cap)
    struct StubRpc {
        held: Map<String, (usize, bool, usize)>,
        pub calls: usize,
        pub slices: Vec<Value>,
        pub fail_for: Vec<String>,
    }

    impl Fetcher for StubRpc {
        fn call(&mut self, method: &str, params: Value) -> Result<Value, String> {
            assert_eq!(method, "getMultipleAccounts", "the data pass may only batch-fetch");
            self.calls += 1;
            let slice = params[1].get("dataSlice").cloned().unwrap_or(Value::Null);
            self.slices.push(slice.clone());
            let chunk = params[0].as_array().expect("chunk array").clone();
            let addr_of = |v: &Value| v.as_str().unwrap_or_default().to_string();
            if let Some(bad) = chunk.iter().find(|a| {
                self.fail_for.iter().any(|f| f == &addr_of(a))
            }) {
                return Err(format!("stub refuses {bad}"));
            }
            let offset = slice.get("offset").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
            let value: Vec<Value> = chunk
                .iter()
                .map(|a| {
                    let key = addr_of(a);
                    match self.held.get(&key) {
                        None => Value::Null,
                        Some((len, exec, extra)) => {
                            let total = len + extra;
                            // The endpoint returns null only when the chain does
                            // not have the account, or when a slice starts past
                            // its end (offset 0 on a 0-byte account still returns
                            // the account with empty data — a real behavior this
                            // stub must match or the product would classify a
                            // real account as missing).
                            if offset > 0 && offset >= total {
                                Value::Null
                            } else {
                                let take = total.saturating_sub(offset).min(ACCOUNT_DATA_CAP);
                                let payload = vec![7u8; take];
                                json!({
                                    "owner": "11111111111111111111111111111111",
                                    "lamports": 1,
                                    "executable": exec,
                                    "space": total,
                                    "data": [
                                        base64::engine::general_purpose::STANDARD.encode(&payload),
                                        "base64"
                                    ],
                                })
                            }
                        }
                    }
                })
                .collect();
            Ok(json!({"context": {"slot": 9}, "value": value}))
        }
    }

    #[test]
    fn stores_bytes_and_records_absence() {
        let j = tmp("store");
        plant(&j, "sigA", &["a1", "a2", "GONE"]);
        let mut stub = StubRpc {
            held: Map::from([
                ("a1".into(), (10, false, 0)),
                ("a2".into(), (0, false, 0)),
            ]),
            calls: 0,
            slices: vec![],
            fail_for: vec![],
        };
        let r = hydrate_account_data(&j, &mut stub, 10, 100).unwrap();
        assert_eq!(r.considered, 1);
        assert_eq!(r.stored, 1, "the row is fully accounted");
        assert_eq!(r.calls, 1, "one batch, no at-cap probe needed");
        assert_eq!(r.addresses_held, 2);
        assert_eq!(r.addresses_missing, 1, "GONE is a recorded fact");
        assert_eq!(r.left, 0);
        assert!(r.complete());

        let (_, env) = j.row_envelope_for_test("sigA").unwrap();
        let v: Value = serde_json::from_str(&env).unwrap();
        let d = &v["account_data"];
        assert_eq!(d["source"], "getMultipleAccounts");
        assert_eq!(d["cap_bytes"], ACCOUNT_DATA_CAP);
        assert_eq!(d["missing"], json!(["GONE"]));
        assert_eq!(d["data"]["a1"]["state"], "held");
        assert_eq!(d["data"]["a1"]["bytes"], 10);
        assert_eq!(d["data"]["a1"]["truncated"], false);
        assert_eq!(d["data"]["a1"]["truncation_verified"], false);
        // The stored bytes must decode back to what the stub served.
        let raw = d["data"]["a1"]["data"].as_str().unwrap();
        let dec = base64::engine::general_purpose::STANDARD.decode(raw).unwrap();
        assert_eq!(dec.len(), 10);
        assert!(d["data"].get("GONE").is_none(), "a gone address carries NO data entry");
        // The original envelope fields survive the json_set merge.
        assert_eq!(v["accounts_flags_source"], "header");
    }

    #[test]
    fn executable_accounts_are_flagged_not_stored() {
        let j = tmp("exec");
        plant(&j, "sigE", &["prog", "state"]);
        let mut stub = StubRpc {
            held: Map::from([
                ("prog".into(), (105_032, true, 0)),
                ("state".into(), (165, false, 0)),
            ]),
            calls: 0,
            slices: vec![],
            fail_for: vec![],
        };
        let r = hydrate_account_data(&j, &mut stub, 10, 100).unwrap();
        assert_eq!(r.stored, 1);
        assert_eq!(r.addresses_skipped_executable, 1);
        assert_eq!(r.addresses_held, 1, "the state account is held");
        let (_, env) = j.row_envelope_for_test("sigE").unwrap();
        let v: Value = serde_json::from_str(&env).unwrap();
        assert_eq!(v["account_data"]["data"]["prog"]["state"], "skipped_executable");
        assert!(v["account_data"]["data"]["prog"].get("data").is_none());
        assert_eq!(v["account_data"]["skipped_executable"], json!(["prog"]));
        assert_eq!(v["account_data"]["data"]["state"]["state"], "held");
    }

    #[test]
    fn a_truncated_account_is_probed_and_the_probe_verifies() {
        let j = tmp("trunc");
        plant(&j, "sigT", &["big"]);
        // 100 bytes beyond the cap → the at-cap probe must find data there.
        let mut stub = StubRpc {
            held: Map::from([("big".into(), (ACCOUNT_DATA_CAP, false, 100))]),
            calls: 0,
            slices: vec![],
            fail_for: vec![],
        };
        let r = hydrate_account_data(&j, &mut stub, 10, 100).unwrap();
        assert_eq!(r.calls, 2, "one batch plus one at-cap probe");
        let (_, env) = j.row_envelope_for_test("sigT").unwrap();
        let v: Value = serde_json::from_str(&env).unwrap();
        let a = &v["account_data"]["data"]["big"];
        assert_eq!(a["truncated"], true, "the endpoint's own size says so");
        assert_eq!(a["truncation_verified"], true, "the probe found data beyond the cap");
        assert_eq!(a["bytes"], (ACCOUNT_DATA_CAP + 100) as u64);
        // The stored bytes are exactly the cap.
        let dec = base64::engine::general_purpose::STANDARD
            .decode(a["data"].as_str().unwrap())
            .unwrap();
        assert_eq!(dec.len(), ACCOUNT_DATA_CAP);
    }

    #[test]
    fn an_account_ending_exactly_at_the_cap_is_reported_unverified() {
        let j = tmp("exact");
        plant(&j, "sigX", &["exact"]);
        // Exactly the cap: the endpoint's space == cap → NOT truncated, and
        // no probe is needed. The ambiguity case is: the endpoint reports
        // more than the cap but the probe finds nothing (account closed
        // between the two calls) → truncated but unverified.
        let mut stub = StubRpc {
            held: Map::from([("exact".into(), (ACCOUNT_DATA_CAP, false, 0))]),
            calls: 0,
            slices: vec![],
            fail_for: vec![],
        };
        let r = hydrate_account_data(&j, &mut stub, 10, 100).unwrap();
        assert_eq!(r.calls, 1, "no probe for an account that is not over the cap");
        let (_, env) = j.row_envelope_for_test("sigX").unwrap();
        let v: Value = serde_json::from_str(&env).unwrap();
        let a = &v["account_data"]["data"]["exact"];
        assert_eq!(a["truncated"], false, "exactly at the cap is not truncated");
        assert_eq!(a["bytes"], ACCOUNT_DATA_CAP as u64);
    }

    #[test]
    fn a_failed_batch_leaves_rows_owed_not_half_written() {
        let j = tmp("failed");
        plant(&j, "sigOk", &["a1"]);
        plant(&j, "sigBad", &["a1", "poison"]);
        let mut stub = StubRpc {
            held: Map::from([
                ("a1".into(), (10, false, 0)),
                ("poison".into(), (10, false, 0)),
            ]),
            calls: 0,
            slices: vec![],
            fail_for: vec!["poison".into()],
        };
        let r = hydrate_account_data(&j, &mut stub, 10, 1).unwrap();
        assert_eq!(r.stored, 1, "the fully-answered row is written");
        assert_eq!(r.failed_batches, 1);
        assert!(!r.complete(), "an errored pass is incomplete");
        assert_eq!(r.left, 1, "the row with the failed address stays owed");
        let (_, envb) = j.row_envelope_for_test("sigBad").unwrap();
        let vb: Value = serde_json::from_str(&envb).unwrap();
        assert!(
            vb.get("account_data").is_none(),
            "the row with an unanswered address is not half-written"
        );

        // Second pass without the failure: exactly the owed row resolves.
        let mut ok = StubRpc {
            held: Map::from([
                ("a1".into(), (10, false, 0)),
                ("poison".into(), (10, false, 0)),
            ]),
            calls: 0,
            slices: vec![],
            fail_for: vec![],
        };
        let r2 = hydrate_account_data(&j, &mut ok, 10, 100).unwrap();
        assert_eq!(r2.stored, 1, "only the owed row remains to write");
        assert_eq!(r2.left, 0);
        assert!(r2.complete());
    }

    #[test]
    fn second_run_is_idempotent_and_costs_nothing() {
        let j = tmp("idem");
        plant(&j, "sig", &["a1"]);
        let held = Map::from([("a1".into(), (10, false, 0))]);
        let mut stub = StubRpc { held: held.clone(), calls: 0, slices: vec![], fail_for: vec![] };
        let first = hydrate_account_data(&j, &mut stub, 10, 100).unwrap();
        assert_eq!(first.stored, 1);
        let mut again = StubRpc { held, calls: 0, slices: vec![], fail_for: vec![] };
        let second = hydrate_account_data(&j, &mut again, 10, 100).unwrap();
        assert_eq!(second.considered, 0, "resolved rows are not owed again");
        assert_eq!(again.calls, 0, "idempotence costs zero calls");
        assert_eq!(second.left, 0);
    }

    #[test]
    fn rows_without_an_enumerable_account_list_are_never_owed() {
        let j = tmp("nolist");
        // Envelope whose flags were unavailable: accounts is null.
        j.insert_for_test("sigNoList", 1, Some("[\"log\"]"), None).unwrap();
        let env = json!({
            "version": "legacy", "slot": 1, "program_id": "P", "err": Value::Null,
            "accounts_total": 5, "accounts_static": 5, "accounts_loaded": 0,
            "accounts_flags_source": "unavailable", "accounts": Value::Null,
            "instructions": [], "lamport_changes": [], "token_changes": [],
        });
        j.set_accounts("sigNoList", &env.to_string()).unwrap();
        assert_eq!(j.account_data_missing_count().unwrap(), 0);
        // A row with no envelope at all is not data-owed.
        j.insert_for_test("sigBare", 2, Some("[\"log\"]"), None).unwrap();
        assert_eq!(j.account_data_missing_count().unwrap(), 0);
    }

    #[test]
    fn set_account_data_is_idempotent_and_guarded() {
        let j = tmp("setguard");
        assert!(!j.set_account_data("never-seen", "{}").unwrap(), "unknown signature writes nothing");
        j.insert_for_test("sig", 1, Some("[\"log\"]"), None).unwrap();
        // No envelope yet: data must not be attachable to a row that has no
        // envelope to attach it to.
        assert!(!j.set_account_data("sig", "{}").unwrap());
        plant(&j, "sig", &["a1"]);
        let payload = json!({"source": "getMultipleAccounts", "data": {"a1": {"state": "held"}}, "missing": []});
        assert!(j.set_account_data("sig", &payload.to_string()).unwrap());
        let before = j.row_envelope_for_test("sig").unwrap().1;
        assert!(!j.set_account_data("sig", "{}").unwrap(), "second write is a no-op");
        assert_eq!(j.row_envelope_for_test("sig").unwrap().1, before, "envelope unchanged");
    }

    /// The debt counter must use the SAME predicate as the queue.
    #[test]
    fn debt_counter_matches_the_queue_predicate() {
        let j = tmp("debt");
        plant(&j, "sig1", &["a1", "a2"]);
        plant(&j, "sig2", &["a1"]);
        assert_eq!(j.account_data_missing_count().unwrap(), 2);
        let mut stub = StubRpc {
            held: Map::from([("a1".into(), (10, false, 0)), ("a2".into(), (20, false, 0))]),
            calls: 0,
            slices: vec![],
            fail_for: vec![],
        };
        let r = hydrate_account_data(&j, &mut stub, 10, 100).unwrap();
        assert_eq!(r.stored, 2);
        assert_eq!(j.account_data_missing_count().unwrap(), 0, "queue and counter agree");
        assert_eq!(j.counts().unwrap().account_data_held, 2);
        assert_eq!(j.counts().unwrap().account_data_missing, 0);
    }
}
