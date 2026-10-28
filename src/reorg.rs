//! Reorg detection: an event the mailbox already promised as final, refused
//! by the chain.
//!
//! The rule this module exists to enforce is one line long: **one source
//! saying "unknown" is not a fact.** `getSignatureStatuses` can answer null
//! while `getTransaction` still serves the transaction, a rate limit looks
//! like silence, and a malformed key is a corrupted row rather than a fork.
//! Only two independent sources both refusing the same transaction makes a
//! revert — anything else is reported as what it actually is.
//!
//! States stay separate, matching the payload module's convention:
//!
//! * `Known`     — at least one source serves the transaction.
//! * `Refused`   — the source answers null: it does not know the transaction.
//! * `Invalid`   — the source refuses the *key* itself (-32602). No verdict
//!   about the chain can be drawn from it, and a corrupted row must not make
//!   every pass fail forever.
//! * `Failed`    — transport or rate limit: no verdict at all.

use crate::backfill::Fetcher;
use crate::journal::Journal;
use serde_json::{json, Value};

#[derive(Debug, Clone, PartialEq)]
pub enum Source {
    Known,
    Refused,
    Invalid,
    Failed(String),
}

#[derive(Debug, Clone, PartialEq)]
pub enum Verdict {
    /// At least one source serves the transaction: it is on chain.
    Known,
    /// Both independent sources refuse it: a reorg.
    Reverted,
    /// A source refuses the key: a corrupted row, never a fork.
    Invalid,
    /// A source failed: no verdict, the pass is incomplete.
    Incomplete,
}

/// Precedence: existence proven beats everything; a bad key beats a transport
/// failure (it is a recorded outcome, not an outage); two refusals make the
/// revert. Note that `Reverted` is only reachable when *neither* source is
/// Known, Invalid or Failed — i.e. when both genuinely answered "unknown".
pub fn classify(a: &Source, b: &Source) -> Verdict {
    use Source::*;
    if matches!(a, Known) || matches!(b, Known) {
        Verdict::Known
    } else if matches!(a, Invalid) || matches!(b, Invalid) {
        Verdict::Invalid
    } else if matches!(a, Failed(_)) || matches!(b, Failed(_)) {
        Verdict::Incomplete
    } else {
        Verdict::Reverted
    }
}

/// `HttpFetcher` formats JSON-RPC errors as
/// `getTransaction: rpc error {"code":-32602,"message":"..."}`. The same
/// classification payload.rs measures against the live endpoint: -32602 means
/// the endpoint refuses the key, which says nothing about the chain.
pub fn is_refused_key(err: &str) -> bool {
    err.contains("code\":-32602") || err.contains("code\": -32602")
}

fn from_call(res: &Result<Value, String>, method: &str) -> Source {
    match res {
        Err(e) if is_refused_key(e) => Source::Invalid,
        Err(e) => Source::Failed(format!("{method}: {e}")),
        Ok(v) => {
            if v.is_null() {
                Source::Refused
            } else if v.is_object() {
                Source::Known
            } else {
                Source::Failed(format!("{method}: unexpected response shape"))
            }
        }
    }
}

/// Status source for a batch of signatures. On success the per-signature
/// verdict lives in `result.value[]`; a `null` element means that source does
/// not know *that* transaction, which is what we want. A failed batch is
/// retried per signature so one malformed key cannot condemn the whole chunk,
/// and so a bad key is recorded as `Invalid` instead of failing the pass.
pub fn status_sources<F: Fetcher>(f: &mut F, sigs: &[&str]) -> Vec<Source> {
    let call = f.call(
        "getSignatureStatuses",
        json!([sigs, {"searchTransactionHistory": true}]),
    );
    match &call {
        Ok(v) => {
            let arr = v.get("value").and_then(|x| x.as_array());
            match arr {
                Some(a) if a.len() == sigs.len() => a
                    .iter()
                    .map(|el| if el.is_null() { Source::Refused } else { Source::Known })
                    .collect(),
                _ => vec![Source::Failed("getSignatureStatuses: bad response shape".into()); sigs.len()],
            }
        }
        Err(e) if is_refused_key(e) => {
            sigs.iter()
                .map(|s| {
                    from_call(
                        &f.call(
                            "getSignatureStatuses",
                            json!([[s], {"searchTransactionHistory": true}]),
                        ),
                        "getSignatureStatuses",
                    )
                })
                .collect()
        }
        Err(e) => {
            let msg = format!("getSignatureStatuses: {e}");
            vec![Source::Failed(msg); sigs.len()]
        }
    }
}

/// Which sealed events a check may still find reverted, newest first: a fork
/// can only unmake an event close to the tip, and that is where a consumer's
/// exposure is. Rows already recorded as reorgs are excluded so a re-run
/// never re-reports one.
pub fn candidates(j: &Journal, limit: i64) -> Result<Vec<(String, i64)>, String> {
    j.sealed_for_reorg_check(limit)
}

#[derive(Debug, Default, Clone)]
pub struct ReorgReport {
    /// Rows the check looked at.
    pub considered: i64,
    /// Both sources (or one) still serve them.
    pub intact: i64,
    /// Two independent sources refused them: recorded, delivered.
    pub reverted: i64,
    /// A source refused the key: a corrupted row, no chain verdict.
    pub invalid: i64,
    /// A source failed: undecided, and the pass is incomplete.
    pub incomplete: i64,
    /// True when the limit cut the check short: more sealed rows remain.
    pub more: bool,
    pub error: Option<String>,
    pub reverted_sample: Vec<String>,
}

impl ReorgReport {
    /// A pass is complete only when nothing is undecided and nothing failed.
    /// `invalid` does not block it: a corrupted row is a known outcome, not an
    /// unfinished check — the same reasoning payload.rs measures for `hydrate`.
    pub fn complete(&self) -> bool {
        self.error.is_none() && self.incomplete == 0
    }
}

/// Re-check sealed events against two independent sources and record the
/// reverts. Batched: one `getSignatureStatuses` call covers 200 rows, and
/// `getTransaction` is only spent on rows the first source already refused —
/// on a healthy chain that is zero extra calls per pass.
pub fn check_sealed<F: Fetcher>(j: &Journal, f: &mut F, limit: i64) -> Result<ReorgReport, String> {
    let rows = candidates(j, limit)?;
    let mut r = ReorgReport {
        more: rows.len() as i64 >= limit && limit > 0,
        ..Default::default()
    };
    if rows.is_empty() {
        return Ok(r);
    }

    for chunk in rows.chunks(200) {
        let sigs: Vec<&str> = chunk.iter().map(|(s, _)| s.as_str()).collect();
        let statuses = status_sources(f, &sigs);

        for ((sig, slot), st) in chunk.iter().zip(statuses.iter()) {
            r.considered += 1;
            let tx_source = if *st == Source::Known {
                // The status source serves it: existence proven, no second
                // call needed.
                Source::Known
            } else {
                let res = f.call(
                    "getTransaction",
                    json!([sig, {"encoding": "json", "maxSupportedTransactionVersion": 1, "commitment": "finalized"}]),
                );
                from_call(&res, "getTransaction")
            };

            match classify(st, &tx_source) {
                Verdict::Known => r.intact += 1,
                Verdict::Reverted => {
                    let detail = format!(
                        "getSignatureStatuses={} getTransaction={} at check",
                        name(st),
                        name(&tx_source)
                    );
                    if j.mark_reverted(sig, *slot, name(st), name(&tx_source), &detail)? {
                        r.reverted += 1;
                        if r.reverted_sample.len() < 5 {
                            r.reverted_sample.push(sig.clone());
                        }
                    }
                }
                Verdict::Invalid => r.invalid += 1,
                Verdict::Incomplete => {
                    r.incomplete += 1;
                    let why = match (st, &tx_source) {
                        (Source::Failed(e), _) => e.clone(),
                        (_, Source::Failed(e)) => e.clone(),
                        _ => "no source produced a verdict".into(),
                    };
                    if r.error.is_none() {
                        r.error = Some(why);
                    }
                }
            }
        }
    }
    Ok(r)
}

fn name(s: &Source) -> &'static str {
    match s {
        Source::Known => "known",
        Source::Refused => "refused",
        Source::Invalid => "invalid_key",
        Source::Failed(_) => "failed",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::journal::Journal;
    use std::collections::HashMap;

    fn tmp(tag: &str) -> Journal {
        use std::sync::atomic::{AtomicU64, Ordering};
        static N: AtomicU64 = AtomicU64::new(0);
        let n = N.fetch_add(1, Ordering::SeqCst);
        let p = std::env::temp_dir().join(format!(
            "reeflog-reorg-{}-{}-{}.db",
            std::process::id(),
            tag,
            n
        ));
        let _ = std::fs::remove_file(&p);
        Journal::open(&p).expect("open journal")
    }

    /// Chain model: per signature, what each source answers, plus optional
    /// failure injection. Deliberately able to make the two sources disagree,
    /// because that is the case a single-source detector gets wrong.
    #[derive(Default)]
    struct FakeChain {
        status: HashMap<String, &'static str>,
        tx: HashMap<String, &'static str>,
        status_fail: HashMap<String, &'static str>,
        calls: HashMap<String, usize>,
    }

    impl FakeChain {
        fn set(mut self, sig: &str, status: &'static str, tx: &'static str) -> Self {
            self.status.insert(sig.into(), status);
            self.tx.insert(sig.into(), tx);
            self
        }
        fn fail_status(mut self, sig: &str, err: &'static str) -> Self {
            self.status_fail.insert(sig.into(), err);
            self
        }
    }

    impl Fetcher for FakeChain {
        fn call(&mut self, method: &str, params: Value) -> Result<Value, String> {
            match method {
                "getSignatureStatuses" => {
                    *self.calls.entry("status".into()).or_insert(0) += 1;
                    let sigs: Vec<String> = params[0]
                        .as_array()
                        .expect("signature array")
                        .iter()
                        .map(|v| v.as_str().unwrap().to_string())
                        .collect();
                    // The batch shape holds the array-of-signatures form.
                    let sigs = if sigs.len() == 1 && sigs[0].len() > 1000 {
                        unreachable!("not used")
                    } else {
                        sigs
                    };
                    let els: Vec<Value> = sigs
                        .iter()
                        .map(|s| match self.status_fail.get(s) {
                            // A failed batch surfaces as an error, and the
                            // per-signature retry below recovers the good rows.
                            Some(_) => Value::Null,
                            None => match self.status.get(s).copied() {
                                Some("known") => json!({"confirmationStatus":"finalized"}),
                                _ => Value::Null,
                            },
                        })
                        .collect();
                    Ok(json!({"context":{"slot":1},"value":els}))
                }
                "getTransaction" => {
                    *self.calls.entry("tx".into()).or_insert(0) += 1;
                    let sig = params[0].as_str().unwrap();
                    if let Some(e) = self.status_fail.get(sig).copied() {
                        // Same key refusal surfaces on both sources.
                        return Err(format!(
                            "getTransaction: rpc error {{\"code\":-32602,\"message\":\"{e}\"}}"
                        ));
                    }
                    match self.tx.get(sig).copied() {
                        Some("known") => Ok(json!({"slot": 7, "meta": {"logMessages": []}})),
                        _ => Ok(Value::Null),
                    }
                }
                other => panic!("unexpected method {other}"),
            }
        }
    }

    #[test]
    fn classify_never_calls_one_refusal_a_revert() {
        // The single-source trap: one source silent, the other serving.
        assert_eq!(
            classify(&Source::Refused, &Source::Known),
            Verdict::Known
        );
        assert_eq!(
            classify(&Source::Known, &Source::Refused),
            Verdict::Known
        );
        // A rate limit is not a fork.
        assert_eq!(
            classify(&Source::Refused, &Source::Failed("429".into())),
            Verdict::Incomplete
        );
        // A corrupted key is not a fork either.
        assert_eq!(
            classify(&Source::Refused, &Source::Invalid),
            Verdict::Invalid
        );
        // Only two refusals make a revert.
        assert_eq!(
            classify(&Source::Refused, &Source::Refused),
            Verdict::Reverted
        );
    }

    #[test]
    fn two_refusals_record_a_revert_and_a_disagreeing_source_does_not() {
        let j = tmp("mixed");
        // A: sealed, both sources refuse -> reverted.
        // B: sealed, status refuses but the transaction is still served -> intact.
        // C: sealed, both serve -> intact.
        for (sig, slot) in [("sigA", 10i64), ("sigB", 11), ("sigC", 12)] {
            j.append(sig, slot, None, None, "ws").unwrap();
            j.seal(sig, "finalized", None).unwrap();
        }
        let mut chain = FakeChain::default()
            .set("sigA", "refused", "refused")
            .set("sigB", "refused", "known")
            .set("sigC", "known", "known");

        let r = check_sealed(&j, &mut chain, 100).unwrap();
        assert_eq!(r.considered, 3, "all three sealed rows were checked");
        assert_eq!(r.reverted, 1, "only the double refusal is a revert");
        assert_eq!(r.intact, 2, "a single refusal and a known row stay intact");
        assert_eq!(r.invalid, 0);
        assert_eq!(r.incomplete, 0);
        assert!(r.complete());
        assert_eq!(r.reverted_sample, vec!["sigA".to_string()]);
        assert_eq!(j.reorg_count().unwrap(), 1);
        assert_eq!(
            j.seal_status_of("sigA").unwrap().as_deref(),
            Some("reverted")
        );
        assert_eq!(
            j.seal_status_of("sigB").unwrap().as_deref(),
            Some("finalized"),
            "a disagreeing source must not change the seal"
        );
        // No second call for rows the first source already served: sigC was
        // known from the batched status call, so only the two refused rows
        // (sigA, sigB) cost a getTransaction. On a healthy chain where the
        // status source serves every row, the whole check is one call.
        assert_eq!(
            chain.calls.get("tx"),
            Some(&2),
            "getTransaction is spent only on rows the status source refused"
        );
        assert_eq!(
            chain.calls.get("status"),
            Some(&1),
            "the status source is queried as one batch, not per row"
        );
    }

    #[test]
    fn a_failed_source_leaves_the_row_undecided_and_fails_the_pass() {
        let j = tmp("failed");
        j.append("sigOK", 10, None, None, "ws").unwrap();
        j.seal("sigOK", "finalized", None).unwrap();
        j.append("sigBad", 11, None, None, "ws").unwrap();
        j.seal("sigBad", "finalized", None).unwrap();

        let mut chain = FakeChain::default()
            .set("sigOK", "known", "known")
            .fail_status("sigBad", "Invalid param: Invalid");
        // The batch carries a bad key: the isolation retry classifies sigBad
        // as Invalid rather than failing the whole chunk.
        let r = check_sealed(&j, &mut chain, 100).unwrap();
        assert_eq!(r.invalid, 1, "a bad key is a recorded outcome");
        assert_eq!(r.incomplete, 0, "it must not fail the pass forever");
        assert!(r.complete(), "known outcome != unfinished check");
        assert_eq!(
            j.seal_status_of("sigBad").unwrap().as_deref(),
            Some("finalized"),
            "an invalid key never becomes a reorg"
        );
    }

    #[test]
    fn a_transport_failure_is_incomplete_not_a_revert() {
        let j = tmp("transport");
        j.append("sigT", 10, None, None, "ws").unwrap();
        j.seal("sigT", "finalized", None).unwrap();

        struct Broken;
        impl Fetcher for Broken {
            fn call(&mut self, _m: &str, _p: Value) -> Result<Value, String> {
                Err("getSignatureStatuses: http 429 rate limited".into())
            }
        }
        let r = check_sealed(&j, &mut Broken, 100).unwrap();
        assert_eq!(r.incomplete, 1, "no verdict was reached");
        assert_eq!(r.reverted, 0, "a rate limit must never record a revert");
        assert!(!r.complete(), "an unfinished pass must not claim completion");
        assert!(r.error.as_deref().unwrap().contains("429"));
        assert_eq!(j.reorg_count().unwrap(), 0);
        assert_eq!(
            j.seal_status_of("sigT").unwrap().as_deref(),
            Some("finalized")
        );
    }

    #[test]
    fn reverts_are_recorded_once_and_delivered_in_order() {
        let j = tmp("deliver");
        j.append("sigR", 10, None, None, "ws").unwrap();
        j.seal("sigR", "confirmed", None).unwrap();
        let mut chain = FakeChain::default().set("sigR", "refused", "refused");

        let first = check_sealed(&j, &mut chain, 100).unwrap();
        assert_eq!(first.reverted, 1);
        // A re-run must not re-report it: the row left the candidate set.
        let second = check_sealed(&j, &mut chain, 100).unwrap();
        assert_eq!(second.considered, 0, "recorded reorgs leave the candidate set");
        assert_eq!(second.reverted, 0);
        assert_eq!(j.reorg_count().unwrap(), 1);

        let rows = j.reorgs_after(0, 100).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].signature, "sigR");
        assert_eq!(rows[0].slot, 10);
        assert!(j.reorgs_after(rows[0].id, 100).unwrap().is_empty(), "cursor is strict");
    }

    #[test]
    fn unsealed_rows_are_not_checked_because_nothing_was_promised() {
        let j = tmp("unsealed");
        j.append("sigU", 10, None, None, "ws").unwrap();
        let mut chain = FakeChain::default().set("sigU", "refused", "refused");
        let r = check_sealed(&j, &mut chain, 100).unwrap();
        assert_eq!(r.considered, 0, "an unsealed row was never promised final");
        assert_eq!(r.reverted, 0);
    }
}
