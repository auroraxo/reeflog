//! The stranger path: what a third party does AFTER `cargo install`.
//!
//! These tests drive the real binary the way a first-time user drives it,
//! in an empty directory, with no checkout and no fixtures. They are about
//! the SILENCE of the tool, not about its throughput:
//!
//!   1. A program address that is not an address must not be accepted. A
//!      typo'd `--program` that is silently accepted produces a mailbox that
//!      watches nothing and reports health — the most expensive kind of
//!      quiet failure this product can make, because every later number
//!      ("0 events", "0 gaps") is true and useless.
//!
//!   2. A command that needs a journal must not CREATE one. `status` on a
//!      path that does not exist currently prints a complete, healthy,
//!      empty status and exits 0. A third party cannot tell "no events
//!      yet" from "you never initialized a mailbox".
//!
//!   3. The refusal must NAME THE FIX. "missing --program <value>" does
//!      not tell a stranger what to run; the init line does.
//!
//! These are CLI-level tests on purpose: the defect lives in the surface a
//! stranger touches, not in a library function.

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

#[test]
fn init_rejects_a_program_that_is_not_an_address() {
    // A garbage program is what a typo looks like. It must be refused AT
    // INIT, because after init the journal is a healthy-looking empty
    // mailbox and nobody will look again.
    let dir = unique_dir("init-garbage");

    let out = run(&dir, &["init", "--db", "reeflog.db", "--program", "not-a-pubkey"]);

    assert!(
        !out.status.success(),
        "init must refuse a --program that is not a Solana address (stdout: {})",
        String::from_utf8_lossy(&out.stdout)
    );
    let err = stderr_of(&out);
    assert!(
        err.contains("program"),
        "the refusal must name the offending flag, got: {err}"
    );
    assert!(
        !dir.join("reeflog.db").exists(),
        "a refused init must not leave a journal behind — the stranger must not \
         find a mailbox they never asked for"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn init_accepts_a_real_address() {
    // The false-positive guard: this must keep working. A validator that
    // rejects valid input is worse than no validator.
    let dir = unique_dir("init-valid");

    let out = run(&dir, &["init", "--db", "reeflog.db", "--program", VALID_PROGRAM]);

    assert!(
        out.status.success(),
        "init must accept a real address (stderr: {})",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        dir.join("reeflog.db").exists(),
        "a successful init must leave the journal behind"
    );
    assert!(
        String::from_utf8_lossy(&out.stdout).contains(VALID_PROGRAM),
        "init must echo the program it watches"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn status_on_a_missing_journal_refuses_instead_of_reporting_health() {
    // The defect: `status --db nope.db` printed a complete, healthy,
    // empty status and exited 0. "0 events, 0 gaps" is indistinguishable
    // from a working mailbox that has not seen traffic.
    let dir = unique_dir("status-missing");

    let out = run(&dir, &["status", "--db", "nope.db"]);

    assert!(
        !out.status.success(),
        "status on a journal that does not exist must not report success (stdout: {})",
        String::from_utf8_lossy(&out.stdout)
    );
    let err = stderr_of(&out);
    assert!(
        err.contains("init"),
        "the refusal must name the fix (`reeflog init`), got: {err}"
    );
    assert!(
        !dir.join("nope.db").exists(),
        "status must not CREATE the journal it was asked about"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn tail_on_a_missing_journal_refuses_instead_of_reporting_zero_events() {
    // Same defect on the delivery path: `tail` on a missing journal
    // printed "0 events after cursor 0" and exited 0, which a consumer
    // reads as "the mailbox is working and there is nothing to deliver".
    let dir = unique_dir("tail-missing");

    let out = run(&dir, &["tail", "--db", "nope.db", "--cursor", "./c.txt"]);

    assert!(
        !out.status.success(),
        "tail on a journal that does not exist must not report success (stdout: {})",
        String::from_utf8_lossy(&out.stdout)
    );
    let err = stderr_of(&out);
    assert!(
        err.contains("init"),
        "the refusal must name the fix (`reeflog init`), got: {err}"
    );
    assert!(
        !dir.join("nope.db").exists(),
        "tail must not CREATE the journal it was asked about"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn start_refuses_a_program_that_is_not_an_address() {
    // `start --program` may create a journal (the README allows it), but it
    // may not accept a garbage address either — that is the same typo, one
    // command later.
    let dir = unique_dir("start-garbage");

    let out = run(&dir, &["start", "--db", "reeflog.db", "--program", "not-a-pubkey"]);

    assert!(
        !out.status.success(),
        "start must refuse a --program that is not a Solana address (stdout: {})",
        String::from_utf8_lossy(&out.stdout)
    );
    let err = stderr_of(&out);
    assert!(
        err.contains("program"),
        "the refusal must name the offending flag, got: {err}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn the_bare_binary_prints_usage_and_fails() {
    // The false-positive guard for the guard: an unknown command must still
    // exit non-zero with the usage text, not be swallowed by the new
    // "missing journal" path.
    let dir = unique_dir("bare");

    let out = run(&dir, &[]);

    assert!(!out.status.success(), "the bare binary must not exit 0");
    let err = stderr_of(&out);
    assert!(err.contains("usage"), "the bare binary must print usage, got: {err}");

    let _ = std::fs::remove_dir_all(&dir);
}
