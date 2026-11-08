//! A flag VALUE that is not a number must be a REFUSAL, not a fallback to
//! the default.
//!
//! Measured 2026-11-08 on the published v0.7.1 tree (HEAD 1fd2361), against
//! a journal with 650 real mainnet events:
//!
//!   reeflog tail --db p.db --cursor c.txt --limit abc
//!     -> exit 0, 650 rows delivered (the DEFAULT limit of 1000), no complaint
//!   reeflog tail --db p.db --cursor c.txt --limit -1
//!     -> exit 0, 650 rows delivered — SQL's `LIMIT -1` means UNLIMITED
//!   reeflog verify --db p.db --pages abc
//!     -> exit 3, the scan ran with the DEFAULT page budget of 40
//!   reeflog start --db p.db --seconds abc
//!     -> the daemon ran for the DEFAULT 60 seconds
//!
//! This is the same defect class the strict flag parser already fixed for
//! FLAGS (v0.5.1, 9dbd393): "a typo in `--pages` does not fail — it produces
//! a scan with a different budget than the operator asked for, and the output
//! looks exactly like a successful run". The parser refuses an unknown flag
//! BEFORE anything is opened; it then hands the VALUE of a known flag to
//! `arg(...).and_then(|s| s.parse().ok()).unwrap_or(DEFAULT)` — and every
//! unparseable value silently becomes the default. The tool says it did
//! something it did not do, which is the most expensive failure this product
//! can have: a consumer that asked for 5 events and got 650 has no way to
//! tell from the output.
//!
//! A negative count is the same lie with a sharper edge: `--limit -1` is not
//! "the last event", it is "no limit at all" to SQLite.
//!
//! The rule, applied before any journal is opened or created:
//!
//! * a numeric flag whose value is not a whole number is a REFUSAL that names
//!   the flag and the value;
//! * a negative value is a REFUSAL that names the flag;
//! * `0` stays legal — several flags document `0` as "disable this pass";
//! * a flag that is not numeric (`--db`, `--rpc`, `--config`, ...) is
//!   untouched;
//! * `--anchor-slot` WITHOUT a value keeps its documented meaning (anchor at
//!   the journal's own window), and WITH a value it must be a slot number.
//!
//! The false-positive guards matter as much: every documented invocation
//! must keep working, `0` must stay legal, and the bare `--anchor-slot` must
//! not be turned into a refusal.

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
    String::from_utf8_lossy(&out.stderr).to_lowercase()
}

fn stdout_of(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).to_lowercase()
}

/// A journal that exists, with a program, so the command under test gets
/// past the "no journal at ..." gate and reaches the value handling.
fn with_journal(tag: &str) -> PathBuf {
    let dir = unique_dir(tag);
    let out = run(&dir, &["init", "--db", "reeflog.db", "--program", VALID_PROGRAM]);
    assert!(
        out.status.success(),
        "init must succeed (stderr: {})",
        String::from_utf8_lossy(&out.stderr)
    );
    dir
}

/// The refusal must say what was wrong and which flag carried the bad value.
/// An operator who typed `--limit abc` needs both halves.
fn assert_value_refusal(out: &Output, flag: &str, value: &str) {
    assert!(
        !out.status.success(),
        "a non-numeric {flag} value must not run the command (stdout: {})",
        String::from_utf8_lossy(&out.stdout)
    );
    let err = stderr_of(out);
    assert!(
        err.contains(flag),
        "the refusal must name the flag {flag}, got: {err}"
    );
    assert!(
        err.contains(value),
        "the refusal must name the offending value {value}, got: {err}"
    );
    assert!(
        err.contains("not a number") || err.contains("not a valid") || err.contains("must be"),
        "the refusal must say what a valid value looks like, got: {err}"
    );
}

// --- the defect: an unparseable value becomes the default ------------------

#[test]
fn a_non_numeric_limit_is_refused_instead_of_becoming_the_default() {
    let dir = with_journal("non-numeric-limit");

    let out = run(&dir, &["tail", "--db", "reeflog.db", "--cursor", "c.txt", "--limit", "abc"]);

    assert_value_refusal(&out, "--limit", "abc");
    assert!(
        !stdout_of(&out).contains("\"signature\""),
        "no row may be delivered by a refused command"
    );
    assert!(
        !dir.join("c.txt").exists(),
        "a refused command must not advance (or create) a cursor"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_non_numeric_page_budget_is_refused() {
    let dir = with_journal("non-numeric-pages");

    let out = run(&dir, &["verify", "--db", "reeflog.db", "--pages", "abc"]);

    assert_value_refusal(&out, "--pages", "abc");
    assert!(
        !stdout_of(&out).contains("\"complete\""),
        "no scan may run when a value was not understood"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_non_numeric_duration_is_refused_before_the_daemon_starts() {
    let dir = with_journal("non-numeric-seconds");

    let out = run(&dir, &["start", "--db", "reeflog.db", "--seconds", "abc"]);

    assert_value_refusal(&out, "--seconds", "abc");

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_non_numeric_hydrate_budget_is_refused() {
    let dir = with_journal("non-numeric-hydrate");

    let out = run(&dir, &["hydrate", "--db", "reeflog.db", "--limit", "abc"]);

    assert_value_refusal(&out, "--limit", "abc");

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_non_numeric_reorg_budget_is_refused() {
    let dir = with_journal("non-numeric-reorg");

    let out = run(&dir, &["reorg", "check", "--db", "reeflog.db", "--limit", "abc"]);

    assert_value_refusal(&out, "--limit", "abc");

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn the_value_is_checked_before_the_other_arguments_are_honoured() {
    // A webhook push with a missing config file would otherwise die with
    // "config not found" — a refusal about something else. The value is the
    // operator's mistake and must be named first: the command line is
    // rejected as a whole, before any of it is acted upon.
    let dir = with_journal("value-before-config");

    let out = run(
        &dir,
        &[
            "webhook",
            "push",
            "--db",
            "reeflog.db",
            "--config",
            "does-not-exist.json",
            "--limit",
            "abc",
        ],
    );

    assert_value_refusal(&out, "--limit", "abc");

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_negative_limit_is_refused_instead_of_being_unlimited() {
    let dir = with_journal("negative-limit");

    let out = run(&dir, &["tail", "--db", "reeflog.db", "--cursor", "c.txt", "--limit", "-1"]);

    assert!(
        !out.status.success(),
        "a negative limit must not run the command (stdout: {})",
        String::from_utf8_lossy(&out.stdout)
    );
    let err = stderr_of(&out);
    assert!(
        err.contains("--limit"),
        "the refusal must name the flag, got: {err}"
    );
    assert!(
        err.contains("-1"),
        "the refusal must name the offending value, got: {err}"
    );
    assert!(
        !dir.join("c.txt").exists(),
        "a refused command must not advance (or create) a cursor"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_negative_page_budget_is_refused() {
    let dir = with_journal("negative-pages");

    let out = run(&dir, &["verify", "--db", "reeflog.db", "--pages", "-5"]);

    assert!(
        !out.status.success(),
        "a negative page budget must not run the scan"
    );
    assert!(
        stderr_of(&out).contains("--pages"),
        "the refusal must name the flag"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

// --- guards: the documented behaviour must keep working --------------------

#[test]
fn a_numeric_limit_still_works() {
    // False-positive guard: the refusal must not turn a legal value into a
    // failure. This is the only check here that needs no network.
    let dir = with_journal("guard-numeric-limit");

    let out = run(&dir, &["tail", "--db", "reeflog.db", "--cursor", "c.txt", "--limit", "5"]);

    assert!(
        out.status.success(),
        "a numeric --limit must still work (stderr: {})",
        String::from_utf8_lossy(&out.stderr)
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn zero_stays_legal() {
    // Several flags document `0` as "disable this pass" (`--payload-limit 0`,
    // `--repair-pages 0`). A refusal that also swallowed `0` would break a
    // documented meaning.
    let dir = with_journal("guard-zero");

    let out = run(&dir, &["hydrate", "--db", "reeflog.db", "--limit", "0"]);

    assert!(
        !stderr_of(&out).contains("not a number"),
        "0 is a legal value (stderr: {})",
        String::from_utf8_lossy(&out.stderr)
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn the_bare_anchor_slot_still_means_the_journals_own_window() {
    // The README documents `--anchor-slot` WITHOUT a value. It must not be
    // turned into "needs a value" by a numeric check.
    let dir = with_journal("guard-bare-anchor");

    let out = run(&dir, &["verify", "--db", "reeflog.db", "--pages", "1", "--anchor-slot"]);

    let err = stderr_of(&out);
    assert!(
        !err.contains("not a number") && !err.contains("needs a value"),
        "the bare --anchor-slot must stay accepted (stderr: {err})"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn an_anchor_slot_with_a_number_stays_accepted() {
    let dir = with_journal("guard-anchor-number");

    let out = run(&dir, &["verify", "--db", "reeflog.db", "--pages", "1", "--anchor-slot", "400000000"]);

    let err = stderr_of(&out);
    assert!(
        !err.contains("not a number"),
        "a numeric --anchor-slot must stay accepted (stderr: {err})"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_non_numeric_flag_is_still_refused_as_a_flag() {
    // The old refusal must survive the new one: an unknown flag is not a
    // number problem.
    let dir = with_journal("guard-unknown-flag");

    let out = run(&dir, &["status", "--db", "reeflog.db", "--bogus-flag"]);

    assert!(
        !out.status.success(),
        "an unknown flag must still be refused"
    );
    assert!(
        stderr_of(&out).contains("unknown flag"),
        "the refusal must still be about the unknown flag (stderr: {})",
        String::from_utf8_lossy(&out.stderr)
    );

    let _ = std::fs::remove_dir_all(&dir);
}
