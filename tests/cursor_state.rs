//! An EXISTING cursor file that does not carry a cursor must be a REFUSAL,
//! not a silent restart at 0.
//!
//! Measured 2026-11-08 on the published v0.7.3 tree (HEAD 3461f0d), against
//! a journal with 20 planted rows and a cursor file that already existed:
//!
//!   echo abc > c.txt   && reeflog tail --db p.db --cursor c.txt
//!     -> exit 0, ALL 20 rows delivered, "20 events after cursor 0" — the
//!        corrupt file was read as cursor 0, and then OVERWRITTEN with the
//!        tip. The consumer cannot tell this apart from a first run.
//!   echo 9223372036854775808 > c.txt && reeflog tail ...
//!     -> exit 0, all rows: a value outside the consumer's i64 space parsed
//!        as a failure, and the failure became 0 — the exact defect class
//!        v0.7.2 closed for flag values, one layer over: this value never
//!        reaches the flag parser, it is read from the state file.
//!   echo -1 > c.txt    && reeflog tail ...
//!     -> exit 0, all rows: a negative cursor is not a position.
//!   : > c.txt          && reeflog tail ...
//!     -> exit 0, all rows: an empty file is what a crash BETWEEN truncate
//!        and write leaves behind (write_cursor truncates first), so the
//!        cursor that said 500 silently became 0.
//!
//! `read_cursor` used `.ok().and_then(parse).unwrap_or(0)` — every failure
//! mode of a file the product's central promise depends on ("resumes exactly
//! where it stopped") collapsed into "start from the beginning", with exit 0.
//! Full replay is not data loss, but it is the same lie the rest of this
//! family refuses: the run claims a position the operator never had.
//!
//! The rule, applied BEFORE the journal is opened:
//!
//! * a MISSING cursor file is a first run: cursor 0, every row delivered —
//!   that is the documented start, not an error;
//! * an EXISTING file whose content is not a non-negative whole number
//!   inside the consumer's i64 space is a REFUSAL that names the file and
//!   the offending content, and the file is left untouched;
//! * an existing EMPTY file is a refusal too: it is indistinguishable from
//!   a truncated write, and restarting at 0 behind the operator's back is
//!   what this ladder exists to prevent;
//! * `0` in a file stays legal (it is how a consumer deliberately rewinds),
//!   a trailing newline stays legal (it is how write_cursor writes), and a
//!   cursor ahead of the tip stays legal (it simply delivers 0 rows).
//!
//! The guards matter as much as the refusals: every documented invocation,
//! the first run on a missing file, and the exactly-once resume must keep
//! working. A cursor reader that refuses a valid file is worse than the
//! defect it fixes.

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
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// The numeric POSITION a cursor file holds, in either form it may be
/// written: a bare whole number (the operator's deliberate rewind) or the
/// structured cursor reeflog writes since v0.7.13 (`"value":N`, provenance
/// in the rest). These tests pin the POSITION — "the cursor advanced to the
/// tip" — and must not re-pin the encoding; `tests/cursor_provenance.rs`
/// owns the encoding contract.
fn cursor_position(cursor: &Path) -> String {
    let raw = std::fs::read_to_string(cursor).expect("cursor readable");
    let trimmed = raw.trim();
    if trimmed.starts_with('{') {
        let needle = "\"value\":";
        let start = trimmed
            .find(needle)
            .unwrap_or_else(|| panic!("a structured cursor must carry {needle}: {trimmed}"))
            + needle.len();
        let rest = &trimmed[start..];
        let end = rest
            .find(|c: char| !c.is_ascii_digit())
            .unwrap_or(rest.len());
        return rest[..end].to_string();
    }
    trimmed.to_string()
}

/// A journal with a program and `rows` planted events, so a full replay is
/// visible in the output and a refusal can be told apart from "no rows".
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

/// A refusal must name the FILE and the offending CONTENT, and it must not
/// deliver a single row: a run that refuses and still plays events has not
/// refused.
fn assert_cursor_refusal(out: &Output, cursor: &Path, content: &str) {
    assert!(
        !out.status.success(),
        "an invalid existing cursor must not run the command (stdout: {})",
        stdout_of(out)
    );
    assert_eq!(
        delivered_rows(out),
        0,
        "a refused run must not deliver a single row (stdout: {})",
        stdout_of(out)
    );
    let err = stderr_of(out);
    let name = cursor.file_name().expect("cursor has a name").to_string_lossy();
    assert!(
        err.contains(&name.to_lowercase()),
        "the refusal must name the cursor file {name}, got: {err}"
    );
    let trimmed = content.trim().to_lowercase();
    if !trimmed.is_empty() {
        assert!(
            err.contains(&trimmed),
            "the refusal must name the offending content `{trimmed}`, got: {err}"
        );
    }
    let after = std::fs::read_to_string(cursor).unwrap_or_default();
    assert_eq!(
        after, content,
        "a refused run must leave the cursor file untouched"
    );
}

// --- the defect: an unreadable existing cursor silently becomes 0 ----------

#[test]
fn a_corrupt_existing_cursor_is_refused_instead_of_restarting_at_zero() {
    let dir = with_rows("cursor-corrupt", 20);
    let cursor = dir.join("c.txt");
    std::fs::write(&cursor, "abc\n").expect("write cursor");

    let out = run(
        &dir,
        &["tail", "--db", "reeflog.db", "--cursor", "c.txt"],
    );

    assert_cursor_refusal(&out, &cursor, "abc\n");
}

#[test]
fn an_existing_cursor_outside_the_consumer_i64_space_is_refused() {
    // The value never reaches the flag parser — it is read from state — so
    // v0.7.3's numeric flag gate does not see it. It must still be refused:
    // the consumer binds i64, and a parse failure here became cursor 0.
    let dir = with_rows("cursor-i64max", 20);
    let cursor = dir.join("c.txt");
    std::fs::write(&cursor, "9223372036854775808\n").expect("write cursor");

    let out = run(
        &dir,
        &["tail", "--db", "reeflog.db", "--cursor", "c.txt"],
    );

    assert_cursor_refusal(&out, &cursor, "9223372036854775808\n");
}

#[test]
fn a_negative_existing_cursor_is_refused() {
    // `-1` parses as i64 and reads as "before seq 1" — every row. A cursor
    // is a position, and no position is negative.
    let dir = with_rows("cursor-negative", 20);
    let cursor = dir.join("c.txt");
    std::fs::write(&cursor, "-1\n").expect("write cursor");

    let out = run(
        &dir,
        &["tail", "--db", "reeflog.db", "--cursor", "c.txt"],
    );

    assert_cursor_refusal(&out, &cursor, "-1\n");
}

#[test]
fn an_existing_but_empty_cursor_is_refused() {
    // What a crash between truncate and write leaves behind. Restarting at 0
    // behind the operator's back replays the whole journal with exit 0.
    let dir = with_rows("cursor-empty", 20);
    let cursor = dir.join("c.txt");
    std::fs::write(&cursor, "").expect("write cursor");

    let out = run(
        &dir,
        &["tail", "--db", "reeflog.db", "--cursor", "c.txt"],
    );

    assert_cursor_refusal(&out, &cursor, "");
}

#[test]
fn a_float_existing_cursor_is_refused() {
    // "15.0" is a number to a human and not a seq position: the consumer
    // binds an integer cursor, and guessing 15 from it is a guess.
    let dir = with_rows("cursor-float", 20);
    let cursor = dir.join("c.txt");
    std::fs::write(&cursor, "15.0\n").expect("write cursor");

    let out = run(
        &dir,
        &["tail", "--db", "reeflog.db", "--cursor", "c.txt"],
    );

    assert_cursor_refusal(&out, &cursor, "15.0\n");
}

#[test]
fn the_retraction_channel_refuses_an_invalid_existing_cursor_too() {
    let dir = with_rows("cursor-reorg", 5);
    let cursor = dir.join("r.txt");
    std::fs::write(&cursor, "garbage").expect("write cursor");

    let out = run(
        &dir,
        &["reorg", "tail", "--db", "reeflog.db", "--cursor", "r.txt"],
    );

    assert_cursor_refusal(&out, &cursor, "garbage");
}

#[test]
fn a_refusal_does_not_create_a_cursor_file_that_was_not_there() {
    // Not applicable to a missing file (that is the documented first run),
    // but an invalid read must never WRITE: writing the tip would destroy
    // the evidence of what was wrong. Pin the whole path: content survives,
    // nothing is appended.
    let dir = with_rows("cursor-no-write", 20);
    let cursor = dir.join("nested").join("c.txt");
    std::fs::create_dir_all(cursor.parent().expect("parent")).expect("dir");
    std::fs::write(&cursor, "not-a-cursor").expect("write cursor");

    let out = run(
        &dir,
        &[
            "tail",
            "--db",
            "reeflog.db",
            "--cursor",
            "nested/c.txt",
        ],
    );

    assert_cursor_refusal(&out, &cursor, "not-a-cursor");
}

// --- guards: the documented behavior must survive --------------------------

#[test]
fn a_missing_cursor_file_is_a_first_run_and_delivers_everything() {
    let dir = with_rows("cursor-missing", 12);
    let cursor = dir.join("c.txt");
    assert!(!cursor.exists(), "precondition: no cursor yet");

    let out = run(
        &dir,
        &["tail", "--db", "reeflog.db", "--cursor", "c.txt"],
    );

    assert!(
        out.status.success(),
        "a first run must succeed (stderr: {})",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(delivered_rows(&out), 12, "the first run delivers all rows");
    assert_eq!(
        cursor_position(&cursor),
        "12",
        "the cursor advances to the tip"
    );
}

#[test]
fn a_zero_in_the_file_stays_legal_a_deliberate_rewind() {
    let dir = with_rows("cursor-zero", 8);
    let cursor = dir.join("c.txt");
    std::fs::write(&cursor, "0\n").expect("write cursor");

    let out = run(
        &dir,
        &["tail", "--db", "reeflog.db", "--cursor", "c.txt"],
    );

    assert!(
        out.status.success(),
        "`0` in a file is a legal cursor (stderr: {})",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(delivered_rows(&out), 8, "`0` means from the beginning");
}

#[test]
fn a_valid_cursor_resumes_exactly_where_it_stopped() {
    let dir = with_rows("cursor-resume", 20);
    let cursor = dir.join("c.txt");
    std::fs::write(&cursor, "15\n").expect("write cursor");

    let first = run(
        &dir,
        &["tail", "--db", "reeflog.db", "--cursor", "c.txt", "--limit", "3"],
    );
    assert!(first.status.success(), "resume must succeed");
    assert_eq!(delivered_rows(&first), 3, "16, 17, 18 are delivered");
    assert_eq!(cursor_position(&cursor), "18");

    let second = run(
        &dir,
        &["tail", "--db", "reeflog.db", "--cursor", "c.txt"],
    );
    assert!(second.status.success(), "second iteration must succeed");
    assert_eq!(
        delivered_rows(&second),
        2,
        "no replay: only 19 and 20 remain"
    );
}

#[test]
fn a_trailing_newline_exactly_as_write_cursor_writes_it_stays_legal() {
    let dir = with_rows("cursor-newline", 6);
    let cursor = dir.join("c.txt");
    std::fs::write(&cursor, "4\n").expect("write cursor exactly as write_cursor does");

    let out = run(
        &dir,
        &["tail", "--db", "reeflog.db", "--cursor", "c.txt"],
    );

    assert!(
        out.status.success(),
        "the file write_cursor itself produces must be readable (stderr: {})",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(delivered_rows(&out), 2, "rows 5 and 6");
}

#[test]
fn a_cursor_ahead_of_the_tip_delivers_nothing_and_is_not_an_error() {
    let dir = with_rows("cursor-ahead", 5);
    let cursor = dir.join("c.txt");
    std::fs::write(&cursor, "5000\n").expect("write cursor");

    let out = run(
        &dir,
        &["tail", "--db", "reeflog.db", "--cursor", "c.txt"],
    );

    assert!(
        out.status.success(),
        "a cursor ahead of the tip is legal (stderr: {})",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        delivered_rows(&out),
        0,
        "nothing after the tip, and the file keeps its value"
    );
    assert_eq!(
        std::fs::read_to_string(&cursor).expect("cursor").trim(),
        "5000",
        "an unchanged cursor must not be rewritten — the bare file stays byte-identical"
    );
}

#[test]
fn cursor_whitespace_the_file_form_of_a_valid_value_stays_legal() {
    let dir = with_rows("cursor-whitespace", 6);
    let cursor = dir.join("c.txt");
    std::fs::write(&cursor, "  4  \n\n").expect("write cursor");

    let out = run(
        &dir,
        &["tail", "--db", "reeflog.db", "--cursor", "c.txt"],
    );

    assert!(
        out.status.success(),
        "surrounding whitespace is trimmed, not refused (stderr: {})",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(delivered_rows(&out), 2, "rows 5 and 6");
}
