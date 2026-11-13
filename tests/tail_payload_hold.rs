//! Plain `tail` must not consume a payload it still owes.
//!
//! The payload is the point of the mailbox: a row without it is a
//! signature, not an event. Measured on the pre-fix tree (v0.7.21, HEAD
//! a19f7b0): plain `tail` printed a row with `logs=null, payload_state=null`
//! and advanced the cursor past it, exit 0. When `hydrate` later filled that
//! row's payload, the next pass reported `0 events after cursor 2` — the
//! now-complete payload was permanently unreachable for that consumer, while
//! a fresh cursor read it fine. Silent, permanent data loss behind the
//! default path, exactly the failure `--with-accounts` was fixed to stop in
//! v0.7.9 (`tests/tail_with_accounts.rs`).
//!
//! The rule mirrors the payload debt queue in journal.rs
//! (`events_missing_payload`):
//!   * logs present, or a recorded outcome (`held`/`unavailable`/`invalid`)
//!                                -> deliver (the outcome marker travels)
//!   * logs null AND state null   -> owed: HOLD. Stop, cursor stays behind
//!     the row, exit 3, name `hydrate`. Nothing at or after it is consumed.
//!   * `--no-logs` callers need identities only — never held.
//!
//! Guards (green on BOTH trees): recorded outcomes are delivered and
//! consumed, `--no-logs` consumes everything, `--with-accounts` behavior is
//! untouched, and a paid debt reaches the same cursor that held.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const VALID_PROGRAM: &str = "whirLbMiicVdio4qvUfM5KAg6Ct8VwpYzGff3uctyCc";
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

/// The numeric position the cursor file holds (structured cursor since
/// v0.7.13; these tests pin the POSITION, not the encoding).
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
        return rest[..end].to_string();
    }
    raw
}

/// (logs, payload_state) per row kind, mirroring journal.rs's SCHEMA.
enum Kind {
    /// payload + account envelope both complete
    Enriched,
    Held,
    Pending,
    Unavailable,
    Invalid,
}

fn setup(tag: &str, rows: &[(Kind, &str)]) -> PathBuf {
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
    for (i, (kind, signature)) in rows.iter().enumerate() {
        let slot = 44_000_000i64 + i as i64;
        let (logs, payload_state, accounts, account_state): (
            Option<&str>,
            Option<&str>,
            Option<&str>,
            Option<&str>,
        ) = match *kind {
            Kind::Enriched => (
                Some(LOG),
                Some("held"),
                Some(r#"{"instructions":[],"accounts_flags_source":"message"}"#),
                Some("held"),
            ),
            Kind::Held => (Some(LOG), Some("held"), None, None),
            Kind::Pending => (None, None, None, None),
            Kind::Unavailable => (None, Some("unavailable"), None, None),
            Kind::Invalid => (None, Some("invalid"), None, None),
        };
        conn.execute(
            "INSERT INTO events(signature, slot, err, logs, source, first_seen, payload_state, accounts, account_state)
             VALUES(?1, ?2, NULL, ?3, 'ws', 1700000000000, ?4, ?5, ?6)",
            rusqlite::params![signature, slot, logs, payload_state, accounts, account_state],
        )
        .expect("plant row");
    }
    conn.close().expect("close journal");
    dir
}

fn pay_debt(dir: &Path, signature: &str) {
    let conn = rusqlite::Connection::open(dir.join("reeflog.db")).expect("open journal");
    conn.execute(
        "UPDATE events SET logs = ?2, payload_state = 'held' WHERE signature = ?1",
        rusqlite::params![signature, LOG],
    )
    .expect("pay debt");
    conn.close().expect("close journal");
}

// ---------------------------------------------------------------- defect

#[test]
fn plain_tail_holds_at_a_pending_payload_and_delivers_it_once_paid() {
    let dir = setup(
        "payload-hold",
        &[
            (Kind::Held, "SigOne1111111111111111111111111111111111111"),
            (Kind::Pending, "SigTwo1111111111111111111111111111111111111"),
            (Kind::Held, "SigThree111111111111111111111111111111111111"),
        ],
    );
    let p1 = run(&dir, &["tail", "--db", "reeflog.db", "--cursor", "cursor.txt"]);
    assert_eq!(
        p1.status.code(),
        Some(3),
        "a row the payload queue still owes is exit 3, never success: {}",
        String::from_utf8_lossy(&p1.stderr)
    );
    assert_eq!(
        delivered_signatures(&p1),
        vec!["SigOne1111111111111111111111111111111111111".to_string()],
        "only the rows before the hold are delivered"
    );
    assert_eq!(cursor_of(&dir), "1", "the cursor must not pass the owed row");
    let err = stderr_of(&p1);
    assert!(err.contains("held"), "the hold is reported, never silent: {err}");
    assert!(err.contains("hydrate"), "the refusal names the filling command: {err}");

    // Pay the debt; the SAME cursor must now deliver what was held.
    pay_debt(&dir, "SigTwo1111111111111111111111111111111111111");
    let p2 = run(&dir, &["tail", "--db", "reeflog.db", "--cursor", "cursor.txt"]);
    assert_eq!(
        p2.status.code(),
        Some(0),
        "once paid, the pass completes: {}",
        String::from_utf8_lossy(&p2.stderr)
    );
    assert_eq!(
        delivered_signatures(&p2),
        vec![
            "SigTwo1111111111111111111111111111111111111".to_string(),
            "SigThree111111111111111111111111111111111111".to_string(),
        ],
        "the owed row arrives once paid, nothing lost"
    );
    assert_eq!(cursor_of(&dir), "3");

    // Pass 3: no replay.
    let p3 = run(&dir, &["tail", "--db", "reeflog.db", "--cursor", "cursor.txt"]);
    assert_eq!(p3.status.code(), Some(0));
    assert!(
        delivered_signatures(&p3).is_empty(),
        "cursor semantics stay exactly-once"
    );
}

#[test]
fn a_hold_reports_the_row_the_client_can_parse() {
    // The Node client parses "HELD at seq <n> <sig>: <why>" (index.js).
    // The plain-path hold must ride the SAME line shape, or exit-3
    // detection silently degrades to a nameless hold.
    let dir = setup(
        "payload-hold-shape",
        &[(Kind::Pending, "SigTwo1111111111111111111111111111111111111")],
    );
    let p = run(&dir, &["tail", "--db", "reeflog.db", "--cursor", "cursor.txt"]);
    assert_eq!(p.status.code(), Some(3));
    let err = String::from_utf8_lossy(&p.stderr).to_string();
    assert!(
        err.contains("HELD at seq 1 SigTwo1111111111111111111111111111111111111:"),
        "the hold line keeps the accounts-path shape: {err}"
    );
    // Nothing consumed: the cursor file is never created (first run) or
    // stays at its old position — never advanced past the owed row.
    let pos = cursor_of(&dir);
    assert!(
        pos.is_empty() || pos == "0",
        "nothing consumed, got cursor position {pos:?}"
    );
}

// ---------------------------------------------------------------- guards

#[test]
fn recorded_outcomes_are_delivered_and_consumed_never_held() {
    // GUARD (green on both trees): a row whose payload can never be filled
    // travels WITH its marker — the consumer must see it, the cursor passes.
    let dir = setup(
        "payload-final",
        &[
            (Kind::Held, "SigOne1111111111111111111111111111111111111"),
            (Kind::Unavailable, "SigTwo1111111111111111111111111111111111111"),
            (Kind::Invalid, "SigThree111111111111111111111111111111111111"),
            (Kind::Held, "SigFour1111111111111111111111111111111111111"),
        ],
    );
    let p = run(&dir, &["tail", "--db", "reeflog.db", "--cursor", "cursor.txt"]);
    assert_eq!(
        p.status.code(),
        Some(0),
        "decided rows must not wedge the consumer: {}",
        String::from_utf8_lossy(&p.stderr)
    );
    assert_eq!(delivered_signatures(&p).len(), 4, "every row travels");
    assert_eq!(cursor_of(&dir), "4");
    let out = String::from_utf8_lossy(&p.stdout).to_string();
    assert!(
        out.contains("\"payload_state\":\"unavailable\"")
            && out.contains("\"payload_state\":\"invalid\"")
            && out.contains("\"logs\":null"),
        "the outcome marker is delivered, not silently omitted: {out}"
    );

    let p2 = run(&dir, &["tail", "--db", "reeflog.db", "--cursor", "cursor.txt"]);
    assert_eq!(p2.status.code(), Some(0));
    assert!(delivered_signatures(&p2).is_empty(), "no replay");
}

#[test]
fn no_logs_callers_are_never_held() {
    // GUARD (green on both trees): `--no-logs` callers need identities only
    // (the kill-test harness does). There is no owed payload for them.
    let dir = setup(
        "payload-nologs",
        &[
            (Kind::Pending, "SigOne1111111111111111111111111111111111111"),
            (Kind::Pending, "SigTwo1111111111111111111111111111111111111"),
        ],
    );
    let p = run(
        &dir,
        &["tail", "--db", "reeflog.db", "--cursor", "cursor.txt", "--no-logs"],
    );
    assert_eq!(p.status.code(), Some(0), "identity shape never blocks");
    assert_eq!(delivered_signatures(&p).len(), 2);
    assert_eq!(cursor_of(&dir), "2");
    assert!(
        !String::from_utf8_lossy(&p.stdout).contains("\"logs\""),
        "compact shape stays compact"
    );
}

#[test]
fn with_accounts_pending_payload_still_holds_with_the_same_rule() {
    // GUARD: the accounts path already held on payload-owed rows; the
    // shared rule must not weaken it.
    let dir = setup(
        "payload-acct-hold",
        &[
            // complete envelope: deliverable under --with-accounts
            (Kind::Enriched, "SigOne1111111111111111111111111111111111111"),
            // payload not fetched yet: can still gain both -> owed here too
            (Kind::Pending, "SigTwo1111111111111111111111111111111111111"),
        ],
    );
    let p = run(
        &dir,
        &[
            "tail",
            "--db",
            "reeflog.db",
            "--cursor",
            "cursor.txt",
            "--with-accounts",
        ],
    );
    assert_eq!(p.status.code(), Some(3));
    assert_eq!(delivered_signatures(&p).len(), 1);
    assert_eq!(cursor_of(&dir), "1");
}
