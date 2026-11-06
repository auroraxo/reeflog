//! A flag the CLI does not know must not be ignored.
//!
//! Measured 2026-11-06 on the pre-fix tree (HEAD e5a7f77, v0.5.0):
//!
//!   reeflog verify --db reeflog.db --pagesx 400   -> exit 3, the scan ran
//!   reeflog tail  --db reeflog.db --cursor c.txt --limitt 5 -> exit 0
//!   reeflog status --dbx nope.db                    -> exit 0, read the DEFAULT db
//!   reeflog status --db=reeflog.db                  -> exit 0, read the DEFAULT db
//!   reeflog status --help                           -> exit 0, ran `status`
//!   reeflog init --db probe.db                      -> "missing --program", but
//!                                                       probe.db was LEFT BEHIND
//!
//! Every one of those is the same kind of defect, and it is the most
//! expensive kind this product can have: the tool says it did something it
//! did not do. `--pagesx 400` scans with the DEFAULT page budget, not the
//! 400 the operator asked for, and reports the result as if it had. A typo
//! is invisible — the output looks exactly like a successful run.
//!
//! The rule the stranger needs: a flag the CLI does not know is a REFUSAL
//! with the likely intent named, a flag that is missing its value is a
//! REFUSAL, and nothing that was not asked for may run. The refusal must
//! happen BEFORE any state is created — a refused `init` must not leave a
//! journal behind (the existing quickstart contract, which only covered a
//! garbage `--program`).
//!
//! The false-positive guards matter as much: every documented invocation
//! must keep working, including the bare `--anchor-slot` (no value — the
//! README documents it as "anchor at the journal's own window"), the
//! `--flag=value` form, and the global `--help` / `--version`.

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
/// past the "no journal at ..." gate and reaches the flag handling.
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

// --- the defect: unknown flags run as something else ------------------------

#[test]
fn an_unknown_flag_is_refused_instead_of_being_ignored() {
    let dir = with_journal("unknown-flag");

    let out = run(&dir, &["status", "--db", "reeflog.db", "--bogus-flag"]);

    assert!(
        !out.status.success(),
        "an unknown flag must not be accepted (stdout: {})",
        String::from_utf8_lossy(&out.stdout)
    );
    let err = stderr_of(&out);
    assert!(
        err.contains("--bogus-flag"),
        "the refusal must name the offending flag, got: {err}"
    );
    assert!(
        !stdout_of(&out).contains("\"account_data_held\""),
        "the command must not have run: a refused flag must not produce a status report"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_typo_in_a_real_flag_is_refused_and_the_intent_is_named() {
    // `--pagesx` is the shape a real typo has. The refusal must point at
    // `--pages`, because "unknown flag" alone leaves the operator guessing.
    let dir = with_journal("typo-flag");

    let out = run(&dir, &["verify", "--db", "reeflog.db", "--pagesx", "400"]);

    assert!(
        !out.status.success(),
        "a typo'd flag must not run the command (stdout: {})",
        String::from_utf8_lossy(&out.stdout)
    );
    let err = stderr_of(&out);
    assert!(
        err.contains("--pagesx"),
        "the refusal must name the offending flag, got: {err}"
    );
    // The intent must be named as a SEPARATE flag. Asserting that the error
    // contains "--pages" is vacuous: "--pagesx" already contains that
    // substring, so the check passed even with the suggestion removed. The
    // suggestion is the whole point of the refusal — without it the operator
    // is left guessing. The mutation ladder caught exactly this.
    assert!(
        err.contains("did you mean") && err.contains("--pages?"),
        "the refusal must name the flag that was probably meant, got: {err}"
    );
    assert!(
        !stdout_of(&out).contains("\"complete\""),
        "no scan may run when a flag was not understood"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_typo_in_a_value_flag_is_refused_too() {
    let dir = with_journal("typo-cursor");

    let out = run(&dir, &["tail", "--db", "reeflog.db", "--cursor", "./c.txt", "--limitt", "5"]);

    assert!(
        !out.status.success(),
        "a typo'd flag must not be accepted (stdout: {})",
        String::from_utf8_lossy(&out.stdout)
    );
    assert!(
        stderr_of(&out).contains("--limitt"),
        "the refusal must name the offending flag"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_typo_in_the_db_flag_does_not_fall_back_to_the_default_journal() {
    // The nastiest shape: `--dbx nope.db` used to read the DEFAULT
    // `reeflog.db` and print a healthy status for it. The operator asked
    // about one journal and got a report about another.
    let dir = with_journal("typo-db");

    let out = run(&dir, &["status", "--dbx", "nope.db"]);

    assert!(
        !out.status.success(),
        "a typo'd --db must not silently read another journal (stdout: {})",
        String::from_utf8_lossy(&out.stdout)
    );
    assert!(
        stderr_of(&out).contains("--dbx"),
        "the refusal must name the offending flag"
    );
    assert!(
        stderr_of(&out).contains("did you mean") && stderr_of(&out).contains("--db?"),
        "the refusal must name the flag that was probably meant"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

// --- the inline form: `--flag=value` must work, not fall back --------------

#[test]
fn the_inline_flag_form_is_honoured_instead_of_falling_back_to_the_default() {
    let dir = unique_dir("inline-db");
    // Two journals with different programs. The inline form must select the
    // one that was named.
    let out = run(
        &dir,
        &["init", "--db", "other.db", "--program", VALID_PROGRAM],
    );
    assert!(out.status.success(), "init must succeed");
    let out = run(&dir, &["init", "--db", "reeflog.db", "--program", VALID_PROGRAM]);
    assert!(out.status.success(), "init must succeed");

    // `--db=other.db` — pre-fix this read the DEFAULT `reeflog.db`.
    let out = run(&dir, &["status", "--db=other.db"]);

    assert!(
        out.status.success(),
        "the inline form must be accepted (stderr: {})",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        stdout_of(&out).contains("other.db"),
        "the inline form must select the journal it names, got: {}",
        String::from_utf8_lossy(&out.stdout)
    );

    let _ = std::fs::remove_dir_all(&dir);
}

// --- missing values --------------------------------------------------------

#[test]
fn a_flag_without_its_value_is_refused() {
    let dir = with_journal("missing-value");

    let out = run(&dir, &["verify", "--db", "reeflog.db", "--pages"]);

    assert!(
        !out.status.success(),
        "a flag without its value must not silently use the default (stdout: {})",
        String::from_utf8_lossy(&out.stdout)
    );
    let err = stderr_of(&out);
    assert!(
        err.contains("--pages"),
        "the refusal must name the flag that lost its value, got: {err}"
    );
    assert!(
        !stdout_of(&out).contains("\"complete\""),
        "no scan may run when a flag lost its value"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_flag_whose_value_is_another_flag_is_refused() {
    // `--db --program` used to make the db path literally "--program".
    let dir = with_journal("value-is-flag");

    let out = run(&dir, &["status", "--db", "--program"]);

    assert!(
        !out.status.success(),
        "a flag value must not swallow the next flag (stdout: {})",
        String::from_utf8_lossy(&out.stdout)
    );
    assert!(
        stderr_of(&out).contains("--db"),
        "the refusal must name the flag that lost its value"
    );
    assert!(
        !dir.join("--program").exists(),
        "no file may be created in place of a flag value"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

// --- nothing may run that was not asked for --------------------------------

#[test]
fn a_refused_init_leaves_no_journal_behind() {
    // The quickstart contract already demands this for a garbage
    // `--program`. It was NOT true for a MISSING `--program`: the journal
    // was created first and the refusal came second.
    let dir = unique_dir("init-missing-program");

    let out = run(&dir, &["init", "--db", "probe.db"]);

    assert!(
        !out.status.success(),
        "init without --program must fail (stdout: {})",
        String::from_utf8_lossy(&out.stdout)
    );
    assert!(
        stderr_of(&out).contains("program"),
        "the refusal must name what is missing"
    );
    assert!(
        !dir.join("probe.db").exists(),
        "a refused init must not leave a journal behind"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn an_init_with_an_unknown_flag_leaves_no_journal_behind() {
    let dir = unique_dir("init-unknown-flag");

    let out = run(
        &dir,
        &["init", "--db", "probe.db", "--program", VALID_PROGRAM, "--nope"],
    );

    assert!(
        !out.status.success(),
        "init with an unknown flag must fail (stdout: {})",
        String::from_utf8_lossy(&out.stdout)
    );
    assert!(
        stderr_of(&out).contains("--nope"),
        "the refusal must name the offending flag"
    );
    assert!(
        !dir.join("probe.db").exists(),
        "a refused init must not leave a journal behind"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_given_flag_given_twice_is_refused() {
    // Two `--db` values mean two journals; picking one silently is a guess.
    let dir = with_journal("duplicate-flag");

    let out = run(&dir, &["status", "--db", "reeflog.db", "--db", "other.db"]);

    assert!(
        !out.status.success(),
        "a repeated flag must not be silently resolved (stdout: {})",
        String::from_utf8_lossy(&out.stdout)
    );
    assert!(
        stderr_of(&out).contains("--db"),
        "the refusal must name the repeated flag"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_stray_positional_argument_is_refused() {
    let dir = with_journal("stray-positional");

    let out = run(&dir, &["status", "--db", "reeflog.db", "extra"]);

    assert!(
        !out.status.success(),
        "an argument that is not a flag must not be ignored (stdout: {})",
        String::from_utf8_lossy(&out.stdout)
    );

    let _ = std::fs::remove_dir_all(&dir);
}

// --- help and version ------------------------------------------------------

#[test]
fn help_prints_the_usage_instead_of_running_the_command() {
    let dir = with_journal("help");

    let out = run(&dir, &["status", "--help"]);

    assert!(
        out.status.success(),
        "--help must exit 0 (stderr: {})",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = stdout_of(&out);
    assert!(
        stdout.contains("usage"),
        "--help must print the usage, got: {stdout}"
    );
    assert!(
        !stdout.contains("\"account_data_held\""),
        "--help must not run the command it was asked about"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

// --- the false-positive guards: everything documented must keep working ----

#[test]
fn every_documented_flag_still_works() {
    // The guard for the guard. A strict parser that refuses a documented
    // flag is worse than the defect it fixes.
    let dir = with_journal("documented-flags");

    let cases: Vec<Vec<&str>> = vec![
        vec!["status", "--db", "reeflog.db"],
        vec!["tail", "--db", "reeflog.db", "--cursor", "./c.txt"],
        vec![
            "tail",
            "--db",
            "reeflog.db",
            "--cursor",
            "./c.txt",
            "--limit",
            "10",
        ],
        vec![
            "tail",
            "--db",
            "reeflog.db",
            "--cursor",
            "./c.txt",
            "--no-logs",
        ],
        vec![
            "tail",
            "--db",
            "reeflog.db",
            "--cursor",
            "./c.txt",
            "--with-accounts",
        ],
        vec!["hydrate", "--db", "reeflog.db", "--accounts"],
        vec!["hydrate", "--db", "reeflog.db", "--owners"],
        vec!["hydrate", "--db", "reeflog.db", "--account-data"],
        vec!["hydrate", "--db", "reeflog.db", "--limit", "10"],
        vec!["seal", "--db", "reeflog.db"],
        vec!["verify", "--db", "reeflog.db", "--pages", "1"],
        vec!["repair", "--db", "reeflog.db", "--pages", "1"],
        vec!["verify", "--db", "reeflog.db", "--pages", "1", "--since-slot", "1"],
        vec!["reorg", "check", "--db", "reeflog.db", "--limit", "10"],
        vec!["reorg", "tail", "--db", "reeflog.db", "--cursor", "./r.txt"],
    ];

    for args in cases {
        let out = run(&dir, &args);
        let err = stderr_of(&out);
        // The assertion is about FLAG HANDLING, not about the exit code:
        // `verify` on an empty journal legitimately exits 3 ("the scan
        // proves nothing"), and that must stay possible. What must NOT
        // happen is a refusal of the flag itself.
        assert!(
            !err.contains("unknown flag"),
            "`reeflog {}` was refused as an unknown flag: {err}",
            args.join(" ")
        );
        assert!(
            !err.contains("needs a value") && !err.contains("missing --"),
            "`reeflog {}` was refused for a missing value: {err}",
            args.join(" ")
        );
        assert!(
            !err.contains("usage"),
            "`reeflog {}` fell through to the usage text: {err}",
            args.join(" ")
        );
    }

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn the_bare_anchor_slot_still_means_the_journals_own_window() {
    // The README documents `--anchor-slot` WITHOUT a value: "anchors at the
    // journal's own window". A strict parser that demands a value here
    // would break a documented form.
    let dir = with_journal("bare-anchor");

    let out = run(&dir, &["verify", "--db", "reeflog.db", "--pages", "1", "--anchor-slot"]);

    let err = stderr_of(&out);
    assert!(
        !err.contains("unknown flag"),
        "the bare --anchor-slot must be accepted, got: {err}"
    );
    assert!(
        !err.contains("needs a value"),
        "the bare --anchor-slot must not demand a value, got: {err}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn the_anchor_slot_with_a_value_still_works() {
    let dir = with_journal("anchor-value");

    let out = run(
        &dir,
        &[
            "verify",
            "--db",
            "reeflog.db",
            "--pages",
            "1",
            "--anchor-slot",
            "400000000",
            "--anchor-tries",
            "1",
        ],
    );

    let err = stderr_of(&out);
    assert!(
        !err.contains("unknown flag"),
        "`--anchor-slot <n>` must be accepted, got: {err}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn the_version_flag_still_works_without_a_journal() {
    let dir = unique_dir("version-flag");

    let out = run(&dir, &["--version"]);

    assert!(out.status.success(), "--version must exit 0");
    assert!(
        stdout_of(&out).contains(env!("CARGO_PKG_VERSION")),
        "--version must report the crate version"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn an_unknown_command_still_prints_the_usage_and_fails() {
    // The unknown-COMMAND path must stay the usage path — a strict flag
    // check must not swallow it.
    let dir = unique_dir("unknown-command");

    let out = run(&dir, &["frobnicate"]);

    assert!(!out.status.success(), "an unknown command must fail");
    assert!(
        stderr_of(&out).contains("usage"),
        "an unknown command must print the usage"
    );

    let _ = std::fs::remove_dir_all(&dir);
}
