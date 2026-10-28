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
//! ANCHOR RULE (measured 2026-10-28, kill-test run `full`):
//!   - A walk must NEVER anchor `before` on a journal-held signature. The
//!     journal's newest held signature was a confirmed-then-forked-out
//!     transaction (`getSignatureStatuses` returned null for it), and
//!     `before=<it>` made `getSignaturesForAddress` answer -32020 for the
//!     whole walk. The old verify did exactly that, then printed
//!     `missing_count: 0` after reading nothing — a hole detector that is
//!     blind precisely when holes exist (it missed 32 real losses).
//!   - A signature-based tail stop has the same flaw: if the boundary
//!     signature is a phantom the chain never returns it, the walk never
//!     matches and runs to max_pages leaving the gap open forever. Tail
//!     stops on the boundary *slot* instead — slot order is the chain's.
//!   - Every walk therefore starts at the chain tip (`before` unset) and
//!     afterwards pages only with signatures the CHAIN itself returned.
//!     The cost of tip-start is breadth: a journal whose ceiling is far
//!     behind the tip spends pages above its own window, and running out is
//!     reported as `truncated` / incomplete — never as a clean scan.
//!
//! Two different gaps exist and need different tools:
//!
//!   1. **Tail gap** — events after the newest slot the journal holds
//!      (a crash, a disconnect, a restart). Fixed by walking back from the
//!      chain tip until the boundary slot is reached. Bounded and cheap.
//!   2. **Deep hole** — events missing from the middle of the journal's
//!      history. Detected by comparing the journal against the chain over a
//!      bounded window. Detection and repair share one walk: the scan
//!      already receives signature, slot and error for every hole it finds,
//!      so `repair_holes` appends them with no extra RPC round trip. What a
//!      public endpoint still charges for is breadth (one page per 500
//!      signatures, HTTP 429 observed), so the walk stays bounded and a
//!      cut-short walk reports itself incomplete instead of clean.

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
        // Public endpoints rate-limit (measured 2026-10-28: 19 of 40
        // consecutive getSignaturesForAddress calls answered HTTP 429, and
        // the successful pages arrived at ~0.63 pages/s). A walk that gives
        // up on the first 429 turns a throttled scan into a false "complete",
        // so we retry with a growing backoff and honour the Retry-After
        // hint when the server sends one.
        let mut last_err = String::from("unreachable");
        for attempt in 0..6 {
            if attempt > 0 {
                // Quadratic backoff, capped: a 429 burst must clear without
                // parking the whole scan for minutes.
                let wait = (400u64 * attempt * attempt).min(8_000);
                std::thread::sleep(std::time::Duration::from_millis(wait));
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
    /// The same holes with the slot/error the repair walk needs, collected
    /// in the same scan so filling costs no extra RPC round trip.
    pub missing_rows: Vec<(String, i64, Option<String>)>,
    /// Rows appended by a repair walk.
    pub filled: i64,
    /// The first (newest) slot the scan actually read. `None` = nothing was
    /// read at all — a scan that read nothing must never claim cleanliness.
    pub scanned_from_slot: Option<i64>,
    /// The oldest slot the scan read. Together with `scanned_from_slot` this
    /// is the slot range the scan actually covered.
    pub scanned_to_slot: Option<i64>,
    /// True when the scan started at or above the journal's claimed ceiling,
    /// i.e. it read every row of the claimed window from the top. Starting
    /// below the ceiling would silently skip rows; that must never count as a
    /// complete scan.
    pub passed_ceiling: bool,
    /// True when the scan read down past the journal's claimed floor.
    pub reached_floor: bool,
    /// Set when the walk aborted early (rate limit, RPC error) or hit its
    /// page budget before reaching the journal floor. A report with an error
    /// or truncation is never proof that the journal is complete.
    pub error: Option<String>,
}

impl Report {
    /// A scan may claim completeness only when it finished without an error
    /// and without hitting its page budget, i.e. it actually read the whole
    /// claimed window. Reading nothing and printing 0 is the exact lie this
    /// crate exists to prevent.
    ///
    /// "The whole claimed window" means: the walk reached the window's floor
    /// AND it did not stop above the window's ceiling. Both are measured, not
    /// assumed.
    pub fn complete(&self) -> bool {
        self.error.is_none()
            && !self.truncated
            && self.pages > 0
            && self.reached_floor
            && self.passed_ceiling
    }
}

/// Fill the journal's **tail gap**: everything on chain newer than the
/// newest slot the journal already holds.
///
/// Walks backwards from the chain tip and stops at the boundary SLOT. The
/// stop is slot-based, not signature-based, because a held signature can be
/// a phantom the chain never returns (measured 2026-10-28) — a signature stop
/// then never matches and the gap is never closed.
///
/// Bounded by `max_pages`; if the boundary is not reached the result is
/// marked truncated and the caller must keep the gap open — an unfilled gap
/// must never look filled.
pub fn backfill_tail<F: Fetcher>(
    j: &Journal,
    f: &mut F,
    program: &str,
    max_pages: usize,
    page_size: usize,
) -> Result<Report, String> {
    let mut report = Report::default();
    let Some(boundary_slot) = j.newest_slot()? else {
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
            let slot = r.get("slot").and_then(|v| v.as_i64()).unwrap_or(0);
            // Slot order is the chain's own order; the boundary is where the
            // journal's coverage ends, not where one held signature sits.
            if slot <= boundary_slot {
                reached_boundary = true;
                break;
            }
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
/// Bounds:
///   - Below `oldest_slot` is out of scope: the journal never claimed it.
///     Scanning past it would flag all of history as "missing".
///   - Above `newest_slot` is the tail: events after capture stopped. They
///     are newer than what the journal claims, not holes in it.
///   - `since_slot` raises the floor further. A long-running journal grows
///     without bound, so a full scan gets more expensive every day; a caller
///     policing a recent rolling window passes it. It can only raise the
///     floor — never widen the claim downward — so `None` and a value below
///     `oldest_slot` mean the same thing.
///
/// ANCHOR (measured 2026-10-28): the walk starts at the CHAIN TIP (`before`
/// unset) and pages backwards using only signatures the chain returned. The
/// old code anchored on `j.newest_signature()`, which was a phantom signature
/// the endpoint answered `-32020 Transaction not found` for, aborting the
/// walk with `pages: 0` while leaving the caller to report `missing: 0`.
/// Tip-start is immune to phantom anchors. The price is paid above the
/// journal's ceiling, and running out of budget there is reported as
/// `truncated` — never as a clean scan.
pub fn verify_recent<F: Fetcher>(
    j: &Journal,
    f: &mut F,
    program: &str,
    max_pages: usize,
    page_size: usize,
    since_slot: Option<i64>,
) -> Result<Report, String> {
    let mut report = Report::default();
    let held = j.all_signatures()?;
    let (Some(oldest), Some(ceil)) = (j.oldest_slot()?, j.newest_slot()?) else {
        return Ok(report); // empty journal: nothing claimed, nothing missing
    };
    let floor = since_slot.map_or(oldest, |s| oldest.max(s));
    if floor > ceil {
        return Ok(report); // caller asked for a window the journal does not hold
    }

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
            if report.scanned_from_slot.is_none() {
                report.scanned_from_slot = Some(slot);
            }
            report.scanned_to_slot = Some(slot);
            if slot > ceil {
                // Newer than the journal's window: tail, not hole.
                continue;
            }
            if slot < floor {
                // Older than anything the journal claims to cover.
                past_floor = true;
                break;
            }
            report.found += 1;
            if !held.contains(&sig) {
                report.missing.push(sig.clone());
                let err = match r.get("err") {
                    None | Some(Value::Null) => None,
                    Some(e) => Some(e.to_string()),
                };
                report.missing_rows.push((sig, slot, err));
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
    // Coverage is measured, not assumed:
    //   - the first row read must be at or above the claimed ceiling, and
    //   - the last row read must be at or below the claimed floor.
    // Either missing means the walk did not read the whole claimed window,
    // so it cannot prove anything about it.
    report.passed_ceiling = report
        .scanned_from_slot
        .map(|s| s >= ceil)
        .unwrap_or(false);
    report.reached_floor = report
        .scanned_to_slot
        .map(|s| s <= floor)
        .unwrap_or(false);
    if !report.reached_floor && report.error.is_none() {
        // The address history ended above the claimed floor: we never read
        // the bottom of the window, so this is not a finished scan.
        report.truncated = true;
    }
    Ok(report)
}

/// Repair the holes a verify scan finds. Because the scan already carries the
/// signature, slot and error of each hole, filling costs ZERO extra RPC round
/// trips. Returns the report with `filled` populated.
///
/// `since_slot` narrows the scanned window (see `verify_recent`); the repair
/// covers exactly what the scan covered — it never claims to have repaired
/// a window it did not read.
pub fn repair_holes<F: Fetcher>(
    j: &Journal,
    f: &mut F,
    program: &str,
    max_pages: usize,
    page_size: usize,
    since_slot: Option<i64>,
) -> Result<Report, String> {
    let mut r = verify_recent(j, f, program, max_pages, page_size, since_slot)?;
    for (sig, slot, err) in &r.missing_rows {
        let (_, is_new) = j.append(sig, *slot, err.as_deref(), None, "repair")?;
        if is_new {
            r.filled += 1;
        }
    }
    Ok(r)
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
        let r = verify_recent(&j, &mut rpc, "prog", 5, 100, None).unwrap();
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
        let r = verify_recent(&j, &mut rpc, "prog", 5, 100, None).unwrap();
        assert_eq!(r.missing.len(), 1);
        assert_eq!(r.missing[0], "sig3");
    }

    #[test]
    fn verify_recent_ignores_history_below_the_journal_floor() {
        let j = tmp("floor");
        j.append("sig8", 8, None, None, "ws").unwrap();
        j.append("sig9", 9, None, None, "ws").unwrap();
        let mut rpc = FakeRpc { chain: chain(10), calls: 0 };
        let r = verify_recent(&j, &mut rpc, "prog", 5, 100, None).unwrap();
        assert!(r.missing.is_empty(), "older history was never in scope");
    }

    #[test]
    fn verify_recent_on_a_complete_journal_finds_nothing() {
        let j = tmp("clean");
        for i in 1..=5 {
            j.append(&format!("sig{i}"), i, None, None, "ws").unwrap();
        }
        let mut rpc = FakeRpc { chain: chain(5), calls: 0 };
        let r = verify_recent(&j, &mut rpc, "prog", 5, 100, None).unwrap();
        assert!(r.missing.is_empty(), "a complete journal reports nothing missing");
    }

    /// REGRESSION — the falsifying case of 2026-10-28 kill-test run `full`:
    /// the journal's newest held signature is a PHANTOM (a confirmed-then-
    /// forked-out transaction). The real endpoint answers
    /// `-32020 Transaction not found` for `before=<it>`, aborting the walk.
    /// The old code then returned `pages: 0, missing: []`, which the caller
    /// printed as `missing_count: 0` while 32 real losses existed.
    /// A hole detector must not be blind precisely when holes exist.
    #[test]
    fn verify_walks_past_a_phantom_anchor_and_still_finds_holes() {
        let j = tmp("phantom");
        // Journal covers slots 1..5 but sig3 is lost; its newest held
        // signature ("phantom") is NOT in the chain's index.
        for i in [1, 2, 4, 5] {
            j.append(&format!("sig{i}"), i, None, None, "ws").unwrap();
        }
        j.append("phantom", 6, None, None, "ws").unwrap();
        let mut rpc = FakeRpc {
            chain: chain(10), // no "phantom" signature anywhere on chain
            calls: 0,
        };
        let r = verify_recent(&j, &mut rpc, "prog", 50, 100, None).unwrap();
        assert!(r.pages > 0, "the scan must actually read pages");
        assert!(r.complete(), "a tip-started scan that finishes is complete");
        assert!(
            r.missing.iter().any(|s| s == "sig3"),
            "hole sig3 must be found even though the newest held signature is a phantom"
        );
        assert!(
            !r.missing.iter().any(|s| s == "phantom"),
            "phantom anchor: slot 6 > ceil? no — phantom is at slot 6 = ceil, so it is in-window but absent from the chain page; it simply does not appear"
        );
    }

    /// The reverse honesty guard: a walk that FAILS must say so, never be
    /// reported as a clean 0. `complete()` is false on error/truncation/0 pages.
    #[test]
    fn a_failed_or_empty_scan_is_never_reported_as_complete() {
        let j = tmp("incomplete");
        j.append("sig1", 1, None, None, "ws").unwrap();
        // Fails on the very first call: nothing was read.
        let mut rpc = FlakyRpc { chain: chain(10), calls: 0, fail_after: 0 };
        let r = verify_recent(&j, &mut rpc, "prog", 5, 3, None).unwrap();
        assert!(!r.complete(), "error + 0 pages must not be complete");
        assert!(r.error.is_some());
        // Page budget reached before the floor: also not complete.
        let mut rpc2 = FakeRpc { chain: chain(100), calls: 0 };
        let r2 = verify_recent(&j, &mut rpc2, "prog", 1, 10, None).unwrap();
        assert!(r2.truncated, "page budget reached => truncated");
        assert!(!r2.complete(), "truncated must never be complete");
    }

    /// REGRESSION — tail stop must be slot-based: the journal's boundary
    /// SIGNATURE can be a phantom the chain never returns, so a signature
    /// comparison never matches and the tail gap is never closed (the walk
    /// runs to max_pages and reports truncated forever).
    #[test]
    fn tail_walk_stops_at_boundary_slot_even_when_boundary_signature_is_a_phantom() {
        let j = tmp("phantom-tail");
        // Journal claims coverage up to slot 3; its newest held signature is
        // a phantom that the chain does not know.
        j.append("sig2", 2, None, None, "ws").unwrap();
        j.append("phantom", 3, None, None, "ws").unwrap();
        let mut rpc = FakeRpc { chain: chain(6), calls: 0 };
        // chain has sig1..sig6 at slots 1..6; journal floor=2 ceil=3.
        let r = backfill_tail(&j, &mut rpc, "prog", 5, 100).unwrap();
        // Chain events above slot 3: sig6, sig5, sig4 — all must be appended.
        assert_eq!(r.inserted, 3, "sig6/sig5/sig4 are newer than the boundary slot");
        assert!(!r.truncated, "slot-based stop terminates the walk");
        assert!(
            j.counts().unwrap().total >= 5,
            "journal now holds sig2, phantom, sig4, sig5, sig6"
        );
    }

    /// repair_holes fills exactly what verify found, from the same scan data
    /// (zero extra RPC round trips).
    #[test]
    fn repair_fills_the_holes_verify_found_and_a_second_scan_is_clean() {
        let j = tmp("repair");
        for i in [1, 2, 4, 5] {
            j.append(&format!("sig{i}"), i, None, None, "ws").unwrap();
        }
        let mut rpc = FakeRpc { chain: chain(5), calls: 0 };
        let r = repair_holes(&j, &mut rpc, "prog", 5, 100, None).unwrap();
        assert_eq!(r.missing, vec!["sig3".to_string()], "verify found the hole");
        assert_eq!(r.filled, 1, "repair appended it");
        let mut rpc2 = FakeRpc { chain: chain(5), calls: 0 };
        let r2 = verify_recent(&j, &mut rpc2, "prog", 5, 100, None).unwrap();
        assert!(r2.missing.is_empty(), "second scan is clean");
        assert!(r2.complete());
    }

    /// An RPC whose first row sits BELOW the journal's ceiling — what an
    /// anchored walk does when the anchor is inside the window. Such a walk
    /// never saw the top of the window, so it cannot claim completeness,
    /// even though it read pages, hit no error, and reached the floor.
    struct StartsMidWindow {
        chain: Vec<(String, i64)>,
        drop_above: i64, // suppress rows with slot > this
        calls: usize,
    }

    impl Fetcher for StartsMidWindow {
        fn call(&mut self, _m: &str, params: Value) -> Result<Value, String> {
            self.calls += 1;
            let limit = params[1]["limit"].as_u64().unwrap_or(100) as usize;
            let before = params[1].get("before").and_then(|v| v.as_str());
            let start = match before {
                Some(b) => self
                    .chain
                    .iter()
                    .position(|(s, _)| s == b)
                    .map(|i| i + 1)
                    .unwrap_or(self.chain.len()),
                None => 0,
            };
            let out: Vec<Value> = self.chain[start..]
                .iter()
                .filter(|(_, slot)| *slot <= self.drop_above)
                .take(limit)
                .map(|(s, slot)| json!({"signature": s, "slot": slot, "err": Value::Null}))
                .collect();
            Ok(Value::Array(out))
        }
    }

    #[test]
    fn a_scan_that_never_reached_the_ceiling_is_not_complete() {
        let j = tmp("mid-window");
        // Journal claims slots 1..10.
        for i in 1..=10 {
            j.append(&format!("sig{i}"), i, None, None, "ws").unwrap();
        }
        // The walk hands us rows only from slot 8 down: it never saw 9 or 10.
        let mut rpc = StartsMidWindow {
            chain: chain(20),
            drop_above: 8,
            calls: 0,
        };
        let r = verify_recent(&j, &mut rpc, "prog", 50, 100, None).unwrap();
        assert!(r.pages > 0, "it did read pages");
        assert!(r.error.is_none(), "and hit no error");
        assert!(r.reached_floor, "and reached the floor");
        assert!(!r.passed_ceiling, "but never saw slot 9..10, the ceiling");
        assert!(!r.complete(), "partial coverage must never prove completeness");
    }

    /// `since_slot` narrows the floor but never widens it downward: holes
    /// older than the requested window stay out of scope, holes inside it
    /// are still found.
    #[test]
    fn since_slot_raises_the_floor_without_hiding_holes_inside_the_window() {
        let j = tmp("rolling");
        // Journal covers slots 1..10 with holes at 2 (old) and 8 (recent).
        for i in [1, 3, 4, 5, 6, 7, 9, 10] {
            j.append(&format!("sig{i}"), i, None, None, "ws").unwrap();
        }
        let mut rpc = FakeRpc { chain: chain(10), calls: 0 };
        let r = verify_recent(&j, &mut rpc, "prog", 50, 100, Some(5)).unwrap();
        assert_eq!(
            r.missing,
            vec!["sig8".to_string()],
            "only the hole inside the rolling window is in scope"
        );
        assert!(r.complete(), "the narrowed window was read top to bottom");
        assert_eq!(r.reached_floor, true);
    }
}
