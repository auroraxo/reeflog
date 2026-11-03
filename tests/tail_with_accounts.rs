//! `tail --with-accounts` must not consume a row it could not deliver.
//!
//! The flag promises "only rows that carry an account envelope". A row whose
//! envelope is not there YET is still owed by a debt queue
//! (`hydrate` / `hydrate --accounts`), so it will exist later. Consuming it
//! at scan time — advancing the cursor past it — makes it unreachable for
//! this consumer forever: the envelope arrives, the row is re-read never.
//!
//! Measured on the pre-fix tree (HEAD 15d1ba3): pass 1 printed seq 1 and 3,
//! skipped seq 2, advanced the cursor to 3, exit 0; after the envelope for
//! seq 2 was filled, pass 2 printed nothing. The row was lost to that
//! consumer while the tool told them hydration "fills them".
//!
//! The correct rule, mirroring the two SQL debt queues in journal.rs:
//!   * envelope present                 -> deliver
//!   * envelope owed (row is in a debt queue) -> HOLD: stop, cursor stays
//!   * recorded outcome (never fillable)      -> skip, report, consume
//!
//! Guards (must be green on BOTH trees): a row with a recorded outcome is
//! still consumed, and plain `tail` (no filter) is completely unchanged.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const VALID_PROGRAM: &str = "whirLbMiicVdio4qvUfM5KAg6Ct8VwpYzGff3uctyCc";
const ENVELOPE: &str = r#"{"instructions":[],"accounts_flags_source":"message","token_changes":[]}"#;
const INCOMPLETE_ENVELOPE: &str = r#"{"token_changes":[]}"#;
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

fn cursor_of(dir: &Path) -> String {
    std::fs::read_to_string(dir.join("cursor.txt"))
        .unwrap_or_default()
        .trim()
        .to_string()
}

/// Create a journal and plant rows in exactly the states the delivery filter
/// classifies. Column shapes mirror `SCHEMA` in journal.rs.
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
            // envelope present, account pass resolved it
            "enriched" => (Some(LOG), Some("held"), Some(ENVELOPE), Some("held")),
            // payload held, account envelope still owed (in the accounts queue)
            "owed" => (Some(LOG), Some("held"), None, None),
            // payload not fetched yet (in the payload queue; can gain both)
            "pending_payload" => (None, None, None, None),
            // recorded outcome for the account envelope: never fillable
            "final_acct" => (Some(LOG), Some("held"), None, Some("unavailable")),
            // recorded outcome for the payload itself: never fillable
            "final_payload" => (None, Some("unavailable"), None, None),
            // stored by an older extractor: in the queue until rewritten
            "half_envelope" => (Some(LOG), Some("held"), Some(INCOMPLETE_ENVELOPE), Some("held")),
            other => panic!("unknown row kind {other}"),
        };
        let (logs, payload_state, accounts, account_state) = row;
        conn.execute(
            "INSERT INTO events(signature, slot, first_seen, source, logs, payload_state, accounts, account_state)
             VALUES(?1, ?2, ?3, 'ws', ?4, ?5, ?6, ?7)",
            rusqlite::params![signature, slot, 1_700_000_000_000i64, logs, payload_state, accounts, account_state],
        )
        .unwrap_or_else(|e| panic!("plant {kind}: {e}"));
    }
    conn.close().expect("close journal");
    dir
}

/// What `hydrate --accounts` does to an owed row: the envelope lands.
fn fill_envelope(dir: &Path, signature: &str) {
    let conn = rusqlite::Connection::open(dir.join("reeflog.db")).expect("open journal");
    let n = conn
        .execute(
            "UPDATE events SET accounts=?2, account_state='held' WHERE signature=?1 AND logs IS NOT NULL",
            rusqlite::params![signature, ENVELOPE],
        )
        .expect("fill envelope");
    assert_eq!(n, 1, "exactly one row is filled");
    conn.close().expect("close journal");
}

#[test]
fn an_owed_row_holds_the_cursor_and_is_delivered_once_filled() {
    let dir = setup(
        "hold-resume",
        &[
            ("SigOne1111111111111111111111111111111111111", "enriched"),
            ("SigTwo1111111111111111111111111111111111111", "owed"),
            ("SigThree111111111111111111111111111111111111", "enriched"),
        ],
    );

    // Pass 1: the owed row must NOT be consumed, and rows behind it must not
    // be delivered out of order. The tool must name the hold and the fix.
    let p1 = run(
        &dir,
        &["tail", "--db", "reeflog.db", "--cursor", "cursor.txt", "--with-accounts"],
    );
    assert_eq!(
        p1.status.code(),
        Some(3),
        "an owed row is incomplete work, exit 3 like hydrate/verify: {}",
        String::from_utf8_lossy(&p1.stderr)
    );
    let got = delivered_signatures(&p1);
    assert_eq!(
        got,
        vec!["SigOne1111111111111111111111111111111111111".to_string()],
        "delivery stops at the first owed row (in order, no skipping ahead)"
    );
    assert_eq!(
        cursor_of(&dir),
        "1",
        "the cursor must stop BEFORE the owed row — consuming it is the defect"
    );
    let err = stderr_of(&p1);
    assert!(
        err.contains("held") && err.contains("hydrate --accounts"),
        "the hold must be reported with the command that fills it: {err}"
    );

    // The debt gets paid, exactly as `hydrate --accounts` would.
    fill_envelope(&dir, "SigTwo1111111111111111111111111111111111111");

    // Pass 2: the previously owed row reaches the consumer, then the rest.
    let p2 = run(
        &dir,
        &["tail", "--db", "reeflog.db", "--cursor", "cursor.txt", "--with-accounts"],
    );
    assert_eq!(
        p2.status.code(),
        Some(0),
        "nothing owed after hydration: {}",
        String::from_utf8_lossy(&p2.stderr)
    );
    let got = delivered_signatures(&p2);
    assert_eq!(
        got,
        vec![
            "SigTwo1111111111111111111111111111111111111".to_string(),
            "SigThree111111111111111111111111111111111111".to_string(),
        ],
        "the row that was owed is delivered once it exists, and not before"
    );
    assert_eq!(cursor_of(&dir), "3");

    // Pass 3: no replay.
    let p3 = run(
        &dir,
        &["tail", "--db", "reeflog.db", "--cursor", "cursor.txt", "--with-accounts"],
    );
    assert_eq!(p3.status.code(), Some(0));
    assert!(
        delivered_signatures(&p3).is_empty(),
        "cursor semantics stay exactly-once"
    );
}

#[test]
fn a_payload_still_pending_holds_too_because_it_can_gain_an_envelope() {
    let dir = setup(
        "hold-payload",
        &[
            ("SigOne1111111111111111111111111111111111111", "enriched"),
            ("SigTwo1111111111111111111111111111111111111", "pending_payload"),
            ("SigThree111111111111111111111111111111111111", "enriched"),
        ],
    );
    let p = run(
        &dir,
        &["tail", "--db", "reeflog.db", "--cursor", "cursor.txt", "--with-accounts"],
    );
    assert_eq!(
        p.status.code(),
        Some(3),
        "a row the payload queue still owes holds the cursor too"
    );
    assert_eq!(delivered_signatures(&p).len(), 1, "stops at the pending row");
    assert_eq!(cursor_of(&dir), "1", "the pending row is not consumed");
}

#[test]
fn recorded_outcomes_are_consumed_not_held() {
    // GUARD: rows that can never gain an envelope must not wedge the
    // consumer's cursor — this passes on the pre-fix tree as well.
    let dir = setup(
        "final-skip",
        &[
            ("SigOne1111111111111111111111111111111111111", "enriched"),
            ("SigTwo1111111111111111111111111111111111111", "final_acct"),
            ("SigThree111111111111111111111111111111111111", "final_payload"),
            ("SigFour111111111111111111111111111111111111", "enriched"),
        ],
    );
    let p1 = run(
        &dir,
        &["tail", "--db", "reeflog.db", "--cursor", "cursor.txt", "--with-accounts"],
    );
    assert_eq!(
        p1.status.code(),
        Some(0),
        "only recorded outcomes were skipped: {}",
        String::from_utf8_lossy(&p1.stderr)
    );
    let got = delivered_signatures(&p1);
    assert_eq!(got.len(), 2, "both enriched rows are delivered, skipping the decided ones");
    assert_eq!(
        cursor_of(&dir),
        "4",
        "rows that can never be enriched are consumed, not held"
    );
    assert!(
        stderr_of(&p1).contains("skipped"),
        "the skips are still reported, never silent: {}",
        String::from_utf8_lossy(&p1.stderr)
    );

    let p2 = run(
        &dir,
        &["tail", "--db", "reeflog.db", "--cursor", "cursor.txt", "--with-accounts"],
    );
    assert_eq!(p2.status.code(), Some(0));
    assert!(delivered_signatures(&p2).is_empty(), "no replay of skipped rows");
}

#[test]
fn plain_tail_is_unchanged() {
    // GUARD: without the filter every row is delivered whatever its state —
    // green on both trees, the filter change must not leak into the default.
    let dir = setup(
        "plain-tail",
        &[
            ("SigOne1111111111111111111111111111111111111", "enriched"),
            ("SigTwo1111111111111111111111111111111111111", "owed"),
            ("SigThree111111111111111111111111111111111111", "pending_payload"),
        ],
    );
    let p = run(&dir, &["tail", "--db", "reeflog.db", "--cursor", "cursor.txt"]);
    assert_eq!(p.status.code(), Some(0), "plain tail never blocks");
    assert_eq!(
        delivered_signatures(&p).len(),
        3,
        "plain tail delivers every row and states each row's debt in-line"
    );
    assert_eq!(cursor_of(&dir), "3", "plain tail consumes everything it printed");
}

#[test]
fn an_incomplete_envelope_from_an_older_extractor_holds_until_rewritten() {
    // The row IS in the accounts debt queue (`events_missing_accounts`
    // counts it), so under the filter it is owed, not deliverable: a half
    // envelope presented as a whole one is exactly what `account_state`
    // exists to prevent.
    let dir = setup(
        "half-envelope",
        &[
            ("SigOne1111111111111111111111111111111111111", "enriched"),
            ("SigTwo1111111111111111111111111111111111111", "half_envelope"),
        ],
    );
    let p = run(
        &dir,
        &["tail", "--db", "reeflog.db", "--cursor", "cursor.txt", "--with-accounts"],
    );
    assert_eq!(
        p.status.code(),
        Some(3),
        "a half envelope is still owed by the accounts queue"
    );
    assert_eq!(cursor_of(&dir), "1", "the half envelope is not consumed");
}
