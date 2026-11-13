//! `--limit 0` must mean ZERO on the delivery channels, and the push report
//! must describe the QUEUE, not the PAGE.
//!
//! PULS AI, 2026-11-14. Measured on the PUBLISHED v0.7.25 linux asset
//! (sha256 b826cbcb14f436fb0168d94b4fa758159fab7674d364ea3d5f17f87ac986193e,
//! == the digest GitHub publishes for the release):
//!
//! * F1a `webhook push --limit 0` POSTED one signed event per pass and
//!   advanced the target cursor (two passes with `--limit 0` delivered 2 of
//!   5 planted rows, exit 0). `limit.max(1)` in `deliver_events` /
//!   `deliver_reorgs` rewrites the operator's ZERO into one delivery per
//!   pass — the same class the repo closed for `timeout_ms: 0` in v0.7.20
//!   ("silently rewritten to 1 ms by the poster"). `tail --limit 0` delivers
//!   ZERO rows and is pinned legal by `numeric_flag_domain.rs`; a value the
//!   checker allows must not be re-written by the consumer into a third
//!   thing that is neither the request nor the default.
//! * F1b `webhook dlq replay --limit 0` attempted one letter
//!   (`webhook_dead_letters` binds `limit.max(1)`): quarantined bytes left
//!   the machine on a pass that was asked to move nothing.
//! * F2  `webhook push --limit 2` against 5 pending rows reported
//!   `delivered: 2`, exit 0, and NOTHING in the report named the three
//!   behind the page — the exact lie v0.7.12 removed from `dlq replay`
//!   ("the REPORT describes the QUEUE, not the PAGE its `--limit`
//!   attempted"). A cron reading the JSON believed the mailbox drained.
//!
//! The rule:
//! * `--limit 0` on `webhook push` attempts NOTHING: no body is sent, no
//!   cursor moves, the pass exits 0 (it did exactly what was asked), and the
//!   report says so (`page_limit: 0`, rows behind the cursor named);
//! * `--limit 0` on `webhook dlq replay` attempts no letters; the letters
//!   still pending keep the pass at exit 3 naming them, as every
//!   never-reached page already does;
//! * every `webhook push` report carries `remaining_behind_cursor` and
//!   `truncated` per target — journal facts, so the page can never again
//!   stand in for the queue. The exit code of a bounded pass that got
//!   through stays 0: unlike the dlq, the next `push` delivers the rest;
//!   the report must simply stop pretending the page was the queue.
//!
//! Guards (green on BOTH trees): a positive `--limit` delivers exactly that
//! many (G1); the default limit drains the queue with `truncated: false`
//! (G2); `tail --limit 0` still delivers zero rows (G3).

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

/// A receiver that answers 200 to everything and KEEPS the bodies.
type Bodies = Arc<Mutex<Vec<String>>>;

fn accepting_server() -> (String, Arc<AtomicBool>, Bodies, std::thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    listener.set_nonblocking(true).expect("nonblocking");
    let addr = listener.local_addr().expect("addr");
    let stop = Arc::new(AtomicBool::new(false));
    let stop2 = stop.clone();
    let bodies: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let bodies2 = bodies.clone();
    let handle = std::thread::spawn(move || {
        while !stop2.load(Ordering::Relaxed) {
            match listener.accept() {
                Ok((mut s, _)) => {
                    let bodies3 = bodies2.clone();
                    std::thread::spawn(move || {
                        s.set_nonblocking(true).expect("nonblocking");
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
                                        let body_start = head
                                            .find("\r\n\r\n")
                                            .map(|i| i + 4)
                                            .unwrap_or(got);
                                        let mut body = buf[body_start..got].to_vec();
                                        while body.len() < want {
                                            match s.read(&mut buf) {
                                                Ok(0) => break,
                                                Ok(n) => body.extend_from_slice(&buf[..n]),
                                                Err(ref e)
                                                    if e.kind() == std::io::ErrorKind::WouldBlock =>
                                                {
                                                    std::thread::sleep(Duration::from_millis(5));
                                                }
                                                Err(_) => break,
                                            }
                                        }
                                        bodies3
                                            .lock()
                                            .expect("bodies")
                                            .push(String::from_utf8_lossy(&body).to_string());
                                        break;
                                    }
                                }
                                Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                                    std::thread::sleep(Duration::from_millis(5));
                                }
                                Err(_) => break,
                            }
                        }
                        let _ = s.write_all(b"HTTP/1.1 200 OK\r\ncontent-length: 2\r\n\r\n{}");
                        let _ = s.flush();
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

fn write_config(dir: &Path, url: &str) -> PathBuf {
    let cfg = dir.join("wh.json");
    std::fs::write(
        &cfg,
        format!(
            r#"{{"targets":[{{"id":"t1","url":"{url}","secret":"s3cret","timeout_ms":5000,"dead_letter_after":0}}]}}"#
        ),
    )
    .expect("write config");
    cfg
}

fn init(dir: &Path) {
    let out = run(dir, &["init", "--db", "reeflog.db", "--program", VALID_PROGRAM]);
    assert!(out.status.success(), "init: {}", stderr_of(&out));
}

/// A recorded retraction: the second channel `--limit 0` must also hold.
fn plant_reorg(dir: &Path) {
    let conn = rusqlite::Connection::open(dir.join("reeflog.db")).expect("open journal");
    conn.execute(
        "INSERT INTO reorgs(signature,slot,detected_at,status_source,tx_source,detail)
         VALUES('REORG1',44000001,1700000000000,'ws','ws','')",
        [],
    )
    .expect("plant reorg");
    conn.close().expect("close journal");
}

fn plant_full(dir: &Path, n: usize) {
    let conn = rusqlite::Connection::open(dir.join("reeflog.db")).expect("open journal");
    for i in 1..=n {
        conn.execute(
            "INSERT INTO events(signature,slot,err,logs,source,first_seen,payload_state)
             VALUES(?1,?2,NULL,?3,'ws',?4,'held')",
            rusqlite::params![format!("FULL{i}"), 100 + i as i64, format!(r#"["log {i}"]"#), 1_700_000_000_000i64 + i as i64],
        )
        .expect("plant row");
    }
    conn.close().expect("close journal");
}

fn next_seq(dir: &Path) -> i64 {
    let conn = rusqlite::Connection::open(dir.join("reeflog.db")).expect("open journal");
    let n: i64 = conn
        .query_row(
            "SELECT COALESCE((SELECT next_seq FROM webhook_state WHERE target='t1'),0)",
            [],
            |r| r.get(0),
        )
        .expect("state");
    conn.close().ok();
    n
}

fn pending_letters(dir: &Path) -> i64 {
    let conn = rusqlite::Connection::open(dir.join("reeflog.db")).expect("open journal");
    let n: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM webhook_dead_letters WHERE target='t1' AND state='pending'",
            [],
            |r| r.get(0),
        )
        .expect("count");
    conn.close().ok();
    n
}

/// F1a (defect, RED on published v0.7.25): `push --limit 0` attempted one
/// delivery and advanced the cursor. Must attempt NOTHING and move NOTHING.
#[test]
fn push_with_limit_zero_sends_no_bytes_and_moves_no_cursor() {
    let dir = unique_dir("zero-push");
    init(&dir);
    plant_full(&dir, 5);
    plant_reorg(&dir);
    let (url, stop, bodies, handle) = accepting_server();
    let cfg = write_config(&dir, &url);

    for _ in 1..=2 {
        let out = run(
            &dir,
            &["webhook", "push", "--db", "reeflog.db", "--config", cfg.to_str().unwrap(), "--limit", "0"],
        );
        let rep = report_of(&out);
        let t = &rep["targets"][0];
        assert_eq!(
            t["page_limit"].as_i64(),
            Some(0),
            "the report states the budget it was given: {t}"
        );
        assert_eq!(
            t["delivered"].as_i64(),
            Some(0),
            "a pass asked to deliver zero must deliver zero (got: {t}) — stderr: {}",
            stderr_of(&out)
        );
        assert_eq!(
            t["reorg_delivered"].as_i64(),
            Some(0),
            "and no retraction either — the reorg channel honours zero too: {t}"
        );
        assert_eq!(
            t["next_reorg_id"].as_i64(),
            Some(0),
            "the retraction cursor may not move either: {t}"
        );
        assert_eq!(
            t["next_seq"].as_i64(),
            Some(0),
            "the cursor may not move past a row the pass never sent: {t}"
        );
    }
    let got = bodies.lock().expect("bodies").clone();
    assert!(
        got.is_empty(),
        "the receiver must hold NOTHING: {} body(ies) arrived on --limit 0",
        got.len()
    );
    assert_eq!(next_seq(&dir), 0, "the journal cursor stays at 0");
    stop.store(true, Ordering::Relaxed);
    let _ = handle.join();
}

/// F2 (defect, RED on published v0.7.25): the report of a page that stopped
/// before the queue must NAME the rows behind it. `dlq replay` learned this
/// rule in v0.7.12; `webhook push` never got it.
#[test]
fn a_truncated_push_names_the_rows_behind_the_page() {
    let dir = unique_dir("zero-truncated");
    init(&dir);
    plant_full(&dir, 5);
    let (url, stop, _bodies, handle) = accepting_server();
    let cfg = write_config(&dir, &url);

    let out = run(
        &dir,
        &["webhook", "push", "--db", "reeflog.db", "--config", cfg.to_str().unwrap(), "--limit", "2"],
    );
    let rep = report_of(&out);
    let t = &rep["targets"][0];
    assert_eq!(t["delivered"].as_i64(), Some(2), "the page delivered two: {t}");
    assert_eq!(
        t["remaining_behind_cursor"].as_i64(),
        Some(3),
        "the report must name the three rows still behind the cursor (got: {t})"
    );
    assert_eq!(
        t["truncated"].as_bool(),
        Some(true),
        "a page that stopped before the queue is truncated (got: {t})"
    );
    assert!(
        out.status.success(),
        "a clean bounded pass still exits 0 — the next pass delivers the rest: {}",
        stderr_of(&out)
    );
    stop.store(true, Ordering::Relaxed);
    let _ = handle.join();
}

/// F1b (defect, RED on published v0.7.25): `dlq replay --limit 0` must
/// attempt no letter. Before the fix the journal bound `limit.max(1)` and
/// one quarantined body left the machine anyway; afterwards the still
/// pending letters hold the pass at exit 3, naming them.
#[test]
fn replay_with_limit_zero_moves_no_letter() {
    let dir = unique_dir("zero-replay");
    init(&dir);
    plant_full(&dir, 3);
    let (url, stop, bodies, handle) = accepting_server();
    let cfg = write_config(&dir, &url);

    // Plant two pending letters directly: quarantined bytes the receiver has
    // never held.
    let conn = rusqlite::Connection::open(dir.join("reeflog.db")).expect("open journal");
    for i in 1..=2 {
        conn.execute(
            "INSERT INTO webhook_dead_letters(target,kind,seq,body,event_id,reason,status,attempts,first_failed_at,last_failed_at,state)
             VALUES('t1','event',?1,'{}','t1:event:?1','refused with http 403',403,1,?2,?2,'pending')",
            rusqlite::params![i, 1_700_000_000_000i64],
        )
        .expect("plant letter");
    }
    drop(conn);

    let out = run(
        &dir,
        &["webhook", "dlq", "replay", "--db", "reeflog.db", "--config", cfg.to_str().unwrap(), "--limit", "0"],
    );
    let rep = report_of(&out);
    assert_eq!(
        rep["replayed"].as_i64(),
        Some(0),
        "a replay asked to move zero letters moves zero (got: {rep})"
    );
    assert_eq!(
        rep["pending_before"].as_i64(),
        Some(2),
        "the queue truth still travels: {rep}"
    );
    assert_eq!(
        pending_letters(&dir),
        2,
        "no letter may leave the queue on a --limit 0 pass"
    );
    let got = bodies.lock().expect("bodies").clone();
    assert!(
        got.is_empty(),
        "the receiver must hold NOTHING from a --limit 0 replay: {} body(ies)",
        got.len()
    );
    assert_eq!(
        out.status.code(),
        Some(3),
        "letters the page never reached hold the pass at 3 — it may not read drained"
    );
    let err = stderr_of(&out);
    assert!(
        err.contains("2") && err.contains("not reached"),
        "exit 3 must name the count and the reason (got: {err})"
    );
    stop.store(true, Ordering::Relaxed);
    let _ = handle.join();
}

/// G1 (guard, green on BOTH trees): a positive limit delivers exactly that
/// many — the page still works.
#[test]
fn a_positive_limit_still_delivers_exactly_that_many() {
    let dir = unique_dir("zero-guard-positive");
    init(&dir);
    plant_full(&dir, 5);
    let (url, stop, bodies, handle) = accepting_server();
    let cfg = write_config(&dir, &url);
    let out = run(
        &dir,
        &["webhook", "push", "--db", "reeflog.db", "--config", cfg.to_str().unwrap(), "--limit", "3"],
    );
    let rep = report_of(&out);
    assert_eq!(rep["targets"][0]["delivered"].as_i64(), Some(3), "exactly three: {rep}");
    assert_eq!(bodies.lock().expect("bodies").len(), 3, "three bodies arrived");
    stop.store(true, Ordering::Relaxed);
    let _ = handle.join();
}

/// G2 (guard, green on BOTH trees — new fields read false/0): the default
/// pass drains the queue and says so.
#[test]
fn a_full_pass_reports_the_drained_queue() {
    let dir = unique_dir("zero-guard-full");
    init(&dir);
    plant_full(&dir, 4);
    let (url, stop, _bodies, handle) = accepting_server();
    let cfg = write_config(&dir, &url);
    let out = run(
        &dir,
        &["webhook", "push", "--db", "reeflog.db", "--config", cfg.to_str().unwrap()],
    );
    let rep = report_of(&out);
    let t = &rep["targets"][0];
    assert_eq!(t["delivered"].as_i64(), Some(4), "all four: {t}");
    assert_eq!(t["remaining_behind_cursor"].as_i64(), Some(0), "queue drained: {t}");
    assert_eq!(t["truncated"].as_bool(), Some(false), "not truncated: {t}");
    stop.store(true, Ordering::Relaxed);
    let _ = handle.join();
}

/// G3 (guard, green on BOTH trees): `tail --limit 0` already honours zero —
/// the CLI channel this fix must stay symmetrical with.
#[test]
fn tail_with_limit_zero_still_delivers_zero_rows() {
    let dir = unique_dir("zero-guard-tail");
    init(&dir);
    plant_full(&dir, 5);
    let out = run(&dir, &["tail", "--db", "reeflog.db", "--cursor", "c.txt", "--limit", "0"]);
    assert!(out.status.success(), "tail --limit 0: {}", stderr_of(&out));
    let stdout = String::from_utf8_lossy(&out.stdout);
    let lines: Vec<&str> = stdout.lines().collect();
    assert!(lines.is_empty(), "tail must print zero rows, printed {}", lines.len());
}
