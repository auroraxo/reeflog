//! The dead-letter queue must report the QUEUE, not a filtered view of it —
//! and a `--target` the config does not know must be a REFUSAL, not a
//! successful empty answer.
//!
//! Measured 2026-11-09 on the PUBLISHED v0.7.6 binary (gh release
//! download, sha256 == the release digest), against a journal with planted
//! pending dead letters and a config with two targets:
//!
//!   reeflog webhook dlq list --db j.db --config c.json --target bogus
//!       -> exit 0, `{"targets": [], "pending_total": 0}`.
//!          The journal held 7 pending dead letters. The operator asked
//!          "what is quarantined for target bogus", got "nothing is
//!          quarantined", and the command SUCCEEDED. A typo in a target id
//!          is indistinguishable from an empty queue — and the queue is the
//!          one place where "empty" means "a consumer is not holding events
//!          it was promised".
//!   reeflog webhook dlq replay --db j.db --config c.json --target bogus
//!       -> exit 0, `{"replayed": 0, "failed": 0, "targets": []}` — a
//!          successful no-op. The events were never replayed and nothing
//!          says so.
//!   reeflog webhook dlq list --db j.db --config c.json --limit 2
//!       -> `pending: 2`, `pending_total: 2`, while the journal held 7.
//!          `pending` counted the ROWS THE LISTING SHOWED, not the letters
//!          that are pending. `webhook status` on the same journal said
//!          `dead_letters: 7` — the two commands of the same product
//!          disagreed about the size of the same queue.
//!   reeflog webhook dlq list --db j.db --config c.json
//!       -> a dead letter whose target was REMOVED from the config is
//!          invisible: `pending_total: 0` while the journal holds it. It
//!          can never be replayed (no url, no secret) and no command ever
//!          says so. The product's promise is "nothing is ever lost"; a
//!          letter nobody can reach and nobody is told about is lost.
//!
//! The rule this ladder enforces:
//!
//! * `--target <id>` must name a target the CONFIG knows; an unknown id is
//!   a REFUSAL that names the flag, the value and the known ids — the same
//!   refusal voice the rest of this family uses ("name the flag, name the
//!   value, name the reason");
//! * `pending` and `pending_total` count the letters that are PENDING in
//!   the journal, not the rows a limit happened to show; a truncated
//!   listing must say that it is truncated;
//! * a pending letter whose target is not in the config must be REPORTED
//!   (as an orphan), counted in the total, and must keep the command from
//!   reading as "all healthy" — a letter nobody can replay is a fact the
//!   operator needs, not a number to round to zero;
//! * `replay` must refuse an unknown `--target` too, and must not report
//!   success while letters remain unreachable.
//!
//! The guards matter as much: a checker that refuses a KNOWN target, or
//! that turns a healthy queue into a failure, is worse than the defect it
//! fixes. Every guard below must stay green on BOTH trees.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
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
    String::from_utf8_lossy(&out.stdout).to_string()
}

fn stderr_of(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).to_string()
}

fn report_of(out: &Output) -> Option<serde_json::Value> {
    serde_json::from_str(&stdout_of(out)).ok()
}

/// A journal with `pending` dead letters for `alpha` and, optionally, one
/// for a target that is NOT in the config (an orphan). The letters are
/// planted the way the daemon writes them — pending, with a body and a
/// reason.
fn with_letters(tag: &str, pending: i64, orphan: bool) -> PathBuf {
    let dir = unique_dir(tag);
    let out = Command::new(env!("CARGO_BIN_EXE_reeflog"))
        .args(["init", "--db", "reeflog.db", "--program", VALID_PROGRAM])
        .current_dir(&dir)
        .output()
        .expect("init runs");
    assert!(
        out.status.success(),
        "init must succeed ({})",
        String::from_utf8_lossy(&out.stderr)
    );
    let conn = rusqlite::Connection::open(dir.join("reeflog.db")).expect("open journal");
    for seq in 1..=pending {
        conn.execute(
            "INSERT INTO webhook_dead_letters
               (target,kind,seq,body,event_id,reason,status,attempts,first_failed_at,last_failed_at,state)
             VALUES('alpha','event',?1,'{}',?2,'refused with http 500',500,5,1,1,'pending')",
            rusqlite::params![seq, format!("alpha:event:{seq}")],
        )
        .expect("plant letter");
    }
    if orphan {
        conn.execute(
            "INSERT INTO webhook_dead_letters
               (target,kind,seq,body,event_id,reason,status,attempts,first_failed_at,last_failed_at,state)
             VALUES('gone','event',1,'{}','gone:event:1','refused with http 500',500,5,1,1,'pending')",
            [],
        )
        .expect("plant orphan");
    }
    conn.close().expect("close journal");
    dir
}

/// A config with two known targets. `alpha` points at a closed port (it is
/// never dialled by the listing), `beta` at a closed port too.
fn write_config(dir: &Path) -> PathBuf {
    let cfg = dir.join("wh.json");
    std::fs::write(
        &cfg,
        r#"{"targets":[
             {"id":"alpha","url":"http://127.0.0.1:9/hook","secret":"s1"},
             {"id":"beta","url":"http://127.0.0.1:9/hook","secret":"s2"}
           ]}"#,
    )
    .expect("write config");
    cfg
}

/// A receiver that answers every request with 200 — the shape of a consumer
/// that recovered, so a replay has something to deliver to.
fn accepting_server() -> (String, Arc<AtomicBool>, thread::JoinHandle<()>) {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    listener.set_nonblocking(true).expect("nonblocking");
    let addr = listener.local_addr().expect("addr");
    let stop = Arc::new(AtomicBool::new(false));
    let stop2 = stop.clone();
    let handle = thread::spawn(move || {
        while !stop2.load(Ordering::Relaxed) {
            match listener.accept() {
                Ok((mut s, _)) => {
                    s.set_nonblocking(true).expect("nonblocking");
                    use std::io::Read;
                    // Wait for the request head before answering: closing the
                    // socket while the client is still sending would abort the
                    // delivery before the response bytes arrive.
                    let mut buf = [0u8; 8192];
                    let mut got = 0usize;
                    for _ in 0..100 {
                        match s.read(&mut buf[got..]) {
                            Ok(0) => break,
                            Ok(n) => {
                                got += n;
                                if buf[..got].windows(4).any(|w| w == b"\r\n\r\n") {
                                    break;
                                }
                            }
                            Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                                thread::sleep(Duration::from_millis(5));
                            }
                            Err(_) => break,
                        }
                    }
                    use std::io::Write;
                    let resp = b"HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: 2\r\n\r\n{}";
                    let _ = s.write_all(resp);
                    let _ = s.flush();
                }
                Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    thread::sleep(Duration::from_millis(20));
                }
                Err(_) => break,
            }
        }
    });
    (format!("http://{addr}/hook"), stop, handle)
}

/// The refusal voice: the flag, the offending value, and the known ids.
fn assert_unknown_target_refusal(tag: &str, sub: &str) {
    let dir = with_letters(tag, 3, false);
    let cfg = write_config(&dir);
    let out = run(
        &dir,
        &[
            "webhook",
            "dlq",
            sub,
            "--db",
            "reeflog.db",
            "--config",
            cfg.to_str().expect("config path"),
            "--target",
            "bogus",
        ],
    );
    let err = stderr_of(&out);
    assert!(
        !out.status.success(),
        "an unknown --target must not be a successful answer: exit {:?}, stdout {}",
        out.status.code(),
        stdout_of(&out)
    );
    assert!(
        report_of(&out).is_none(),
        "a refusal must print no report (stdout: {})",
        stdout_of(&out)
    );
    let lower = err.to_lowercase();
    assert!(
        lower.contains("--target"),
        "the refusal must name the flag (stderr: {err})"
    );
    assert!(
        lower.contains("bogus"),
        "the refusal must name the offending value (stderr: {err})"
    );
    assert!(
        lower.contains("alpha"),
        "the refusal must name the known targets (stderr: {err})"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

// --- the defect cases ------------------------------------------------------

/// `dlq list --target <unknown>` must be refused, not answered as "nothing
/// is quarantined".
#[test]
fn dlq_list_refuses_an_unknown_target() {
    assert_unknown_target_refusal("dlq-list-unknown", "list");
}

/// `dlq replay --target <unknown>` must be refused, not reported as a
/// successful no-op.
#[test]
fn dlq_replay_refuses_an_unknown_target() {
    assert_unknown_target_refusal("dlq-replay-unknown", "replay");
}

/// `pending` must count the letters that are PENDING, not the rows a limit
/// happened to show. Before the fix, `--limit 2` reported `pending: 2`
/// while the journal held 7 — and `webhook status` said 7.
#[test]
fn dlq_list_counts_pending_letters_not_listed_rows() {
    let dir = with_letters("dlq-pending-count", 7, false);
    let cfg = write_config(&dir);
    let out = run(
        &dir,
        &[
            "webhook",
            "dlq",
            "list",
            "--db",
            "reeflog.db",
            "--config",
            cfg.to_str().expect("config path"),
            "--limit",
            "2",
        ],
    );
    assert!(
        out.status.success(),
        "listing must succeed (stderr: {})",
        stderr_of(&out)
    );
    let rep = report_of(&out).expect("a report");
    let alpha = rep["targets"]
        .as_array()
        .expect("targets")
        .iter()
        .find(|t| t["target"] == "alpha")
        .expect("alpha is in the config");
    assert_eq!(
        alpha["pending"], 7,
        "pending must count the letters that are pending, not the rows the limit showed (report: {rep})"
    );
    assert_eq!(
        alpha["dead_letters"].as_array().expect("letters").len(),
        2,
        "the listing itself is still limited (report: {rep})"
    );
    assert_eq!(
        rep["pending_total"], 7,
        "pending_total must count the pending letters, not the listed rows (report: {rep})"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// A pending letter whose target is not in the config must be REPORTED and
/// counted — it can never be replayed, and nothing else says so.
#[test]
fn dlq_list_reports_a_letter_whose_target_left_the_config() {
    let dir = with_letters("dlq-orphan", 1, true);
    let cfg = write_config(&dir);
    let out = run(
        &dir,
        &[
            "webhook",
            "dlq",
            "list",
            "--db",
            "reeflog.db",
            "--config",
            cfg.to_str().expect("config path"),
        ],
    );
    assert!(
        out.status.success(),
        "listing must succeed (stderr: {})",
        stderr_of(&out)
    );
    let rep = report_of(&out).expect("a report");
    let total = rep["pending_total"].as_i64().expect("pending_total");
    assert_eq!(
        total, 2,
        "the unreachable letter must be counted (report: {rep})"
    );
    assert!(
        rep.to_string().contains("gone"),
        "the unreachable letter's target must be named (report: {rep})"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// `replay` must not read as healthy while a letter sits in the queue that
/// it cannot deliver.
#[test]
fn dlq_replay_does_not_report_success_while_a_letter_is_unreachable() {
    let dir = with_letters("dlq-replay-orphan", 0, true);
    let cfg = write_config(&dir);
    let out = run(
        &dir,
        &[
            "webhook",
            "dlq",
            "replay",
            "--db",
            "reeflog.db",
            "--config",
            cfg.to_str().expect("config path"),
        ],
    );
    assert!(
        !out.status.success(),
        "a replay that could not deliver the only pending letter must not exit 0 (stdout: {}, stderr: {})",
        stdout_of(&out),
        stderr_of(&out)
    );
    let _ = std::fs::remove_dir_all(&dir);
}

// --- the guards ------------------------------------------------------------

/// A KNOWN target must still be listed, with its letters.
#[test]
fn guard_a_known_target_is_still_listed() {
    let dir = with_letters("dlq-guard-known", 2, false);
    let cfg = write_config(&dir);
    let out = run(
        &dir,
        &[
            "webhook",
            "dlq",
            "list",
            "--db",
            "reeflog.db",
            "--config",
            cfg.to_str().expect("config path"),
            "--target",
            "alpha",
        ],
    );
    assert!(
        out.status.success(),
        "a known target must be listed (stderr: {})",
        stderr_of(&out)
    );
    let rep = report_of(&out).expect("a report");
    let targets = rep["targets"].as_array().expect("targets");
    assert_eq!(
        targets.len(),
        1,
        "only the named target is listed (report: {rep})"
    );
    assert_eq!(targets[0]["target"], "alpha");
    assert_eq!(targets[0]["pending"], 2);
    assert_eq!(targets[0]["dead_letters"].as_array().expect("letters").len(), 2);
    let _ = std::fs::remove_dir_all(&dir);
}

/// Listing without a filter must still work and must report every config
/// target, including one with nothing pending.
#[test]
fn guard_listing_without_a_filter_still_reports_every_config_target() {
    let dir = with_letters("dlq-guard-nofilter", 1, false);
    let cfg = write_config(&dir);
    let out = run(
        &dir,
        &[
            "webhook",
            "dlq",
            "list",
            "--db",
            "reeflog.db",
            "--config",
            cfg.to_str().expect("config path"),
        ],
    );
    assert!(
        out.status.success(),
        "listing must succeed (stderr: {})",
        stderr_of(&out)
    );
    let rep = report_of(&out).expect("a report");
    let targets = rep["targets"].as_array().expect("targets");
    assert_eq!(
        targets.len(),
        2,
        "both config targets are reported (report: {rep})"
    );
    let beta = targets
        .iter()
        .find(|t| t["target"] == "beta")
        .expect("beta is in the config");
    assert_eq!(beta["pending"], 0, "a target with nothing pending says 0");
    assert_eq!(rep["pending_total"], 1);
    let _ = std::fs::remove_dir_all(&dir);
}

/// An empty queue must read healthy: listing exits 0, replay exits 0 and
/// reports nothing left to send.
#[test]
fn guard_an_empty_queue_still_reads_healthy() {
    let dir = with_letters("dlq-guard-empty", 0, false);
    let cfg = write_config(&dir);
    let list = run(
        &dir,
        &[
            "webhook",
            "dlq",
            "list",
            "--db",
            "reeflog.db",
            "--config",
            cfg.to_str().expect("config path"),
        ],
    );
    assert!(
        list.status.success(),
        "an empty queue must list successfully (stderr: {})",
        stderr_of(&list)
    );
    let rep = report_of(&list).expect("a report");
    assert_eq!(rep["pending_total"], 0);

    let replay = run(
        &dir,
        &[
            "webhook",
            "dlq",
            "replay",
            "--db",
            "reeflog.db",
            "--config",
            cfg.to_str().expect("config path"),
        ],
    );
    assert!(
        replay.status.success(),
        "an empty queue must replay successfully (stderr: {})",
        stderr_of(&replay)
    );
    let rrep = report_of(&replay).expect("a report");
    assert_eq!(rrep["replayed"], 0);
    assert_eq!(rrep["failed"], 0);
    let _ = std::fs::remove_dir_all(&dir);
}

/// The replay must still DELIVER: a letter against a live receiver goes
/// out, is marked replayed, and the command reports it.
#[test]
fn guard_a_replay_still_delivers_the_letter() {
    let (base, stop, handle) = accepting_server();
    let dir = unique_dir("dlq-guard-replay");
    let out = Command::new(env!("CARGO_BIN_EXE_reeflog"))
        .args(["init", "--db", "reeflog.db", "--program", VALID_PROGRAM])
        .current_dir(&dir)
        .output()
        .expect("init runs");
    assert!(out.status.success());
    let conn = rusqlite::Connection::open(dir.join("reeflog.db")).expect("open journal");
    conn.execute(
        "INSERT INTO webhook_dead_letters
           (target,kind,seq,body,event_id,reason,status,attempts,first_failed_at,last_failed_at,state)
         VALUES('alpha','event',1,'{\"a\":1}','alpha:event:1','refused with http 500',500,5,1,1,'pending')",
        [],
    )
    .expect("plant letter");
    conn.close().expect("close journal");
    let cfg = dir.join("wh.json");
    std::fs::write(
        &cfg,
        format!(r#"{{"targets":[{{"id":"alpha","url":"{base}","secret":"s1"}}]}}"#),
    )
    .expect("write config");

    let replay = run(
        &dir,
        &[
            "webhook",
            "dlq",
            "replay",
            "--db",
            "reeflog.db",
            "--config",
            cfg.to_str().expect("config path"),
        ],
    );
    stop.store(true, Ordering::Relaxed);
    let _ = handle.join();
    assert!(
        replay.status.success(),
        "a successful replay must exit 0 (stderr: {})",
        stderr_of(&replay)
    );
    let rep = report_of(&replay).expect("a report");
    assert_eq!(rep["replayed"], 1, "one letter was replayed (report: {rep})");
    assert_eq!(rep["failed"], 0);

    let conn = rusqlite::Connection::open(dir.join("reeflog.db")).expect("open journal");
    let state: String = conn
        .query_row(
            "SELECT state FROM webhook_dead_letters WHERE target='alpha' AND seq=1",
            [],
            |r| r.get(0),
        )
        .expect("the letter is still there");
    conn.close().expect("close journal");
    assert_eq!(
        state, "replayed",
        "a delivered letter must be marked replayed"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// The listing must not hang and must not need the network: it reads the
/// journal, so it must finish promptly even though both configured targets
/// point at a closed port.
#[test]
fn guard_the_listing_finishes_promptly() {
    let dir = with_letters("dlq-guard-prompt", 2, false);
    let cfg = write_config(&dir);
    let started = std::time::Instant::now();
    let out = run(
        &dir,
        &[
            "webhook",
            "dlq",
            "list",
            "--db",
            "reeflog.db",
            "--config",
            cfg.to_str().expect("config path"),
        ],
    );
    assert!(
        started.elapsed() < Duration::from_secs(10),
        "the listing must finish promptly (took {:?})",
        started.elapsed()
    );
    assert!(out.status.success());
    let _ = std::fs::remove_dir_all(&dir);
}
