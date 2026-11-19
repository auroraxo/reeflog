//! The two delivery queues must keep their own standing reasons.
//!
//! PULS AT, 2026-11-19. Measured on the PUBLISHED v0.7.36 asset (the linux
//! archive whose sha256 equals the digest the Release API publishes;
//! `tools/measure-status-two-queue-reason.mjs`, the real binary against a
//! real journal with a live sink; `research/status-two-queue-reason-2026-11-19.json`,
//! verdict `status-two-queue-reason-one-field`, 3 defect faces):
//!
//! The event queue and the retraction queue are INDEPENDENT — a retraction
//! must still reach a consumer whose event queue is blocked, which is why
//! `webhook push` reports them on two separate fields (`error` and
//! `reorg_error`) since v0.7.33. The JOURNAL kept ONE standing reason per
//! target (`webhook_state.last_error`): `deliver_events` wrote it, then
//! `deliver_reorgs` wrote the SAME column, and the second write won.
//!
//! With both queues blocked at once — the event queue held at an owed
//! payload, the retraction queue refused with 404 — `webhook push` reported
//! BOTH reasons (`error: "the payload is still owed — run \`hydrate\`"` and
//! `reorg_error: "refused with http 404"`), while `webhook status` — the
//! report a monitoring caller polls — reported ONE: the retraction's 404.
//! The event queue's repair (`hydrate`) was gone from the report, and
//! nothing on the row said which channel the 404 belonged to. An operator
//! reading that report would run the wrong repair for the queue that was
//! actually held.
//!
//! THE RULE NOW ENFORCED:
//!
//!   * each channel's standing reason lives in its OWN column
//!     (`last_error` for events, `last_reorg_error` for retractions), so
//!     one channel's write never erases the other's;
//!   * `webhook status` prints the retraction channel's reason on its own
//!     field `reorg_error` — the same name `webhook push` uses for it, so
//!     one fact carries one name across the surfaces;
//!   * a target blocked on BOTH queues shows BOTH reasons, each on its own
//!     channel.
//!
//! Guards (green on BOTH trees — the normal cases must not move):
//!   G1 only the event queue blocked: `last_error` names it, exit 3;
//!   G2 only the retraction queue blocked: the reason is reported on the
//!      retraction channel and exit 3 holds;
//!   G3 a clean journal: no standing reasons, exit 0.

use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

const PROGRAM: &str = "whirLbMiicVdio4qvUfM5KAg6Ct8VwpYzGff3uctyCc";
const PAYLOAD_REASON: &str = "the payload is still owed";
const REORG_REASON: &str = "refused with http 404";

fn unique_dir(tag: &str) -> PathBuf {
    static N: AtomicU64 = AtomicU64::new(0);
    let n = N.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!("reeflog-two-queue-{tag}-{}-{n}", std::process::id()));
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

fn report_of(out: &Output) -> serde_json::Value {
    serde_json::from_str(&stdout_of(out)).unwrap_or(serde_json::json!({}))
}

fn init(dir: &Path) {
    let out = run(dir, &["init", "--db", "reeflog.db", "--program", PROGRAM]);
    assert!(out.status.success(), "init: {}", stderr_of(&out));
}

/// A live sink that answers 200 to event bodies and 404 to retraction
/// bodies: a consumer that refuses retractions.
/// (url, recorded bodies, stop flag)
type Sink = (String, Arc<Mutex<Vec<String>>>, Arc<Mutex<bool>>);

fn rejecting_reorg_sink() -> Sink {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().expect("addr");
    let hits = Arc::new(Mutex::new(Vec::new()));
    let stop = Arc::new(Mutex::new(false));
    let hits_c = Arc::clone(&hits);
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
                let is_reorg = body.contains("\"type\":\"reorg\"");
                hits_c.lock().unwrap().push(body);
                let resp = if is_reorg {
                    "HTTP/1.1 404 Not Found\r\ncontent-length: 0\r\nconnection: close\r\n\r\n"
                } else {
                    "HTTP/1.1 200 OK\r\ncontent-length: 2\r\nconnection: close\r\n\r\nok"
                };
                let _ = s.write_all(resp.as_bytes());
                let _ = s.flush();
                break;
            }
        }
    });
    (format!("http://{addr}/hook"), hits, stop)
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

/// A journal with one deliverable event, an optional second row the journal
/// still owes its content for, and an optional retraction for the first
/// event.
fn plant(dir: &Path, owe_payload: bool, retract: bool) {
    let conn = rusqlite::Connection::open(dir.join("reeflog.db")).expect("open journal");
    conn.execute(
        "INSERT INTO events(signature,slot,err,logs,source,first_seen,seal_status,payload_state)
         VALUES('SIGONE',100,NULL,'[\"a\"]','ws',1700000000000,'sealed','held')",
        [],
    )
    .expect("plant row 1");
    if owe_payload {
        conn.execute(
            "INSERT INTO events(signature,slot,err,logs,source,first_seen,seal_status,payload_state)
             VALUES('SIGTWO',101,NULL,NULL,'ws',1700000000000,'sealed',NULL)",
            [],
        )
        .expect("plant row 2");
    }
    if retract {
        conn.execute(
            "UPDATE events SET seal_status='reverted' WHERE signature='SIGONE'",
            [],
        )
        .expect("mark reverted");
        conn.execute(
            "INSERT INTO reorgs(signature,slot,detected_at,status_source,tx_source,detail)
             VALUES('SIGONE',100,1700000000001,'status','tx','probe')",
            [],
        )
        .expect("plant retraction");
    }
    conn.close().expect("close journal");
}

struct StatusRow {
    value: serde_json::Value,
    exit: i32,
    stderr: String,
}

fn status(dir: &Path, cfg: &Path) -> StatusRow {
    let out = run(
        dir,
        &[
            "webhook",
            "status",
            "--db",
            "reeflog.db",
            "--config",
            cfg.to_str().unwrap(),
        ],
    );
    let report = report_of(&out);
    StatusRow {
        value: report["targets"][0].clone(),
        exit: out.status.code().unwrap_or(-1),
        stderr: stderr_of(&out),
    }
}

/// D1 (defect): with BOTH queues blocked, the status report carries the
/// EVENT channel's reason. Red on the pre-fix tree: the retraction's 404
/// overwrote the payload hold and `last_error` named the wrong queue.
#[test]
fn both_queues_blocked_keeps_the_event_channel_reason() {
    let dir = unique_dir("both");
    init(&dir);
    plant(&dir, true, true);
    let (url, _hits, stop) = rejecting_reorg_sink();
    let cfg = write_config(&dir, &url);

    let push = run(
        &dir,
        &[
            "webhook",
            "push",
            "--db",
            "reeflog.db",
            "--config",
            cfg.to_str().unwrap(),
        ],
    );
    assert_eq!(
        push.status.code(),
        Some(3),
        "two blocked queues hold the exit code: {}",
        stderr_of(&push)
    );

    let st = status(&dir, &cfg);
    let last_error = st.value["last_error"].as_str().unwrap_or("");
    assert!(
        last_error.contains(PAYLOAD_REASON),
        "the event channel's reason must survive on the status row: {last_error:?} in {}",
        st.value
    );
    *stop.lock().unwrap() = true;
}

/// D2 (defect): the retraction channel's reason is attributable to the
/// retraction channel — a field of its own, named like the one `webhook
/// push` uses.
#[test]
fn the_retraction_reason_is_attributable_to_its_channel() {
    let dir = unique_dir("channel");
    init(&dir);
    plant(&dir, true, true);
    let (url, _hits, stop) = rejecting_reorg_sink();
    let cfg = write_config(&dir, &url);
    run(
        &dir,
        &[
            "webhook",
            "push",
            "--db",
            "reeflog.db",
            "--config",
            cfg.to_str().unwrap(),
        ],
    );

    let st = status(&dir, &cfg);
    let reorg_error = st.value["reorg_error"].as_str().unwrap_or("");
    assert!(
        reorg_error.contains(REORG_REASON),
        "the retraction channel's reason needs its own field on the status row: {reorg_error:?} in {}",
        st.value
    );
    *stop.lock().unwrap() = true;
}

/// D3 (defect): in the JOURNAL, one channel's standing reason does not
/// erase the other's.
#[test]
fn one_channels_reason_does_not_erase_the_other_in_the_journal() {
    let dir = unique_dir("journal");
    init(&dir);
    plant(&dir, true, true);
    let (url, _hits, stop) = rejecting_reorg_sink();
    let cfg = write_config(&dir, &url);
    run(
        &dir,
        &[
            "webhook",
            "push",
            "--db",
            "reeflog.db",
            "--config",
            cfg.to_str().unwrap(),
        ],
    );

    let conn = rusqlite::Connection::open(dir.join("reeflog.db")).expect("open journal");
    let last_error: Option<String> = conn
        .query_row(
            "SELECT last_error FROM webhook_state WHERE target='t1'",
            [],
            |r| r.get(0),
        )
        .expect("state row");
    let columns: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM pragma_table_info('webhook_state') WHERE name='last_reorg_error'",
            [],
            |r| r.get(0),
        )
        .expect("columns");
    let last_reorg_error: Option<String> = if columns > 0 {
        conn.query_row(
            "SELECT last_reorg_error FROM webhook_state WHERE target='t1'",
            [],
            |r| r.get(0),
        )
        .expect("state row")
    } else {
        None
    };
    conn.close().ok();

    assert!(
        last_error
            .as_deref()
            .unwrap_or("")
            .contains(PAYLOAD_REASON),
        "the event channel's reason must survive in the journal: {last_error:?}"
    );
    assert!(
        last_reorg_error
            .as_deref()
            .unwrap_or("")
            .contains(REORG_REASON),
        "the retraction channel's reason needs its own column: {last_reorg_error:?}"
    );
    *stop.lock().unwrap() = true;
}

/// G1 (guard, both trees): only the event queue blocked — its reason is
/// reported and the exit code holds.
#[test]
fn only_the_event_queue_blocked_is_unchanged() {
    let dir = unique_dir("only-event");
    init(&dir);
    plant(&dir, true, false);
    let (url, _hits, stop) = rejecting_reorg_sink();
    let cfg = write_config(&dir, &url);
    run(
        &dir,
        &[
            "webhook",
            "push",
            "--db",
            "reeflog.db",
            "--config",
            cfg.to_str().unwrap(),
        ],
    );

    let st = status(&dir, &cfg);
    assert!(
        st.value["last_error"]
            .as_str()
            .unwrap_or("")
            .contains(PAYLOAD_REASON),
        "the event queue's reason must be reported: {}",
        st.value
    );
    assert_eq!(st.exit, 3, "a standing failure holds the exit code: {}", st.stderr);
    *stop.lock().unwrap() = true;
}

/// G2 (guard, both trees): only the retraction queue blocked — the reason
/// is reported somewhere on the row and the exit code holds.
#[test]
fn only_the_retraction_queue_blocked_is_unchanged() {
    let dir = unique_dir("only-reorg");
    init(&dir);
    plant(&dir, false, true);
    let (url, _hits, stop) = rejecting_reorg_sink();
    let cfg = write_config(&dir, &url);
    run(
        &dir,
        &[
            "webhook",
            "push",
            "--db",
            "reeflog.db",
            "--config",
            cfg.to_str().unwrap(),
        ],
    );

    let st = status(&dir, &cfg);
    let visible = [st.value["last_error"].as_str(), st.value["reorg_error"].as_str()]
        .iter()
        .any(|v| v.unwrap_or("").contains(REORG_REASON));
    assert!(
        visible,
        "the retraction queue's reason must be reported: {}",
        st.value
    );
    assert_eq!(st.exit, 3, "a standing failure holds the exit code: {}", st.stderr);
    *stop.lock().unwrap() = true;
}

/// G3 (guard, both trees): a clean journal reports no standing reasons and
/// exits 0.
#[test]
fn a_clean_journal_reports_no_standing_reasons() {
    let dir = unique_dir("clean");
    init(&dir);
    plant(&dir, false, false);
    let (url, _hits, stop) = rejecting_reorg_sink();
    let cfg = write_config(&dir, &url);
    run(
        &dir,
        &[
            "webhook",
            "push",
            "--db",
            "reeflog.db",
            "--config",
            cfg.to_str().unwrap(),
        ],
    );

    let st = status(&dir, &cfg);
    assert!(
        st.value["last_error"].is_null(),
        "a clean target carries no event reason: {}",
        st.value
    );
    assert!(
        st.value.get("reorg_error").is_none() || st.value["reorg_error"].is_null(),
        "a clean target carries no retraction reason: {}",
        st.value
    );
    assert_eq!(st.exit, 0, "a clean journal exits 0: {}", st.stderr);
    *stop.lock().unwrap() = true;
}

/// The two columns must be independent in the JOURNAL, not only in the
/// report: a retraction hold written after an event hold must not erase the
/// event hold (and the reverse).
#[test]
fn the_two_columns_are_independent() {
    let dir = unique_dir("columns");
    init(&dir);
    let conn = rusqlite::Connection::open(dir.join("reeflog.db")).expect("open journal");
    conn.execute(
        "INSERT INTO webhook_state(target,endpoint,next_seq,next_reorg_id,updated_at)
         VALUES('t1','http://x/y',0,0,0)",
        [],
    )
    .expect("plant state row");
    conn.close().ok();

    // The journal is opened by the binary, so the migration runs there.
    let out = run(&dir, &["status", "--db", "reeflog.db"]);
    assert!(out.status.success(), "status: {}", stderr_of(&out));

    let conn = rusqlite::Connection::open(dir.join("reeflog.db")).expect("open journal");
    let columns: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM pragma_table_info('webhook_state') WHERE name='last_reorg_error'",
            [],
            |r| r.get(0),
        )
        .expect("columns");
    assert!(columns > 0, "the retraction channel has its own column");
    conn.execute(
        "UPDATE webhook_state SET last_error='event reason', last_reorg_error='reorg reason' WHERE target='t1'",
        [],
    )
    .expect("write both");
    let (e, r): (Option<String>, Option<String>) = conn
        .query_row(
            "SELECT last_error, last_reorg_error FROM webhook_state WHERE target='t1'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .expect("read both");
    conn.close().ok();
    assert_eq!(e.as_deref(), Some("event reason"));
    assert_eq!(r.as_deref(), Some("reorg reason"));
}

/// The daemon's own tick path writes the two reasons too: the payload
/// worker and the webhook pass run against the same journal.
#[test]
fn a_recovered_target_clears_both_reasons() {
    let dir = unique_dir("recovered");
    init(&dir);
    plant(&dir, true, true);
    let (url, _hits, stop) = rejecting_reorg_sink();
    let cfg = write_config(&dir, &url);
    run(
        &dir,
        &[
            "webhook",
            "push",
            "--db",
            "reeflog.db",
            "--config",
            cfg.to_str().unwrap(),
        ],
    );
    let st = status(&dir, &cfg);
    assert_eq!(st.exit, 3, "a blocked target holds the exit code: {}", st.stderr);

    // Pay the payload debt and let the retraction through: the next pass
    // must clear BOTH standing reasons (a report that keeps a failure that
    // ended is the v0.7.23 defect).
    let conn = rusqlite::Connection::open(dir.join("reeflog.db")).expect("open journal");
    conn.execute(
        "UPDATE events SET logs='[\"b\"]', payload_state='held' WHERE signature='SIGTWO'",
        [],
    )
    .expect("pay the debt");
    conn.execute(
        "DELETE FROM reorgs WHERE signature='SIGONE'",
        [],
    )
    .expect("remove the retraction");
    conn.close().ok();

    let push = run(
        &dir,
        &[
            "webhook",
            "push",
            "--db",
            "reeflog.db",
            "--config",
            cfg.to_str().unwrap(),
        ],
    );
    assert_eq!(
        push.status.code(),
        Some(0),
        "a clean pass exits 0: {}",
        stderr_of(&push)
    );
    let st = status(&dir, &cfg);
    assert!(
        st.value["last_error"].is_null(),
        "the event reason must be cleared: {}",
        st.value
    );
    assert!(
        st.value.get("reorg_error").is_none() || st.value["reorg_error"].is_null(),
        "the retraction reason must be cleared: {}",
        st.value
    );
    assert_eq!(st.exit, 0, "a clean target exits 0: {}", st.stderr);
    *stop.lock().unwrap() = true;
}
