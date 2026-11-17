//! `webhook push` and the daemon must NAME a retraction-channel block, and
//! the retraction's reason must be reported on its own.
//!
//! PULS AO, 2026-11-17. Measured on this tree BEFORE the fix (the real
//! binary against a real journal, a receiver that accepts events and refuses
//! retractions with http 404; `research/retraction-block-blind-2026-11-17.json`,
//! verdict `retraction-block-blind`):
//!
//! * D1 the push report DOES carry the facts — `blocked_reorg_id: 1`,
//!   `blocked: true`, `remaining_reorgs_behind_cursor: 3` — but the line the
//!   operator actually reads names the WRONG position: `reeflog: blocked —
//!   t1 at seq None: refused with http 404`. The event cursor is `None`
//!   because the block is on the RETRACTION channel, whose cursor is a
//!   reorg id, not an event seq. The line reads as if the event queue were
//!   stuck at nothing, and the operator looks in the wrong place: the
//!   retraction is the one message whose lateness costs money, and the line
//!   that should say so named a position that does not exist.
//!
//! * D2 the operator line leaked Rust's `{:?}` formatting: an EVENT block
//!   printed `at seq Some(1)` instead of `at seq 1` — the same root cause as
//!   D1 (formatting an `Option` the reader has no use for).
//!
//! * D3 the report had no retraction reason of its own: `deliver_reorgs`
//!   wrote its reason into the SAME `blocked_error` the event channel uses,
//!   so a consumer blocked on both queues reported the retraction's reason
//!   against the event's seq. One field, two queues — the reader cannot
//!   tell which one failed.
//!
//! * D4 the daemon printed NOTHING for a retraction block: the event
//!   channel got a line naming its seq and its reason, the retraction
//!   channel got silence. A queue that stays blocked forever, with no line
//!   saying so, is indistinguishable from a queue that is merely idle.
//!
//! NOT part of this fix, and deliberately so: a refused retraction is still
//! never QUARANTINED. The design decision stands (the DLQ commit: "a late
//! retraction costs money, so that queue stays blocked rather than being
//! papered over", pinned by `webhook::tests::a_retraction_is_never_quarantined`).
//! This fix does not overturn it: it makes the block legible, it does not
//! make it drain. G2 pins the rule so it cannot be lost by accident.
//!
//! The rule:
//! * a blocked retraction is NAMED as a retraction: the line says the
//!   RETRACTION channel is blocked, at the reorg id, and that the
//!   retraction is still owed.
//! * the retraction's reason travels on `reorg_error`, never on the event
//!   channel's `error`.
//! * no Rust debug formatting reaches an operator-facing line.
//!
//! Guards (green on BOTH trees):
//! * G1 an accepted retraction is delivered, receipts, and advances the
//!   cursor as before.
//! * G2 a refused retraction is NEVER quarantined and never skips the
//!   cursor: the pre-existing rule, pinned so this fix cannot loosen it.
//! * G3 the report's channel facts (`blocked_seq`, `blocked_reorg_id`) are
//!   unchanged by this fix — the report already carried them correctly; only
//!   the human lines and the reason field were wrong.

use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
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

fn stderr_of(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

fn report_of(out: &Output) -> serde_json::Value {
    serde_json::from_str(String::from_utf8_lossy(&out.stdout).trim()).expect("a JSON report")
}

/// A receiver: the url to post to, a stop flag, the bodies it received,
/// and the thread serving it.
type Receiver = (
    String,
    Arc<AtomicBool>,
    Arc<Mutex<Vec<String>>>,
    std::thread::JoinHandle<()>,
);

/// A receiver that answers 200 to events and REFUSES retractions with
/// http 404 — a consumer that takes the event but rejects the retraction.
fn refusing_retraction_server() -> Receiver {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    listener.set_nonblocking(true).expect("nonblocking");
    let addr = listener.local_addr().expect("addr");
    let stop = Arc::new(AtomicBool::new(false));
    let stop2 = stop.clone();
    let bodies = Arc::new(Mutex::new(Vec::<String>::new()));
    let bodies2 = bodies.clone();
    let handle = std::thread::spawn(move || {
        while !stop2.load(Ordering::Relaxed) {
            match listener.accept() {
                Ok((mut s, _)) => {
                    let bodies3 = bodies2.clone();
                    std::thread::spawn(move || {
                        if let Some(text) = read_body(&mut s) {
                            let refused = text.contains("\"type\":\"reorg\"");
                            bodies3.lock().expect("bodies").push(text);
                            let resp: &[u8] = if refused {
                                b"HTTP/1.1 404 Not Found\r\ncontent-type: application/json\r\ncontent-length: 2\r\n\r\n{}"
                            } else {
                                b"HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: 2\r\n\r\n{}"
                            };
                            let _ = s.write_all(resp);
                            let _ = s.flush();
                        }
                    });
                }
                Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(20));
                }
                Err(_) => break,
            }
        }
    });
    (format!("http://{addr}/hook"), stop, bodies, handle)
}

/// A receiver that answers 200 to everything — the control.
fn accepting_server() -> Receiver {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    listener.set_nonblocking(true).expect("nonblocking");
    let addr = listener.local_addr().expect("addr");
    let stop = Arc::new(AtomicBool::new(false));
    let stop2 = stop.clone();
    let bodies = Arc::new(Mutex::new(Vec::<String>::new()));
    let bodies2 = bodies.clone();
    let handle = std::thread::spawn(move || {
        while !stop2.load(Ordering::Relaxed) {
            match listener.accept() {
                Ok((mut s, _)) => {
                    let bodies3 = bodies2.clone();
                    std::thread::spawn(move || {
                        if let Some(text) = read_body(&mut s) {
                            bodies3.lock().expect("bodies").push(text);
                            let resp = b"HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: 2\r\n\r\n{}";
                            let _ = s.write_all(resp);
                            let _ = s.flush();
                        }
                    });
                }
                Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(20));
                }
                Err(_) => break,
            }
        }
    });
    (format!("http://{addr}/hook"), stop, bodies, handle)
}

/// Read one request body out of a non-blocking connection.
fn read_body(s: &mut std::net::TcpStream) -> Option<String> {
    let mut buf = [0u8; 65536];
    let mut got = 0usize;
    for _ in 0..400 {
        match s.read(&mut buf[got..]) {
            Ok(0) => break,
            Ok(n) => {
                got += n;
                if buf[..got].windows(4).any(|w| w == b"\r\n\r\n") {
                    let head = String::from_utf8_lossy(&buf[..got]).to_string();
                    let want: usize = head
                        .lines()
                        .find(|l| l.to_ascii_lowercase().starts_with("content-length:"))
                        .and_then(|l| l.split(':').nth(1))
                        .and_then(|v| v.trim().parse().ok())
                        .unwrap_or(0);
                    let body_start = head.find("\r\n\r\n").map(|i| i + 4).unwrap_or(got);
                    let mut body = buf[body_start..got].to_vec();
                    while body.len() < want {
                        match s.read(&mut buf) {
                            Ok(0) => break,
                            Ok(n) => body.extend_from_slice(&buf[..n]),
                            Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                                std::thread::sleep(Duration::from_millis(5));
                            }
                            Err(_) => break,
                        }
                    }
                    return Some(String::from_utf8_lossy(&body).to_string());
                }
            }
            Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(5));
            }
            Err(_) => break,
        }
    }
    None
}

fn write_config(dir: &Path, url: &str, dead_letter_after: i64) -> PathBuf {
    let cfg = dir.join("wh.json");
    std::fs::write(
        &cfg,
        format!(
            r#"{{"targets":[{{"id":"t1","url":"{url}","secret":"s3cret","timeout_ms":5000,"dead_letter_after":{dead_letter_after}}}]}}"#
        ),
    )
    .expect("write config");
    cfg
}

fn init(dir: &Path) {
    let out = run(dir, &["init", "--db", "reeflog.db", "--program", VALID_PROGRAM]);
    assert!(out.status.success(), "init: {}", stderr_of(&out));
}

/// One deliverable event and `n` recorded retractions.
fn plant(dir: &Path, n: i64) {
    let conn = rusqlite::Connection::open(dir.join("reeflog.db")).expect("open journal");
    conn.execute(
        "INSERT INTO events(signature,slot,err,logs,source,first_seen,payload_state)
         VALUES('sigEV1',101,NULL,'[\"log\"]','ws',1700000000000,'held')",
        [],
    )
    .expect("plant event");
    for i in 1..=n {
        conn.execute(
            "INSERT INTO reorgs(signature,slot,detected_at,status_source,tx_source,detail)
             VALUES(?1,?2,1700000000000,'slot','tx','the chain unmade it')",
            rusqlite::params![format!("sigRET{i}"), 200 + i],
        )
        .expect("plant reorg");
    }
    conn.close().expect("close journal");
}

fn dead_letters(dir: &Path) -> Vec<(String, i64)> {
    let conn = rusqlite::Connection::open(dir.join("reeflog.db")).expect("open journal");
    let out = {
        let mut stmt = conn
            .prepare("SELECT kind, seq FROM webhook_dead_letters ORDER BY id")
            .expect("prepare");
        let rows = stmt
            .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))
            .expect("query");
        rows.map(|r| r.expect("row")).collect::<Vec<_>>()
    };
    conn.close().ok();
    out
}

fn next_reorg_id(dir: &Path) -> i64 {
    let conn = rusqlite::Connection::open(dir.join("reeflog.db")).expect("open journal");
    let n: i64 = conn
        .query_row(
            "SELECT COALESCE((SELECT next_reorg_id FROM webhook_state WHERE target='t1'),0)",
            [],
            |r| r.get(0),
        )
        .expect("state");
    conn.close().ok();
    n
}

fn push(dir: &Path, cfg: &Path) -> Output {
    run(dir, &["webhook", "push", "--db", "reeflog.db", "--config", cfg.to_str().unwrap()])
}

// ---------------------------------------------------------------- defect

/// D1: the line the operator reads must name the RETRACTION channel and the
/// reorg id — not `seq None`.
///
/// (Defect: the pre-fix tree prints `blocked — t1 at seq None: refused with
/// http 404` — the event cursor is `None` because the block is on the
/// retraction channel, so the line points the operator at a position that
/// does not exist.)
#[test]
fn a_blocked_retraction_is_named_as_a_retraction() {
    let dir = unique_dir("retr-named");
    init(&dir);
    plant(&dir, 3);

    let (url, stop, _bodies, handle) = refusing_retraction_server();
    let cfg = write_config(&dir, &url, 5);
    let out = push(&dir, &cfg);
    let stderr = stderr_of(&out);
    stop.store(true, Ordering::Relaxed);
    let _ = handle.join();

    let report = report_of(&out);
    let t = &report["targets"][0];
    assert_eq!(t["blocked"], serde_json::json!(true), "the target is blocked");
    assert_eq!(
        t["blocked_reorg_id"], serde_json::json!(1),
        "the blocked retraction is named in the report: {t}"
    );

    assert!(
        stderr.to_lowercase().contains("retraction"),
        "the operator's line must say the RETRACTION channel is blocked: {stderr}"
    );
    assert!(
        !stderr.contains("at seq None"),
        "the line must not point at a non-existent event position: {stderr}"
    );
    assert!(
        stderr.contains("reorg 1"),
        "the line must name the reorg id that is owed: {stderr}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// D1b: the REPORT must carry the retraction's reason on its own field, so
/// one queue's reason is never read as the other's.
///
/// (Defect: `reorg_error` did not exist; the reason was written into the
/// shared `blocked_error`.)
#[test]
fn the_report_carries_the_retraction_reason_on_its_own_field() {
    let dir = unique_dir("retr-reason");
    init(&dir);
    plant(&dir, 1);

    let (url, stop, _bodies, handle) = refusing_retraction_server();
    let cfg = write_config(&dir, &url, 5);
    let out = push(&dir, &cfg);
    stop.store(true, Ordering::Relaxed);
    let _ = handle.join();

    let report = report_of(&out);
    let t = &report["targets"][0];
    let reason = t["reorg_error"].as_str().unwrap_or("");
    assert!(
        reason.contains("404"),
        "the retraction's own reason must be reported: {t}"
    );
    assert_eq!(
        t["error"], serde_json::json!(null),
        "the EVENT channel's error must stay null when only the retraction queue is blocked: {t}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

// ---------------------------------------------------------------- guards

/// G1: an accepted retraction is delivered and receipts as before.
#[test]
fn an_accepted_retraction_is_delivered() {
    let dir = unique_dir("retr-accepted");
    init(&dir);
    plant(&dir, 2);

    let (url, stop, bodies, handle) = accepting_server();
    let cfg = write_config(&dir, &url, 1);
    let out = push(&dir, &cfg);
    let got = bodies.lock().expect("bodies").clone();
    stop.store(true, Ordering::Relaxed);
    let _ = handle.join();

    let report = report_of(&out);
    let t = &report["targets"][0];
    assert_eq!(t["reorg_delivered"], serde_json::json!(2), "both retractions delivered: {t}");
    assert_eq!(t["blocked"], serde_json::json!(false), "nothing is blocked");
    assert_eq!(next_reorg_id(&dir), 2, "the retraction cursor advanced");
    assert_eq!(
        got.iter().filter(|b| b.contains("\"type\":\"reorg\"")).count(),
        2,
        "both retraction bodies reached the receiver"
    );
    assert!(dead_letters(&dir).is_empty(), "nothing was quarantined");

    let _ = std::fs::remove_dir_all(&dir);
}

/// D2: the operator line leaked Rust's `{:?}` — a blocked EVENT printed
/// `at seq Some(1)`. The reader has no use for the `Option` wrapper, and
/// `Some(1)` in a log line is noise that hides the position.
///
/// (Defect: the pre-fix tree prints `blocked — t1 at seq Some(1): the
/// payload is still owed`.)
#[test]
fn a_blocked_event_names_the_event_seq_without_debug_formatting() {
    let dir = unique_dir("retr-event-block");
    init(&dir);
    // One event whose payload is owed: the event channel holds there.
    {
        let conn = rusqlite::Connection::open(dir.join("reeflog.db")).expect("open");
        conn.execute(
            "INSERT INTO events(signature,slot,err,logs,source,first_seen,payload_state)
             VALUES('sigOWED',101,NULL,NULL,'ws',1700000000000,NULL)",
            [],
        )
        .expect("plant");
        conn.close().expect("close");
    }

    let (url, stop, _bodies, handle) = accepting_server();
    let cfg = write_config(&dir, &url, 1);
    let out = push(&dir, &cfg);
    let stderr = stderr_of(&out);
    stop.store(true, Ordering::Relaxed);
    let _ = handle.join();

    let report = report_of(&out);
    let t = &report["targets"][0];
    assert_eq!(t["blocked_seq"], serde_json::json!(1), "the owed event is named: {t}");
    assert_eq!(t["blocked_reorg_id"], serde_json::json!(null), "no retraction is named: {t}");
    assert!(stderr.contains("seq 1"), "the event block must still name the event seq: {stderr}");
    assert!(
        !stderr.contains("Some(") && !stderr.contains("None"),
        "no Rust debug formatting may reach an operator-facing line: {stderr}"
    );
    assert!(
        !stderr.to_lowercase().contains("retraction"),
        "an EVENT block must not be reported as a retraction block: {stderr}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// D4: `dead_letter_after` must not move the diagnosis. Whatever the
/// operator configured, a refused retraction is reported the same way — the
/// config decides what happens to the LETTER, never whether the operator
/// hears about it.
///
/// (Defect: the pre-fix tree printed `at seq None` for every setting.)
#[test]
fn dead_letter_after_does_not_move_where_a_retraction_block_is_reported() {
    for dla in [0i64, 5] {
        let dir = unique_dir(&format!("retr-dla{dla}"));
        init(&dir);
        plant(&dir, 2);

        let (url, stop, _bodies, handle) = refusing_retraction_server();
        let cfg = write_config(&dir, &url, dla);
        let out = push(&dir, &cfg);
        let stderr = stderr_of(&out);
        stop.store(true, Ordering::Relaxed);
        let _ = handle.join();

        assert!(
            stderr.to_lowercase().contains("retraction"),
            "dead_letter_after={dla}: the block must still be named as a retraction: {stderr}"
        );
        assert!(
            !stderr.contains("at seq None"),
            "dead_letter_after={dla}: no non-existent position may be printed: {stderr}"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }
}

/// G3: the report's channel facts are UNCHANGED by this fix — the report
/// already carried them correctly; only the human lines and the reason
/// field were wrong. A fix that quietly moved a fact would be a second
/// defect wearing the first one's coat.
#[test]
fn the_report_keeps_naming_the_channel_facts_it_already_named() {
    let dir = unique_dir("retr-facts");
    init(&dir);
    plant(&dir, 3);

    let (url, stop, _bodies, handle) = refusing_retraction_server();
    let cfg = write_config(&dir, &url, 5);
    let out = push(&dir, &cfg);
    stop.store(true, Ordering::Relaxed);
    let _ = handle.join();

    let report = report_of(&out);
    let t = &report["targets"][0];
    assert_eq!(t["blocked"], serde_json::json!(true), "the target is blocked");
    assert_eq!(t["blocked_seq"], serde_json::json!(null), "no event is blocked: {t}");
    assert_eq!(t["blocked_reorg_id"], serde_json::json!(1), "the retraction is named: {t}");
    assert_eq!(
        t["remaining_reorgs_behind_cursor"], serde_json::json!(3),
        "the queue behind the cursor is still named: {t}"
    );
    assert_eq!(t["next_reorg_id"], serde_json::json!(0), "the cursor stayed: {t}");

    let _ = std::fs::remove_dir_all(&dir);
}

/// G2: a refused retraction is NEVER quarantined and NEVER skips the
/// cursor. This is the pre-existing design rule (the DLQ commit excludes
/// retractions on purpose — "a late retraction costs money, so that queue
/// stays blocked rather than being papered over"). Pinned so that this fix,
/// which only makes the block legible, cannot loosen the rule by accident.
#[test]
fn a_refused_retraction_is_never_quarantined_and_never_skips_the_cursor() {
    let dir = unique_dir("retr-no-quarantine");
    init(&dir);
    plant(&dir, 3);

    let (url, stop, _bodies, handle) = refusing_retraction_server();
    // The setting under which an EVENT would be quarantined: a refused
    // retraction must still not be.
    let cfg = write_config(&dir, &url, 1);
    for _ in 0..3 {
        let out = push(&dir, &cfg);
        assert!(
            !out.status.success(),
            "a refused retraction is not a clean pass: {}",
            stderr_of(&out)
        );
    }
    stop.store(true, Ordering::Relaxed);
    let _ = handle.join();

    assert!(
        dead_letters(&dir).is_empty(),
        "a refused retraction must NEVER be quarantined (design rule): {:?}",
        dead_letters(&dir)
    );
    assert_eq!(
        next_reorg_id(&dir), 0,
        "the retraction cursor must stay behind the refused retraction — nothing is skipped"
    );

    let _ = std::fs::remove_dir_all(&dir);
}
