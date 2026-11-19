//! A delivery channel's standing verdict must survive a pass that never
//! reaches it.
//!
//! PULS AW, 2026-11-19. Measured on this tree before the fix
//! (`tools/measure-reorg-verdict-survival.mjs`, the real binary;
//! `research/reorg-verdict-survival-prefix-2026-11-19.json`, verdict
//! `reorg-verdict-erased-by-idle-pass`, 5/6 defect faces red, 4/4 guards
//! green; binary sha256 2272a7c3…):
//!
//! The two queues have had their own standing-reason columns since v0.7.37
//! (`last_error` for events, `last_reorg_error` for retractions) precisely
//! so one channel's write never erases the other's. But the CLEAR stayed
//! shared: `deliver_events` calls `webhook_clear_error`, which set BOTH
//! columns to NULL in one statement, and `deliver_reorgs` never cleared its
//! own column at all — it re-established the verdict only when it actually
//! examined a row. So a pass whose event phase runs while the retraction
//! phase examines NO row erased the retraction verdict on disk and
//! re-established nothing:
//!
//! * `webhook push --limit 0` — the pass the README documents as sending
//!   zero bytes and moving no cursor — erased BOTH columns. A target whose
//!   retraction queue was held read `last_reorg_error: null` afterwards,
//!   and `webhook status` — the report a monitoring caller polls — said
//!   the target was healthy and exited 0 while the retraction queue was
//!   still stopped. The mirror image held on the event channel: a recorded
//!   payload-hold verdict (`run \`hydrate\``) was erased by the same
//!   zero-byte pass.
//!
//! THE RULE NOW ENFORCED (pass-level, per channel — the v0.7.23 rule, made
//! two-channel):
//!
//!   * `last_error` is the reason the most recent EVENT phase stopped; the
//!     event phase clears and re-sets it, and nothing else writes it;
//!   * `last_reorg_error` is the reason the most recent RETRACTION phase
//!     stopped; the retraction phase clears and re-sets it, and nothing
//!     else writes it;
//!   * a phase that examines no rows leaves the verdict it finds standing.
//!
//! Guards (green on BOTH trees — the normal cases must not move):
//!   G1 a pass blocked on both queues records BOTH reasons, each on its own
//!      channel (the v0.7.37 write rule);
//!   G2 `--limit 0` sends zero bytes (the budget rule);
//!   G3 a pass that GETS THROUGH clears both columns (the v0.7.23
//!      stale-error rule), and delivers the owed retraction (no progress
//!      lost);
//!   G4 the event channel's stale verdict is cleared by a pass that gets
//!      through;
//!   G5 the retraction channel's stale verdict is cleared by a pass that
//!      gets through.

use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

const PROGRAM: &str = "whirLbMiicVdio4qvUfM5KAg6Ct8VwpYzGff3uctyCc";
const EVENT_VERDICT: &str = "the payload is still owed";
const REORG_VERDICT: &str = "refused with http 404";

fn unique_dir(tag: &str) -> PathBuf {
    static N: AtomicU64 = AtomicU64::new(0);
    let n = N.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!("reeflog-verdict-{tag}-{}-{n}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("temp dir");
    dir
}

fn run(dir: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_reeflog"))
        .args(args)
        .current_dir(dir)
        .output()
        .expect("run reeflog")
}

fn stdout_of(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).to_string()
}

fn stderr_of(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).to_string()
}

fn init(dir: &Path) {
    let out = run(dir, &["init", "--db", "reeflog.db", "--program", PROGRAM]);
    assert!(out.status.success(), "init: {}", stderr_of(&out));
}

/// A sink that answers statuses from a script, in request order, and
/// records every body it was sent (the last status repeats if the script
/// runs out).
fn scripted_sink(script: &[u16]) -> (String, Arc<Mutex<Vec<String>>>, Arc<Mutex<Vec<u16>>>, Arc<Mutex<bool>>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().expect("addr");
    let bodies = Arc::new(Mutex::new(Vec::new()));
    let answered = Arc::new(Mutex::new(Vec::new()));
    let stop = Arc::new(Mutex::new(false));
    let script: Vec<u16> = script.to_vec();
    let bodies_c = Arc::clone(&bodies);
    let answered_c = Arc::clone(&answered);
    let stop_c = Arc::clone(&stop);
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            if *stop_c.lock().unwrap() {
                return;
            }
            let mut s = match stream {
                Ok(s) => s,
                Err(_) => return,
            };
            let mut acc: Vec<u8> = Vec::new();
            let mut buf = [0u8; 4096];
            loop {
                let n = match s.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => n,
                    Err(_) => break,
                };
                acc.extend_from_slice(&buf[..n]);
                let head = String::from_utf8_lossy(&acc).to_string();
                let p = match head.find("\r\n\r\n") {
                    Some(p) => p,
                    None => continue,
                };
                let header = head[..p].to_string();
                let cl: usize = header
                    .to_lowercase()
                    .split("\r\n")
                    .find_map(|l| l.strip_prefix("content-length:"))
                    .and_then(|v| v.trim().parse().ok())
                    .unwrap_or(0);
                if acc.len() < p + 4 + cl {
                    continue;
                }
                let body = String::from_utf8_lossy(&acc[p + 4..p + 4 + cl]).to_string();
                let idx = answered_c.lock().unwrap().len();
                let answer = if script.is_empty() {
                    200
                } else {
                    script[idx.min(script.len() - 1)]
                };
                bodies_c.lock().unwrap().push(body);
                answered_c.lock().unwrap().push(answer);
                let resp = if answer == 200 {
                    "HTTP/1.1 200 OK\r\ncontent-length: 2\r\nconnection: close\r\n\r\nok".to_string()
                } else {
                    format!("HTTP/1.1 {answer} No\r\ncontent-length: 0\r\nconnection: close\r\n\r\n")
                };
                let _ = s.write_all(resp.as_bytes());
                let _ = s.flush();
                break;
            }
        }
    });
    (format!("http://{addr}/hook"), bodies, answered, stop)
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

/// One deliverable event, `n` retractions, and a webhook_state row planted
/// with the given cursor and standing verdicts.
fn plant(dir: &Path, events: usize, reorgs: usize, next_seq: i64, next_reorg_id: i64, last_error: Option<&str>, last_reorg_error: Option<&str>, url: &str) {
    let conn = rusqlite::Connection::open(dir.join("reeflog.db")).expect("open journal");
    for i in 0..events {
        conn.execute(
            &format!(
                "INSERT INTO events(signature,slot,err,logs,source,first_seen,seal_status,payload_state)
                 VALUES('SIG{i}',{},NULL,'[\"a\"]','ws',1700000000000,'sealed','held')",
                100 + i
            ),
            [],
        )
        .expect("plant event");
    }
    for i in 0..reorgs {
        conn.execute(
            &format!(
                "INSERT INTO reorgs(signature,slot,detected_at,status_source,tx_source,detail)
                 VALUES('SIGR{i}',{},1700000000001,'status','tx','probe retraction {i}')",
                90 + i
            ),
            [],
        )
        .expect("plant retraction");
    }
    conn.execute(
        "INSERT INTO webhook_state(target,endpoint,next_seq,next_reorg_id,last_error,last_reorg_error,updated_at)
         VALUES('t1',?1,?2,?3,?4,?5,1700000000002)",
        rusqlite::params![url, next_seq, next_reorg_id, last_error, last_reorg_error],
    )
    .expect("plant state row");
    conn.close().expect("close journal");
}

fn journal_verdicts(dir: &Path) -> (Option<String>, Option<String>) {
    let conn = rusqlite::Connection::open(dir.join("reeflog.db")).expect("open journal");
    let last_error: Option<String> = conn
        .query_row("SELECT last_error FROM webhook_state WHERE target='t1'", [], |r| r.get(0))
        .expect("state row");
    let last_reorg_error: Option<String> = conn
        .query_row("SELECT last_reorg_error FROM webhook_state WHERE target='t1'", [], |r| r.get(0))
        .expect("state row");
    conn.close().expect("close journal");
    (last_error, last_reorg_error)
}

fn status(dir: &Path, cfg: &Path) -> (serde_json::Value, i32, String) {
    let out = run(dir, &["webhook", "status", "--db", "reeflog.db", "--config", cfg.to_str().unwrap()]);
    let report: serde_json::Value = serde_json::from_str(&stdout_of(&out)).unwrap_or(serde_json::json!({}));
    (report["targets"][0].clone(), out.status.code().unwrap_or(-1), stderr_of(&out))
}

fn push(dir: &Path, cfg: &Path, limit: Option<&str>) -> Output {
    let mut args = vec!["webhook", "push", "--db", "reeflog.db", "--config", cfg.to_str().unwrap()];
    if let Some(l) = limit {
        args.push("--limit");
        args.push(l);
    }
    run(dir, &args)
}

/// D1 (defect): a zero-byte pass on a journal whose retraction queue is
/// drained leaves the recorded retraction verdict STANDING in the journal.
/// Red on the pre-fix tree: the shared clear erased it to null.
#[test]
fn an_idle_pass_keeps_the_retraction_verdict_in_the_journal() {
    let dir = unique_dir("d1");
    init(&dir);
    let (url, bodies, _answered, stop) = scripted_sink(&[200]);
    let cfg = write_config(&dir, &url);
    plant(&dir, 1, 1, 1, 1, None, Some(REORG_VERDICT), &url);

    let out = push(&dir, &cfg, Some("0"));
    assert_eq!(out.status.code(), Some(0), "a zero-byte pass exits 0: {}", stderr_of(&out));

    let (_, last_reorg_error) = journal_verdicts(&dir);
    assert_eq!(
        last_reorg_error.as_deref(),
        Some(REORG_VERDICT),
        "the retraction verdict must survive a pass that examined no retraction row (bytes sent: {})",
        bodies.lock().unwrap().len()
    );
    *stop.lock().unwrap() = true;
}

/// D2 (defect): after that pass, `webhook status` — the report a monitoring
/// caller polls — still reports `reorg_error` and holds exit 3. Red on the
/// pre-fix tree: the report said healthy and exited 0.
#[test]
fn an_idle_pass_keeps_the_retraction_verdict_on_the_status_report() {
    let dir = unique_dir("d2");
    init(&dir);
    let (url, _bodies, _answered, stop) = scripted_sink(&[200]);
    let cfg = write_config(&dir, &url);
    plant(&dir, 1, 1, 1, 1, None, Some(REORG_VERDICT), &url);
    push(&dir, &cfg, Some("0"));

    let (row, exit, _err) = status(&dir, &cfg);
    let reorg_error = row["reorg_error"].as_str().unwrap_or("");
    assert!(
        reorg_error.contains(REORG_VERDICT),
        "the retraction verdict must still be reported after an idle pass: {reorg_error:?} in {row}"
    );
    assert_eq!(exit, 3, "a held retraction queue holds the exit code");
    *stop.lock().unwrap() = true;
}

/// D3 (defect): the retraction queue HELD at an undelivered reorg keeps its
/// verdict through an idle pass (the verdict is the block's own record).
#[test]
fn an_idle_pass_keeps_the_verdict_of_a_held_retraction_queue() {
    let dir = unique_dir("d3");
    init(&dir);
    let (url, _bodies, _answered, stop) = scripted_sink(&[404]);
    let cfg = write_config(&dir, &url);
    // cursor at reorg 1 of 2: the second retraction is still owed
    plant(&dir, 1, 2, 1, 1, None, Some(REORG_VERDICT), &url);

    let out = push(&dir, &cfg, Some("0"));
    assert_eq!(out.status.code(), Some(0), "a zero-byte pass exits 0: {}", stderr_of(&out));

    let (last_error, last_reorg_error) = journal_verdicts(&dir);
    assert!(last_error.is_none(), "the event queue was never examined: {last_error:?}");
    assert_eq!(last_reorg_error.as_deref(), Some(REORG_VERDICT));
    *stop.lock().unwrap() = true;
}

/// D4 (defect): the held queue's verdict reaches `webhook status` after the
/// idle pass, with exit 3.
#[test]
fn an_idle_pass_keeps_a_held_queue_on_the_status_report() {
    let dir = unique_dir("d4");
    init(&dir);
    let (url, _bodies, _answered, stop) = scripted_sink(&[404]);
    let cfg = write_config(&dir, &url);
    plant(&dir, 1, 2, 1, 1, None, Some(REORG_VERDICT), &url);
    push(&dir, &cfg, Some("0"));

    let (row, exit, _err) = status(&dir, &cfg);
    let reorg_error = row["reorg_error"].as_str().unwrap_or("");
    assert!(reorg_error.contains(REORG_VERDICT), "reorg_error: {reorg_error:?} in {row}");
    assert_eq!(exit, 3);
    *stop.lock().unwrap() = true;
}

/// D5 (defect, the mirror image): the EVENT channel's recorded verdict —
/// the payload-hold repair — survives a zero-byte pass too. A fix that only
/// moved the retraction column would leave the identical hole open here.
#[test]
fn an_idle_pass_keeps_the_event_verdict_in_the_journal() {
    let dir = unique_dir("d5");
    init(&dir);
    let (url, _bodies, _answered, stop) = scripted_sink(&[200]);
    let cfg = write_config(&dir, &url);
    // event queue drained (cursor past the only event), event verdict recorded
    plant(&dir, 1, 0, 1, 0, Some(EVENT_VERDICT), None, &url);

    let out = push(&dir, &cfg, Some("0"));
    assert_eq!(out.status.code(), Some(0), "a zero-byte pass exits 0: {}", stderr_of(&out));

    let (last_error, _) = journal_verdicts(&dir);
    assert_eq!(
        last_error.as_deref(),
        Some(EVENT_VERDICT),
        "the event verdict must survive a pass that examined no event row"
    );
    *stop.lock().unwrap() = true;
}

/// G1 (guard, both trees): a pass blocked on BOTH queues records BOTH
/// reasons, each on its own channel — the v0.7.37 write rule must survive.
#[test]
fn a_blocked_pass_records_both_verdicts_on_their_own_channels() {
    let dir = unique_dir("g1");
    init(&dir);
    let (url, _bodies, _answered, stop) = scripted_sink(&[200, 404, 404]);
    let cfg = write_config(&dir, &url);
    plant(&dir, 2, 1, 0, 0, None, None, &url);

    let out = push(&dir, &cfg, None);
    assert_eq!(out.status.code(), Some(3), "two blocked queues hold exit 3: {}", stderr_of(&out));

    let (last_error, last_reorg_error) = journal_verdicts(&dir);
    assert!(last_error.as_deref().unwrap_or("").contains("404"), "event verdict: {last_error:?}");
    assert!(
        last_reorg_error.as_deref().unwrap_or("").contains("404"),
        "retraction verdict on its own column: {last_reorg_error:?}"
    );
    *stop.lock().unwrap() = true;
}

/// G2 (guard, both trees): `--limit 0` sends zero bytes and moves no
/// cursor — the budget rule, unchanged.
#[test]
fn a_zero_limit_pass_sends_zero_bytes() {
    let dir = unique_dir("g2");
    init(&dir);
    let (url, bodies, _answered, stop) = scripted_sink(&[200, 200]);
    let cfg = write_config(&dir, &url);
    plant(&dir, 1, 1, 0, 0, None, None, &url);

    let out = push(&dir, &cfg, Some("0"));
    assert_eq!(out.status.code(), Some(0));
    assert!(bodies.lock().unwrap().is_empty(), "a zero-byte pass must send nothing");
    *stop.lock().unwrap() = true;
}

/// G3 (guard, both trees): a pass that GETS THROUGH delivers the owed
/// retraction and clears BOTH columns — the fix must not cost queue
/// progress nor freeze a verdict forever.
#[test]
fn a_through_pass_delivers_and_clears_both_columns() {
    let dir = unique_dir("g3");
    init(&dir);
    let (url, bodies, _answered, stop) = scripted_sink(&[200]);
    let cfg = write_config(&dir, &url);
    // one owed retraction behind the cursor, verdict recorded
    plant(&dir, 1, 1, 1, 0, None, Some(REORG_VERDICT), &url);

    let out = push(&dir, &cfg, None);
    assert_eq!(out.status.code(), Some(0), "a through pass exits 0: {}", stderr_of(&out));
    assert_eq!(bodies.lock().unwrap().len(), 1, "the owed retraction was delivered");

    let (last_error, last_reorg_error) = journal_verdicts(&dir);
    assert!(last_error.is_none() && last_reorg_error.is_none(), "verdicts: {last_error:?} / {last_reorg_error:?}");
    *stop.lock().unwrap() = true;
}

/// G4 (guard, both trees): the v0.7.23 rule on the event channel — a
/// recovered target stops reporting the old failure.
#[test]
fn a_recovered_event_queue_stops_reporting_the_old_failure() {
    let dir = unique_dir("g4");
    init(&dir);
    let (url, _bodies, _answered, stop) = scripted_sink(&[404, 200, 200]);
    let cfg = write_config(&dir, &url);
    plant(&dir, 2, 0, 0, 0, None, None, &url);

    let p1 = push(&dir, &cfg, None);
    assert_eq!(p1.status.code(), Some(3), "pass 1 is blocked: {}", stderr_of(&p1));
    let (last_error, _) = journal_verdicts(&dir);
    assert!(last_error.as_deref().unwrap_or("").contains("404"), "pass 1 records its block");

    let p2 = push(&dir, &cfg, None);
    assert_eq!(p2.status.code(), Some(0), "pass 2 gets through: {}", stderr_of(&p2));
    let (last_error, _) = journal_verdicts(&dir);
    assert!(last_error.is_none(), "a recovered queue reports no failure: {last_error:?}");
    *stop.lock().unwrap() = true;
}

/// G5 (guard, both trees): the v0.7.23 rule on the RETRACTION channel — a
/// recovered retraction queue stops reporting the old failure too.
#[test]
fn a_recovered_retraction_queue_stops_reporting_the_old_failure() {
    let dir = unique_dir("g5");
    init(&dir);
    let (url, _bodies, _answered, stop) = scripted_sink(&[200, 404, 200]);
    let cfg = write_config(&dir, &url);
    plant(&dir, 1, 1, 0, 0, None, None, &url);

    let p1 = push(&dir, &cfg, None);
    assert_eq!(p1.status.code(), Some(3), "pass 1 is blocked at the retraction: {}", stderr_of(&p1));
    let (_, last_reorg_error) = journal_verdicts(&dir);
    assert!(
        last_reorg_error.as_deref().unwrap_or("").contains("404"),
        "pass 1 records the retraction's block"
    );

    let p2 = push(&dir, &cfg, None);
    assert_eq!(p2.status.code(), Some(0), "pass 2 gets through: {}", stderr_of(&p2));
    let (_, last_reorg_error) = journal_verdicts(&dir);
    assert!(last_reorg_error.is_none(), "a recovered queue reports no failure: {last_reorg_error:?}");
    *stop.lock().unwrap() = true;
}
