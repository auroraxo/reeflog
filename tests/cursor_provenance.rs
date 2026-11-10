//! A cursor file must say WHICH journal and WHICH channel wrote it.
//!
//! Why this test exists (measured 2026-11-10 on the published/current tree,
//! artifact `research/cursor-provenance-2026-11-10.json`): a cursor was a bare
//! integer with no provenance. `tail` writes an event `seq` and `reorg tail`
//! writes a reorg `id` — two different number lines — into files of exactly
//! the same shape, and each reader accepted any number from any source. Every
//! one of the three crossings below exited **0**:
//!
//!   D1  `tail` delivers 20 events, cursor file holds `20`. Hand that same
//!       file to `reorg tail`: "0 reorg(s) after cursor 20", exit 0 — while
//!       the journal holds 3 undelivered reorgs. The retraction channel is
//!       silently lost, and nothing tells the consumer.
//!   D2  `reorg tail` delivers 3 reorgs, cursor holds `3`. Hand it to `tail`:
//!       17 already-delivered events replay, exit 0. Duplicate delivery
//!       behind a healthy exit code.
//!   D3  a cursor from journal A copied to journal B: "0 events after cursor
//!       20", exit 0 — a fresh mailbox reads as fully delivered.
//!
//! The Node client hides the hazard by naming default cursor files per
//! channel (`db.tail.default.cursor` vs `db.reorg.default.cursor`); a CLI
//! consumer that shares one `--cursor` path eats all three.
//!
//! The rule (see `src/cursorfile.rs`):
//!
//!   * a cursor reeflog WROTE carries channel + journal id, and is honoured
//!     only by the same channel of the same journal — anything else is a
//!     refusal that names both sides and the repair, delivers nothing, and
//!     leaves the file untouched;
//!   * a bare hand-written non-negative whole number stays legal: it is the
//!     documented rewind (`0`) and resume (`15`), and a reader that refuses
//!     the operator's own deliberate numbers is worse than the defect.
//!
//! Guards (green on BOTH trees) pin the false-positive side: first run,
//! deliberate rewind, exactly-once resume on the same journal and channel,
//! the retraction channel's own cursor, an untouched refusal, and the cursor
//! file that is simply the same file used twice in a row.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const VALID_PROGRAM: &str = "whirLbMiicVdio4qvUfM5KAg6Ct8VwpYzGff3uctyCc";
const OTHER_PROGRAM: &str = "6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P";

fn unique_dir(tag: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("reeflog-prov-{tag}-{}-{nanos}", std::process::id()));
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

fn stdout_of(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn stderr_of(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

fn delivered_rows(out: &Output) -> usize {
    stdout_of(out)
        .lines()
        .filter(|l| l.starts_with('{'))
        .count()
}

/// A journal with `events` planted events and `reorgs` planted reorgs. Both
/// channels carry pending work so a lost channel is VISIBLE as a count, not
/// as an absence.
fn journal_with(tag: &str, program: &str, events: i64, reorgs: i64) -> PathBuf {
    let dir = unique_dir(tag);
    let out = run(&dir, &["init", "--db", "reeflog.db", "--program", program]);
    assert!(
        out.status.success(),
        "init must succeed (stderr: {})",
        stderr_of(&out)
    );
    let conn = rusqlite::Connection::open(dir.join("reeflog.db")).expect("open journal");
    let now = 1_700_000_000_000i64;
    for i in 0..events {
        conn.execute(
            "INSERT INTO events(signature, slot, first_seen, source, logs)
             VALUES(?1, ?2, ?3, 'ws', '[\"log\"]')",
            rusqlite::params![format!("Sig{i:043}"), 44_000_000i64 + i, now],
        )
        .expect("plant event");
    }
    for i in 0..reorgs {
        conn.execute(
            "INSERT INTO reorgs(signature, slot, detected_at, status_source, tx_source, detail)
             VALUES(?1, ?2, ?3, 'rpc', 'archive', 'planted')",
            rusqlite::params![format!("Reorg{i:040}"), 44_000_000i64 + i, now],
        )
        .expect("plant reorg");
    }
    conn.close().expect("close journal");
    dir
}

/// The crossing must be a REFUSAL: nothing delivered, nothing written, and
/// the refusal names the cursor file and says why it does not belong.
fn assert_crossing_refused(out: &Output, cursor_name: &str) {
    assert!(
        !out.status.success(),
        "a cursor from another channel/journal must not run the command (stdout: {})",
        stdout_of(out)
    );
    assert_eq!(
        delivered_rows(out),
        0,
        "a refused crossing must deliver nothing (stdout: {})",
        stdout_of(out)
    );
    let err = stderr_of(out);
    assert!(
        err.contains(cursor_name),
        "the refusal must name the cursor file {cursor_name}, got: {err}"
    );
    assert!(
        err.contains("journal") || err.contains("channel"),
        "the refusal must say WHICH provenance is wrong (journal or channel), got: {err}"
    );
}

// --- the defect: a bare cursor is honoured anywhere ------------------------

#[test]
fn d1_an_event_cursor_handed_to_the_retraction_channel_is_refused() {
    let dir = journal_with("d1", VALID_PROGRAM, 20, 3);
    let first = run(&dir, &["tail", "--db", "reeflog.db", "--cursor", "c.txt"]);
    assert!(first.status.success(), "the event pass must work: {}", stderr_of(&first));
    assert_eq!(delivered_rows(&first), 20);

    // The SAME file is now the reorg cursor. The journal owes 3 reorgs.
    let out = run(&dir, &["reorg", "tail", "--db", "reeflog.db", "--cursor", "c.txt"]);
    assert_crossing_refused(&out, "c.txt");
}

#[test]
fn d2_a_reorg_cursor_handed_to_the_event_channel_is_refused() {
    let dir = journal_with("d2", VALID_PROGRAM, 20, 3);
    let first = run(&dir, &["reorg", "tail", "--db", "reeflog.db", "--cursor", "c.txt"]);
    assert!(first.status.success(), "the reorg pass must work: {}", stderr_of(&first));
    assert_eq!(delivered_rows(&first), 3);

    // The SAME file read as an event seq would replay 17 delivered events.
    let out = run(&dir, &["tail", "--db", "reeflog.db", "--cursor", "c.txt"]);
    assert_crossing_refused(&out, "c.txt");
}

#[test]
fn d3_a_cursor_copied_to_a_different_journal_is_refused() {
    let a = journal_with("d3a", VALID_PROGRAM, 20, 0);
    let first = run(&a, &["tail", "--db", "reeflog.db", "--cursor", "c.txt"]);
    assert!(first.status.success());
    assert_eq!(delivered_rows(&first), 20);

    let b = journal_with("d3b", OTHER_PROGRAM, 20, 0);
    std::fs::copy(a.join("c.txt"), b.join("c.txt")).expect("copy cursor");

    // Journal B holds 20 events this consumer has never seen. A bare `20`
    // would read them as already delivered.
    let out = run(&b, &["tail", "--db", "reeflog.db", "--cursor", "c.txt"]);
    assert_crossing_refused(&out, "c.txt");
    assert_eq!(
        delivered_rows(&out),
        0,
        "a foreign cursor must not consume journal B's events"
    );
}

#[test]
fn d4_a_crossing_refusal_leaves_the_cursor_file_untouched() {
    let dir = journal_with("d4", VALID_PROGRAM, 20, 3);
    let first = run(&dir, &["tail", "--db", "reeflog.db", "--cursor", "c.txt"]);
    assert!(first.status.success());
    let before = std::fs::read_to_string(dir.join("c.txt")).expect("cursor exists");

    let out = run(&dir, &["reorg", "tail", "--db", "reeflog.db", "--cursor", "c.txt"]);
    assert_crossing_refused(&out, "c.txt");
    let after = std::fs::read_to_string(dir.join("c.txt")).expect("cursor exists");
    assert_eq!(
        after, before,
        "a refused crossing must not destroy the position it refused"
    );
}

// --- guards: documented behavior must survive ------------------------------

#[test]
fn g1_a_first_run_with_no_cursor_file_delivers_everything() {
    let dir = journal_with("g1", VALID_PROGRAM, 12, 0);
    let out = run(&dir, &["tail", "--db", "reeflog.db", "--cursor", "c.txt"]);
    assert!(out.status.success(), "a missing file is the documented start: {}", stderr_of(&out));
    assert_eq!(delivered_rows(&out), 12);
}

#[test]
fn g2_a_bare_hand_written_number_stays_legal() {
    // The operator's own rewind/resume. Provenance cannot be assumed of a
    // number a human typed, so it is honoured — and it stays documented.
    let dir = journal_with("g2", VALID_PROGRAM, 20, 0);
    std::fs::write(dir.join("c.txt"), "15\n").expect("write cursor");
    let out = run(&dir, &["tail", "--db", "reeflog.db", "--cursor", "c.txt"]);
    assert!(out.status.success(), "a bare `15` is a legal cursor: {}", stderr_of(&out));
    assert_eq!(delivered_rows(&out), 5, "16..20");
}

#[test]
fn g3_the_same_cursor_reused_on_its_own_channel_resumes_exactly() {
    let dir = journal_with("g3", VALID_PROGRAM, 20, 0);
    let first = run(&dir, &["tail", "--db", "reeflog.db", "--cursor", "c.txt", "--limit", "3"]);
    assert!(first.status.success());
    assert_eq!(delivered_rows(&first), 3);

    let second = run(&dir, &["tail", "--db", "reeflog.db", "--cursor", "c.txt"]);
    assert!(second.status.success(), "a cursor's own channel must honour it: {}", stderr_of(&second));
    assert_eq!(delivered_rows(&second), 17, "no replay, no loss");

    let third = run(&dir, &["tail", "--db", "reeflog.db", "--cursor", "c.txt"]);
    assert!(third.status.success());
    assert_eq!(delivered_rows(&third), 0, "and the tip stays consumed");
}

#[test]
fn g4_each_channel_keeps_its_own_cursor_file_and_both_work() {
    let dir = journal_with("g4", VALID_PROGRAM, 20, 3);
    let events = run(&dir, &["tail", "--db", "reeflog.db", "--cursor", "ev.txt"]);
    let reorgs = run(&dir, &["reorg", "tail", "--db", "reeflog.db", "--cursor", "re.txt"]);
    assert!(events.status.success());
    assert!(reorgs.status.success());
    assert_eq!(delivered_rows(&events), 20);
    assert_eq!(delivered_rows(&reorgs), 3);

    let again_e = run(&dir, &["tail", "--db", "reeflog.db", "--cursor", "ev.txt"]);
    let again_r = run(&dir, &["reorg", "tail", "--db", "reeflog.db", "--cursor", "re.txt"]);
    assert!(again_e.status.success(), "{}", stderr_of(&again_e));
    assert!(again_r.status.success(), "{}", stderr_of(&again_r));
    assert_eq!(delivered_rows(&again_e), 0);
    assert_eq!(delivered_rows(&again_r), 0);
}

#[test]
fn g5_an_invalid_bare_cursor_is_still_refused_and_untouched() {
    // The v0.7.4 contract survives: garbage is a refusal, not cursor 0.
    let dir = journal_with("g5", VALID_PROGRAM, 20, 0);
    std::fs::write(dir.join("c.txt"), "abc\n").expect("write cursor");
    let out = run(&dir, &["tail", "--db", "reeflog.db", "--cursor", "c.txt"]);
    assert!(!out.status.success(), "garbage must be refused");
    assert_eq!(delivered_rows(&out), 0);
    assert_eq!(
        std::fs::read_to_string(dir.join("c.txt")).expect("cursor"),
        "abc\n",
        "a refusal leaves the evidence"
    );
}

#[test]
fn g6_the_status_report_and_stderr_still_name_a_plain_number() {
    // The operator-facing line "cursor now N" must stay a NUMBER: every
    // harness and the kill test read that line, and a JSON blob there would
    // break the delivery contract for the people the fix protects.
    let dir = journal_with("g6", VALID_PROGRAM, 7, 0);
    let out = run(&dir, &["tail", "--db", "reeflog.db", "--cursor", "c.txt"]);
    assert!(out.status.success());
    let err = stderr_of(&out);
    assert!(
        err.contains("cursor now 7"),
        "the summary must report the numeric position: {err}"
    );
}

#[test]
fn g7_a_cursor_written_by_the_pinned_shape_resumes_after_a_reopen() {
    // Two separate process invocations already cover reopen, but pin the
    // claim explicitly: the written file must be readable by the SAME
    // channel after the journal is opened again by a later command.
    let dir = journal_with("g7", VALID_PROGRAM, 5, 2);
    let a = run(&dir, &["tail", "--db", "reeflog.db", "--cursor", "c.txt"]);
    assert!(a.status.success());
    run(&dir, &["status", "--db", "reeflog.db"]);
    let b = run(&dir, &["tail", "--db", "reeflog.db", "--cursor", "c.txt"]);
    assert!(b.status.success(), "reopen must not invalidate the cursor: {}", stderr_of(&b));
    assert_eq!(delivered_rows(&b), 0);
}
