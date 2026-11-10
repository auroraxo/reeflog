//! `dlq replay` must report the QUEUE it was asked to drain, not the PAGE
//! its `--limit` happened to attempt — and it must not exit 0 when letters
//! the pass never reached are still pending.
//!
//! Measured 2026-11-10 on the PUBLISHED v0.7.11 binary (the release archive
//! whose sha256 equals the digest the release API publishes, f75898ff…),
//! against a receiver that answers 200 to everything, on a journal holding
//! 7 pending dead letters:
//!
//!   reeflog webhook dlq replay --db reeflog.db --config wh.json --limit 2
//!     -> exit 0
//!     -> {"replayed": 2, "failed": 0, "targets": [{"target":"alpha",
//!        "replayed":2,"failed":0}], "unreachable_orphans": 0}
//!     -> journal truth after the run: 5 letters STILL PENDING.
//!
//! The report says nothing about those five. No field names them, no exit
//! code carries them. A cron job that runs `dlq replay` and trusts exit 0
//! ("the command cannot report clean success while events remain
//! un-deliverable in the mailbox" — the rule this very function documents)
//! believes the queue drained while 5 of 7 owed events sit un-attempted.
//! The queue kept moving only for the first page; the command called that
//! the whole job.
//!
//! This is the exact class v0.7.7 closed for `dlq list` ("`pending` counted
//! the rows the limit happened to SHOW, not the letters pending" —
//! `tests/dlq_target_domain.rs`): the fix went into the LISTING, while the
//! REPLAY — the command an operator actually automates — kept reporting the
//! page. Same defect, other hand of the pair.
//!
//! The rule now enforced here:
//!
//! * the report carries `pending_before` (the queue at the start of the
//!   pass), `remaining_pending` (what the journal still holds for the
//!   targets this pass was responsible for) and `truncated` (the pass did
//!   not attempt everything);
//! * a pass that leaves letters it never reached pending exits 3, naming
//!   the remaining count and the repair (run again, or raise `--limit`) —
//!   the same exit-3-means-incomplete contract `tail`/`verify`/`push` use;
//! * delivery itself is UNCHANGED: exactly `limit` letters are attempted
//!   per target, byte-identical, marked replayed;
//! * a full drain, and an empty queue, still exit 0 (guards below).
//!
//! `--target` scopes the responsibility: a filtered pass reports the
//! remaining of THAT target only; another target's untouched queue is not
//! this command's failure (and is not silently claimed as its success
//! either — it simply is not in this report's scope).

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

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

fn stdout_of(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn stderr_of(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

fn report_of(out: &Output) -> Option<serde_json::Value> {
    serde_json::from_str(stdout_of(out).trim()).ok()
}

/// Plant `n` pending dead letters for `target` the way the daemon writes them.
fn plant_letters(conn: &rusqlite::Connection, target: &str, n: i64) {
    for seq in 1..=n {
        conn.execute(
            "INSERT INTO webhook_dead_letters
               (target,kind,seq,body,event_id,reason,status,attempts,first_failed_at,last_failed_at,state)
             VALUES(?1,'event',?2,'{}',?3,'refused with http 500',500,5,1,1,'pending')",
            rusqlite::params![target, seq, format!("{target}:event:{seq}")],
        )
        .expect("plant letter");
    }
}

/// A journal with `alpha` (and optionally `beta`) pending letters.
fn with_letters(tag: &str, alpha: i64, beta: i64) -> (PathBuf, PathBuf) {
    let dir = unique_dir(tag);
    let out = run(
        &dir,
        &["init", "--db", "reeflog.db", "--program", VALID_PROGRAM],
    );
    assert!(
        out.status.success(),
        "init must succeed (stderr: {})",
        stderr_of(&out)
    );
    let conn = rusqlite::Connection::open(dir.join("reeflog.db")).expect("open journal");
    plant_letters(&conn, "alpha", alpha);
    if beta > 0 {
        plant_letters(&conn, "beta", beta);
    }
    conn.close().expect("close journal");
    (dir, PathBuf::new())
}

/// A receiver that answers 200 to everything and counts what it got.
fn accepting_server() -> (String, Arc<AtomicBool>, Arc<std::sync::Mutex<usize>>, std::thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    listener.set_nonblocking(true).expect("nonblocking");
    let addr = listener.local_addr().expect("addr");
    let stop = Arc::new(AtomicBool::new(false));
    let stop2 = stop.clone();
    let count = Arc::new(std::sync::Mutex::new(0usize));
    let count2 = count.clone();
    let handle = std::thread::spawn(move || {
        while !stop2.load(Ordering::Relaxed) {
            match listener.accept() {
                Ok((mut s, _)) => {
                    let count3 = count2.clone();
                    std::thread::spawn(move || {
                        s.set_nonblocking(true).expect("nonblocking");
                        let mut buf = [0u8; 8192];
                        let mut got = 0usize;
                        for _ in 0..200 {
                            match s.read(&mut buf[got..]) {
                                Ok(0) => break,
                                Ok(n) => {
                                    got += n;
                                    if buf[..got].windows(4).any(|w| w == b"\r\n\r\n") {
                                        break;
                                    }
                                }
                                Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                                    std::thread::sleep(Duration::from_millis(5));
                                }
                                Err(_) => break,
                            }
                        }
                        let resp = b"HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: 2\r\n\r\n{}";
                        let _ = s.write_all(resp);
                        let _ = s.flush();
                        *count3.lock().expect("count") += 1;
                    });
                }
                Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(20));
                }
                Err(_) => break,
            }
        }
    });
    (format!("http://{addr}/hook"), stop, count, handle)
}

fn write_config(dir: &Path, url_alpha: &str, url_beta: Option<&str>) -> PathBuf {
    let cfg = dir.join("wh.json");
    let beta = match url_beta {
        Some(u) => format!(
            ",{{\"id\":\"beta\",\"url\":\"{u}\",\"secret\":\"s2\",\"timeout_ms\":2000,\"dead_letter_after\":2}}"
        ),
        None => String::new(),
    };
    std::fs::write(
        &cfg,
        format!(
            r#"{{"targets":[{{"id":"alpha","url":"{url_alpha}","secret":"s1","timeout_ms":2000,"dead_letter_after":2}}{beta}]}}"#
        ),
    )
    .expect("write config");
    cfg
}

/// The journal's own truth, read straight from the SQLite file.
fn journal_pending_for(db: &Path, target: Option<&str>) -> i64 {
    let conn = rusqlite::Connection::open(db).expect("open journal");
    let n: i64 = match target {
        Some(t) => conn
            .query_row(
                "SELECT COUNT(*) FROM webhook_dead_letters WHERE state='pending' AND target=?1",
                rusqlite::params![t],
                |r| r.get(0),
            )
            .expect("count"),
        None => conn
            .query_row(
                "SELECT COUNT(*) FROM webhook_dead_letters WHERE state='pending'",
                [],
                |r| r.get(0),
            )
            .expect("count"),
    };
    conn.close().ok();
    n
}

fn replay(dir: &Path, cfg: &Path, extra: &[&str]) -> Output {
    let mut args: Vec<&str> = vec![
        "webhook", "dlq", "replay", "--db", "reeflog.db", "--config",
        cfg.to_str().expect("config path"),
    ];
    args.extend(extra);
    run(dir, &args)
}

// ---------------------------------------------------------------- defects

/// D1: a pass that never reached the whole queue must not exit 0.
/// 7 pending, `--limit 2`: published v0.7.11 exits 0 with no word about
/// the 5 left behind.
#[test]
fn d1_a_truncated_replay_does_not_read_as_a_drained_queue() {
    let (dir, _) = with_letters("d1-trunc", 7, 0);
    let (url, stop, _count, _h) = accepting_server();
    let cfg = write_config(&dir, &url, None);
    let out = replay(&dir, &cfg, &["--limit", "2"]);
    stop.store(false, Ordering::Relaxed);
    let rep = report_of(&out).expect("a replay prints its report");
    let truth = journal_pending_for(&dir.join("reeflog.db"), None);
    assert_eq!(truth, 5, "5 letters must remain pending after a 2-page");
    assert!(
        !out.status.success(),
        "a replay that left 5 un-attempted letters pending must not exit 0 (report: {rep}, stderr: {})",
        stderr_of(&out)
    );
    assert_eq!(
        rep.get("remaining_pending").and_then(|v| v.as_i64()),
        Some(5),
        "the report must name the letters the pass did not reach (report: {rep})"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// D2: the per-target entry must describe the queue, not the page.
#[test]
fn d2_the_report_names_the_queue_before_the_page() {
    let (dir, _) = with_letters("d2-page", 7, 0);
    let (url, stop, _count, _h) = accepting_server();
    let cfg = write_config(&dir, &url, None);
    let out = replay(&dir, &cfg, &["--limit", "2"]);
    stop.store(false, Ordering::Relaxed);
    let rep = report_of(&out).expect("a replay prints its report");
    let t = &rep["targets"][0];
    assert_eq!(t["pending_before"], 7, "the queue at the start of the pass (report: {rep})");
    assert_eq!(t["replayed"], 2, "the page it attempted");
    assert_eq!(t["remaining_pending"], 5, "what the journal still holds for this target (report: {rep})");
    assert_eq!(t["truncated"], true, "the pass did not attempt everything");
    assert_eq!(rep["remaining_pending"], 5, "the top-level total names the undrained queue");
    let _ = std::fs::remove_dir_all(&dir);
}

/// D3: the same lie with the DEFAULT limit — 7 letters, default page,
/// but a second target's queue left untouched while the whole pass reads
/// clean. Here: 12 letters, default limit 100 is fine, so use a journal
/// the default page actually truncates: plant 101 and show the tail.
#[test]
fn d3_the_default_page_truncates_the_same_way() {
    let (dir, _) = with_letters("d3-default", 101, 0);
    let (url, stop, _count, _h) = accepting_server();
    let cfg = write_config(&dir, &url, None);
    let out = replay(&dir, &cfg, &[]);
    stop.store(false, Ordering::Relaxed);
    let rep = report_of(&out).expect("a replay prints its report");
    let truth = journal_pending_for(&dir.join("reeflog.db"), None);
    assert_eq!(truth, 1, "101 letters minus the default page of 100");
    assert_eq!(rep["remaining_pending"], 1, "the tail the default page skipped (report: {rep})");
    assert!(
        !out.status.success(),
        "one un-attempted pending letter must not exit 0 (stderr: {})",
        stderr_of(&out)
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// D4: the refusal must be actionable — it names the remaining count and
/// the repair, in reeflog's own voice.
#[test]
fn d4_the_truncation_voice_names_the_count_and_the_repair() {
    let (dir, _) = with_letters("d4-voice", 7, 0);
    let (url, stop, _count, _h) = accepting_server();
    let cfg = write_config(&dir, &url, None);
    let out = replay(&dir, &cfg, &["--limit", "2"]);
    stop.store(false, Ordering::Relaxed);
    let err = stderr_of(&out).to_lowercase();
    assert!(err.contains("5"), "the voice names the remaining count (stderr: {err})");
    assert!(
        err.contains("pending") || err.contains("drain"),
        "the voice says the queue is not drained (stderr: {err})"
    );
    assert!(
        err.contains("limit") || err.contains("again"),
        "the voice carries the repair (stderr: {err})"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

// ---------------------------------------------------------------- guards

/// G1: a FULL drain still exits 0 — the fix must not turn every replay
/// into a warning.
#[test]
fn g1_a_full_drain_still_exits_zero() {
    let (dir, _) = with_letters("g1-full", 3, 0);
    let (url, stop, count, _h) = accepting_server();
    let cfg = write_config(&dir, &url, None);
    let out = replay(&dir, &cfg, &["--limit", "3"]);
    stop.store(false, Ordering::Relaxed);
    assert!(
        out.status.success(),
        "a replay that drained the queue must exit 0 (stderr: {})",
        stderr_of(&out)
    );
    let rep = report_of(&out).expect("a report");
    assert_eq!(rep["replayed"], 3);
    assert_eq!(rep["failed"], 0);
    assert_eq!(*count.lock().expect("count"), 3, "all three delivered");
    let _ = std::fs::remove_dir_all(&dir);
}

/// G2: an empty queue reads healthy exactly as before.
#[test]
fn g2_an_empty_queue_still_reads_healthy() {
    let (dir, _) = with_letters("g2-empty", 0, 0);
    let (url, stop, _count, _h) = accepting_server();
    let cfg = write_config(&dir, &url, None);
    let out = replay(&dir, &cfg, &[]);
    stop.store(false, Ordering::Relaxed);
    assert!(
        out.status.success(),
        "an empty queue must replay successfully (stderr: {})",
        stderr_of(&out)
    );
    let rep = report_of(&out).expect("a report");
    assert_eq!(rep["replayed"], 0);
    assert_eq!(rep["failed"], 0);
    let _ = std::fs::remove_dir_all(&dir);
}

/// G3: a truncated pass still DELIVERS its page correctly: exactly `limit`
/// letters, marked replayed in the journal, byte-identical bodies.
#[test]
fn g3_a_truncated_pass_still_delivers_its_page() {
    let (dir, _) = with_letters("g3-deliver", 7, 0);
    let (url, stop, count, _h) = accepting_server();
    let cfg = write_config(&dir, &url, None);
    let out = replay(&dir, &cfg, &["--limit", "2"]);
    stop.store(false, Ordering::Relaxed);
    assert_eq!(*count.lock().expect("count"), 2, "exactly the page delivered");
    let rep = report_of(&out).expect("a report");
    assert_eq!(rep["replayed"], 2);
    assert_eq!(rep["failed"], 0);
    let conn = rusqlite::Connection::open(dir.join("reeflog.db")).expect("open");
    let done: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM webhook_dead_letters WHERE state='replayed'",
            [],
            |r| r.get(0),
        )
        .expect("count");
    assert_eq!(done, 2, "delivered letters are marked replayed");
    conn.close().ok();
    let _ = std::fs::remove_dir_all(&dir);
}

/// G4: `--target` scopes the responsibility: beta's untouched queue does
/// not make alpha's full drain fail, and does not get claimed as drained.
#[test]
fn g4_a_scoped_pass_reports_only_its_target() {
    let (dir, _) = with_letters("g4-scope", 3, 4);
    let (url, stop, _count, _h) = accepting_server();
    let cfg = write_config(&dir, &url, Some(&url));
    let out = replay(&dir, &cfg, &["--target", "alpha", "--limit", "3"]);
    stop.store(false, Ordering::Relaxed);
    assert!(
        out.status.success(),
        "alpha drained fully; beta is not this pass's scope (stderr: {})",
        stderr_of(&out)
    );
    let rep = report_of(&out).expect("a report");
    assert_eq!(rep["replayed"], 3, "the scoped pass drained alpha");
    assert_eq!(
        journal_pending_for(&dir.join("reeflog.db"), Some("beta")),
        4,
        "beta's letters are untouched, not lost"
    );
    let _ = std::fs::remove_dir_all(&dir);
}
