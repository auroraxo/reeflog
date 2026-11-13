//! The webhook channel must not hand over a payload it still owes.
//!
//! PULS AF, 2026-11-13. PULS AE fixed the CLI cursor channel (`tail`): a row
//! whose payload the journal owes (`logs IS NULL AND payload_state IS NULL`)
//! HOLDS delivery until `hydrate` pays the debt. The webhook fan-out — the
//! channel for the consumer that is NOT on this machine — had no such rule.
//! Measured on this tree before the fix (`tools/measure-webhook-payload-hold.mjs`,
//! verdict `webhook-payload-hold-defect-present`): `deliver_events` posted the
//! owed row as a full, receipted event with `logs: null` and advanced the
//! target's `next_seq` past it (0 -> 1). When `hydrate` later filled the
//! payload, the next pass delivered NOTHING — the finished event is
//! permanently unreachable for that consumer, while a fresh target reads it
//! fine. The same silent loss PULS AE stopped on the CLI path, one channel
//! over, and the webhook body is SIGNED — the receiver cannot tell a
//! signature without content from a real event.
//!
//! The rule mirrors `tail` exactly (one debt, one rule, every channel):
//!   * payload present, or a recorded outcome (`held`/`unavailable`/
//!     `invalid`) -> deliver; the outcome marker travels in the body.
//!   * owed -> the queue stops at the row. `blocked_seq` names it, the
//!     cursor stays behind it, the body is never sent, no receipt is
//!     written, and the pass exits 3 like any other blocked target.
//!   * retractions are NEVER held by this rule — a late retraction costs
//!     money (existing guard, green on both trees).
//!
//! Guards (green on BOTH trees): a full row delivers and receipts as before,
//! a recorded outcome is delivered (it can never be filled), and a paid debt
//! reaches the SAME target on the next pass.

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
fn accepting_server() -> (String, Arc<AtomicBool>, Arc<Mutex<Vec<String>>>, std::thread::JoinHandle<()>) {
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
                        s.set_nonblocking(true).expect("nonblocking");
                        let mut buf = [0u8; 65536];
                        let mut got = 0usize;
                        for _ in 0..400 {
                            match s.read(&mut buf[got..]) {
                                Ok(0) => break,
                                Ok(n) => {
                                    got += n;
                                    if buf[..got].windows(4).any(|w| w == b"\r\n\r\n") {
                                        // The body is what follows the header
                                        // block; read until the end of the
                                        // declared length.
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
                        let resp = b"HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: 2\r\n\r\n{}";
                        let _ = s.write_all(resp);
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

/// A journal with one row per (signature, logs, payload_state) triple.
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

fn journal_facts(dir: &Path) -> (i64, i64) {
    let conn = rusqlite::Connection::open(dir.join("reeflog.db")).expect("open journal");
    let next_seq: i64 = conn
        .query_row(
            "SELECT COALESCE((SELECT next_seq FROM webhook_state WHERE target='t1'),0)",
            [],
            |r| r.get(0),
        )
        .expect("state");
    let receipts: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM webhook_receipts WHERE target='t1'",
            [],
            |r| r.get(0),
        )
        .expect("receipts");
    conn.close().ok();
    (next_seq, receipts)
}

/// W1 (defect): an owed row must NOT be delivered, and the target's cursor
/// must NOT move past it. Red on the pre-fix tree: the receiver got the row
/// with `logs: null`, `next_seq` advanced, a receipt was written.
#[test]
fn an_owed_payload_is_not_delivered_to_a_webhook_target() {
    let dir = unique_dir("whhold-owed");
    let out = run(
        &dir,
        &["init", "--db", "reeflog.db", "--program", VALID_PROGRAM],
    );
    assert!(out.status.success(), "init: {}", stderr_of(&out));
    plant(&dir, &[("OWED", None, None)]);

    let (url, stop, bodies, handle) = accepting_server();
    let cfg = write_config(&dir, &url);
    let out = run(
        &dir,
        &["webhook", "push", "--db", "reeflog.db", "--config", cfg.to_str().unwrap()],
    );

    let (next_seq, receipts) = journal_facts(&dir);
    let got = bodies.lock().expect("bodies").clone();
    stop.store(true, Ordering::Relaxed);
    let _ = handle.join();

    assert!(
        got.is_empty(),
        "an owed payload must not be delivered to a webhook target (got: {got:?})"
    );
    assert_eq!(
        next_seq, 0,
        "the target cursor must stay behind the owed row (next_seq={next_seq})"
    );
    assert_eq!(receipts, 0, "an owed row must not leave a delivery receipt");
    assert_eq!(
        out.status.code(),
        Some(3),
        "a held target is a blocked target: exit 3 (stderr: {})",
        stderr_of(&out)
    );
    let report = report_of(&out);
    let t = &report["targets"][0];
    assert_eq!(t["blocked_seq"], serde_json::json!(1), "the owed row is named");
    assert_eq!(t["delivered"], serde_json::json!(0), "nothing was delivered");
}

/// W2 (defect): once the debt is paid, the SAME target must receive the
/// finished payload. Red on the pre-fix tree: the cursor had already moved
/// past the row, so the second pass delivered nothing and the payload stayed
/// unreachable for that consumer forever.
#[test]
fn a_paid_payload_reaches_the_same_webhook_target() {
    let dir = unique_dir("whhold-paid");
    let out = run(
        &dir,
        &["init", "--db", "reeflog.db", "--program", VALID_PROGRAM],
    );
    assert!(out.status.success(), "init: {}", stderr_of(&out));
    plant(&dir, &[("OWED", None, None)]);

    let (url, stop, bodies, handle) = accepting_server();
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
    assert_eq!(first.status.code(), Some(3), "held: {}", stderr_of(&first));

    // `hydrate` pays the debt — the payload lands in the journal.
    let conn = rusqlite::Connection::open(dir.join("reeflog.db")).expect("open journal");
    conn.execute(
        "UPDATE events SET logs=?2, payload_state='held' WHERE signature='OWED'",
        rusqlite::params!["OWED", r#"["Program log: hi"]"#],
    )
    .expect("pay the debt");
    conn.close().ok();

    let second = run(&dir, &push);
    let got = bodies.lock().expect("bodies").clone();
    stop.store(true, Ordering::Relaxed);
    let _ = handle.join();

    assert!(
        second.status.success(),
        "the paid row must deliver cleanly (stderr: {})",
        stderr_of(&second)
    );
    assert_eq!(
        got.len(),
        1,
        "the paid payload must reach the same target exactly once (got: {got:?})"
    );
    let body: serde_json::Value = serde_json::from_str(&got[0]).expect("a JSON body");
    assert_eq!(body["signature"], serde_json::json!("OWED"));
    assert!(
        body["logs"].is_array(),
        "the delivered body must carry the paid payload (logs: {})",
        body["logs"]
    );
    let (next_seq, receipts) = journal_facts(&dir);
    assert_eq!(next_seq, 1, "the cursor advances only over the delivered row");
    assert_eq!(receipts, 1, "the delivered row leaves exactly one receipt");
}

/// Guard: a row WITH its payload still delivers and receipts exactly as
/// before (green on both trees).
#[test]
fn a_full_payload_is_delivered_as_before() {
    let dir = unique_dir("whhold-full");
    let out = run(
        &dir,
        &["init", "--db", "reeflog.db", "--program", VALID_PROGRAM],
    );
    assert!(out.status.success(), "init: {}", stderr_of(&out));
    plant(&dir, &[("FULL", Some(r#"["Program log: hi"]"#), Some("held"))]);

    let (url, stop, bodies, handle) = accepting_server();
    let cfg = write_config(&dir, &url);
    let out = run(
        &dir,
        &["webhook", "push", "--db", "reeflog.db", "--config", cfg.to_str().unwrap()],
    );
    let got = bodies.lock().expect("bodies").clone();
    stop.store(true, Ordering::Relaxed);
    let _ = handle.join();

    assert!(out.status.success(), "push: {}", stderr_of(&out));
    assert_eq!(got.len(), 1, "the full row must be delivered (got: {got:?})");
    let (next_seq, receipts) = journal_facts(&dir);
    assert_eq!(next_seq, 1);
    assert_eq!(receipts, 1);
}

/// Guard: a recorded outcome (`unavailable`) can never be filled, so it is
/// delivered with its marker instead of being held forever (green on both
/// trees).
#[test]
fn a_recorded_outcome_is_delivered_with_its_marker() {
    let dir = unique_dir("whhold-outcome");
    let out = run(
        &dir,
        &["init", "--db", "reeflog.db", "--program", VALID_PROGRAM],
    );
    assert!(out.status.success(), "init: {}", stderr_of(&out));
    plant(&dir, &[("GONE", None, Some("unavailable"))]);

    let (url, stop, bodies, handle) = accepting_server();
    let cfg = write_config(&dir, &url);
    let out = run(
        &dir,
        &["webhook", "push", "--db", "reeflog.db", "--config", cfg.to_str().unwrap()],
    );
    let got = bodies.lock().expect("bodies").clone();
    stop.store(true, Ordering::Relaxed);
    let _ = handle.join();

    assert!(out.status.success(), "push: {}", stderr_of(&out));
    assert_eq!(got.len(), 1, "a final outcome must be delivered (got: {got:?})");
    let body: serde_json::Value = serde_json::from_str(&got[0]).expect("a JSON body");
    assert_eq!(
        body["payload_state"],
        serde_json::json!("unavailable"),
        "the outcome marker travels with the body"
    );
    let (next_seq, receipts) = journal_facts(&dir);
    assert_eq!(next_seq, 1);
    assert_eq!(receipts, 1);
}

/// Guard: a retraction is never held by the payload rule — a late retraction
/// costs money (green on both trees).
#[test]
fn a_retraction_is_not_held_by_the_payload_rule() {
    let dir = unique_dir("whhold-reorg");
    let out = run(
        &dir,
        &["init", "--db", "reeflog.db", "--program", VALID_PROGRAM],
    );
    assert!(out.status.success(), "init: {}", stderr_of(&out));
    plant(&dir, &[("OWED", None, None)]);
    {
        let conn = rusqlite::Connection::open(dir.join("reeflog.db")).expect("open journal");
        conn.execute(
            "INSERT INTO reorgs(id,signature,slot,detected_at,status_source,tx_source,detail)
             VALUES(1,'OWED',100,1,'status','tx','gone')",
            [],
        )
        .expect("plant reorg");
        conn.close().ok();
    }

    let (url, stop, bodies, handle) = accepting_server();
    let cfg = write_config(&dir, &url);
    let out = run(
        &dir,
        &["webhook", "push", "--db", "reeflog.db", "--config", cfg.to_str().unwrap()],
    );
    let got = bodies.lock().expect("bodies").clone();
    stop.store(true, Ordering::Relaxed);
    let _ = handle.join();

    let kinds: Vec<String> = got
        .iter()
        .map(|b| {
            serde_json::from_str::<serde_json::Value>(b)
                .map(|v| v["type"].as_str().unwrap_or("?").to_string())
                .unwrap_or_else(|_| "?".to_string())
        })
        .collect();
    assert!(
        kinds.contains(&"reorg".to_string()),
        "the retraction must be delivered even while the event queue holds (got: {kinds:?})"
    );
    let _ = out;
}
