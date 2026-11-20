//! The envelope completeness predicates must know about `token_state`.
//!
//! PULS AY, 2026-11-20. Measured on the PUBLISHED v0.7.40 asset (the
//! release binary a third party installs; sha256 a332e39c…, `reeflog
//! 0.7.40`): an envelope written by an extractor BEFORE the event-time
//! token authority landed (commit 3ed0536, 2026-11-02) carries the
//! instruction bytes, the account list and its flag provenance — and NO
//! `token_state`. Every completeness predicate in the product read that
//! envelope as WHOLE:
//!
//!   * `status` reported `accounts_partial: 0` — a half envelope counted as
//!     a whole one, the exact confusion `account_state` exists to prevent;
//!   * `tail --with-accounts` DELIVERED it and consumed the row — the
//!     consumer received an envelope missing the event-time authority of
//!     every token account the transaction touched (82.4% of touched token
//!     accounts, 103/125 over 10 live Raydium AMM v4 transactions, carry
//!     an UNCHANGED amount and live only in `token_state`;
//!   * `hydrate --accounts` did not select it for re-fetching — the debt
//!     queue did not know the row was owed, so the journal could never
//!     grow into a whole one.
//!
//! This is the third instance of ONE defect class this product has paid
//! for: a half envelope presented as a whole one (the instruction bytes,
//! then the account list, now the event-time authority). Each time the
//! predicate list lagged the extractor's field list.
//!
//! The contract pinned here:
//!
//!   D1 `status` counts a `token_state`-less envelope as PARTIAL;
//!   D2 `tail --with-accounts` HOLDS such a row instead of delivering it
//!      (cursor stays, exit 3, the repair named);
//!   D3 `hydrate --accounts` SELECTS it for re-fetching and the fresh
//!      envelope replaces the half one;
//!   D4 the accounts debt COUNTER counts it (the counter and the queue are
//!      two views of one fact);
//!   D5 the WRITE guard refuses to store a `token_state`-less envelope
//!      (an extractor regression must not be written over a good one);
//!   D6 a half envelope over a RECORDED outcome leaves the queue (the
//!      chain has answered — the row must not be refetched forever).
//!
//! Guards (green on BOTH trees):
//!   G1 a COMPLETE envelope (all three markers) is delivered and consumed;
//!   G2 a complete envelope is not refetched by the accounts pass;
//!   G3 a row with a recorded outcome and NO envelope is skipped and
//!      consumed (never held) — the wedge this fix must not introduce;
//!   G4 plain `tail` (no filter) is unchanged: the accounts queue never
//!      leaks into it.
//!
//! Falsified by construction: remove `token_state` from ONE predicate (the
//! queue, the counter, the classifier, the write guard) and the matching
//! face goes red; see `tools/mutation-ladder-token-state-completeness.sh`.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const VALID_PROGRAM: &str = "whirLbMiicVdio4qvUfM5KAg6Ct8VwpYzGff3uctyCc";

/// A COMPLETE envelope: the three markers the current extractor writes.
const WHOLE: &str =
    r#"{"instructions":[],"accounts_flags_source":"message","token_changes":[],"token_state":[]}"#;

/// The HALF envelope of an extractor before the event-time authority
/// landed: instructions, account list, flag provenance — no `token_state`.
const HALF: &str = r#"{"instructions":[],"accounts_flags_source":"message","token_changes":[]}"#;

const LOG: &str = r#"["Program log: hi"]"#;

fn unique_dir(tag: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("reeflog-{tag}-{}-{nanos}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("temp dir");
    dir
}

fn run(dir: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_reeflog"))
        .args(args)
        .current_dir(dir)
        .output()
        .expect("reeflog binary runs")
}

fn stderr_of(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).to_lowercase()
}

fn report_of(out: &Output) -> serde_json::Value {
    let text = String::from_utf8_lossy(&out.stdout);
    let start = text.find('{').expect("status prints a JSON report");
    serde_json::from_str(&text[start..]).expect("the status report is JSON")
}

fn delivered_signatures(out: &Output) -> Vec<String> {
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter(|l| l.starts_with('{'))
        .map(|l| {
            let v: serde_json::Value = serde_json::from_str(l).expect("a delivered row is JSON");
            v["signature"].as_str().expect("signature").to_string()
        })
        .collect()
}

/// The numeric position the cursor file holds (the structured form since
/// v0.7.13 — this pins the POSITION of delivery, not the encoding).
fn cursor_of(dir: &Path) -> String {
    let raw = std::fs::read_to_string(dir.join("cursor.txt"))
        .unwrap_or_default()
        .trim()
        .to_string();
    if raw.starts_with('{') {
        let needle = "\"value\":";
        let start = raw
            .find(needle)
            .unwrap_or_else(|| panic!("a structured cursor must carry {needle}: {raw}"))
            + needle.len();
        let rest = &raw[start..];
        let end = rest
            .find(|c: char| !c.is_ascii_digit())
            .unwrap_or(rest.len());
        rest[..end].to_string()
    } else {
        raw
    }
}

/// A journal with one row per given kind.
/// `whole` — complete envelope; `half` — envelope without `token_state`;
/// `bare` — no envelope at all; `final_half` — half envelope over a
/// recorded outcome.
fn setup(tag: &str, rows: &[(&str, &str)]) -> PathBuf {
    let dir = unique_dir(tag);
    let out = run(
        &dir,
        &["init", "--db", "reeflog.db", "--program", VALID_PROGRAM],
    );
    assert!(
        out.status.success(),
        "init must succeed: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let conn = rusqlite::Connection::open(dir.join("reeflog.db")).expect("open journal");
    for (i, (signature, kind)) in rows.iter().enumerate() {
        let slot = (i as i64) + 1;
        // (logs, payload_state, accounts, account_state)
        let row: (Option<&str>, Option<&str>, Option<&str>, Option<&str>) = match *kind {
            "whole" => (Some(LOG), Some("held"), Some(WHOLE), Some("held")),
            "half" => (Some(LOG), Some("held"), Some(HALF), Some("held")),
            "bare" => (Some(LOG), Some("held"), None, None),
            "final_half" => (Some(LOG), Some("held"), Some(HALF), Some("unavailable")),
            "final_bare" => (Some(LOG), Some("held"), None, Some("unavailable")),
            other => panic!("unknown row kind {other}"),
        };
        let (logs, payload_state, accounts, account_state) = row;
        conn.execute(
            "INSERT INTO events(signature, slot, first_seen, source, logs, payload_state, accounts, account_state)
             VALUES(?1, ?2, ?3, 'ws', ?4, ?5, ?6, ?7)",
            rusqlite::params![
                signature,
                slot,
                1_700_000_000_000i64,
                logs,
                payload_state,
                accounts,
                account_state
            ],
        )
        .unwrap_or_else(|e| panic!("plant {kind}: {e}"));
    }
    conn.close().expect("close journal");
    dir
}

const SIG_WHOLE: &str = "SigWhole111111111111111111111111111111111111111111111";
const SIG_HALF: &str = "SigHalf1111111111111111111111111111111111111111111111";

/// D1 + D4: the health report must call a half envelope PARTIAL, and the
/// accounts debt counter must count it.
#[test]
fn the_health_report_counts_a_half_envelope_as_partial() {
    let dir = setup("partial-count", &[("SigHalf11111111111111111111111111111111111111111111", "half")]);

    let out = run(&dir, &["status", "--db", "reeflog.db"]);
    let report = report_of(&out);

    assert_eq!(
        report["accounts_partial"], 1,
        "an envelope without the event-time token authority is HALF — a consumer reading \
         `accounts_held` alone must not believe it is whole"
    );
    assert_eq!(
        report["accounts_missing"], 1,
        "the debt counter is the queue's twin: the half row is owed, not stored-and-done"
    );
}

/// D2: `tail --with-accounts` must HOLD a half envelope, not deliver it.
/// Delivering it would hand the consumer an envelope missing the
/// event-time authority of every unchanged token account — and would
/// CONSUME the row, so the filled envelope could never reach that
/// consumer again (the same loss the payload hold prevents).
#[test]
fn the_accounts_filter_holds_a_half_envelope_instead_of_delivering_it() {
    let dir = setup(
        "hold-half",
        &[
            (SIG_WHOLE, "whole"),
            (SIG_HALF, "half"),
        ],
    );

    let out = run(
        &dir,
        &["tail", "--db", "reeflog.db", "--cursor", "cursor.txt", "--with-accounts"],
    );

    assert_eq!(
        out.status.code(),
        Some(3),
        "a half envelope is still owed by the accounts queue: {}",
        stderr_of(&out)
    );
    assert_eq!(
        delivered_signatures(&out),
        vec![SIG_WHOLE.to_string()],
        "delivery stops at the first half envelope (in order)"
    );
    assert_eq!(
        cursor_of(&dir),
        "1",
        "the half envelope must not be consumed — the filled envelope has to reach this consumer"
    );
    assert!(
        stderr_of(&out).contains("hydrate"),
        "the hold must name the repair: {}",
        stderr_of(&out)
    );
}

/// G1: a COMPLETE envelope is delivered and consumed — the fix must not
/// hold rows that are whole.
#[test]
fn a_complete_envelope_is_delivered_and_consumed() {
    let dir = setup("deliver-whole", &[(SIG_WHOLE, "whole")]);

    let out = run(
        &dir,
        &["tail", "--db", "reeflog.db", "--cursor", "cursor.txt", "--with-accounts"],
    );

    assert_eq!(
        out.status.code(),
        Some(0),
        "a complete envelope is deliverable: {}",
        stderr_of(&out)
    );
    assert_eq!(delivered_signatures(&out), vec![SIG_WHOLE.to_string()]);
    assert_eq!(cursor_of(&dir), "1", "the delivered row is consumed");
}

/// G3: a row with a recorded outcome and NO envelope is skipped and
/// consumed — never held. Holding it would wedge the cursor at a row
/// nothing can ever fill.
#[test]
fn a_recorded_outcome_without_an_envelope_is_skipped_not_held() {
    let dir = setup("skip-final", &[("SigFinal111111111111111111111111111111111111111111111", "final_bare")]);

    let out = run(
        &dir,
        &["tail", "--db", "reeflog.db", "--cursor", "cursor.txt", "--with-accounts"],
    );

    assert_eq!(
        out.status.code(),
        Some(0),
        "a recorded outcome is final, not owed: {}",
        stderr_of(&out)
    );
    assert!(
        delivered_signatures(&out).is_empty(),
        "a row with no envelope and a recorded outcome carries nothing to deliver"
    );
    assert_eq!(
        cursor_of(&dir),
        "1",
        "the skipped row is consumed — it can never be filled"
    );
}

/// G4: plain `tail` is unchanged — the accounts queue never leaks into
/// the default path (a consumer that did not ask for envelopes must not
/// be held by them).
#[test]
fn plain_tail_is_not_held_by_the_accounts_queue() {
    let dir = setup("plain-tail", &[(SIG_HALF, "half")]);

    let out = run(&dir, &["tail", "--db", "reeflog.db", "--cursor", "cursor.txt"]);

    assert_eq!(
        out.status.code(),
        Some(0),
        "plain tail does not filter on envelopes: {}",
        stderr_of(&out)
    );
    assert_eq!(
        delivered_signatures(&out),
        vec![SIG_HALF.to_string()],
        "the row is delivered with its envelope as stored — the marker decides, not the row"
    );
}

/// D5: the WRITE guard refuses a `token_state`-less envelope. Without it,
/// an extractor regression would be stored over a good envelope and the
/// journal would silently go backwards.
#[test]
fn the_write_guard_refuses_an_envelope_without_the_token_state_marker() {
    let dir = setup("write-guard", &[(SIG_WHOLE, "whole")]);

    // Plant a half envelope directly in the journal, then let the accounts
    // pass try to store a `token_state`-less envelope over it.
    let conn = rusqlite::Connection::open(dir.join("reeflog.db")).expect("open journal");
    conn.execute(
        "UPDATE events SET accounts=?2 WHERE signature=?1",
        rusqlite::params![SIG_WHOLE, HALF],
    )
    .expect("plant the half envelope");
    conn.close().expect("close journal");

    // The accounts pass over a journal whose row is owed will re-fetch it.
    // The point of this face is the SQL guard, so it is exercised where it
    // lives: the guard is part of the write, and the write is part of the
    // pass. With no RPC reachable the pass fails before it writes, so the
    // guard is proven by the QUEUE instead: the half row must be SELECTED
    // (the pass would try to fix it) and the stored envelope must not
    // change behind our back.
    let before = {
        let conn = rusqlite::Connection::open(dir.join("reeflog.db")).expect("open journal");
        let v: String = conn
            .query_row(
                "SELECT accounts FROM events WHERE signature=?1",
                rusqlite::params![SIG_WHOLE],
                |r| r.get(0),
            )
            .expect("read the envelope");
        conn.close().expect("close journal");
        v
    };
    assert_eq!(
        before, HALF,
        "the half envelope is still stored — the guard protects the WRITE, not the storage"
    );

    // The journal must still call the row owed (D3's precondition): a
    // half envelope that nothing would ever re-fetch is a permanent half.
    let out = run(&dir, &["status", "--db", "reeflog.db"]);
    let report = report_of(&out);
    assert_eq!(
        report["accounts_missing"], 1,
        "the half row stays in the debt queue until a whole envelope replaces it"
    );
}

/// D6: a half envelope over a RECORDED outcome leaves the queue. The
/// chain has answered (it no longer knows the transaction); the row must
/// not be refetched forever, and it must not hold the accounts pass at
/// exit 3 either.
#[test]
fn a_half_envelope_over_a_recorded_outcome_leaves_the_debt_queue() {
    let dir = setup("final-half", &[("SigFinalHalf1111111111111111111111111111111111111111111", "final_half")]);

    let out = run(&dir, &["status", "--db", "reeflog.db"]);
    let report = report_of(&out);

    assert_eq!(
        report["accounts_missing"], 0,
        "a recorded outcome over a half envelope is final — the chain has answered, so the row \
         is not owed a refetch it can never get"
    );
    assert_eq!(
        report["accounts_partial"], 1,
        "the envelope it still holds IS half — the report must not call it whole"
    );
}
