//! `webhook status` must not report a failure a target has already recovered
//! from.
//!
//! PULS AG, 2026-11-13. `webhook_state.last_error` is written by every
//! blocked pass (`webhook_set_error`) and read by `webhook status` — but
//! nothing ever clears it. Measured on this tree before the fix
//! (`tools/measure-webhook-stale-error.mjs`, verdict
//! `stale-error-verdict-present`): pass 1 refused at seq 1 (`http 500`,
//! exit 3, `last_error = "http 500"`), pass 2 delivered BOTH rows against
//! the healthy endpoint and exited 0 — and `webhook status` still reported
//! `last_error: "http 500"`. The same class AD/AF closed on the delivery
//! channels, one command over: a report that says a target is stuck when it
//! is not. A monitoring caller that polls `status` alarms forever on a
//! failure that ended; an operator repairs the same endpoint twice.
//!
//! The rule (pass-level, not attempt-level): `last_error` is the reason the
//! most recent delivery pass for that target STOPPED, and it is cleared at
//! the start of the next pass for the same target — after the endpoint
//! binding is verified, so a refusal to run at all leaves the previous
//! verdict standing. A persistent failure re-sets the field on every pass
//! (the block recurs), so a standing failure is never lost; a recovered one
//! survives exactly one pass. Reorg failures are set inside the same pass,
//! after the event channel clears, so a reorg block is reported too.
//!
//! Guards (green on BOTH trees): a failing pass DOES report its failure, the
//! cursor does not move past the refusal, and a target that never failed
//! reports no error.
//!
//! Measured on the published v0.7.23 asset: 2 defect faces present.

use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
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

/// A receiver whose first `fail_n` REQUESTS get a 500 and the rest a 200.
/// `fail_n = 0` is the always-accepting receiver the other gates use.
fn flaky_server(fail_n: usize) -> (String, Arc<AtomicBool>, Arc<Mutex<Vec<String>>>, std::thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    listener.set_nonblocking(true).expect("nonblocking");
    let addr = listener.local_addr().expect("addr");
    let stop = Arc::new(AtomicBool::new(false));
    let stop2 = stop.clone();
    let bodies = Arc::new(Mutex::new(Vec::<String>::new()));
    let bodies2 = bodies.clone();
    let remaining = Arc::new(AtomicUsize::new(fail_n));
    let remaining2 = remaining.clone();
    let handle = std::thread::spawn(move || {
        while !stop2.load(Ordering::Relaxed) {
            match listener.accept() {
                Ok((mut s, _)) => {
                    let bodies3 = bodies2.clone();
                    let remaining3 = remaining2.clone();
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
                        // Fail the first N requests, then accept. The counter
                        // must NOT wrap: `fetch_sub` on 0 underflows to
                        // usize::MAX and every later request fails again.
                        let failed = remaining3
                            .try_update(Ordering::SeqCst, Ordering::SeqCst, |v| v.checked_sub(1))
                            .is_ok();
                        let status: &[u8] = if failed {
                            b"HTTP/1.1 500 Server Error\r\ncontent-type: application/json\r\ncontent-length: 4\r\n\r\n{\"e\"}"
                        } else {
                            b"HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: 2\r\n\r\n{}"
                        };
                        let _ = s.write_all(status);
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

fn plant(dir: &Path, rows: &[(&str, Option<&str>, Option<&str>)]) {
    let conn = rusqlite::Connection::open(dir.join("reeflog.db")).expect("open journal");
    for (i, (sig, logs, state)) in rows.iter().enumerate() {
        conn.execute(
            "INSERT INTO events(signature,slot,err,logs,source,first_seen,payload_state)
             VALUES(?1,?2,NULL,?3,'ws',?4,?5)",
            rusqlite::params![sig, 100 + i as i64, logs, 1_700_000_000_000i64, state],
        )
        .expect("plant row");
    }
    conn.close().expect("close journal");
}

/// `last_error` as `webhook status` reports it, plus the cursor it sits on.
fn status_report(dir: &Path, cfg: &Path) -> (serde_json::Value, i64) {
    let out = run(
        dir,
        &["webhook", "status", "--db", "reeflog.db", "--config", cfg.to_str().unwrap()],
    );
    assert!(out.status.success(), "status: {}", stderr_of(&out));
    let t = report_of(&out)["targets"][0].clone();
    let conn = rusqlite::Connection::open(dir.join("reeflog.db")).expect("open journal");
    let next_seq: i64 = conn
        .query_row(
            "SELECT COALESCE((SELECT next_seq FROM webhook_state WHERE target='t1'),0)",
            [],
            |r| r.get(0),
        )
        .expect("state");
    conn.close().ok();
    (t, next_seq)
}

fn init(dir: &Path) {
    let out = run(dir, &["init", "--db", "reeflog.db", "--program", VALID_PROGRAM]);
    assert!(out.status.success(), "init: {}", stderr_of(&out));
}

/// F1 (defect): a target that RECOVERED must not keep reporting the failure
/// it recovered from. Red on the pre-fix tree: pass 2 delivered everything
/// and exited 0, and `status` still said `http 500`.
#[test]
fn a_recovered_target_stops_reporting_the_old_failure() {
    let dir = unique_dir("stale-recovered");
    init(&dir);
    plant(
        &dir,
        &[
            ("FULL1", Some(r#"["log one"]"#), Some("held")),
            ("FULL2", Some(r#"["log two"]"#), Some("held")),
        ],
    );

    // First REQUEST refused (the seq 1 POST), endpoint healthy afterwards.
    // In order delivery stops at seq 1; seq 2 is attempted next pass.
    let (url, stop, bodies, handle) = flaky_server(1);
    let cfg = write_config(&dir, &url);
    let push = [
        "webhook",
        "push",
        "--db",
        "reeflog.db",
        "--config",
        cfg.to_str().unwrap(),
    ];

    let first = run(&dir, &push);
    assert_eq!(first.status.code(), Some(3), "a refusal blocks: {}", stderr_of(&first));
    let (after_fail, _) = status_report(&dir, &cfg);
    assert_eq!(
        after_fail["last_error"],
        serde_json::json!("http 500"),
        "a standing failure IS reported (guard, both trees): {}",
        after_fail
    );

    // The endpoint is healthy now: the second pass delivers everything.
    let second = run(&dir, &push);
    let rep = report_of(&second);
    assert_eq!(
        rep["targets"][0]["delivered"],
        serde_json::json!(2),
        "both rows: {}",
        rep
    );
    let got = bodies.lock().expect("bodies").clone();
    stop.store(true, Ordering::Relaxed);
    let _ = handle.join();
    // At-least-once: the refused POST of seq 1 still reached the receiver, so
    // seq 1 may appear twice. What must NOT appear is a missing seq 2.
    let seqs: Vec<String> = got
        .iter()
        .filter_map(|b| serde_json::from_str::<serde_json::Value>(b).ok())
        .map(|v| v["signature"].as_str().unwrap_or("?").to_string())
        .collect();
    assert!(seqs.contains(&"FULL1".to_string()) && seqs.contains(&"FULL2".to_string()), "both rows reached the receiver: {seqs:?}");

    let (after_ok, next_seq) = status_report(&dir, &cfg);
    assert_eq!(
        after_ok["last_error"],
        serde_json::json!(null),
        "a pass that delivered everything must leave no standing failure behind (got: {})",
        after_ok["last_error"]
    );
    assert_eq!(next_seq, 2, "the cursor advanced over both delivered rows");
}

/// F2 (defect): the same rule on the payload-hold channel — once `hydrate`
/// pays the debt and the row delivers, the hold reason must not stand. Red
/// on the pre-fix tree: `status` still said "the payload is still owed".
#[test]
fn a_paid_payload_clears_the_hold_reason_status_reports() {
    let dir = unique_dir("stale-hold");
    init(&dir);
    plant(&dir, &[("OWED", None, None)]);

    let (url, stop, bodies, handle) = flaky_server(0);
    let cfg = write_config(&dir, &url);
    let push = [
        "webhook",
        "push",
        "--db",
        "reeflog.db",
        "--config",
        cfg.to_str().unwrap(),
    ];

    let held = run(&dir, &push);
    assert_eq!(held.status.code(), Some(3), "the owed row holds: {}", stderr_of(&held));
    let (during_hold, _) = status_report(&dir, &cfg);
    assert!(
        during_hold["last_error"]
            .as_str()
            .is_some_and(|s| s.contains("hydrate")),
        "the standing hold IS reported with its repair (guard, both trees): {}",
        during_hold["last_error"]
    );

    // `hydrate` pays the debt.
    let conn = rusqlite::Connection::open(dir.join("reeflog.db")).expect("open journal");
    conn.execute(
        "UPDATE events SET logs=?2, payload_state='held' WHERE signature='OWED'",
        rusqlite::params!["OWED", r#"["Program log: paid"]"#],
    )
    .expect("pay the debt");
    conn.close().ok();

    let paid = run(&dir, &push);
    assert!(
        paid.status.success(),
        "the paid row must deliver (stderr: {})",
        stderr_of(&paid)
    );
    let got = bodies.lock().expect("bodies").clone();
    stop.store(true, Ordering::Relaxed);
    let _ = handle.join();
    assert_eq!(got.len(), 1, "the paid row arrived exactly once: {got:?}");

    let (after_paid, next_seq) = status_report(&dir, &cfg);
    assert_eq!(
        after_paid["last_error"],
        serde_json::json!(null),
        "the hold is paid — the report must not still name it (got: {})",
        after_paid["last_error"]
    );
    assert_eq!(next_seq, 1, "the cursor advanced over the paid row");
}

/// Guard: a target that NEVER failed reports no error (green on both trees).
#[test]
fn a_clean_target_reports_no_error() {
    let dir = unique_dir("stale-clean");
    init(&dir);
    plant(&dir, &[("FULL", Some(r#"["log one"]"#), Some("held"))]);

    let (url, stop, _bodies, handle) = flaky_server(0);
    let cfg = write_config(&dir, &url);
    let out = run(
        &dir,
        &["webhook", "push", "--db", "reeflog.db", "--config", cfg.to_str().unwrap()],
    );
    assert!(out.status.success(), "clean pass: {}", stderr_of(&out));
    stop.store(true, Ordering::Relaxed);
    let _ = handle.join();

    let (t, _) = status_report(&dir, &cfg);
    assert_eq!(
        t["last_error"],
        serde_json::json!(null),
        "a target that never failed must report no error: {}",
        t
    );
    assert_eq!(t["delivered"], serde_json::json!(1), "and it did deliver: {t}");
}
