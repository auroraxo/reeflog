//! `status` must hold the EXIT CODE for the payload debt its own report
//! already prints.
//!
//! PULS AM, 2026-11-16. Measured on the PUBLISHED v0.7.30 asset (the linux
//! archive whose sha256 equals the digest the Release API publishes;
//! `tools/measure-status-payload-debt-exit.mjs`, the real binary against a
//! real journal;
//! `research/status-payload-debt-exit-published-v0730-2026-11-16.json`,
//! verdict `status-payload-debt-exit-blind`):
//!
//! * `status` prints `payload_pending: 1` and exits **0**, while the default
//!   payload-bearing `tail` on the same journal prints
//!   `HELD at seq 1 <sig>: the payload is still owed — run hydrate` and
//!   exits **3** — the exact predicate the report already counts
//!   (`logs IS NULL AND payload_state IS NULL`).
//!
//! The other surfaces already hold this code: `webhook push` holds its
//! target cursor at an owed row (v0.7.23), `hydrate` exits 3 while
//! `pending_left` is non-zero, and the daemon's shutdown exits 3 while the
//! payload hydrate is incomplete. Only `status` — the surface a monitoring
//! caller polls — read the same number as health.
//!
//! The rule:
//! * `status` exits 3 while a row still owes its payload, and names
//!   `reeflog hydrate` on stderr.
//! * paying the debt flips it back to 0 — a verdict that never comes down is
//!   an alarm, not a verdict.
//!
//! Guards (green on both trees once G1's `payload_pending: 0` is read —
//! the fields already existed, so the guards are about NOT widening the
//! verdict):
//! * G1 a journal with no debt reports `payload_pending: 0` and exits 0.
//! * G2 rows in a RECORDED outcome (`unavailable`/`invalid`) are not a debt
//!   — `hydrate` counts them as a completed pass; a status that alarmed on
//!   them would alarm forever.
//! * G3 the OPT-IN queues (`accounts_missing`, `owners_missing`,
//!   `account_data_missing`) do not set the exit code: they are filled only
//!   by `hydrate --accounts|--owners|--account-data` and held only behind
//!   `tail --with-accounts`, so for a default consumer they never drain —
//!   folding them in would be the permanent false alarm v0.7.27 removed
//!   from `gaps_open`.
//! * G4 `gaps_open` does not set the exit code either (same class).

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const VALID_PROGRAM: &str = "whirLbMiicVdio4qvUfM5KAg6Ct8VwpYzGff3uctyCc";

fn unique_dir(tag: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("reeflog-status-payload-{tag}-{}-{nanos}", std::process::id()));
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
    String::from_utf8_lossy(&out.stderr).into_owned()
}

fn stdout_of(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn report_of(out: &Output) -> serde_json::Value {
    serde_json::from_str(stdout_of(out).trim()).expect("a JSON report")
}

/// A journal that exists, with a program — no events yet.
fn with_journal(tag: &str) -> PathBuf {
    let dir = unique_dir(tag);
    let out = run(&dir, &["init", "--db", "reeflog.db", "--program", VALID_PROGRAM]);
    assert!(out.status.success(), "init must succeed: {}", stderr_of(&out));
    dir
}

/// One event row, with the given `logs`/`payload_state`/`accounts`/
/// `account_state` triple — the schema under test is the binary's own.
fn plant(dir: &Path, rows: &[(String, Option<&str>, Option<&str>, Option<&str>, Option<&str>)]) {
    let conn = rusqlite::Connection::open(dir.join("reeflog.db")).expect("open journal");
    for (i, (sig, logs, state, accounts, account_state)) in rows.iter().enumerate() {
        conn.execute(
            "INSERT INTO events(signature, slot, err, logs, source, first_seen, seal_status,
                                payload_state, accounts, account_state)
             VALUES(?1, ?2, NULL, ?3, 'ws', 1700000000000, 'finalized', ?4, ?5, ?6)",
            rusqlite::params![sig, 44_000_000 + i as i64, logs, state, accounts, account_state],
        )
        .expect("plant row");
    }
    conn.close().expect("close journal");
}

fn owed_row(tag: &str) -> (String, Option<&'static str>, Option<&'static str>, Option<&'static str>, Option<&'static str>) {
    (format!("SIG{tag}{}", "0".repeat(60 - tag.len())), None, None, None, None)
}

// ---------------------------------------------------------------- defect

/// D1: the report prints `payload_pending: 1` and must not exit 0 for it.
#[test]
fn a_journal_that_owes_a_payload_exits_3_and_names_the_command_that_pays_it() {
    let dir = with_journal("payload-owed");
    plant(&dir, &[owed_row("owed")]);

    let out = run(&dir, &["status", "--db", "reeflog.db"]);

    let report = report_of(&out);
    assert_eq!(
        report["payload_pending"], 1,
        "the report must still count the debt it holds the code for"
    );
    assert_eq!(
        out.status.code(),
        Some(3),
        "a printed debt holds the exit code (status, stderr: {})",
        stderr_of(&out)
    );
    assert!(
        stderr_of(&out).contains("hydrate"),
        "the stderr line must name the command that pays it: {}",
        stderr_of(&out)
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// D2: one fact, one verdict — status and the payload-bearing tail must
/// agree on the same journal. They disagreed on the published v0.7.30 asset
/// (status 0, tail 3).
#[test]
fn status_and_the_payload_bearing_tail_agree_on_the_exit_code() {
    let dir = with_journal("payload-parity");
    plant(&dir, &[owed_row("parity")]);

    let status = run(&dir, &["status", "--db", "reeflog.db"]);
    let tail = run(&dir, &["tail", "--db", "reeflog.db", "--cursor", "c.txt"]);

    assert_eq!(tail.status.code(), Some(3), "tail holds at the owed row: {}", stderr_of(&tail));
    assert_eq!(
        status.status.code(),
        tail.status.code(),
        "one fact must carry one verdict across the two surfaces (status {}, tail {})",
        status.status.code().unwrap_or(-1),
        tail.status.code().unwrap_or(-1)
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// D3: the verdict tracks the debt. Paying the row (what `hydrate` does)
/// flips status back to 0 — an exit code that never comes back down is an
/// alarm, not a verdict.
#[test]
fn paying_the_payload_debt_flips_the_verdict_back_to_zero() {
    let dir = with_journal("payload-paid");
    let row = owed_row("paid");
    plant(&dir, &[row.clone()]);

    let before = run(&dir, &["status", "--db", "reeflog.db"]);
    assert_eq!(before.status.code(), Some(3), "owed: {}", stderr_of(&before));

    // The debt is paid exactly the way `hydrate` pays it: content arrives,
    // the state becomes `held`, on the SAME row.
    {
        let conn = rusqlite::Connection::open(dir.join("reeflog.db")).expect("open journal");
        conn.execute(
            "UPDATE events SET logs = '[\"log\"]', payload_state = 'held'
             WHERE signature = ?1 AND logs IS NULL AND payload_state IS NULL",
            rusqlite::params![row.0],
        )
        .expect("pay debt");
        conn.close().expect("close journal");
    }

    let after = run(&dir, &["status", "--db", "reeflog.db"]);
    let report = report_of(&after);
    assert_eq!(report["payload_pending"], 0, "the debt is paid");
    assert_eq!(
        after.status.code(),
        Some(0),
        "a paid debt must read healthy again: {}",
        stderr_of(&after)
    );

    let _ = std::fs::remove_dir_all(&dir);
}

// ---------------------------------------------------------------- guards

/// G1: a journal with no debt reports 0 pending and exits 0.
#[test]
fn a_journal_without_payload_debt_still_exits_0() {
    let dir = with_journal("payload-clean");

    let out = run(&dir, &["status", "--db", "reeflog.db"]);
    let report = report_of(&out);

    assert_eq!(report["payload_pending"], 0);
    assert_eq!(
        out.status.code(),
        Some(0),
        "a report that always exits 3 is not a report: {}",
        stderr_of(&out)
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// G2: a RECORDED outcome is not a debt. The chain forgot the transaction
/// (`unavailable`) or refused the key (`invalid`) — `hydrate` counts those
/// as a completed pass, never as pending. A status that alarmed on them
/// would alarm forever, because no pass will ever fill them.
#[test]
fn a_recorded_outcome_is_not_a_debt() {
    let dir = with_journal("payload-recorded");
    plant(
        &dir,
        &[
            ("RECORDEDA00000000000000000000000000000000000000000000000000".to_string(), None, Some("unavailable"), None, None),
            ("RECORDEDB00000000000000000000000000000000000000000000000000".to_string(), None, Some("invalid"), None, None),
        ],
    );

    let out = run(&dir, &["status", "--db", "reeflog.db"]);
    let report = report_of(&out);

    assert_eq!(report["payload_unavailable"], 1);
    assert_eq!(report["payload_invalid"], 1);
    assert_eq!(report["payload_pending"], 0, "a recorded outcome is not pending");
    assert_eq!(
        out.status.code(),
        Some(0),
        "a recorded outcome must not hold the verdict: {}",
        stderr_of(&out)
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// G3: the OPT-IN queues do not set the exit code. They are filled only by
/// `hydrate --accounts|--owners|--account-data` and held only behind
/// `tail --with-accounts` — for a default consumer they never drain, so
/// folding them into the verdict would be a permanent false alarm.
#[test]
fn the_opt_in_envelope_queues_do_not_set_the_exit_code() {
    let dir = with_journal("payload-optin");
    let rows: Vec<(String, Option<&str>, Option<&str>, Option<&str>, Option<&str>)> = (0..5)
        .map(|i| (format!("OPTIN{i}{}", "0".repeat(55)), Some("[\"log\"]"), Some("held"), None, None))
        .collect();
    plant(&dir, &rows);

    let out = run(&dir, &["status", "--db", "reeflog.db"]);
    let report = report_of(&out);

    assert_eq!(report["accounts_missing"], 5, "the queue is counted, as before");
    assert_eq!(
        out.status.code(),
        Some(0),
        "the opt-in account queue must not hold the default verdict: {}",
        stderr_of(&out)
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// G4: `gaps_open` does not set the exit code either — the same class of
/// permanent alarm v0.7.27 had to remove from this very field.
#[test]
fn an_open_gap_does_not_set_the_status_exit_code() {
    let dir = with_journal("payload-gap");
    let conn = rusqlite::Connection::open(dir.join("reeflog.db")).expect("open journal");
    conn.execute(
        "INSERT INTO gaps(kind, since_slot, detail, detected_at)
         VALUES('hole', 440000000, 'planted', 1700000000000)",
        [],
    )
    .expect("plant gap");
    conn.close().expect("close journal");

    let out = run(&dir, &["status", "--db", "reeflog.db"]);
    let report = report_of(&out);

    assert_eq!(report["gaps_open"], 1, "the ledger still reports the gap");
    assert_eq!(
        out.status.code(),
        Some(0),
        "an open gap must not hold the status verdict (verify/repair own that code): {}",
        stderr_of(&out)
    );

    let _ = std::fs::remove_dir_all(&dir);
}
