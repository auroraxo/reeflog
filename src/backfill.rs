//! Gap handling: catch the journal up on signatures it missed, and prove
//! whether anything is missing.
//!
//! Measured 2026-10-27 against api.mainnet-beta.solana.com with
//! getSignaturesForAddress (anchor = 4th of 10 tip signatures):
//!
//!   - `before=<sig>` returns signatures **older** than the anchor. This is
//!     the backwards-paging primitive.
//!   - `until=<sig>` / `since=<sig>` return signatures **newer** than the
//!     anchor. Passing the newest signature we hold as `since` yields an
//!     empty window — a backfill that reports no loss while filling nothing.
//!   - `before` + `until` together are rejected (-32020 "Transaction not
//!     found") when they describe an inverted window, so a single parameter
//!     cannot express "between X and Y". Paging must use `before` alone.
//!
//! Two different gaps exist and need different tools:
//!
//!   1. **Tail gap** — events after the newest signature the journal holds
//!      (a crash, a disconnect, a restart). Fixed by walking back from the
//!      chain tip with `before` until we reach the boundary signature. This
//!      is bounded and cheap.
//!   2. **Deep hole** — events missing from the middle of the journal's
//!      history. Detected by comparing the journal against the chain over a
//!      bounded window. Filling it costs one RPC page per 500 signatures,
//!      and public endpoints rate-limit (observed: HTTP 429), so v0 detects
//!      and *reports* deep holes instead of pretending to repair them.

use crate::journal::Journal;
use serde_json::{json, Value};

pub trait Fetcher {
    fn call(&mut self, method: &str, params: Value) -> Result<Value, String>;
}

pub struct HttpFetcher {
    pub url: String,
}

impl HttpFetcher {
    pub fn new(url: String) -> Self {
        Self { url }
    }
}

impl Fetcher for HttpFetcher {
    fn call(&mut self, method: &str, params: Value) -> Result<Value, String> {
        let body = json!({"jsonrpc":"2.0","id":1,"method":method,"params":params}).to_string();
        // Public endpoints rate-limit (observed live: HTTP 429 mid-scan).
        // Back off and retry instead of aborting the walk and reporting a
        // gap as closed when it is not.
        let mut last_err = String::from("unreachable");
        for attempt in 0..4 {
            if attempt > 0 {
                std::thread::sleep(std::time::Duration::from_millis(600 * attempt));
            }
            let mut resp = match ureq::post(&self.url)
                .header("content-type", "application/json")
                .send(body.as_str())
            {
                Ok(r) => r,
                Err(ureq::Error::StatusCode(429)) => {
                    last_err = format!("{method}: http 429 rate limited");
                    continue;
                }
                Err(ureq::Error::StatusCode(c)) => {
                    return Err(format!("{method}: http status {c}"));
                }
                Err(e) => return Err(format!("{method}: {e}")),
            };
            let v: Value = resp
                .body_mut()
                .read_to_string()
                .ok()
                .and_then(|s| serde_json::from_str(&s).ok())
                .ok_or_else(|| format!("{method}: bad rpc response"))?;
            if let Some(err) = v.get("error") {
                if !err.is_null() {
                    return Err(format!("{method}: rpc error {err}"));
                }
            }
            return Ok(v.get("result").cloned().unwrap_or(Value::Null));
        }
        Err(last_err)
    }
}

#[derive(Debug, Default, Clone)]
pub struct Report {
    /// Signatures the RPC returned in the scanned window.
    pub found: i64,
    /// Of those, how many were new rows in the journal.
    pub inserted: i64,
    /// How many the RPC returned that we already had (dedup working).
    pub duplicates: i64,
    /// Pages walked before stopping.
    pub pages: i64,
    /// True when we stopped before reaching the boundary.
    pub truncated: bool,
    /// Signatures the chain has that the journal does not.
    pub missing: Vec<String>,
    /// Set when the walk aborted early (rate limit, RPC error). A report
    /// with an error is never proof that the gap was filled.
    pub error: Option<String>,
}

/// Fill the journal's **tail gap**: everything on chain newer than the
/// newest signature the journal already holds.
///
/// Walks backwards from the chain tip with `before` and stops at the
/// boundary signature. Bounded by `max_pages`; if the boundary is not
/// reached the result is marked truncated and the caller must keep the gap
/// open — an unfilled gap must never look filled.
pub fn backfill_tail<F: Fetcher>(
    j: &Journal,
    f: &mut F,
    program: &str,
    max_pages: usize,
    page_size: usize,
) -> Result<Report, String> {
    let mut report = Report::default();
    let Some(boundary) = j.newest_signature()? else {
        return Ok(report); // empty journal: no boundary, no tail gap
    };

    let mut before: Option<String> = None;
    loop {
        if report.pages >= max_pages as i64 {
            report.truncated = true;
            break;
        }
        let mut params = json!([program, {"limit": page_size, "commitment": "finalized"}]);
        if let Some(b) = &before {
            params[1]["before"] = Value::String(b.clone());
        }
        // A mid-walk failure must preserve what was already collected and
        // mark the walk truncated — never abort silently as if complete.
        let result = match f.call("getSignaturesForAddress", params) {
            Ok(v) => v,
            Err(e) => {
                report.error = Some(e);
                report.truncated = true;
                break;
            }
        };
        let rows = result.as_array().cloned().unwrap_or_default();
        if rows.is_empty() {
            break;
        }
        report.pages += 1;
        let mut reached_boundary = false;
        for r in &rows {
            let sig = r
                .get("signature")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            if sig.is_empty() {
                continue;
            }
            if sig == boundary {
                reached_boundary = true;
                break;
            }
            let slot = r.get("slot").and_then(|v| v.as_i64()).unwrap_or(0);
            let err = match r.get("err") {
                None | Some(Value::Null) => None,
                Some(e) => Some(e.to_string()),
            };
            let (_, is_new) = j.append(&sig, slot, err.as_deref(), None, "backfill")?;
            if is_new {
                report.inserted += 1;
            } else {
                report.duplicates += 1;
            }
            report.found += 1;
        }
        if reached_boundary || rows.len() < page_size {
            break;
        }
        let Some(next) = rows
            .last()
            .and_then(|r| r.get("signature"))
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
        else {
            break;
        };
        if before.as_deref() == Some(next.as_str()) {
            break; // no forward progress
        }
        before = Some(next);
    }
    Ok(report)
}

/// Verify the journal against the chain over the range the journal claims to
/// cover — `[oldest_slot, newest_slot]` — and report every signature in that
/// range the journal does not have.
///
/// Two bounds matter:
///   - Below `oldest_slot` is out of scope: the journal never claimed it.
///     Scanning past it would flag all of history as "missing".
///   - Above `newest_slot` is the tail: events after capture stopped. They
///     are newer than what the journal claims, not holes in it.
///
/// This is the honest hole detector: it says exactly how far back it looked
/// and lists what is missing instead of claiming the journal is complete.
pub fn verify_recent<F: Fetcher>(
    j: &Journal,
    f: &mut F,
    program: &str,
    max_pages: usize,
    page_size: usize,
) -> Result<Report, String> {
    let mut report = Report::default();
    let held = j.all_signatures()?;
    let (Some(floor), Some(ceil)) = (j.oldest_slot()?, j.newest_slot()?) else {
        return Ok(report); // empty journal: nothing claimed, nothing missing
    };
    // Start just below the journal's newest event rather than at the chain
    // tip: everything at or above `ceil` is newer than the journal claims
    // and paging down to it from the tip would burn pages on non-holes.
    let start = j.newest_signature()?;
    let mut before: Option<String> = None;
    loop {
        if before.is_none() {
            before = start.clone();
            if before.is_none() {
                break;
            }
        }
        if report.pages >= max_pages as i64 {
            report.truncated = true;
            break;
        }
        let mut params = json!([program, {"limit": page_size, "commitment": "finalized"}]);
        if let Some(b) = &before {
            params[1]["before"] = Value::String(b.clone());
        }
        let result = match f.call("getSignaturesForAddress", params) {
            Ok(v) => v,
            Err(e) => {
                report.error = Some(e);
                report.truncated = true;
                break;
            }
        };
        let rows = result.as_array().cloned().unwrap_or_default();
        if rows.is_empty() {
            break;
        }
        report.pages += 1;
        let mut past_floor = false;
        for r in &rows {
            let sig = r
                .get("signature")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            if sig.is_empty() {
                continue;
            }
            let slot = r.get("slot").and_then(|v| v.as_i64()).unwrap_or(0);
            if slot < floor {
                // Older than anything the journal claims to cover.
                past_floor = true;
                break;
            }
            if slot > ceil {
                continue; // newer than the journal's window: not a hole
            }
            report.found += 1;
            if !held.contains(&sig) {
                report.missing.push(sig);
            }
        }
        if past_floor || rows.len() < page_size {
            break;
        }
        let Some(next) = rows
            .last()
            .and_then(|r| r.get("signature"))
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
        else {
            break;
        };
        if before.as_deref() == Some(next.as_str()) {
            break;
        }
        before = Some(next);
    }
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::journal::Journal;

    /// Fake RPC mimicking the **measured** getSignaturesForAddress contract:
    /// `before` = stop boundary; walk from the tip backwards and return
    /// signatures strictly older than `before`. `until`/`since` return newer
    /// signatures and are not used for paging.
    struct FakeRpc {
        chain: Vec<(String, i64)>, // newest-first, like the real endpoint
        calls: usize,
    }

    impl Fetcher for FakeRpc {
        fn call(&mut self, method: &str, params: Value) -> Result<Value, String> {
            assert_eq!(method, "getSignaturesForAddress");
            self.calls += 1;
            let limit = params[1]["limit"].as_u64().unwrap_or(100) as usize;
            let before = params[1].get("before").and_then(|v| v.as_str());
            let mut start = 0usize;
            if let Some(b) = before {
                start = self
                    .chain
                    .iter()
                    .position(|(sig, _)| sig == b)
                    .map(|i| i + 1)
                    .unwrap_or(self.chain.len());
            }
            let mut out = Vec::new();
            for i in start..self.chain.len() {
                let (sig, slot) = &self.chain[i];
                out.push(json!({"signature": sig, "slot": slot, "err": Value::Null}));
                if out.len() >= limit {
                    break;
                }
            }
            Ok(Value::Array(out))
        }
    }

    fn tmp(tag: &str) -> Journal {
        use std::sync::atomic::{AtomicU64, Ordering};
        static N: AtomicU64 = AtomicU64::new(0);
        let n = N.fetch_add(1, Ordering::SeqCst);
        let p = std::env::temp_dir().join(format!(
            "reeflog-bf-{}-{}-{}.db",
            std::process::id(),
            tag,
            n
        ));
        let _ = std::fs::remove_file(&p);
        Journal::open(&p).expect("open journal")
    }

    fn chain(n: i64) -> Vec<(String, i64)> {
        (1..=n).rev().map(|i| (format!("sig{i}"), i)).collect()
    }

    /// Fake RPC that pages backwards like the real endpoint and fails after
    /// N successful pages, to simulate a rate limit aborting a walk mid-way.
    struct FlakyRpc {
        chain: Vec<(String, i64)>,
        calls: usize,
        fail_after: usize,
    }

    impl Fetcher for FlakyRpc {
        fn call(&mut self, _method: &str, params: Value) -> Result<Value, String> {
            self.calls += 1;
            if self.calls > self.fail_after {
                return Err("getSignaturesForAddress: http 429 rate limited".into());
            }
            let limit = params[1]["limit"].as_u64().unwrap_or(100) as usize;
            let before = params[1].get("before").and_then(|v| v.as_str());
            let mut start = 0usize;
            if let Some(b) = before {
                start = self
                    .chain
                    .iter()
                    .position(|(sig, _)| sig == b)
                    .map(|i| i + 1)
                    .unwrap_or(self.chain.len());
            }
            let out: Vec<Value> = self.chain[start..]
                .iter()
                .take(limit)
                .map(|(sig, slot)| json!({"signature": sig, "slot": slot, "err": Value::Null}))
                .collect();
            Ok(Value::Array(out))
        }
    }

    #[test]
    fn a_mid_walk_failure_keeps_the_partial_result_and_marks_it_truncated() {
        let j = tmp("flaky");
        j.append("sig1", 1, None, None, "ws").unwrap();
        let mut rpc = FlakyRpc {
            chain: chain(10),
            calls: 0,
            fail_after: 1, // one page succeeds, then the endpoint rate-limits
        };
        let r = backfill_tail(&j, &mut rpc, "prog", 20, 3).unwrap();
        assert!(r.error.is_some(), "the failure must be reported");
        assert!(r.truncated, "a partially filled gap is not a closed gap");
        assert!(r.inserted > 0, "work done before the failure is kept");
    }

    #[test]
    fn a_fully_failed_walk_inserts_nothing_and_stays_open() {
        let j = tmp("dead");
        j.append("sig1", 1, None, None, "ws").unwrap();
        let mut rpc = FlakyRpc {
            chain: chain(10),
            calls: 0,
            fail_after: 0,
        };
        let r = backfill_tail(&j, &mut rpc, "prog", 20, 3).unwrap();
        assert!(r.error.is_some());
        assert!(r.truncated);
        assert_eq!(r.inserted, 0);
    }

    #[test]
    fn empty_journal_backfills_nothing_and_does_not_call_rpc() {
        let j = tmp("empty");
        let mut rpc = FakeRpc { chain: chain(5), calls: 0 };
        let r = backfill_tail(&j, &mut rpc, "prog", 5, 100).unwrap();
        assert_eq!(r.inserted, 0);
        assert_eq!(rpc.calls, 0, "no boundary means no tail gap");
    }

    #[test]
    fn tail_gap_is_filled_up_to_the_boundary() {
        let j = tmp("tail");
        // Journal holds up to sig3; the chain has sig1..sig5.
        j.append("sig3", 3, None, None, "ws").unwrap();
        let mut rpc = FakeRpc { chain: chain(5), calls: 0 };
        let r = backfill_tail(&j, &mut rpc, "prog", 5, 100).unwrap();
        assert_eq!(r.inserted, 2, "sig5 and sig4 are newer than our boundary");
        assert!(!r.truncated);
        assert_eq!(j.counts().unwrap().total, 3);
        let newest = j.newest_signature().unwrap().unwrap();
        assert_eq!(newest, "sig5", "journal now reaches the tip");
    }

    #[test]
    fn tail_backfill_is_idempotent() {
        let j = tmp("idem");
        j.append("sig3", 3, None, None, "ws").unwrap();
        let mut rpc = FakeRpc { chain: chain(5), calls: 0 };
        let first = backfill_tail(&j, &mut rpc, "prog", 5, 100).unwrap();
        assert_eq!(first.inserted, 2);
        let mut rpc2 = FakeRpc { chain: chain(5), calls: 0 };
        let second = backfill_tail(&j, &mut rpc2, "prog", 5, 100).unwrap();
        // The journal already reaches the tip, so the first row IS the
        // boundary: nothing to scan, nothing to insert, no wasted work.
        assert_eq!(second.inserted, 0, "second pass inserts nothing");
        assert_eq!(second.found, 0, "second pass stops at the tip immediately");
        assert!(!second.truncated);
    }

    #[test]
    fn tail_backfill_reports_truncation_when_the_boundary_is_out_of_reach() {
        let j = tmp("trunc");
        j.append("sig1", 1, None, None, "ws").unwrap();
        let mut rpc = FakeRpc { chain: chain(100), calls: 0 };
        // One page of 10 can never reach sig1 from the tip.
        let r = backfill_tail(&j, &mut rpc, "prog", 1, 10).unwrap();
        assert!(r.truncated, "an unfilled gap must be reported as truncated");
    }

    #[test]
    fn verify_recent_lists_exactly_what_is_missing() {
        let j = tmp("verify");
        j.append("sig1", 1, None, None, "ws").unwrap();
        j.append("sig3", 3, None, None, "ws").unwrap();
        let mut rpc = FakeRpc { chain: chain(5), calls: 0 };
        let r = verify_recent(&j, &mut rpc, "prog", 5, 100).unwrap();
        // The journal covers slots 1..3 (floor=1, ceil=3). The chain has
        // sig1..sig5; sig4 and sig5 are NEWER than the journal's window and
        // are not holes. Only sig2 sits inside the window and is missing.
        assert_eq!(r.missing.len(), 1, "only sig2 is inside the claimed window");
        assert!(r.missing.contains(&"sig2".to_string()));
    }

    #[test]
    fn verify_recent_flags_a_hole_in_the_middle_of_the_window() {
        let j = tmp("midhole");
        // Journal covers slots 1..5 but sig3 was lost.
        for i in [1, 2, 4, 5] {
            j.append(&format!("sig{i}"), i, None, None, "ws").unwrap();
        }
        let mut rpc = FakeRpc { chain: chain(5), calls: 0 };
        let r = verify_recent(&j, &mut rpc, "prog", 5, 100).unwrap();
        assert_eq!(r.missing.len(), 1);
        assert_eq!(r.missing[0], "sig3");
    }

    #[test]
    fn verify_recent_ignores_history_below_the_journal_floor() {
        let j = tmp("floor");
        j.append("sig8", 8, None, None, "ws").unwrap();
        j.append("sig9", 9, None, None, "ws").unwrap();
        let mut rpc = FakeRpc { chain: chain(10), calls: 0 };
        let r = verify_recent(&j, &mut rpc, "prog", 5, 100).unwrap();
        assert!(r.missing.is_empty(), "older history was never in scope");
    }

    #[test]
    fn verify_recent_on_a_complete_journal_finds_nothing() {
        let j = tmp("clean");
        for i in 1..=5 {
            j.append(&format!("sig{i}"), i, None, None, "ws").unwrap();
        }
        let mut rpc = FakeRpc { chain: chain(5), calls: 0 };
        let r = verify_recent(&j, &mut rpc, "prog", 5, 100).unwrap();
        assert!(r.missing.is_empty(), "a complete journal reports nothing missing");
    }
}
