//! A numeric flag's value must fit the DOMAIN its consumer uses — the
//! parser's `u64` is not the consumer's number space.
//!
//! Measured 2026-11-08 on the published v0.7.2 tree (HEAD 2c05d31), against
//! a journal with 1200 planted rows:
//!
//!   reeflog tail --db p.db --cursor c.txt --limit 18446744073709551615
//!     -> exit 0, 1000 rows delivered (the DEFAULT limit of 1000). The
//!        parser accepted the value (it is a whole number), the consumer
//!        parsed it as `i64`, the parse FAILED, and the failure became the
//!        default — the exact defect class v0.7.2 was supposed to close,
//!        one layer up: the checker and the consumer disagree about what a
//!        number is.
//!
//!   reeflog verify --db p.db --pages 1 --since-slot 9223372036854775808
//!     -> exit 3, and the report says `"since_slot": null` — the operator's
//!        window was DROPPED, not refused. The scan ran over a different
//!        window than the one that was asked for.
//!
//!   reeflog start --db p.db --seconds 18446744073709551615
//!     -> panic: "overflow when adding duration to instant" (src/main.rs:
//!        534). The daemon died with a Rust backtrace instead of a refusal.
//!
//!   reeflog start --db p.db --repair-every 18446744073709551615
//!     -> panic: "overflow when adding duration to instant" (src/main.rs:
//!        657) — the scheduler, not the deadline.
//!
//! The rule:
//!
//! * a COUNT/SLOT flag must fit the signed 64-bit space its consumer uses
//!   (SQLite `LIMIT`, slot arithmetic);
//! * a DURATION flag must fit the space a clock can represent;
//! * a value outside the domain is a REFUSAL that names the flag, the
//!   value and the domain — before any journal is opened;
//! * no consumer may ever fall back to a default because a parse failed:
//!   the parser has already refused what cannot be parsed, so a failure
//!   here is a bug and must stop the run, not change its meaning.
//!
//! The guards matter as much: `0` stays legal, a value inside the domain
//! still means exactly what it meant, and the bare `--anchor-slot` keeps
//! its documented meaning.

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

/// Run with a deadline. The pre-fix tree does not always fail loudly here:
/// an unschedulable cadence value makes the daemon HANG (the panic happens
/// on the async runtime's worker while the main task awaits a deadline that
/// never comes). A test that waits for such a run would hang the suite, so
/// the run is killed and reported as a timeout — which is a failure, not a
/// pass.
fn run_bounded(dir: &Path, args: &[&str], secs: u64) -> Option<Output> {
    let mut child = Command::new(env!("CARGO_BIN_EXE_reeflog"))
        .args(args)
        .current_dir(dir)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("reeflog binary runs");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(secs);
    loop {
        match child.try_wait().expect("wait") {
            Some(_) => {
                return Some(child.wait_with_output().expect("collect output"));
            }
            None => {
                if std::time::Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    return None;
                }
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
        }
    }
}

fn stderr_of(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).to_lowercase()
}

fn stdout_of(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// A journal that exists, with a program, and enough rows that a limit
/// above the default is visible in the output.
fn with_rows(tag: &str, rows: i64) -> PathBuf {
    let dir = unique_dir(tag);
    let out = run(
        &dir,
        &["init", "--db", "reeflog.db", "--program", VALID_PROGRAM],
    );
    assert!(
        out.status.success(),
        "init must succeed (stderr: {})",
        String::from_utf8_lossy(&out.stderr)
    );
    let conn = rusqlite::Connection::open(dir.join("reeflog.db")).expect("open journal");
    for i in 0..rows {
        conn.execute(
            "INSERT INTO events(signature, slot, first_seen, source, logs)
             VALUES(?1, ?2, ?3, 'ws', '[\"log\"]')",
            rusqlite::params![
                format!("Sig{:043}", i),
                44_000_000i64 + i,
                1_700_000_000_000i64
            ],
        )
        .expect("plant row");
    }
    conn.close().expect("close journal");
    dir
}

fn delivered_rows(out: &Output) -> usize {
    stdout_of(out)
        .lines()
        .filter(|l| l.starts_with('{'))
        .count()
}

/// A refusal must name the flag and the offending value, and it must not be
/// a panic: a Rust backtrace is not a message an operator can act on.
fn assert_domain_refusal(out: &Output, flag: &str, value: &str) {
    let err = stderr_of(out);
    assert!(
        !err.contains("panicked"),
        "the run must not panic (a backtrace is not a refusal), got: {err}"
    );
    assert!(
        !out.status.success(),
        "a value outside the domain must not run the command (stdout: {})",
        stdout_of(out)
    );
    assert!(
        err.contains(flag),
        "the refusal must name the flag {flag}, got: {err}"
    );
    assert!(
        err.contains(value),
        "the refusal must name the offending value {value}, got: {err}"
    );
}

/// The same check, for a run that had to be killed: a run that never ends is
/// not a refusal.
fn assert_domain_refusal_bounded(
    out: Option<Output>,
    flag: &str,
    value: &str,
) {
    let out = match out {
        Some(o) => o,
        None => panic!(
            "the run with {flag} {value} never ended — it must be refused, not hang"
        ),
    };
    assert_domain_refusal(&out, flag, value);
}

// --- the defect: a value outside the consumer's number space ---------------

#[test]
fn a_limit_above_the_signed_range_is_refused_instead_of_becoming_the_default() {
    let dir = with_rows("limit-domain", 1200);
    let out = run(
        &dir,
        &[
            "tail",
            "--db",
            "reeflog.db",
            "--cursor",
            "c.txt",
            "--limit",
            "18446744073709551615",
        ],
    );
    assert_domain_refusal(&out, "--limit", "18446744073709551615");
    assert_eq!(
        delivered_rows(&out),
        0,
        "a refused limit must not deliver anything"
    );
}

#[test]
fn a_hydrate_limit_above_the_signed_range_is_refused() {
    let dir = with_rows("hydrate-limit-domain", 5);
    let out = run(
        &dir,
        &["hydrate", "--db", "reeflog.db", "--limit", "9223372036854775808"],
    );
    assert_domain_refusal(&out, "--limit", "9223372036854775808");
}

#[test]
fn a_since_slot_above_the_signed_range_is_refused_instead_of_being_dropped() {
    let dir = with_rows("since-slot-domain", 5);
    let out = run(
        &dir,
        &[
            "verify",
            "--db",
            "reeflog.db",
            "--program",
            VALID_PROGRAM,
            "--pages",
            "1",
            "--since-slot",
            "9223372036854775808",
        ],
    );
    assert_domain_refusal(&out, "--since-slot", "9223372036854775808");
    assert!(
        !stdout_of(&out).contains("\"since_slot\": null"),
        "the refused window must not be reported as \"no window\" — that is a \
         scan over a DIFFERENT window than the one that was asked for"
    );
}

#[test]
fn an_unschedulable_seconds_value_is_refused_instead_of_panicking() {
    let dir = unique_dir("seconds-domain");
    let out = run_bounded(
        &dir,
        &[
            "start",
            "--db",
            "reeflog.db",
            "--program",
            VALID_PROGRAM,
            "--seconds",
            "18446744073709551615",
            "--rpc",
            "http://127.0.0.1:9",
            "--ws",
            "ws://127.0.0.1:9",
        ],
        15,
    );
    assert_domain_refusal_bounded(out, "--seconds", "18446744073709551615");
}

#[test]
fn an_unschedulable_scheduler_value_is_refused_instead_of_panicking() {
    let dir = unique_dir("scheduler-domain");
    let out = run_bounded(
        &dir,
        &[
            "start",
            "--db",
            "reeflog.db",
            "--program",
            VALID_PROGRAM,
            "--seconds",
            "1",
            "--repair-every",
            "18446744073709551615",
            "--rpc",
            "http://127.0.0.1:9",
            "--ws",
            "ws://127.0.0.1:9",
        ],
        15,
    );
    assert_domain_refusal_bounded(out, "--repair-every", "18446744073709551615");
}

/// The DURATION gate on its own. `i64::MAX` seconds is exactly the largest
/// value the count-range gate accepts, but it is NOT schedulable: the
/// largest delay this host accepts measured 9223372036852975902 s, about 21
/// days below `i64::MAX`. So this case isolates the schedulability check —
/// it fails if that check is dropped or inverted while the range check is
/// still in place, which is how the two gates are pinned apart.
#[test]
fn a_count_legal_but_unschedulable_delay_is_refused() {
    let dir = unique_dir("seconds-unschedulable");
    let out = run_bounded(
        &dir,
        &[
            "start",
            "--db",
            "reeflog.db",
            "--program",
            VALID_PROGRAM,
            "--seconds",
            "9223372036854775807",
            "--rpc",
            "http://127.0.0.1:9",
            "--ws",
            "ws://127.0.0.1:9",
        ],
        15,
    );
    let err = match &out {
        Some(o) => stderr_of(o),
        None => String::from("<the run never ended>"),
    };
    assert_domain_refusal_bounded(out, "--seconds", "9223372036854775807");
    assert!(
        err.contains("schedule"),
        "the refusal must say the value cannot be scheduled, got: {err}"
    );
}

// --- guards: the documented behaviour must not change ----------------------

#[test]
fn a_limit_inside_the_domain_still_delivers_exactly_that_many_rows() {
    let dir = with_rows("limit-guard", 1200);
    let out = run(
        &dir,
        &["tail", "--db", "reeflog.db", "--cursor", "c.txt", "--limit", "5"],
    );
    assert!(
        out.status.success(),
        "a legal limit must succeed (stderr: {})",
        stderr_of(&out)
    );
    assert_eq!(delivered_rows(&out), 5, "exactly the requested rows");
}

#[test]
fn a_zero_limit_is_still_legal() {
    let dir = with_rows("limit-zero-guard", 5);
    let out = run(
        &dir,
        &["tail", "--db", "reeflog.db", "--cursor", "c.txt", "--limit", "0"],
    );
    assert!(
        out.status.success(),
        "0 stays legal (stderr: {})",
        stderr_of(&out)
    );
    assert_eq!(delivered_rows(&out), 0, "0 rows");
}

#[test]
fn a_since_slot_inside_the_domain_is_still_honoured() {
    let dir = with_rows("since-slot-guard", 5);
    let out = run(
        &dir,
        &[
            "verify",
            "--db",
            "reeflog.db",
            "--program",
            VALID_PROGRAM,
            "--pages",
            "1",
            "--since-slot",
            "440000001",
        ],
    );
    assert!(
        stdout_of(&out).contains("\"since_slot\": 440000001"),
        "the requested window must be reported back (stdout: {})",
        stdout_of(&out)
    );
}

#[test]
fn the_bare_anchor_slot_keeps_its_meaning() {
    let dir = with_rows("anchor-bare-guard", 5);
    let out = run(
        &dir,
        &[
            "verify",
            "--db",
            "reeflog.db",
            "--program",
            VALID_PROGRAM,
            "--pages",
            "1",
            "--anchor-slot",
        ],
    );
    assert!(
        !stderr_of(&out).contains("anchor-slot"),
        "the bare --anchor-slot must not be turned into a refusal (stderr: {})",
        stderr_of(&out)
    );
}
