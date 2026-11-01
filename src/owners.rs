//! Which program OWNS each account a transaction touched — resolved in
//! batches, never guessed.
//!
//! Why this exists
//! ---------------
//! The envelope's other fields ride for free inside the `getTransaction`
//! response hydration already pays for; `owner` is not one of them — the
//! response never states the owner of an arbitrary account (only a changed
//! token account's `programId`, see src/accounts.rs). So ownership is a
//! separate, explicitly-budgeted pass, and the whole design of this module
//! follows from one live measurement (2026-11-01, `tools/probe-owners.mjs`,
//! 30 Raydium AMM v4 transactions, 406 distinct addresses):
//!
//!   - 1,455 address slots repeated down to 406 distinct addresses →
//!     **batching 100 per `getMultipleAccounts` call replaces 1,455 naive
//!     calls with 5 (291×)**. Per-row or per-address fetching would burn the
//!     same budget for the same facts, so this pass collects addresses
//!     across rows first, then fetches.
//!   - **63 of 406 addresses (15.5%) no longer exist** at fetch time —
//!     closed between the transaction and the owner fetch. An address the
//!     chain does not have is a FACT, not a failure: it lands in an explicit
//!     `missing` list. Writing `null`, omitting the entry, or — worst —
//!     guessing a plausible program would all be indistinguishable to a
//!     consumer from "not fetched yet". The probe's negative control pins
//!     this: an account the chain does not have must produce NO owner.
//!
//! What this module claims
//! -----------------------
//! For every row it writes, `account_owners` inside the envelope maps every
//! address the row enumerates to either an owning program (the account
//! exists; `owners`) or an entry in `missing` (the chain does not have it).
//! Every address is accounted for before the row is marked — a row whose
//! addresses span a failed batch stays owed instead of being half-written.
//!
//! What it does NOT claim: owners are read AT FETCH TIME, not at event time.
//! An account can change owner after the transaction (upgrade authority,
//! token-2022 mint migration, close-and-recreate). The envelope's own slot
//! and the `source` tag say when the facts were true; nothing here is
//! historical state. That boundary is the same one the README draws for the
//! envelope itself, and the remaining open item (account bytes, historical
//! authority) stays open.

use crate::backfill::Fetcher;
use crate::journal::Journal;
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};

/// One `hydrate --owners` pass, in the same shape the payload and account
/// passes report: considered / written / cost / known-missing / failed / left.
#[derive(Debug, Default, Clone)]
pub struct OwnersReport {
    /// Rows the envelope predicate put in front of this pass.
    pub considered: i64,
    /// Rows whose addresses were fully accounted for and whose envelope now
    /// carries `account_owners`.
    pub stored: i64,
    /// Addresses resolved to an owning program (this pass, all rows).
    pub addresses_owned: i64,
    /// Addresses the chain did not have at fetch time — recorded as facts.
    pub addresses_missing: i64,
    /// `getMultipleAccounts` calls actually made. The cost claim is this
    /// number against the address count, not a hope about batching.
    pub calls: i64,
    /// Batches the endpoint refused or broke on. Rows containing those
    /// addresses stay owed — never half-written.
    pub failed_batches: i64,
    pub error: Option<String>,
    /// Rows still owed after this pass (limit reached or a batch failed).
    pub left: i64,
    pub stored_sample: Vec<String>,
}

impl OwnersReport {
    /// Complete = no batch failed and nothing is left owed. A known-missing
    /// address is a RESOLVED fact and never makes a pass incomplete.
    pub fn complete(&self) -> bool {
        self.error.is_none() && self.failed_batches == 0 && self.left == 0
    }
}

/// Extract the address list a stored envelope enumerates.
///
/// Only `accounts[]` counts: the flag-verified canonical list. An envelope
/// whose flags were unavailable carries `accounts: null` and names no
/// enumerable set — such rows are excluded by the journal predicate, and
/// this function returns None as defense in depth so a predicate bug cannot
/// produce an owner map over a set this function never agreed to.
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

/// Resolve owners for rows the journal still owes, in batches of `batch`
/// addresses per `getMultipleAccounts` call.
///
/// Order of operations is the cost claim: addresses are collected ACROSS all
/// considered rows first, deduplicated, then fetched. Writing happens last
/// and only for rows whose every address came back with an answer.
pub fn hydrate_owners<F: Fetcher>(
    j: &Journal,
    f: &mut F,
    limit: i64,
    batch: usize,
) -> Result<OwnersReport, String> {
    let mut r = OwnersReport::default();
    let rows = j.events_missing_owners(limit + 1)?; // +1: detect "more remain"
    if rows.is_empty() {
        r.left = j.owners_missing_count()?;
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
        r.left = j.owners_missing_count()?;
        return Ok(r);
    }

    // Fetch: deduplicated addresses, in batches, all rows together.
    let addresses: Vec<String> = all.into_iter().collect();
    let batch = batch.max(1);
    let mut owned: BTreeMap<String, String> = BTreeMap::new();
    let mut gone: BTreeSet<String> = BTreeSet::new();
    let chunks: Vec<&[String]> = addresses.chunks(batch).collect();
    for chunk in chunks {
        let result = match f.call(
            "getMultipleAccounts",
            json!([
                chunk,
                {
                    "encoding": "base64",
                    // Owner-only: the `owner` field is returned regardless of
                    // the slice, and full data would multiply bytes per call
                    // for facts this pass does not store.
                    "dataSlice": {"offset": 0, "length": 1},
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
                gone.insert(addr.clone());
            } else if let Some(o) = entry.get("owner").and_then(|v| v.as_str()) {
                owned.insert(addr.clone(), o.to_string());
            } else {
                // Exists but no owner field: no answer, so no write for the
                // rows that need it. Leaves them owed instead of inventing.
                r.failed_batches += 1;
            }
        }
    }

    // Write: only rows where EVERY address has an answer (owned or gone).
    // A row containing an address from a failed batch stays owed — the
    // half-written-row defect the payload pass already paid for once.
    for (signature, addrs) in &per_row {
        let mut owners = serde_json::Map::new();
        let mut missing: Vec<String> = Vec::new();
        let mut complete = true;
        for a in addrs {
            if let Some(o) = owned.get(a) {
                owners.insert(a.clone(), Value::String(o.clone()));
            } else if gone.contains(a) {
                missing.push(a.clone());
            } else {
                complete = false;
                break;
            }
        }
        if !complete {
            continue;
        }
        let owners_json = json!({
            // "fetch time" is the honest tag: these owners were read when
            // this pass ran, not when the event happened. The envelope's
            // own slot and the row's first_seen bracket it.
            "source": "getMultipleAccounts",
            "owners": owners,
            "missing": missing,
        });
        if j.set_owners(signature, &owners_json.to_string())? {
            r.stored += 1;
            if r.stored_sample.len() < 5 {
                r.stored_sample.push(signature.clone());
            }
        }
    }
    r.addresses_owned = owned.len() as i64;
    r.addresses_missing = gone.len() as i64;
    r.left = j.owners_missing_count()?;
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
            "reeflog-owners-test-{}-{}-{}.db",
            std::process::id(),
            tag,
            n
        ));
        let _ = std::fs::remove_file(&p);
        Journal::open(&p).expect("open journal")
    }

    /// A row with logs (so set_payload/set_accounts guards pass) and an
    /// envelope whose `accounts` list enumerates the given addresses.
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

    /// Stub: answers getMultipleAccounts from a fixed table; `null` for
    /// addresses absent from the table (chain does not have them).
    struct StubRpc {
        table: Map<String, String>,
        pub calls: usize,
        pub fail_for: Vec<String>,
    }

    impl Fetcher for StubRpc {
        fn call(&mut self, method: &str, params: Value) -> Result<Value, String> {
            assert_eq!(method, "getMultipleAccounts", "owners pass may only batch-fetch");
            self.calls += 1;
            let chunk = params[0].as_array().expect("chunk array").clone();
            let addr_of = |v: &Value| v.as_str().unwrap_or_default().to_string();
            if let Some(bad) = chunk.iter().find(|a| {
                self.fail_for.iter().any(|f| f == &addr_of(a))
            }) {
                return Err(format!("stub refuses {bad}"));
            }
            let value: Vec<Value> = chunk
                .iter()
                .map(|a| match self.table.get(&addr_of(a)) {
                    Some(o) => json!({"owner": o, "lamports": 1, "executable": false, "data": ["", "base64"]}),
                    None => Value::Null,
                })
                .collect();
            Ok(json!({"context": {"slot": 9}, "value": value}))
        }
    }

    #[test]
    fn resolves_owned_and_gone_per_row_in_batches() {
        let j = tmp("resolve");
        plant(&j, "sigA", &["a1", "a2", "a3"]);
        plant(&j, "sigB", &["a3", "a4", "GONE"]);
        let mut stub = StubRpc {
            table: Map::from([
                ("a1".into(), "11111111111111111111111111111111".into()),
                ("a2".into(), "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA".into()),
                ("a3".into(), "675kPX9MHTjS2zt1qfr1NYHuzeLXfQM9H24wFSUt1Mp8".into()),
                ("a4".into(), "11111111111111111111111111111111".into()),
            ]),
            calls: 0,
            fail_for: vec![],
        };
        let r = hydrate_owners(&j, &mut stub, 10, 100).unwrap();
        assert_eq!(r.considered, 2);
        assert_eq!(r.stored, 2, "both rows fully accounted");
        // The cost claim: distinct addresses across BOTH rows in ONE call.
        assert_eq!(r.calls, 1, "batching collects across rows");
        assert_eq!(r.addresses_owned, 4);
        assert_eq!(r.addresses_missing, 1, "GONE is a recorded fact");
        assert_eq!(r.left, 0);
        assert!(r.complete());

        let (sig, env) = j.row_envelope_for_test("sigA").unwrap();
        assert_eq!(sig, "sigA");
        let v: Value = serde_json::from_str(&env).unwrap();
        let owners = &v["account_owners"];
        assert_eq!(owners["source"], "getMultipleAccounts");
        assert_eq!(owners["owners"]["a1"], "11111111111111111111111111111111");
        assert_eq!(owners["missing"], json!([]), "sigA has no gone address");

        let (_, envb) = j.row_envelope_for_test("sigB").unwrap();
        let vb: Value = serde_json::from_str(&envb).unwrap();
        assert_eq!(vb["account_owners"]["missing"], json!(["GONE"]));
        assert!(
            vb["account_owners"]["owners"].get("GONE").is_none(),
            "a gone address must carry NO owner"
        );
        // The original envelope fields survive the json_set merge.
        assert_eq!(vb["accounts_flags_source"], "header");
        assert!(vb["instructions"].is_array());
    }

    #[test]
    fn batching_honors_the_chunk_size() {
        let j = tmp("chunk");
        let addrs: Vec<String> = (0..7).map(|i| format!("addr{i}")).collect();
        let refs: Vec<&str> = addrs.iter().map(|s| s.as_str()).collect();
        plant(&j, "sigBig", &refs);
        let table: Map<String, String> = addrs
            .iter()
            .map(|a| (a.clone(), "11111111111111111111111111111111".into()))
            .collect();
        let mut stub = StubRpc { table, calls: 0, fail_for: vec![] };
        let r = hydrate_owners(&j, &mut stub, 10, 3).unwrap();
        assert_eq!(r.calls, 3, "7 addresses in batches of 3 = 3 calls");
        assert_eq!(r.stored, 1);
        assert!(r.complete());
    }

    #[test]
    fn a_failed_batch_leaves_rows_owed_not_half_written() {
        let j = tmp("failed");
        plant(&j, "sigOk", &["a1"]);
        plant(&j, "sigBad", &["a1", "poison"]);
        // Batch size 1: 'a1' (BTreeSet order resolves first) answers, then
        // the 'poison' batch fails. The fully-answered row must be written;
        // the row containing the failed address must stay owed — a failed
        // batch must not leak into unrelated rows, and an unanswered address
        // must never become a silent hole inside a written row.
        let mut stub = StubRpc {
            table: Map::from([
                ("a1".into(), "11111111111111111111111111111111".into()),
                ("poison".into(), "11111111111111111111111111111111".into()),
            ]),
            calls: 0,
            fail_for: vec!["poison".into()],
        };
        let r = hydrate_owners(&j, &mut stub, 10, 1).unwrap();
        assert_eq!(r.stored, 1, "the fully-answered row is written");
        assert_eq!(r.failed_batches, 1);
        assert!(!r.complete(), "an errored pass is incomplete");
        assert_eq!(r.left, 1, "the row with the failed address stays owed");
        let (_, env) = j.row_envelope_for_test("sigOk").unwrap();
        let v: Value = serde_json::from_str(&env).unwrap();
        assert!(v["account_owners"].is_object(), "the good row is written");
        let (_, envb) = j.row_envelope_for_test("sigBad").unwrap();
        let vb: Value = serde_json::from_str(&envb).unwrap();
        assert!(
            vb.get("account_owners").is_none(),
            "the row with an unanswered address is not half-written"
        );

        // Second pass without the failure: exactly the owed row resolves.
        let mut ok = StubRpc {
            table: Map::from([
                ("a1".into(), "11111111111111111111111111111111".into()),
                ("poison".into(), "TokenzQdBNbLqP5VEhdkAS6EPFLC1PHnBqCXEpPxuEb".into()),
            ]),
            calls: 0,
            fail_for: vec![],
        };
        let r2 = hydrate_owners(&j, &mut ok, 10, 100).unwrap();
        assert_eq!(r2.stored, 1, "only the owed row remains to write");
        assert_eq!(r2.left, 0);
        assert!(r2.complete());
    }

    #[test]
    fn second_run_is_idempotent_and_costs_nothing() {
        let j = tmp("idem");
        plant(&j, "sig", &["a1"]);
        let table = Map::from([("a1".into(), "11111111111111111111111111111111".into())]);
        let mut stub = StubRpc { table: table.clone(), calls: 0, fail_for: vec![] };
        let first = hydrate_owners(&j, &mut stub, 10, 100).unwrap();
        assert_eq!(first.stored, 1);
        let mut again = StubRpc { table, calls: 0, fail_for: vec![] };
        let second = hydrate_owners(&j, &mut again, 10, 100).unwrap();
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
        assert_eq!(j.owners_missing_count().unwrap(), 0);
        // A row with no envelope at all is accounts-owed, not owners-owed.
        j.insert_for_test("sigBare", 2, Some("[\"log\"]"), None).unwrap();
        assert_eq!(j.owners_missing_count().unwrap(), 0);
    }

    #[test]
    fn set_owners_is_idempotent_and_guarded() {
        let j = tmp("setguard");
        assert!(!j.set_owners("never-seen", "{}").unwrap(), "unknown signature writes nothing");
        j.insert_for_test("sig", 1, Some("[\"log\"]"), None).unwrap();
        // No envelope yet: owners must not be attachable to a row that has
        // no envelope to attach them to.
        assert!(!j.set_owners("sig", "{}").unwrap());
        plant(&j, "sig", &["a1"]);
        let payload = json!({"source": "getMultipleAccounts", "owners": {"a1": "X"}, "missing": []});
        assert!(j.set_owners("sig", &payload.to_string()).unwrap());
        let before = j.row_envelope_for_test("sig").unwrap().1;
        assert!(!j.set_owners("sig", "{}").unwrap(), "second write is a no-op");
        assert_eq!(j.row_envelope_for_test("sig").unwrap().1, before, "envelope unchanged");
    }

    /// End-to-end pass behavior on a journal that mixes owed and resolved
    /// rows, plus the debt counter the status line reports.
    #[test]
    fn debt_counter_matches_the_queue_predicate() {
        let j = tmp("debt");
        plant(&j, "sig1", &["a1", "a2"]);
        plant(&j, "sig2", &["a1"]);
        assert_eq!(j.owners_missing_count().unwrap(), 2);
        let mut stub = StubRpc {
            table: Map::from([("a1".into(), "X".into()), ("a2".into(), "Y".into())]),
            calls: 0,
            fail_for: vec![],
        };
        let r = hydrate_owners(&j, &mut stub, 10, 100).unwrap();
        assert_eq!(r.stored, 2);
        assert_eq!(j.owners_missing_count().unwrap(), 0, "queue and counter agree");
        assert_eq!(j.counts().unwrap().owners_held, 2);
        assert_eq!(j.counts().unwrap().owners_missing, 0);
    }
}
