//! Payload hydration: an event is a signature plus what happened.
//!
//! The gap machinery (see `backfill.rs`) recovers *signatures* — a repaired
//! or backfilled row arrives from `getSignaturesForAddress`, which carries a
//! slot and an error but no `logs`. Measured 2026-10-28 on a live 60 s run
//! against mainnet: 13 rows filled by `repair`, all 13 stored with
//! `logs IS NULL`, while all 2,082 rows the socket delivered held payload.
//!
//! That asymmetry is a product defect, not a footnote: the rows the journal
//! had to *recover* are exactly the rows a consumer cannot read. A mailbox
//! that delivers "something happened at slot N" without saying what happened
//! forces the consumer back onto the RPC, which is the fragility reeflog
//! exists to remove.
//!
//! This module fetches the missing payload with `getTransaction` and records
//! the outcome honestly:
//!
//! Four states, kept apart because they make different promises.
//!
//! `held` — the chain returned `meta.logMessages`, stored as JSON.
//!
//! `unavailable` — the chain does not know the transaction (forked out or
//! pruned). Measured 2026-10-28: a well-formed signature that is not on
//! chain answers `result: null` with no error; the row keeps `logs IS NULL`
//! and says why. An empty array would claim "no logs" for a transaction
//! that no longer exists, which is a different statement.
//!
//! `invalid` — the endpoint refuses the key itself (-32602 Invalid param:
//! not a decodable signature). No transaction can ever exist under it, so it
//! is a known outcome too — but a *different* one from a fork-out: it
//! signals a corrupted row, and folding it into `unavailable` would hide
//! that. Never counted as a failure either, or one corrupted row would make
//! every pass exit 3 forever.
//!
//! `pending` — `logs IS NULL` and no outcome recorded. Unknown, not
//! known-empty. Pending rows are what make a hydration pass incomplete, and
//! incomplete passes exit non-zero (same honesty rule as `verify` and
//! `repair`).

use crate::accounts;
use crate::journal::Journal;
use crate::backfill::Fetcher;
use serde_json::{json, Value};

#[derive(Debug, Default, Clone)]
pub struct PayloadReport {
    /// Rows whose payload was still unknown when this pass started.
    pub considered: i64,
    /// Rows that received their payload from the chain.
    pub hydrated: i64,
    /// Rows that gained an account envelope alongside their logs. The
    /// envelope rides in the same `getTransaction` response, so this never
    /// costs an extra RPC call.
    pub accounts_stored: i64,
    /// Rows the chain does not know (forked out or pruned).
    pub unavailable: i64,
    /// Rows whose key the endpoint refuses as not-a-signature: a corrupted
    /// row, distinct from a fork-out.
    pub invalid: i64,
    /// Rows the RPC failed on. A failure leaves the row pending, never held.
    pub failed: i64,
    /// Rows still pending after this pass (its `--limit` was reached).
    pub pending_left: i64,
    pub error: Option<String>,
    pub hydrated_sample: Vec<String>,
    pub unavailable_sample: Vec<String>,
    pub invalid_sample: Vec<String>,
}

impl PayloadReport {
    /// Complete means: every row has a known outcome, nothing RPC-failed,
    /// and the pass was not cut short by its limit. A known-empty result
    /// (`unavailable`) is a *recorded* outcome and does not make the pass
    /// incomplete — the journal knows, which is the point.
    pub fn complete(&self) -> bool {
        self.error.is_none() && self.failed == 0 && self.pending_left == 0
    }
}

/// Fetch payloads for rows whose content is still unknown, up to `limit`.
/// Ordered by `seq` (delivery order) so the consumer's earliest rows are
/// filled first — a cursor consumer reads from the front of the journal.
pub fn hydrate_payloads<F: Fetcher>(
    j: &Journal,
    f: &mut F,
    limit: i64,
) -> Result<PayloadReport, String> {
    let mut r = PayloadReport::default();
    let rows = j.events_missing_payload(limit + 1)?; // +1: detect "more remain"
    if rows.is_empty() {
        return Ok(r);
    }
    let take = limit.max(0).min(rows.len() as i64) as usize;
    let batch = &rows[..take];
    r.considered = batch.len() as i64;

    for (seq, signature) in batch {
        let _ = seq;
        let result = match f.call(
            "getTransaction",
            json!([
                signature,
                {"encoding": "json", "maxSupportedTransactionVersion": 1, "commitment": "finalized"}
            ]),
        ) {
            Ok(v) => v,
            Err(e) => {
                if is_refused_key(&e) {
                    // The endpoint says this is not a decodable signature at
                    // all. Nothing can ever be fetched for it, so it is a
                    // recorded outcome — but `invalid`, not `unavailable`:
                    // a fork-out and a corrupted row must not look alike.
                    j.mark_payload_invalid(signature)?;
                    r.invalid += 1;
                    if r.invalid_sample.len() < 5 {
                        r.invalid_sample.push(signature.clone());
                    }
                    continue;
                }
                // The RPC failed: the row stays pending and this pass is
                // incomplete. Recording a failure as an outcome would turn a
                // rate limit into a permanent hole.
                r.failed += 1;
                r.error = Some(e);
                break;
            }
        };
        if result.is_null() {
            // Chain does not know this transaction: confirmed-then-forked-out
            // or pruned past history. This is the same class of transaction
            // that poisoned the old verify anchor (see backfill.rs).
            j.mark_payload_unavailable(signature)?;
            r.unavailable += 1;
            if r.unavailable_sample.len() < 5 {
                r.unavailable_sample.push(signature.clone());
            }
            continue;
        }
        let logs = result
            .pointer("/meta/logMessages")
            .cloned()
            .unwrap_or(Value::Null);
        let stored = match &logs {
            Value::Array(_) => serde_json::to_string(&logs).map_err(|e| e.to_string())?,
            // The transaction exists but carries no log array: record it as
            // unavailable rather than writing `[]`, for the same reason as
            // above — "no logs" and "no transaction" are different claims.
            _ => {
                j.mark_payload_unavailable(signature)?;
                r.unavailable += 1;
                if r.unavailable_sample.len() < 5 {
                    r.unavailable_sample.push(signature.clone());
                }
                continue;
            }
        };
        // `set_payload` only writes rows still missing payload, so a row the
        // socket already delivered is never overwritten.
        //
        // The account envelope comes out of THIS SAME response: the metadata
        // reeflog pays for already names every account the transaction
        // touched and how each balance moved. Keeping it costs no extra call;
        // dropping it (what reeflog did until now) threw the content away.
        let envelope = accounts::extract(&result).map(|e| e.value.to_string());
        if j.set_payload(signature, &stored, envelope.as_deref())? {
            r.hydrated += 1;
            if envelope.is_some() {
                r.accounts_stored += 1;
            }
            if r.hydrated_sample.len() < 5 {
                r.hydrated_sample.push(signature.clone());
            }
        }
    }

    r.pending_left = j.payload_pending_count()?;
    Ok(r)
}

/// Fill in the account envelope for rows whose payload is already held but
/// whose account data was never fetched.
///
/// Two kinds of rows exist: rows this pass (or `hydrate`) recovered from
/// `getTransaction` — those keep their envelope from the same response, free
/// — and rows the WEBSOCKET delivered. The socket's `logsNotification`
/// carries only logs: it has no balances, no account keys. So the socket
/// rows are the rows that cost a `getTransaction` to enrich, and
/// `hydrate --accounts` is the bounded pass that pays it.
///
/// The honesty rules are the same as for payload: a rate limit leaves the
/// row without an envelope (never an empty one) and makes the pass
/// incomplete; a transaction the chain does not know is recorded as such.
pub fn hydrate_accounts<F: Fetcher>(
    j: &Journal,
    f: &mut F,
    limit: i64,
) -> Result<AccountReport, String> {
    let mut r = AccountReport::default();
    let rows = j.events_missing_accounts(limit + 1)?;
    if rows.is_empty() {
        return Ok(r);
    }
    let take = limit.max(0).min(rows.len() as i64) as usize;
    let batch = &rows[..take];
    r.considered = batch.len() as i64;

    for (seq, signature) in batch {
        let _ = seq;
        let result = match f.call(
            "getTransaction",
            json!([
                signature,
                {"encoding": "json", "maxSupportedTransactionVersion": 1, "commitment": "finalized"}
            ]),
        ) {
            Ok(v) => v,
            Err(e) => {
                if is_refused_key(&e) {
                    j.mark_account_outcome(signature, "invalid")?;
                    r.invalid += 1;
                    if r.invalid_sample.len() < 5 {
                        r.invalid_sample.push(signature.clone());
                    }
                    continue;
                }
                r.failed += 1;
                r.error = Some(e);
                break;
            }
        };
        if result.is_null() {
            // The logs are already held — the socket delivered them — but the
            // transaction is gone from the chain now. The account data the
            // response would have carried is unreachable, not empty.
            // Recorded, not retried forever: an unrecoverable row must not
            // make every future pass exit 3 (the lesson payload_state taught).
            j.mark_account_outcome(signature, "unavailable")?;
            r.unavailable += 1;
            if r.unavailable_sample.len() < 5 {
                r.unavailable_sample.push(signature.clone());
            }
            continue;
        }
        let Some(envelope) = accounts::extract(&result) else {
            // A response without usable metadata: no envelope, no claim — and
            // a recorded reason, so the row is not "owed" forever.
            j.mark_account_outcome(signature, "no_metadata")?;
            r.no_metadata += 1;
            continue;
        };
        if j.set_accounts(signature, &envelope.value.to_string())? {
            r.stored += 1;
            if r.stored_sample.len() < 5 {
                r.stored_sample.push(signature.clone());
            }
        }
    }

    r.left = j.accounts_missing_count()?;
    Ok(r)
}

/// What an `hydrate --accounts` pass did, in the same shape the payload
/// pass reports: considered / stored / known-unreachable / failed / left.
#[derive(Debug, Default, Clone)]
pub struct AccountReport {
    pub considered: i64,
    pub stored: i64,
    pub unavailable: i64,
    pub invalid: i64,
    pub no_metadata: i64,
    pub failed: i64,
    pub left: i64,
    pub error: Option<String>,
    pub stored_sample: Vec<String>,
    pub unavailable_sample: Vec<String>,
    pub invalid_sample: Vec<String>,
}

impl AccountReport {
    /// Same rule as the payload pass: a recorded outcome is an outcome, a
    /// rate limit is not, and a pass cut short is not clean.
    pub fn complete(&self) -> bool {
        self.error.is_none() && self.failed == 0 && self.left == 0
    }
}

/// `HttpFetcher` formats a JSON-RPC error as
/// `getTransaction: rpc error {"code":-32602,"message":"Invalid param: Invalid"}`.
/// An endpoint that refuses the *key* will never answer for it, whatever it
/// costs in retries, so treating it as a transport failure would leave one
/// corrupted row making every pass — and every `start` that hydrates at
/// shutdown — exit 3 forever. Only -32602 is classified this way: a rate
/// limit stays a failure, and a request-shape bug on our side must never be
/// silently recorded as "the chain does not know it".
fn is_refused_key(err: &str) -> bool {
    err.contains("code\":-32602") || err.contains("code\": -32602")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::journal::Journal;

    struct FakeTxRpc {
        /// signature -> logMessages, or None for "chain does not know it".
        known: std::collections::HashMap<String, Option<Vec<String>>>,
        fail_after: Option<usize>,
        calls: usize,
        /// Errors for specific signatures, e.g. the endpoint refusing a key.
        errors: std::collections::HashMap<String, String>,
    }

    impl Fetcher for FakeTxRpc {
        fn call(&mut self, method: &str, params: Value) -> Result<Value, String> {
            assert_eq!(method, "getTransaction");
            let sig = params[0].as_str().unwrap().to_string();
            if let Some(e) = self.errors.get(&sig) {
                return Err(e.clone());
            }
            if let Some(n) = self.fail_after {
                if self.calls >= n {
                    return Err("getTransaction: http 429 rate limited".into());
                }
            }
            self.calls += 1;
            match self.known.get(&sig) {
                None => Ok(Value::Null),
                Some(None) => Ok(Value::Null),
                Some(Some(logs)) => Ok(json!({"meta": {"logMessages": logs}})),
            }
        }
    }

    fn tmp(tag: &str) -> Journal {
        use std::sync::atomic::{AtomicU64, Ordering};
        static N: AtomicU64 = AtomicU64::new(0);
        let n = N.fetch_add(1, Ordering::SeqCst);
        let p = std::env::temp_dir().join(format!(
            "reeflog-payload-{}-{}-{}.db",
            std::process::id(),
            tag,
            n
        ));
        let _ = std::fs::remove_file(&p);
        Journal::open(&p).expect("open journal")
    }

    #[test]
    fn hydration_fills_only_rows_missing_payload() {
        let j = tmp("fill");
        // Socket row: payload already held.
        j.append("sigWs", 10, None, Some(r#"["Program log: hi"]"#), "ws")
            .unwrap();
        // Repair row: signature only — the shape `repair_holes` writes.
        j.append("sigRepair", 11, None, None, "repair").unwrap();
        assert_eq!(j.payload_pending_count().unwrap(), 1);

        let mut rpc = FakeTxRpc {
            known: std::collections::HashMap::from([(
                "sigRepair".to_string(),
                Some(vec!["Program log: recovered".to_string()]),
            )]),
            fail_after: None,
            calls: 0,
            errors: std::collections::HashMap::new(),
        };
        let r = hydrate_payloads(&j, &mut rpc, 10).unwrap();
        assert_eq!(r.hydrated, 1);
        assert_eq!(r.unavailable, 0);
        assert!(r.complete(), "a full pass must be complete");
        assert_eq!(j.payload_pending_count().unwrap(), 0);

        let rows = j.events_after(0, 10).unwrap();
        let ws = rows.iter().find(|x| x.signature == "sigWs").unwrap();
        assert_eq!(
            ws.logs.as_deref(),
            Some(r#"["Program log: hi"]"#),
            "an already-held payload must not be overwritten"
        );
        let rep = rows.iter().find(|x| x.signature == "sigRepair").unwrap();
        assert_eq!(
            rep.logs.as_deref(),
            Some(r#"["Program log: recovered"]"#),
            "a repaired row must gain its payload"
        );
        assert_eq!(rpc.calls, 1, "held rows must not cost an RPC call");
    }

    #[test]
    fn an_unknown_transaction_is_recorded_as_unavailable_not_empty() {
        let j = tmp("unavail");
        j.append("forked", 10, None, None, "repair").unwrap();
        let mut rpc = FakeTxRpc {
            known: std::collections::HashMap::new(), // chain knows neither
            fail_after: None,
            calls: 0,
            errors: std::collections::HashMap::new(),
        };
        let r = hydrate_payloads(&j, &mut rpc, 10).unwrap();
        assert_eq!(r.unavailable, 1);
        assert_eq!(r.hydrated, 0);
        assert!(r.complete(), "a recorded outcome is a known outcome");
        assert_eq!(j.payload_pending_count().unwrap(), 0);

        let rows = j.events_after(0, 10).unwrap();
        assert!(
            rows[0].logs.is_none(),
            "a transaction the chain does not know must not claim `[]`"
        );
        assert_eq!(rows[0].payload_state.as_deref(), Some("unavailable"));
    }

    #[test]
    fn an_rpc_failure_leaves_the_row_pending_and_the_pass_incomplete() {
        let j = tmp("fail");
        j.append("sig1", 10, None, None, "repair").unwrap();
        let mut rpc = FakeTxRpc {
            known: std::collections::HashMap::new(),
            fail_after: Some(0), // fails on the first call
            calls: 0,
            errors: std::collections::HashMap::new(),
        };
        let r = hydrate_payloads(&j, &mut rpc, 10).unwrap();
        assert_eq!(r.failed, 1);
        assert!(!r.complete(), "a rate limit must never look like a clean pass");
        assert_eq!(j.payload_pending_count().unwrap(), 1, "the row stays pending");
        let rows = j.events_after(0, 10).unwrap();
        assert_eq!(
            rows[0].payload_state.as_deref(),
            None,
            "a failure records no outcome"
        );
    }

    #[test]
    fn hitting_the_limit_reports_the_rows_left_behind() {
        let j = tmp("limit");
        j.append("a", 10, None, None, "repair").unwrap();
        j.append("b", 11, None, None, "repair").unwrap();
        let mut rpc = FakeTxRpc {
            known: std::collections::HashMap::new(),
            fail_after: None,
            calls: 0,
            errors: std::collections::HashMap::new(),
        };
        let r = hydrate_payloads(&j, &mut rpc, 1).unwrap();
        assert_eq!(r.considered, 1);
        assert_eq!(r.unavailable, 1);
        assert_eq!(r.pending_left, 1, "the untouched row must still be pending");
        assert!(!r.complete(), "a cut-short pass is not a clean pass");
    }

    #[test]
    fn a_journal_with_nothing_to_do_is_complete_and_costs_nothing() {
        let j = tmp("idle");
        j.append("sigWs", 10, None, Some(r#"["x"]"#), "ws").unwrap();
        let mut rpc = FakeTxRpc {
            known: std::collections::HashMap::new(),
            fail_after: Some(0),
            calls: 0,
            errors: std::collections::HashMap::new(),
        };
        let r = hydrate_payloads(&j, &mut rpc, 10).unwrap();
        assert!(r.complete());
        assert_eq!(r.considered, 0);
        assert_eq!(rpc.calls, 0, "held payload must not trigger RPC");
    }

    #[test]
    fn a_key_the_endpoint_refuses_is_invalid_not_unavailable_and_not_a_failure() {
        // Measured 2026-10-28: a signature that is not decodable answers
        // -32602 "Invalid param: Invalid". Treating that as a transport
        // failure would leave one corrupted row making every pass exit 3
        // forever; treating it as a fork-out would hide a corrupted row.
        let j = tmp("invalid");
        j.append("garbage", 10, None, None, "repair").unwrap();
        let mut rpc = FakeTxRpc {
            known: std::collections::HashMap::new(),
            fail_after: None,
            calls: 0,
            errors: std::collections::HashMap::from([(
                "garbage".to_string(),
                "getTransaction: rpc error {\"code\":-32602,\"message\":\"Invalid param: Invalid\"}"
                    .to_string(),
            )]),
        };
        let r = hydrate_payloads(&j, &mut rpc, 10).unwrap();
        assert_eq!(r.invalid, 1);
        assert_eq!(r.unavailable, 0, "a corrupted key is not a fork-out");
        assert_eq!(r.failed, 0, "a refused key is not a transport failure");
        assert!(r.complete(), "a recorded outcome is a known outcome");
        assert_eq!(j.payload_pending_count().unwrap(), 0);

        let rows = j.events_after(0, 10).unwrap();
        assert!(rows[0].logs.is_none());
        assert_eq!(rows[0].payload_state.as_deref(), Some("invalid"));
        assert_eq!(j.counts().unwrap().payload_invalid, 1);
    }

    #[test]
    fn a_rate_limit_is_still_a_failure_and_never_an_outcome() {
        let j = tmp("ratelimit");
        j.append("sig1", 10, None, None, "repair").unwrap();
        let mut rpc = FakeTxRpc {
            known: std::collections::HashMap::new(),
            fail_after: Some(0),
            calls: 0,
            errors: std::collections::HashMap::new(),
        };
        let r = hydrate_payloads(&j, &mut rpc, 10).unwrap();
        assert_eq!(r.failed, 1);
        assert_eq!(r.invalid, 0, "a rate limit is not a refused key");
        assert!(!r.complete());
        assert_eq!(j.payload_pending_count().unwrap(), 1);
    }

    // -------------------------------------------------------------------
    // Account envelopes
    // -------------------------------------------------------------------

    /// A full `getTransaction`-shaped response: logs AND account metadata.
    /// This is what the RPC actually returns and what reeflog already paid
    /// for before the envelope was stored — see `src/accounts.rs`.
    fn full_tx_response() -> Value {
        json!({
            "slot": 10,
            "transaction": {
                "message": {
                    "accountKeys": ["Payer", "Vault", "Program"],
                    "instructions": [{"programIdIndex": 2}]
                }
            },
            "meta": {
                "logMessages": ["Program invoke"],
                "preBalances": [1000, 500, 0],
                "postBalances": [900, 600, 0],
                "preTokenBalances": [
                    {"accountIndex": 1, "mint": "M", "owner": "O", "uiTokenAmount": {"amount": "7"}}
                ],
                "postTokenBalances": [
                    {"accountIndex": 1, "mint": "M", "owner": "O", "uiTokenAmount": {"amount": "9"}}
                ]
            }
        })
    }

    struct FullRpc {
        calls: usize,
        fail_after: Option<usize>,
    }

    impl Fetcher for FullRpc {
        fn call(&mut self, method: &str, _params: Value) -> Result<Value, String> {
            assert_eq!(method, "getTransaction");
            if let Some(n) = self.fail_after {
                if self.calls >= n {
                    return Err("getTransaction: http 429 rate limited".into());
                }
            }
            self.calls += 1;
            Ok(full_tx_response())
        }
    }

    #[test]
    fn hydration_stores_the_account_envelope_from_the_same_paid_response() {
        let j = tmp("free-envelope");
        j.append("recovered", 10, None, None, "repair").unwrap();
        let mut rpc = FullRpc { calls: 0, fail_after: None };
        let r = hydrate_payloads(&j, &mut rpc, 10).unwrap();
        assert_eq!(r.hydrated, 1);
        assert_eq!(
            r.accounts_stored, 1,
            "the envelope rides in the same response — stored, not discarded"
        );
        assert_eq!(rpc.calls, 1, "one call paid for logs AND accounts");

        let rows = j.events_after(0, 10).unwrap();
        let env: Value = serde_json::from_str(rows[0].accounts.as_ref().expect("envelope held"))
            .expect("valid JSON");
        assert_eq!(env["lamport_changes"].as_array().unwrap().len(), 2);
        assert_eq!(env["lamport_changes"][0]["address"], "Payer");
        assert_eq!(env["lamport_changes"][0]["delta"], -100);
        assert_eq!(env["token_changes"][0]["delta"], "2");
    }

    #[test]
    fn account_pass_enriches_a_socket_row_without_touching_its_logs() {
        // The socket delivered the logs — but `logsNotification` carries no
        // balances. The envelope is owed by a separate paid fetch.
        let j = tmp("socket-row");
        j.append("from-socket", 10, None, Some(r#"["Program invoke"]"#), "ws")
            .unwrap();
        assert_eq!(j.accounts_missing_count().unwrap(), 1);

        let mut rpc = FullRpc { calls: 0, fail_after: None };
        let r = hydrate_accounts(&j, &mut rpc, 10).unwrap();
        assert_eq!(r.stored, 1);
        assert!(r.complete(), "a stored envelope is an outcome");
        assert_eq!(rpc.calls, 1);

        let rows = j.events_after(0, 10).unwrap();
        assert_eq!(
            rows[0].logs.as_deref(),
            Some(r#"["Program invoke"]"#),
            "the captured logs must not be overwritten by the enrichment pass"
        );
        assert!(rows[0].accounts.is_some());
        assert_eq!(j.accounts_missing_count().unwrap(), 0);
    }

    #[test]
    fn account_pass_is_idempotent_and_costs_nothing_the_second_time() {
        let j = tmp("idempotent-accts");
        j.append("row", 10, None, Some(r#"["x"]"#), "ws").unwrap();
        let mut rpc = FullRpc { calls: 0, fail_after: None };
        hydrate_accounts(&j, &mut rpc, 10).unwrap();
        let before = rpc.calls;
        let r = hydrate_accounts(&j, &mut rpc, 10).unwrap();
        assert_eq!(r.considered, 0, "a row already holding an envelope is not refetched");
        assert_eq!(rpc.calls, before, "the second pass must cost no RPC");
    }

    /// A journal written by an older binary: balances stored, instruction
    /// bytes never extracted. The envelope exists, every count-based query
    /// sees it, and a pass that only asked "is there an envelope?" would
    /// leave it half forever while reporting the journal complete.
    fn plant_pre_instruction_envelope(j: &Journal, signature: &str) {
        let old = serde_json::json!({
            "version": "legacy", "slot": 10, "program_id": "Program",
            "accounts_total": 3, "accounts_static": 3, "accounts_loaded": 0,
            "lamport_changes": [{"index": 0, "address": "Payer", "before": 1000, "after": 900, "delta": -100}],
            "token_changes": []
        });
        j.db
            .execute(
                "UPDATE events SET accounts=?2, account_state='held' WHERE signature=?1",
                rusqlite::params![signature, old.to_string()],
            )
            .unwrap();
    }

    #[test]
    fn an_envelope_from_an_older_binary_is_owed_again_not_quoted_back() {
        // The claim under test: a pre-instruction envelope is INCOMPLETE, so
        // it is selected for re-enrichment and overwritten by the newer
        // extractor — a mailbox must not mistake "stored earlier" for
        // "complete".
        let j = tmp("partial-envelope");
        j.append("legacy", 10, None, Some(r#"["x"]"#), "ws").unwrap();
        plant_pre_instruction_envelope(&j, "legacy");
        assert_eq!(
            j.accounts_missing_count().unwrap(),
            1,
            "a half envelope is still a debt, not an outcome"
        );
        let mut rpc = FullRpc { calls: 0, fail_after: None };
        let r = hydrate_accounts(&j, &mut rpc, 10).unwrap();
        assert_eq!(r.stored, 1, "the half envelope is re-fetched and replaced");
        assert!(r.complete());
        let rows = j.events_after(0, 10).unwrap();
        let env: Value = serde_json::from_str(rows[0].accounts.as_ref().unwrap()).unwrap();
        assert!(
            env["instructions"].is_array(),
            "the replacement carries the instruction bytes the old one lacked"
        );
        // And the journal is genuinely complete afterwards — not "complete
        // because the query stopped looking".
        assert_eq!(j.accounts_missing_count().unwrap(), 0);
        let mut rpc2 = FullRpc { calls: 0, fail_after: None };
        let r2 = hydrate_accounts(&j, &mut rpc2, 10).unwrap();
        assert_eq!(r2.considered, 0, "a complete envelope is not refetched");
    }

    #[test]
    fn an_older_binary_never_its_own_half_envelope() {
        // Symmetry check: set_accounts must also refuse to write a NEW
        // half-envelope — the guard is on the FIELD, not on who calls it.
        // Without this, a future extractor regression would be stored over a
        // good envelope and the journal would silently go backwards.
        let j = tmp("refuse-half-write");
        j.append("row", 10, None, Some(r#"["x"]"#), "ws").unwrap();
        let half = serde_json::json!({"lamport_changes": []});
        let wrote = j
            .set_accounts("row", &half.to_string())
            .expect("set_accounts");
        assert!(!wrote, "a response without instruction bytes must not overwrite a real envelope");
    }

    #[test]
    fn a_rate_limit_leaves_no_envelope_and_the_pass_incomplete() {
        // The rule that keeps the mailbox honest: an endpoint failure must
        // never look like "nothing changed".
        let j = tmp("acct-rate-limit");
        j.append("row", 10, None, Some(r#"["x"]"#), "ws").unwrap();
        let mut rpc = FullRpc { calls: 0, fail_after: Some(0) };
        let r = hydrate_accounts(&j, &mut rpc, 10).unwrap();
        assert_eq!(r.failed, 1);
        assert_eq!(r.stored, 0);
        assert!(!r.complete(), "a rate limit must never look like a clean pass");
        assert_eq!(j.accounts_missing_count().unwrap(), 1, "the debt stays");
    }

    #[test]
    fn a_row_without_logs_never_gains_an_account_envelope_alone() {
        // A recovered signature with no content: accounts without logs would
        // be half an event presented as a whole one.
        let j = tmp("no-logs-accts");
        j.append("recovered", 10, None, None, "repair").unwrap();
        let mut rpc = FullRpc { calls: 0, fail_after: None };
        let r = hydrate_accounts(&j, &mut rpc, 10).unwrap();
        assert_eq!(r.considered, 0, "rows without payload are the payload pass's job");
        assert_eq!(r.stored, 0);
        assert_eq!(rpc.calls, 0);
    }

    #[test]
    fn a_transaction_the_chain_no_longer_knows_is_unavailable_not_empty() {
        let j = tmp("acct-unavailable");
        j.append("row", 10, None, Some(r#"["x"]"#), "ws").unwrap();
        struct EmptyRpc;
        impl Fetcher for EmptyRpc {
            fn call(&mut self, _m: &str, _p: Value) -> Result<Value, String> {
                Ok(Value::Null)
            }
        }
        let r = hydrate_accounts(&j, &mut EmptyRpc, 10).unwrap();
        assert_eq!(r.unavailable, 1);
        assert_eq!(r.stored, 0, "no metadata, no envelope — never an empty claim");
        assert!(r.complete(), "a recorded outcome is a known outcome");
        let row = &j.events_after(0, 10).unwrap()[0];
        assert!(row.accounts.is_none());
        assert_eq!(
            row.account_state.as_deref(),
            Some("unavailable"),
            "known-unreachable, not pending forever"
        );
        // The next pass must not refetch it.
        let again = hydrate_accounts(&j, &mut EmptyRpc, 10).unwrap();
        assert_eq!(again.considered, 0, "a recorded outcome is never retried");
    }
}
