//! `webhook status` must hold the EXIT CODE for the debts its own report
//! already prints.
//!
//! PULS AM, 2026-11-16. Measured on the PUBLISHED v0.7.29 asset (the linux
//! archive whose sha256 equals the digest the Release API publishes;
//! `tools/measure-status-debt-exit.mjs`, the real binary against a real
//! journal; `research/status-debt-exit-published-v0729-2026-11-16.json`,
//! verdict `status-debt-exit-blind`):
//!
//! * `webhook status` is the report a monitoring caller polls. v0.7.29
//!   taught it to exit 3 for an ORPHANED dead letter, on the stated ground
//!   that this is "the one debt that never drains by itself". Two debts the
//!   SAME report already prints are still read as health:
//!
//!   * a CONFIGURED target's pending dead letters — the report prints
//!     `dead_letters: 1` and exits 0, while `webhook push` on the same
//!     journal prints `dead_letters_pending: 1` and exits 3. The premise
//!     that separates the two does not hold: `webhook_dead_letter` moves the
//!     target's cursor PAST the quarantined row, and `deliver_events` reads
//!     `events_after(next_seq)` — so `push` never re-attempts a quarantined
//!     row. Measured: after three push passes the letter is still pending
//!     and the cursor stands at 1. Only `dlq replay` hands it over, and that
//!     is a human action. The debt does not drain by itself either.
//!
//!   * a configured target whose standing verdict is a FAILURE — the report
//!     prints `last_error` and exits 0, while `push` exits 3 for the very
//!     block that wrote it.
//!
//! The rule:
//! * `webhook status` exits 3 while a configured target holds pending dead
//!   letters or carries a standing failure verdict — the SAME debt `push`
//!   turns into exit 3 on the same journal, so one fact carries one verdict
//!   across the surfaces.
//! * the v0.7.29 orphan rule holds: an orphaned letter still exits 3.
//! * a journal with no debt still exits 0 — a report that always exits 3 is
//!   not a report.
//!
//! Guards: the orphan rule G2 stays green on BOTH trees (the v0.7.29 rule
//! must not regress). G1 (a clean journal reports 0 letters and exits 0)
//! asserts the two NEW counted fields as well, so it is red before the fix
//! — a report that never counted them cannot prove it counts 0 of them.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const VALID_PROGRAM: &str = "whirLbMiicVdio4qvUfM5KAg6Ct8VwpYzGff3uctyCc";

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
    String::from_utf8_lossy(&out.stderr).into_owned()
}

fn stdout_of(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// The whole `webhook status` report and its exit code.
fn status_report(dir: &Path, cfg: &Path) -> (serde_json::Value, i32) {
    let out = run(
        dir,
        &[
            "webhook",
            "status",
            "--db",
            "reeflog.db",
            "--config",
            cfg.to_str().expect("cfg path"),
        ],
    );
    let code = out.status.code().unwrap_or(-1);
    let v: serde_json::Value =
        serde_json::from_str(stdout_of(&out).trim()).expect("a JSON report");
    (v, code)
}

fn push_exit(dir: &Path, cfg: &Path) -> i32 {
    run(
        dir,
        &[
            "webhook",
            "push",
            "--db",
            "reeflog.db",
            "--config",
            cfg.to_str().expect("cfg path"),
        ],
    )
    .status
    .code()
    .unwrap_or(-1)
}

fn write_config(dir: &Path, url: &str) -> PathBuf {
    let cfg = dir.join("wh.json");
    std::fs::write(
        &cfg,
        format!(
            r#"{{"targets":[{{"id":"t1","url":"{url}","secret":"s3cret","timeout_ms":2000,"dead_letter_after":5}}]}}"#
        ),
    )
    .expect("write config");
    cfg
}

fn init(dir: &Path) {
    let out = run(dir, &["init", "--db", "reeflog.db", "--program", VALID_PROGRAM]);
    assert!(out.status.success(), "init: {}", stderr_of(&out));
}

/// Plant one pending dead letter for a CONFIGURED target, with the target's
/// cursor already past it — the state `webhook push` leaves behind after a
/// refusal that quarantined the row.
fn plant_configured_letter(dir: &Path, target: &str, url: &str) {
    let conn = rusqlite::Connection::open(dir.join("reeflog.db")).expect("open journal");
    conn.execute(
        "INSERT INTO webhook_dead_letters(target,kind,seq,body,event_id,reason,status,
                                          attempts,first_failed_at,last_failed_at,state)
         VALUES(?1,'event',1,'{}','EVC','http 403',403,3,1700000000000,1700000000000,'pending')",
        rusqlite::params![target],
    )
    .expect("plant dead letter");
    conn.execute(
        "INSERT INTO webhook_state(target,endpoint,next_seq,next_reorg_id,last_error,updated_at)
         VALUES(?1,?2,1,0,NULL,1700000000000)",
        rusqlite::params![target, url],
    )
    .expect("plant state");
    conn.close().expect("close journal");
}

/// A pending dead letter for a target that LEFT the config.
fn plant_orphan_letter(dir: &Path) {
    let conn = rusqlite::Connection::open(dir.join("reeflog.db")).expect("open journal");
    conn.execute(
        "INSERT INTO webhook_dead_letters(target,kind,seq,body,event_id,reason,status,
                                          attempts,first_failed_at,last_failed_at,state)
         VALUES('gone','event',1,'{}','EVO','http 500',500,3,1700000000000,1700000000000,'pending')",
        [],
    )
    .expect("plant orphan letter");
    conn.close().expect("close journal");
}

/// One real event, so a push pass against a dead endpoint produces a REAL
/// blocked pass and a REAL standing verdict.
fn plant_event(dir: &Path, seq: i64, sig: &str) {
    let conn = rusqlite::Connection::open(dir.join("reeflog.db")).expect("open journal");
    conn.execute(
        "INSERT INTO events(seq,signature,slot,logs,source,first_seen,seal_status,payload_state)
         VALUES(?1,?2,?3,'[\"log\"]','ws',1700000000000,'finalized','held')",
        rusqlite::params![seq, sig, 440000000 + seq],
    )
    .expect("plant event");
    conn.close().expect("close journal");
}

#[test]
fn a_configured_targets_pending_dead_letter_holds_the_exit_code() {
    // D1: the report PRINTS the debt and calls it health. A monitoring
    // caller that polls the exit code read "all clear" while an event sat
    // in the quarantine — the same lie v0.7.29 removed for orphans.
    let dir = unique_dir("status-debt-configured-letter");
    init(&dir);
    let url = "http://127.0.0.1:9971/hook";
    let cfg = write_config(&dir, url);
    plant_configured_letter(&dir, "t1", url);

    let (v, code) = status_report(&dir, &cfg);
    assert_eq!(
        v["targets"][0]["dead_letters"].as_i64(),
        Some(1),
        "the letter must stay visible on its own target's row: {v}"
    );
    assert_eq!(
        v["dead_letters_pending"].as_i64(),
        Some(1),
        "the report must carry the counted debt under the same name `push` \
         uses — one fact, one number across the surfaces: {v}"
    );
    assert_eq!(
        code, 3,
        "a pending quarantine does not drain by itself — the cursor stands \
         past it and only `dlq replay` hands it over — so the report that \
         prints it must hold the exit code like `push` does (stderr: {})",
        stderr_of(&run(
            &dir,
            &[
                "webhook",
                "status",
                "--db",
                "reeflog.db",
                "--config",
                cfg.to_str().expect("cfg path"),
            ]
        ))
    );
}

#[test]
fn a_standing_failure_verdict_holds_the_exit_code() {
    // D2: the standing verdict `last_error` is written by the very block
    // `push` turns into exit 3. The report that prints it must not call it
    // health.
    let dir = unique_dir("status-debt-standing-failure");
    init(&dir);
    let cfg = write_config(&dir, "http://127.0.0.1:9/dead");
    plant_event(&dir, 1, "SIGD2");

    // A REAL blocked pass: the endpoint refuses to connect.
    assert_eq!(
        push_exit(&dir, &cfg),
        3,
        "a blocked target holds the push exit code"
    );
    let (v, code) = status_report(&dir, &cfg);
    assert!(
        v["targets"][0]["last_error"]
            .as_str()
            .is_some_and(|s| !s.is_empty()),
        "the standing verdict IS reported: {v}"
    );
    assert_eq!(
        v["failing_targets"].as_i64(),
        Some(1),
        "the failing targets must be COUNTED, not only named in prose: {v}"
    );
    assert_eq!(
        code, 3,
        "a target that is still failing is not health (stderr: {})",
        stderr_of(&run(
            &dir,
            &[
                "webhook",
                "status",
                "--db",
                "reeflog.db",
                "--config",
                cfg.to_str().expect("cfg path"),
            ]
        ))
    );
}

#[test]
fn status_and_push_agree_on_the_exit_code_for_a_pending_letter() {
    // D3: one fact, one verdict across the surfaces — the rule the repo
    // already holds for the orphan count.
    let dir = unique_dir("status-debt-parity");
    init(&dir);
    let url = "http://127.0.0.1:9971/hook";
    let cfg = write_config(&dir, url);
    plant_configured_letter(&dir, "t1", url);

    let status_code = status_report(&dir, &cfg).1;
    let push_code = push_exit(&dir, &cfg);
    assert_eq!(
        status_code, push_code,
        "the same journal state must not read as health on one surface and \
         as debt on the other (status {status_code}, push {push_code})"
    );
}

#[test]
fn a_clean_journal_still_exits_zero() {
    // G1: a report that always exits 3 is not a report.
    let dir = unique_dir("status-debt-clean");
    init(&dir);
    let cfg = write_config(&dir, "http://127.0.0.1:9971/hook");

    let (v, code) = status_report(&dir, &cfg);
    assert_eq!(
        v["targets"][0]["dead_letters"].as_i64(),
        Some(0),
        "a clean journal holds no quarantined event: {v}"
    );
    assert_eq!(
        v["orphaned_dead_letters"].as_i64(),
        Some(0),
        "a clean journal holds no orphaned letter: {v}"
    );
    assert_eq!(
        v["dead_letters_pending"].as_i64(),
        Some(0),
        "a clean journal holds no pending quarantine: {v}"
    );
    assert_eq!(
        v["failing_targets"].as_i64(),
        Some(0),
        "a clean journal holds no failing target: {v}"
    );
    assert_eq!(code, 0, "a clean journal is health");
}

#[test]
fn an_orphaned_dead_letter_still_holds_the_exit_code() {
    // G2: the v0.7.29 rule must not regress while the line is redrawn.
    let dir = unique_dir("status-debt-orphan-holds");
    init(&dir);
    let cfg = write_config(&dir, "http://127.0.0.1:9971/hook");
    plant_orphan_letter(&dir);

    let (v, code) = status_report(&dir, &cfg);
    assert_eq!(
        v["orphaned_dead_letters"].as_i64(),
        Some(1),
        "the orphan count must still be reported: {v}"
    );
    assert_eq!(code, 3, "a stranded letter still holds the exit code");
}
