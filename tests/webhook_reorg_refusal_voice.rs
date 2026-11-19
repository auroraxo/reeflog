//! The RETRACTION channel's refusal voice must be the same mint as the
//! event channel's: a 3xx refusal names the redirect and the explicit
//! repoint repair, on both queues.
//!
//! PULS AV, 2026-11-19. Measured with `tools/probe-reorg-refusal-voice.mjs`
//! against the published v0.7.38 asset (research/reorg-refusal-voice-2026-11-19.json,
//! verdict `reorg-refusal-voice-borrowed-flat-wording`, 3 defect faces):
//!
//!   * the EVENT channel said `refused with http 302 redirect — … repoint
//!     this target's url explicitly …` (the v0.7.36 one-mint rule);
//!   * the RETRACTION channel said the flat `refused with http 302` — the
//!     recorded attempt, the report's `reorg_error`, `webhook status`, and
//!     the operator's blocked line, all four, while the queue that cannot
//!     afford lateness is exactly the queue that read the borrowed
//!     wording. No mechanism, no repair: an operator reading the
//!     retraction block sees a bare status where the event block names
//!     the config change that fixes it.
//!
//! THE RULE NOW ENFORCED: `deliver_reorgs` calls the same `refusal_reason`
//! mint `deliver_events` and `webhook dlq replay` already call. A 3xx on
//! the retraction queue names the redirect and the repair; a 4xx keeps the
//! flat wording it always had.
//!
//! Guards (green on both trees — the normal cases must not move):
//!   G1 a 200 sink still delivers the retraction (receipt, cursor, exit 0);
//!   G2 a 404 refusal keeps the flat 4xx wording on the retraction queue;
//!   G3 the redirect refusal still blocks the retraction queue with its
//!      owed id and exit code 3 (classification unchanged — only the words
//!      move).
//!
//! Falsified by construction: revert `deliver_reorgs` to
//! `format!("refused with http {s}")` and D1-D3 go red.

use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

const PROGRAM: &str = "whirLbMiicVdio4qvUfM5KAg6Ct8VwpYzGff3uctyCc";

fn unique_dir(tag: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let dir = std::env::temp_dir().join(format!(
        "reeflog-reorg-voice-{}-{}-{}",
        tag,
        std::process::id(),
        nanos
    ));
    std::fs::create_dir_all(&dir).expect("temp dir");
    dir
}

/// A raw-socket receiver that answers ONE scripted status for every
/// request and records every request it received. Recorded BEFORE the
/// answer is written.
struct Recorder {
    port: u16,
    hits: Arc<Mutex<Vec<String>>>,
    answer: Arc<AtomicU64>,
    location: Arc<Mutex<Option<String>>>,
}

impl Recorder {
    fn start() -> Recorder {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind recorder");
        let port = listener.local_addr().expect("addr").port();
        let hits = Arc::new(Mutex::new(Vec::new()));
        let answer = Arc::new(AtomicU64::new(200));
        let location = Arc::new(Mutex::new(None));
        {
            let hits = hits.clone();
            let answer = answer.clone();
            let location = location.clone();
            listener.set_nonblocking(true).ok();
            std::thread::spawn(move || loop {
                match listener.accept() {
                    Ok((mut sock, _)) => {
                        sock.set_read_timeout(Some(Duration::from_secs(5))).ok();
                        let mut buf = [0u8; 65536];
                        let mut raw = Vec::new();
                        loop {
                            match sock.read(&mut buf) {
                                Ok(0) => break,
                                Ok(n) => {
                                    raw.extend_from_slice(&buf[..n]);
                                    let head = String::from_utf8_lossy(&raw).to_string();
                                    if head.contains("\r\n\r\n") {
                                        break;
                                    }
                                }
                                Err(_) => break,
                            }
                        }
                        let text = String::from_utf8_lossy(&raw).to_string();
                        let first_line = text.lines().next().unwrap_or("").to_string();
                        hits.lock().expect("hits lock").push(first_line);
                        let code = answer.load(Ordering::SeqCst) as u16;
                        let response = if code == 200 {
                            b"HTTP/1.1 200 OK\r\ncontent-length: 2\r\nconnection: close\r\n\r\nok".to_vec()
                        } else if let Some(loc) = location.lock().expect("loc lock").clone() {
                            format!(
                                "HTTP/1.1 {code} Moved\r\nlocation: {loc}\r\ncontent-length: 0\r\nconnection: close\r\n\r\n"
                            )
                            .into_bytes()
                        } else {
                            format!(
                                "HTTP/1.1 {code} Gone\r\ncontent-length: 0\r\nconnection: close\r\n\r\n"
                            )
                            .into_bytes()
                        };
                        let _ = sock.write_all(&response);
                    }
                    Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(5));
                    }
                    Err(_) => break,
                }
            });
        }
        Recorder {
            port,
            hits,
            answer,
            location,
        }
    }

    fn url(&self) -> String {
        format!("http://127.0.0.1:{}/hook", self.port)
    }

    fn url_path(&self, suffix: &str) -> String {
        format!("http://127.0.0.1:{}/{suffix}", self.port)
    }

    fn set_answer(&self, code: u16) {
        self.answer.store(code as u64, Ordering::SeqCst);
    }

    fn set_location(&self, loc: Option<String>) {
        *self.location.lock().expect("loc lock") = loc;
    }

    fn post_count(&self) -> usize {
        self.hits
            .lock()
            .expect("hits lock")
            .iter()
            .filter(|l| l.starts_with("POST"))
            .count()
    }
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

fn json_of(out: &Output) -> serde_json::Value {
    serde_json::from_str(&stdout_of(out)).unwrap_or_else(|e| {
        panic!(
            "the command must PRINT a report: {e}\nstdout: {}\nstderr: {}",
            stdout_of(out),
            stderr_of(out)
        )
    })
}

/// One deliverable event and one recorded retraction, so BOTH queues have
/// an owed row: the event channel's voice must not move while the
/// retraction channel's does.
fn plant(db: &Path) {
    let js = r#"
const { DatabaseSync } = require('node:sqlite');
const d = new DatabaseSync(process.argv[1]);
d.exec("INSERT INTO reorgs(signature, slot, detected_at, status_source, tx_source, detail) VALUES('sigRET1', 201, 1700000000000, 'slot', 'tx', 'the chain unmade it')");
d.exec("INSERT INTO events(signature, slot, err, logs, source, first_seen, seal_status, payload_state) VALUES('sigEV1', 101, NULL, NULL, 'ws', 1700000000000, 'sealed', 'held')");
"#;
    let out = Command::new("node")
        .args(["-e", js])
        .arg(db)
        .output()
        .expect("node plants rows");
    assert!(out.status.success(), "plant: {}", stderr_of(&out));
}

fn write_config(dir: &Path, name: &str, url: &str) -> PathBuf {
    let path = dir.join(name);
    std::fs::write(
        &path,
        serde_json::to_string(&serde_json::json!({
            "targets": [{ "id": "t1", "url": url, "secret": "s-t1", "timeout_ms": 2000, "dead_letter_after": 0 }]
        }))
        .unwrap(),
    )
    .unwrap();
    path
}

fn attempt_error(db: &Path, kind: &str) -> Option<String> {
    let js = format!(
        r#"
const {{ DatabaseSync }} = require('node:sqlite');
const d = new DatabaseSync(process.argv[1]);
const rows = d.prepare("SELECT status, error FROM webhook_attempts WHERE kind='{kind}' ORDER BY id").all();
console.log(JSON.stringify(rows));
"#
    );
    let out = Command::new("node")
        .args(["-e", &js])
        .arg(db)
        .output()
        .expect("node reads attempts");
    let v: serde_json::Value = serde_json::from_str(&stdout_of(&out)).ok()?;
    v.as_array()?.first()?.get("error").and_then(|e| e.as_str()).map(|s| s.to_string())
}

fn setup(tag: &str) -> (PathBuf, PathBuf) {
    let dir = unique_dir(tag);
    let db = dir.join("reeflog.db");
    let out = run(&dir, &["init", "--db", db.to_str().unwrap(), "--program", PROGRAM]);
    assert_eq!(out.status.code(), Some(0), "{}", stderr_of(&out));
    plant(&db);
    (dir, db)
}

// ---------------------------------------------------------------------------
// D1: the recorded RETRACTION attempt names the redirect mechanism (and
// its repair). Measured on the published v0.7.38 asset: flat
// "refused with http 302" where the event attempt said "… redirect — …
// repoint this target's url explicitly …".
// ---------------------------------------------------------------------------

#[test]
fn d1_the_retraction_attempt_names_the_redirect_and_the_repair() {
    let (dir, db) = setup("d1");
    let sink = Recorder::start();
    sink.set_answer(302);
    sink.set_location(Some(sink.url_path("elsewhere")));
    let cfg = write_config(&dir, "cfg.json", &sink.url());

    let out = run(
        &dir,
        &[
            "webhook",
            "push",
            "--db",
            db.to_str().unwrap(),
            "--config",
            cfg.to_str().unwrap(),
        ],
    );
    let reason = attempt_error(&db, "reorg").expect("the retraction attempt is recorded");
    assert!(
        reason.contains("refused with http 302") && reason.contains("redirect"),
        "the retraction attempt must name the redirect mechanism, got: {reason}"
    );
    assert!(
        reason.contains("repoint this target's url explicitly"),
        "the retraction attempt must name the repair, got: {reason}"
    );
    // The event channel's own record must be byte-identical: ONE mint.
    let ev = attempt_error(&db, "event").expect("the event attempt is recorded");
    assert_eq!(
        ev, reason,
        "both queues speak ONE refusal voice — a 3xx must read the same on both, got event {ev:?} vs reorg {reason:?}"
    );
    assert_eq!(out.status.code(), Some(3), "{}", stderr_of(&out));
}

// ---------------------------------------------------------------------------
// D2: the report row's `reorg_error` names the redirect mechanism. Measured
// on the published v0.7.38 asset: `reorg_error: "refused with http 302"`
// while `error` (the event queue) carried the full reason.
// ---------------------------------------------------------------------------

#[test]
fn d2_the_report_reorg_error_names_the_redirect() {
    let (dir, db) = setup("d2");
    let sink = Recorder::start();
    sink.set_answer(301);
    sink.set_location(Some(sink.url_path("elsewhere")));
    let cfg = write_config(&dir, "cfg.json", &sink.url());

    let out = run(
        &dir,
        &[
            "webhook",
            "push",
            "--db",
            db.to_str().unwrap(),
            "--config",
            cfg.to_str().unwrap(),
        ],
    );
    assert_eq!(out.status.code(), Some(3), "{}", stderr_of(&out));
    let rep = json_of(&out);
    let reason = rep["targets"][0]["reorg_error"]
        .as_str()
        .unwrap_or("")
        .to_string();
    assert!(
        reason.contains("301") && reason.contains("redirect"),
        "the report's retraction reason must name the redirect, got: {reason}"
    );
    assert!(
        reason.contains("repoint this target's url explicitly"),
        "the report's retraction reason must name the repair, got: {reason}"
    );
}

// ---------------------------------------------------------------------------
// D3: the operator's blocked line on stderr names the redirect. Measured on
// the published v0.7.38 asset: "t1 at reorg 1: refused with http 302 — the
// RETRACTION is still owed …" — mechanism and repair both absent on the
// queue whose lateness costs money.
// ---------------------------------------------------------------------------

#[test]
fn d3_the_operator_blocked_line_names_the_redirect() {
    let (dir, db) = setup("d3");
    let sink = Recorder::start();
    sink.set_answer(302);
    sink.set_location(Some(sink.url_path("elsewhere")));
    let cfg = write_config(&dir, "cfg.json", &sink.url());

    let out = run(
        &dir,
        &[
            "webhook",
            "push",
            "--db",
            db.to_str().unwrap(),
            "--config",
            cfg.to_str().unwrap(),
        ],
    );
    let line = stderr_of(&out)
        .lines()
        .find(|l| l.contains("reorg 1"))
        .unwrap_or("")
        .to_string();
    assert!(
        line.contains("redirect") && line.contains("repoint this target's url explicitly"),
        "the operator's retraction block line must name the redirect and the repair, got: {line}"
    );
    assert!(
        line.contains("the RETRACTION is still owed"),
        "the line must still say which queue is owed, got: {line}"
    );
}

// ---------------------------------------------------------------------------
// G1: a 200 sink still delivers the retraction normally — the rule must not
// turn the normal case into a refusal.
// ---------------------------------------------------------------------------

#[test]
fn g1_a_200_sink_still_delivers_the_retraction() {
    let (dir, db) = setup("g1");
    let sink = Recorder::start();
    let cfg = write_config(&dir, "cfg.json", &sink.url());

    let out = run(
        &dir,
        &[
            "webhook",
            "push",
            "--db",
            db.to_str().unwrap(),
            "--config",
            cfg.to_str().unwrap(),
        ],
    );
    assert_eq!(out.status.code(), Some(0), "{}", stderr_of(&out));
    let rep = json_of(&out);
    assert_eq!(rep["targets"][0]["reorg_delivered"].as_i64(), Some(1));
    assert_eq!(rep["targets"][0]["reorg_error"], serde_json::Value::Null);
    assert_eq!(sink.post_count(), 2, "one event + one retraction");
}

// ---------------------------------------------------------------------------
// G2: a 404 refusal keeps the flat 4xx wording on the retraction queue —
// the mint only speaks differently for 3xx; a 4xx reads exactly as before.
// ---------------------------------------------------------------------------

#[test]
fn g2_a_404_refusal_keeps_the_flat_wording() {
    let (dir, db) = setup("g2");
    let sink = Recorder::start();
    sink.set_answer(404);
    let cfg = write_config(&dir, "cfg.json", &sink.url());

    let out = run(
        &dir,
        &[
            "webhook",
            "push",
            "--db",
            db.to_str().unwrap(),
            "--config",
            cfg.to_str().unwrap(),
        ],
    );
    assert_eq!(out.status.code(), Some(3), "{}", stderr_of(&out));
    let rep = json_of(&out);
    assert_eq!(
        rep["targets"][0]["reorg_error"].as_str(),
        Some("refused with http 404"),
        "a 4xx must not grow redirect wording"
    );
    let reason = attempt_error(&db, "reorg").expect("attempt recorded");
    assert_eq!(reason, "refused with http 404");
}

// ---------------------------------------------------------------------------
// G3: the redirect refusal still BLOCKS the retraction queue with its owed
// id and exit 3 — only the words changed, never the classification.
// ---------------------------------------------------------------------------

#[test]
fn g3_a_redirect_still_blocks_the_retraction_queue() {
    let (dir, db) = setup("g3");
    let sink = Recorder::start();
    sink.set_answer(302);
    sink.set_location(Some(sink.url_path("elsewhere")));
    let cfg = write_config(&dir, "cfg.json", &sink.url());

    let out = run(
        &dir,
        &[
            "webhook",
            "push",
            "--db",
            db.to_str().unwrap(),
            "--config",
            cfg.to_str().unwrap(),
        ],
    );
    assert_eq!(out.status.code(), Some(3), "the queue must hold exit 3");
    let rep = json_of(&out);
    assert_eq!(rep["targets"][0]["blocked_reorg_id"].as_i64(), Some(1));
    assert_eq!(rep["targets"][0]["blocked"].as_bool(), Some(true));
    assert_eq!(rep["targets"][0]["reorg_delivered"].as_i64(), Some(0));
    // The redirect is never followed: the configured endpoint received the
    // two refusals it owed (one event, one retraction) and the redirect
    // destination received NOTHING.
    assert_eq!(
        sink.post_count(),
        2,
        "the configured endpoint receives the refusals it owes (one event, one retraction)"
    );
    let hop = Recorder::start();
    sink.set_location(Some(hop.url_path("elsewhere")));
    assert_eq!(
        hop.post_count(),
        0,
        "no request may follow the redirect: the config did not name that url"
    );
}

// ---------------------------------------------------------------------------
// D4: the POLLED surface tells the same story — `webhook status` names the
// redirect on the retraction queue, without a new delivery pass. Measured on
// the published v0.7.38 asset: the standing retraction reason read
// "refused with http 302" where the event queue's read named the redirect.
// ---------------------------------------------------------------------------

#[test]
fn d4_the_polled_status_names_the_redirect_on_the_retraction_queue() {
    let (dir, db) = setup("d4");
    let sink = Recorder::start();
    sink.set_answer(302);
    sink.set_location(Some(sink.url_path("elsewhere")));
    let cfg = write_config(&dir, "cfg.json", &sink.url());

    let _ = run(
        &dir,
        &[
            "webhook",
            "push",
            "--db",
            db.to_str().unwrap(),
            "--config",
            cfg.to_str().unwrap(),
        ],
    );
    let status = run(
        &dir,
        &[
            "webhook",
            "status",
            "--db",
            db.to_str().unwrap(),
            "--config",
            cfg.to_str().unwrap(),
        ],
    );
    let srep = json_of(&status);
    let last = srep["targets"][0]["reorg_error"].as_str().unwrap_or("");
    assert!(
        last.contains("redirect") && last.contains("repoint this target's url explicitly"),
        "`webhook status` must name the redirect on the retraction queue, got: {last}"
    );
}
