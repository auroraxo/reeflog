//! One journal, one program: the program a journal records IS the journal's
//! identity, and a caller that disagrees must be REFUSED — never silently
//! ignored, never silently overridden.
//!
//! MEASURED 2026-11-10 against the PUBLISHED v0.7.10 asset (the release
//! archive's sha256 equals the digest the Release API publishes): the rule
//! existed, but only ONE command of four defended it. Three faces of the
//! same hole:
//!
//!   reeflog init --db j.db --program <B>       (journal watches <A>)
//!       -> exit 0, "initialized … for program B". The mailbox silently
//!          CHANGED IDENTITY: every later command spoke about B while the
//!          events in the journal were A's. The operator believes the
//!          journal watches B; it watches A.
//!
//!   reeflog verify --db j.db --program <B>      (journal watches <A>)
//!       -> exit 0, a full report — about A. The flag was SILENTLY IGNORED:
//!          the operator asked about B and got a verdict about A, with no
//!          hint that the two differ.
//!
//!   reeflog repair --db j.db --program <B>      (journal watches <A>)
//!       -> the same silent ignore, on the command that WRITES to the
//!          journal.
//!
//!   reeflog start --db j.db --program <B>       (journal watches <A>)
//!       -> exit 1, "journal watches A, refusing --program B". The rule
//!          ALREADY EXISTED — it just applied to one command of four.
//!
//! The rule this ladder enforces:
//!
//! * a program recorded in the journal is the journal's identity;
//! * a command given a DIFFERENT program is refused, naming the recorded
//!   program, the given program and the command — before anything is
//!   scanned, rewritten or opened for writing;
//! * a command given the SAME program is served;
//! * a command given NO program uses the recorded one;
//! * `init` on an existing journal with the SAME program is idempotent
//!   (exit 0, journal untouched) — it must not become a second refusal for
//!   a harmless repeat.
//!
//! The guards matter as much: a checker that refuses a MATCHING program, or
//! that refuses a journal without a program when one was supplied, is worse
//! than the defect it fixes. Every guard below must stay green.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const PROGRAM_A: &str = "11111111111111111111111111111111";
const PROGRAM_B: &str = "whirLbMiicVdio4qvUfM5KAg6Ct8VwpYzGff3uctyCc";

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

/// The program's identity is read from the JOURNAL itself, never from the
/// command that was refused: the report is the thing that lied. `status`
/// reports the journal's recorded program.
fn recorded_program(dir: &Path, db: &Path) -> Option<String> {
    let out = run(dir, &["status", "--db", db.to_str().unwrap()]);
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    let key = "\"program\":";
    let start = stdout.find(key)? + key.len();
    let rest = stdout[start..].trim_start();
    let rest = rest.strip_prefix('"')?;
    let end = rest.find('"')?;
    Some(rest[..end].to_string())
}

/// A journal that watches PROGRAM_A.
fn journal_watching_a(dir: &Path) -> PathBuf {
    let db = dir.join("j.db");
    let out = run(dir, &["init", "--db", db.to_str().unwrap(), "--program", PROGRAM_A]);
    assert!(out.status.success(), "init must succeed: {out:?}");
    db
}

fn stderr_of(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).to_string()
}

// ---------------------------------------------------------------- defects

#[test]
fn init_must_not_repoint_a_journal_at_another_program() {
    let dir = unique_dir("program-identity-init");
    let db = journal_watching_a(&dir);

    let out = run(
        &dir,
        &[
            "init",
            "--db",
            db.to_str().unwrap(),
            "--program",
            PROGRAM_B,
        ],
    );

    assert!(
        !out.status.success(),
        "re-init with a different program must be REFUSED, not a silent re-point: {out:?}"
    );
    let err = stderr_of(&out);
    assert!(
        err.contains(PROGRAM_A) && err.contains(PROGRAM_B),
        "the refusal must name the recorded program and the given one: {err}"
    );
    assert_eq!(
        recorded_program(&dir, &db).as_deref(),
        Some(PROGRAM_A),
        "the journal's identity must be unchanged after the refusal"
    );
}

#[test]
fn verify_must_not_silently_ignore_a_different_program() {
    let dir = unique_dir("program-identity-verify");
    let db = journal_watching_a(&dir);

    let out = run(
        &dir,
        &[
            "verify",
            "--db",
            db.to_str().unwrap(),
            "--program",
            PROGRAM_B,
            "--pages",
            "1",
        ],
    );

    assert!(
        !out.status.success(),
        "verify with a different program must be REFUSED, not silently ignored: {out:?}"
    );
    let err = stderr_of(&out);
    assert!(
        err.contains(PROGRAM_A) && err.contains(PROGRAM_B),
        "the refusal must name the recorded program and the given one: {err}"
    );
}

#[test]
fn repair_must_not_silently_ignore_a_different_program() {
    let dir = unique_dir("program-identity-repair");
    let db = journal_watching_a(&dir);

    let out = run(
        &dir,
        &[
            "repair",
            "--db",
            db.to_str().unwrap(),
            "--program",
            PROGRAM_B,
            "--pages",
            "1",
        ],
    );

    assert!(
        !out.status.success(),
        "repair with a different program must be REFUSED, not silently ignored: {out:?}"
    );
    let err = stderr_of(&out);
    assert!(
        err.contains(PROGRAM_A) && err.contains(PROGRAM_B),
        "the refusal must name the recorded program and the given one: {err}"
    );
}

// ---------------------------------------------------------------- guards

#[test]
fn a_matching_program_is_served() {
    let dir = unique_dir("program-identity-same");
    let db = journal_watching_a(&dir);

    for args in [
        vec!["verify", "--db", db.to_str().unwrap(), "--program", PROGRAM_A, "--pages", "1"],
        vec!["repair", "--db", db.to_str().unwrap(), "--program", PROGRAM_A, "--pages", "1"],
    ] {
        let out = run(&dir, &args);
        assert!(
            !stderr_of(&out).contains("refusing"),
            "a MATCHING program must be served, not refused: {args:?} -> {out:?}"
        );
    }
}

#[test]
fn a_silent_caller_gets_the_recorded_program() {
    let dir = unique_dir("program-identity-silent");
    let db = journal_watching_a(&dir);

    let out = run(&dir, &["verify", "--db", db.to_str().unwrap(), "--pages", "1"]);
    assert!(
        !stderr_of(&out).contains("refusing"),
        "a caller that names no program must be served with the recorded one: {out:?}"
    );
}

#[test]
fn init_with_the_same_program_is_idempotent() {
    let dir = unique_dir("program-identity-idempotent");
    let db = journal_watching_a(&dir);

    let out = run(
        &dir,
        &[
            "init",
            "--db",
            db.to_str().unwrap(),
            "--program",
            PROGRAM_A,
        ],
    );
    assert!(
        out.status.success(),
        "re-init with the SAME program must stay idempotent: {out:?}"
    );
    assert_eq!(
        recorded_program(&dir, &db).as_deref(),
        Some(PROGRAM_A),
        "the journal's identity must be unchanged"
    );
}

#[test]
fn the_refusal_is_one_line_of_reeflogs_own() {
    let dir = unique_dir("program-identity-voice");
    let db = journal_watching_a(&dir);

    let out = run(
        &dir,
        &[
            "verify",
            "--db",
            db.to_str().unwrap(),
            "--program",
            PROGRAM_B,
            "--pages",
            "1",
        ],
    );
    let err = stderr_of(&out);
    let lines: Vec<&str> = err
        .lines()
        .filter(|l| !l.trim().is_empty())
        .collect();
    assert_eq!(
        lines.len(),
        1,
        "the refusal must be exactly ONE line: {err}"
    );
    assert!(
        lines[0].starts_with("reeflog: error: "),
        "the refusal must speak in reeflog's own voice: {err}"
    );
}
