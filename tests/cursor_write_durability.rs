//! A FAILED cursor write must not destroy the cursor it was replacing.
//!
//! Why this test exists (measured 2026-11-10 on the PUBLISHED v0.7.13
//! binary, artifact `research/cursor-write-durability-2026-11-10.json`):
//! `write_cursor` called `std::fs::write(path, …)` — open with `O_TRUNC`,
//! then write. When the write itself failed (disk full; this repo's own
//! build host ran at 100% full twice in the two days before this test),
//! the truncation had ALREADY happened. Measured faces of the same defect,
//! all with rows already delivered on stdout:
//!
//!   D1  cursor held `0`, `tail` batch write fails ENOSPC -> exit 1 and
//!       the file is left ZERO BYTES. The next run refuses with "an empty
//!       file is what a crash between truncate and write leaves behind" —
//!       a state the READER names as a crash remnant while the WRITER kept
//!       producing it. The consumer is wedged: every later run exits 1,
//!       and the only documented repair is deleting the file = full rewind
//!       = replay of everything already delivered.
//!   D2  same face on the retraction channel (`reorg tail`).
//!   D3  no cursor file yet (first batch): the failed write CREATED an
//!       empty file, turning the healthy first-run state into the wedged
//!       empty-file state.
//!
//! The injection is CONTENT-targeted: `tools/failcursorwrite.c` (compiled
//! by this test into the target dir) is an LD_PRELOAD shim that fails the
//! write whose buffer is the cursor document (`{"v":1,` …) with ENOSPC.
//! It hits exactly the cursor write in BOTH trees — the direct write
//! pre-fix, the temp write post-fix — and never touches stdout, stderr or
//! the journal. The measurement tool
//! (`tools/measure-cursor-write-durability.mjs`) runs the same three cases
//! against the PUBLISHED release binary and this tree and writes the
//! pinned artifact.
//!
//! The rule the fix implements: render the cursor, write it to a sibling
//! temp file, fsync the temp file, rename it over the cursor path — the
//! cursor path always holds the OLD full cursor or the NEW full cursor,
//! never a hole. A failed write removes the temp file and refuses, naming
//! file, errno and the delivery contract: delivery is at-least-once, the
//! position is unchanged, the next run redelivers the same rows.
//!
//! Guards (green on BOTH trees) pin the false-positive side:
//!   G1  the successful path still lands the structured cursor and resume
//!       continues without replay;
//!   G2  no stray temp file survives a successful swap;
//!   G3  neighbour files in the same directory are untouched by the swap.
//!
//! Rust integration tests spawn the binary as a child; the shim is
//! injected through the child's environment.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const VALID_PROGRAM: &str = "whirLbMiicVdio4qvUfM5KAg6Ct8VwpYzGff3uctyCc";

fn unique_dir(tag: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("reeflog-cw-{tag}-{}-{nanos}", std::process::id()));
    fs::create_dir_all(&dir).expect("temp dir");
    dir
}

/// Compile the LD_PRELOAD shim once per test process into a stable path.
fn shim_path() -> PathBuf {
    let out_dir = std::env::temp_dir().join("reeflog-cw-shims");
    fs::create_dir_all(&out_dir).expect("shim dir");
    let so = out_dir.join("failcursorwrite.so");
    if so.exists() {
        return so;
    }
    let manifest = env!("CARGO_MANIFEST_DIR");
    let src = Path::new(manifest).join("tools/failcursorwrite.c");
    let status = Command::new("cc")
        .arg("-shared")
        .arg("-fPIC")
        .arg("-o")
        .arg(&so)
        .arg(&src)
        .arg("-ldl")
        .status()
        .expect("cc runs");
    assert!(status.success(), "compiling the fault shim failed");
    so
}

fn run_injected(dir: &Path, args: &[&str], shim: &Path) -> Output {
    Command::new(env!("CARGO_BIN_EXE_reeflog"))
        .args(args)
        .current_dir(dir)
        .env("LD_PRELOAD", shim)
        .output()
        .expect("reeflog binary runs")
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

fn delivered_rows(out: &Output) -> usize {
    stdout_of(out).lines().filter(|l| l.starts_with('{')).count()
}

fn journal_with(tag: &str, events: i64, reorgs: i64) -> PathBuf {
    let dir = unique_dir(tag);
    let out = run(&dir, &["init", "--db", "reeflog.db", "--program", VALID_PROGRAM]);
    assert!(out.status.success(), "init must succeed: {}", stderr_of(&out));
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

// ---------------------------------------------------------------- defect

/// D1 — the cursor write fails after delivery: the OLD cursor (`0`) must
/// survive the failed run byte-exactly, so the consumer can just retry.
#[test]
fn d1_tail_failed_write_keeps_the_old_cursor() {
    let shim = shim_path();
    let dir = journal_with("d1", 20, 0);
    fs::write(dir.join("c.txt"), "0\n").expect("hand-written rewind cursor");
    let out = run_injected(
        &dir,
        &["tail", "--db", "reeflog.db", "--cursor", "c.txt", "--limit", "5"],
        &shim,
    );
    assert!(
        !out.status.success(),
        "a cursor that cannot be written must not exit 0 (stderr: {})",
        stderr_of(&out)
    );
    assert_eq!(
        delivered_rows(&out),
        5,
        "the rows WERE delivered to stdout before the write failed"
    );
    let err = stderr_of(&out);
    assert!(err.contains("c.txt"), "refusal must name the cursor file: {err}");
    assert!(
        err.contains("No space left") || err.contains("os error 28"),
        "refusal must name the ENOSPC errno: {err}"
    );
    assert!(
        err.contains("unchanged") || err.contains("redeliver") || err.contains("at-least-once"),
        "refusal must carry the delivery contract (position unchanged, redeliver): {err}"
    );
    assert_eq!(
        fs::read_to_string(dir.join("c.txt")).expect("cursor survives"),
        "0\n",
        "the failed write must not touch the cursor file's content"
    );
    let stray: Vec<String> = fs::read_dir(&dir)
        .expect("read dir")
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.ends_with(".tmp"))
        .collect();
    assert!(
        stray.is_empty(),
        "a failed cursor write must clean up its staged temp file, got: {stray:?}"
    );
    fs::remove_dir_all(&dir).ok();
}

/// D2 — same face on the retraction channel.
#[test]
fn d2_reorg_tail_failed_write_keeps_the_old_cursor() {
    let shim = shim_path();
    let dir = journal_with("d2", 0, 3);
    fs::write(dir.join("r.txt"), "0\n").expect("hand-written rewind cursor");
    let out = run_injected(
        &dir,
        &["reorg", "tail", "--db", "reeflog.db", "--cursor", "r.txt", "--limit", "2"],
        &shim,
    );
    assert!(
        !out.status.success(),
        "a reorg cursor that cannot be written must not exit 0 (stderr: {})",
        stderr_of(&out)
    );
    assert_eq!(
        delivered_rows(&out),
        2,
        "two reorgs were delivered before the write failed"
    );
    let err = stderr_of(&out);
    assert!(err.contains("r.txt"), "refusal must name the cursor file: {err}");
    assert!(
        err.contains("No space left") || err.contains("os error 28"),
        "refusal must name the ENOSPC errno: {err}"
    );
    assert_eq!(
        fs::read_to_string(dir.join("r.txt")).expect("cursor survives"),
        "0\n",
        "the reorg cursor file must survive the failed write byte-exactly"
    );
    fs::remove_dir_all(&dir).ok();
}

/// D3 — no cursor file yet (first batch): a failed write must not CREATE
/// an empty file at the cursor path. Pre-fix, the O_TRUNC/O_CREAT open
/// produced exactly the empty-file wedge the reader refuses forever.
#[test]
fn d3_first_batch_failed_write_leaves_no_file_at_the_cursor_path() {
    let shim = shim_path();
    let dir = journal_with("d3", 8, 0);
    let out = run_injected(
        &dir,
        &["tail", "--db", "reeflog.db", "--cursor", "c.txt", "--limit", "3"],
        &shim,
    );
    assert!(
        !out.status.success(),
        "a first cursor write that fails must not exit 0 (stderr: {})",
        stderr_of(&out)
    );
    assert!(
        !dir.join("c.txt").exists(),
        "a failed FIRST cursor write must not leave any file at the cursor path"
    );
    let err = stderr_of(&out);
    assert!(
        err.contains("No space left") || err.contains("os error 28"),
        "refusal must name the ENOSPC errno: {err}"
    );
    let stray: Vec<String> = fs::read_dir(&dir)
        .expect("read dir")
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.ends_with(".tmp"))
        .collect();
    assert!(
        stray.is_empty(),
        "a failed cursor write must clean up its staged temp file, got: {stray:?}"
    );
    fs::remove_dir_all(&dir).ok();
}

/// D4 — after the failed write the consumer can simply RETRY: the same
/// command without the fault succeeds and delivers exactly the same rows
/// again (at-least-once), then advances normally.
#[test]
fn d4_retry_after_failed_write_redelivers_and_advances() {
    let shim = shim_path();
    let dir = journal_with("d4", 20, 0);
    fs::write(dir.join("c.txt"), "0\n").expect("hand-written rewind cursor");
    let failed = run_injected(
        &dir,
        &["tail", "--db", "reeflog.db", "--cursor", "c.txt", "--limit", "5"],
        &shim,
    );
    assert!(!failed.status.success());
    assert_eq!(fs::read_to_string(dir.join("c.txt")).expect("kept"), "0\n");
    let retry = run(&dir, &["tail", "--db", "reeflog.db", "--cursor", "c.txt", "--limit", "5"]);
    assert!(retry.status.success(), "retry must succeed: {}", stderr_of(&retry));
    assert_eq!(
        delivered_rows(&retry),
        5,
        "the retry redelivers the same rows (at-least-once) instead of replaying 20"
    );
    let content = fs::read_to_string(dir.join("c.txt")).expect("cursor advanced");
    assert!(content.contains("\"value\":5"), "cursor advanced to 5: {content}");
    fs::remove_dir_all(&dir).ok();
}

// ---------------------------------------------------------------- guards

/// G1 — the successful path is untouched: structured cursor, byte-exact,
/// resume continues without replay. (Green on BOTH trees.)
#[test]
fn g1_successful_write_still_lands_the_structured_cursor() {
    let dir = journal_with("g1", 10, 0);
    let first = run(&dir, &["tail", "--db", "reeflog.db", "--cursor", "c.txt", "--limit", "4"]);
    assert!(first.status.success(), "normal tail must succeed: {}", stderr_of(&first));
    assert_eq!(delivered_rows(&first), 4);
    let content = fs::read_to_string(dir.join("c.txt")).expect("cursor written");
    assert!(
        content.starts_with("{\"v\":1,\"channel\":\"events\""),
        "structured cursor: {content}"
    );
    let second = run(&dir, &["tail", "--db", "reeflog.db", "--cursor", "c.txt", "--limit", "4"]);
    assert!(second.status.success());
    assert_eq!(delivered_rows(&second), 4, "resume delivers the NEXT four");
    assert!(
        stderr_of(&second).contains("cursor now 8"),
        "the summary advances: {}",
        stderr_of(&second)
    );
    fs::remove_dir_all(&dir).ok();
}

/// G2 — after a successful write only the cursor path carries the cursor
/// name; no stray temp file survives the swap. (Green on BOTH trees: the
/// pre-fix tree never created a temp file either — documented.)
#[test]
fn g2_no_stray_temp_after_successful_write() {
    let dir = journal_with("g2", 5, 0);
    let out = run(&dir, &["tail", "--db", "reeflog.db", "--cursor", "c.txt", "--limit", "5"]);
    assert!(out.status.success(), "tail must succeed: {}", stderr_of(&out));
    let stray: Vec<String> = fs::read_dir(&dir)
        .expect("read dir")
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.starts_with('.') || n.ends_with(".tmp") || n.ends_with(".partial"))
        .collect();
    assert!(
        stray.is_empty(),
        "no temp file may survive the cursor swap, got: {stray:?}"
    );
    fs::remove_dir_all(&dir).ok();
}

/// G3 — neighbour files in the same directory are untouched by the swap.
/// (Green on BOTH trees.)
#[test]
fn g3_neighbour_files_survive_the_swap() {
    let dir = journal_with("g3", 6, 0);
    fs::write(dir.join("other.txt"), "3\n").expect("plant neighbour");
    let out = run(&dir, &["tail", "--db", "reeflog.db", "--cursor", "c.txt", "--limit", "2"]);
    assert!(out.status.success(), "tail must succeed: {}", stderr_of(&out));
    assert_eq!(
        fs::read_to_string(dir.join("other.txt")).expect("neighbour"),
        "3\n"
    );
    fs::remove_dir_all(&dir).ok();
}
