//! A webhook endpoint must be delivered to AS CONFIGURED — a 3xx answer is
//! not a delivery verdict.
//!
//! PULS AS, 2026-11-19. Measured on this tree at v0.7.35 (df514f7, release
//! binary) with a live two-sink probe: the configured endpoint answered
//! `302` with `location:` pointing at a second local sink, and `HttpPoster`
//! followed it (ureq 3.5 defaults: 10 redirects, `RedirectAuthHeaders::
//! Never` keeps CUSTOM headers, 302-on-POST downgrades the method to GET
//! and DROPS the body):
//!
//!   * the sink BEHIND the redirect received a `GET` with the signature
//!     header still attached and ZERO body bytes;
//!   * the journal recorded the event `delivered` (attempt status 200, the
//!     redirect target's own answer), the cursor advanced past the seq and
//!     the exit code was 0 — `webhook status` reports a healthy consumer
//!     for a sink that never received one event body;
//!   * the configured endpoint itself received exactly one POST — the
//!     delivery it refused.
//!
//! Three facts break at once:
//!
//!   1. AT-LEAST-ONCE TO THE CONFIGURED CONSUMER: the endpoint the
//!      operator configured did not accept the event, but the journal says
//!      it did. The event can never be re-delivered (the cursor moved).
//!      `webhook push` exists so the CONFIGURED sink gets every event —
//!      "the code followed the event somewhere else" is a different
//!      consumer, and the config never named it.
//!   2. THE SIGNATURE IS A PROMISE ABOUT THE BODY: `x-reeflog-signature`
//!      signs the exact bytes of ONE body (v0.7.16 rule: one secret per
//!      target identity). A redirect hop that drops the body keeps the
//!      signature header — the receiver is invited to verify bytes that
//!      are not there.
//!   3. THE REPORT NAMES THE WRONG URL: the report row and the recorded
//!      endpoint print the url the OPERATOR configured, while the bytes
//!      went to the `location:` url — the one surface that tells the
//!      truth about where delivery went described a delivery that never
//!      happened on that endpoint.
//!
//! THE RULE NOW ENFORCED (in ONE place, `HttpPoster` — every CLI surface
//! pushes through it: `webhook push`, `webhook dlq replay`, the daemon's
//! own payload worker):
//!
//!   * a 3xx answer is a TRANSIENT transport fact named with its status,
//!     never a verdict: ureq is configured with `max_redirects(0)` so the
//!     redirect is reported, not followed;
//!   * no request is ever sent to a url the config did not name: one POST
//!     per attempt, with the body and the signature headers;
//!   * a 3xx answer blocks the queue like any other transient outage —
//!     the cursor stays behind the seq, the attempt is recorded with the
//!     status, and the exit code holds (3) — the operator can repoint the
//!     target's url at the redirect destination EXPLICITLY if that is
//!     what they meant, which is a config change, not a silent hop.
//!
//! Guards (green on BOTH trees — the normal cases must not move):
//!   G1 a plain 200 sink delivers normally, cursor advances, exit 0;
//!   G2 a 404 sink is refused exactly as before (4xx classification);
//!   G3 a 500 sink is transient exactly as before (5xx classification).
//!
//! (The G1/G2/G3 classification itself is pinned by the ScriptedPoster
//! unit tests in src/webhook.rs; these are the end-to-end variants over
//! the REAL HttpPoster on a REAL socket.)

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
        "reeflog-webhook-redirect-{}-{}-{}",
        tag,
        std::process::id(),
        nanos
    ));
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

fn stdout_of(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn json_of(out: &Output) -> serde_json::Value {
    serde_json::from_str(&stdout_of(out)).unwrap_or_else(|e| {
        panic!(
            "the command must PRINT a report (a caller polls it): {e}\nstdout: {}\nstderr: {}",
            stdout_of(out),
            stderr_of(out)
        )
    })
}

/// A raw-socket receiver that answers ONE scripted status for every
/// request and records every request it received: method, path, whether a
/// body arrived, and which headers survived. Recorded BEFORE the answer is
/// written, so a request the probe answers badly is still a fact.
struct Recorder {
    port: u16,
    hits: Arc<Mutex<Vec<serde_json::Value>>>,
    /// None = answer 200 with a body; Some(code) = answer that status.
    answer: Arc<AtomicU64>,
    /// The `location:` header value for redirect answers.
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
                                    let head =
                                        String::from_utf8_lossy(&raw).to_string();
                                    if head.contains("\r\n\r\n") {
                                        break;
                                    }
                                }
                                Err(_) => break,
                            }
                        }
                        let text = String::from_utf8_lossy(&raw).to_string();
                        let head_end = text.find("\r\n\r\n").unwrap_or(text.len());
                        let head = &text[..head_end];
                        let body_len = raw.len() - head_end.min(raw.len()) - 4;
                        let first_line = head.lines().next().unwrap_or("").to_string();
                        let has_sig = head.contains("x-reeflog-signature:");
                        let has_seq = head.contains("x-reeflog-seq:");
                        hits.lock().expect("hits lock").push(serde_json::json!({
                            "request_line": first_line,
                            "body_bytes": body_len.max(0),
                            "signature_header": has_sig,
                            "seq_header": has_seq,
                        }));
                        let code = answer.load(Ordering::SeqCst) as u16;
                        let response = if code == 200 {
                            b"HTTP/1.1 200 OK\r\ncontent-length: 2\r\nconnection: close\r\n\r\nok".to_vec()
                        } else if let Some(loc) = location.lock().expect("loc lock").clone() {
                            format!(
                                "HTTP/1.1 {code} Moved\r\nlocation: {loc}\r\ncontent-length: 0\r\nconnection: close\r\n\r\n"
                            )
                            .into_bytes()
                        } else {
                            format!("HTTP/1.1 {code} Gone\r\ncontent-length: 0\r\nconnection: close\r\n\r\n").into_bytes()
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

    fn hits(&self) -> Vec<serde_json::Value> {
        self.hits.lock().expect("hits lock").clone()
    }

    fn post_count(&self) -> usize {
        self.hits()
            .iter()
            .filter(|h| {
                h["request_line"]
                    .as_str()
                    .unwrap_or("")
                    .starts_with("POST")
            })
            .count()
    }
}

fn seed_events(db: &Path, n: usize) {
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let js = format!(
        r#"
const {{ DatabaseSync }} = require('node:sqlite');
const d = new DatabaseSync(process.argv[1]);
const n = parseInt(process.argv[2], 10);
const nonce = process.argv[3];
for (let i = 1; i <= n; i++) {{
  d.exec(`INSERT INTO events(signature, slot, err, logs, source, first_seen, seal_status, payload_state)
          VALUES ('R${{nonce}}_${{i}}${{'x'.repeat(40)}}', ${{44000000 + i}}, NULL, '[]', 'ws', 1700000000000, 'sealed', 'held')`);
}}
"#
    );
    let out = Command::new("node")
        .args(["-e", &js])
        .arg(db)
        .arg(n.to_string())
        .arg(nonce.to_string())
        .output()
        .expect("node seeds events");
    assert!(out.status.success(), "seeding failed: {}", stderr_of(&out));
}

fn write_config(dir: &Path, name: &str, targets: &[serde_json::Value]) -> PathBuf {
    let path = dir.join(name);
    std::fs::write(
        &path,
        serde_json::to_string(&serde_json::json!({ "targets": targets })).unwrap(),
    )
    .unwrap();
    path
}

fn target(id: &str, url: &str) -> serde_json::Value {
    serde_json::json!({ "id": id, "url": url, "secret": format!("s-{id}"), "timeout_ms": 2000, "dead_letter_after": 0 })
}

struct Fixture {
    dir: PathBuf,
    db: PathBuf,
}

impl Fixture {
    fn new(tag: &str) -> Fixture {
        let dir = unique_dir(tag);
        let db = dir.join("j.db");
        let out = run(&dir, &["init", "--db", db.to_str().unwrap(), "--program", PROGRAM]);
        assert_eq!(out.status.code(), Some(0), "{}", stderr_of(&out));
        Fixture { dir, db }
    }

    fn cfg_for(&self, name: &str, t: serde_json::Value) -> PathBuf {
        write_config(&self.dir, name, &[t])
    }

    fn push(&self, cfg: &Path) -> Output {
        run(
            &self.dir,
            &[
                "webhook",
                "push",
                "--db",
                self.db.to_str().unwrap(),
                "--config",
                cfg.to_str().unwrap(),
            ],
        )
    }
}

/// The recorded attempt status for the (only) target's first event.
fn first_attempt_status(db: &Path) -> Option<i64> {
    let js = r#"
const { DatabaseSync } = require('node:sqlite');
const d = new DatabaseSync(process.argv[1]);
const rows = d.prepare('SELECT status, error FROM webhook_attempts ORDER BY id').all();
console.log(JSON.stringify(rows));
"#;
    let out = Command::new("node")
        .args(["-e", js])
        .arg(db)
        .output()
        .expect("node reads attempts");
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    let v: serde_json::Value = serde_json::from_str(&text).ok()?;
    v.as_array()?.first().and_then(|r| r["status"].as_i64())
}

fn first_attempt_error(db: &Path) -> Option<String> {
    let js = r#"
const { DatabaseSync } = require('node:sqlite');
const d = new DatabaseSync(process.argv[1]);
const rows = d.prepare('SELECT status, error FROM webhook_attempts ORDER BY id').all();
console.log(JSON.stringify(rows));
"#;
    let out = Command::new("node")
        .args(["-e", js])
        .arg(db)
        .output()
        .expect("node reads attempts");
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    let v: serde_json::Value = serde_json::from_str(&text).ok()?;
    v.as_array()?
        .first()
        .and_then(|r| r["error"].as_str())
        .map(|s| s.to_string())
}

fn cursor_of(db: &Path) -> i64 {
    let js = r#"
const { DatabaseSync } = require('node:sqlite');
const d = new DatabaseSync(process.argv[1]);
const rows = d.prepare('SELECT next_seq FROM webhook_state').all();
console.log(JSON.stringify(rows));
"#;
    let out = Command::new("node")
        .args(["-e", js])
        .arg(db)
        .output()
        .expect("node reads state");
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    let v: serde_json::Value = serde_json::from_str(&text).expect("state json");
    v.as_array()
        .and_then(|a| a.first())
        .and_then(|r| r["next_seq"].as_i64())
        .unwrap_or(0)
}

fn state_row_exists(db: &Path) -> bool {
    let js = r#"
const { DatabaseSync } = require('node:sqlite');
const d = new DatabaseSync(process.argv[1]);
const rows = d.prepare('SELECT endpoint FROM webhook_state').all();
console.log(JSON.stringify(rows));
"#;
    let out = Command::new("node")
        .args(["-e", js])
        .arg(db)
        .output()
        .expect("node reads state");
    !String::from_utf8_lossy(&out.stdout).trim().is_empty()
}

fn journal_delivered_count(db: &Path) -> i64 {
    let js = r#"
const { DatabaseSync } = require('node:sqlite');
const d = new DatabaseSync(process.argv[1]);
const rows = d.prepare('SELECT COUNT(*) AS n FROM webhook_receipts').all();
console.log(JSON.stringify(rows));
"#;
    let out = Command::new("node")
        .args(["-e", js])
        .arg(db)
        .output()
        .expect("node reads receipts");
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    let v: serde_json::Value = serde_json::from_str(&text).expect("receipts json");
    v.as_array()
        .and_then(|a| a.first())
        .and_then(|r| r["n"].as_i64())
        .unwrap_or(0)
}

// ---------------------------------------------------------------------------
// D1: a 302 answer must not be recorded as a delivery. Measured on the
// pre-fix tree: attempt status 200 (the redirect target's answer), the
// cursor advanced past the seq, exit 0 — while the configured endpoint
// received exactly ONE request (the POST it redirected) and the second
// sink received the redirect hop.
// ---------------------------------------------------------------------------

#[test]
fn d1_a_302_answer_is_not_a_delivery() {
    let fx = Fixture::new("d1");
    let primary = Recorder::start();
    let hop = Recorder::start();
    primary.set_answer(302);
    primary.set_location(Some(hop.url_path("sink")));

    let cfg = fx.cfg_for("cfg.json", target("t1", &primary.url()));
    seed_events(&fx.db, 1);

    let out = fx.push(&cfg);
    assert_eq!(
        out.status.code(),
        Some(3),
        "a 3xx endpoint is an outage: the queue must hold the exit code\n{}",
        stderr_of(&out)
    );
    let rep = json_of(&out);
    let row = &rep["targets"][0];
    assert_eq!(
        row["delivered"].as_i64(),
        Some(0),
        "a redirect hop is not a delivery: {}",
        stdout_of(&out)
    );
    assert_eq!(journal_delivered_count(&fx.db), 0, "the journal must not hold a receipt for an event the configured endpoint never accepted");
    assert_eq!(
        first_attempt_status(&fx.db),
        Some(302),
        "the attempt must record the endpoint's OWN answer (302), not the redirect hop's: got {:?}",
        first_attempt_status(&fx.db)
    );
    assert_eq!(cursor_of(&fx.db), 0, "the cursor must stay behind the undelivered event");

    // The configured endpoint received exactly the one POST it refused.
    assert_eq!(primary.post_count(), 1, "exactly one request goes to the configured endpoint");
    // The redirect destination received NOTHING — the config never named it.
    assert_eq!(hop.hits().len(), 0, "no request may follow a redirect: the config did not name that url");
}

// ---------------------------------------------------------------------------
// D2: the redirect hop must not carry the signed request. Measured on the
// pre-fix tree: the hop received a GET with the signature header attached
// and zero body bytes — a signed delivery of nothing, to a host the
// operator never configured.
// ---------------------------------------------------------------------------

#[test]
fn d2_no_request_follows_the_redirect_not_even_a_bodyless_one() {
    let fx = Fixture::new("d2");
    let primary = Recorder::start();
    let hop = Recorder::start();
    // 303 See Other: ureq downgrades POST -> GET and DROPS the body while
    // keeping the custom signature headers — the exact leak face.
    primary.set_answer(303);
    primary.set_location(Some(hop.url_path("sink")));

    let cfg = fx.cfg_for("cfg.json", target("t1", &primary.url()));
    seed_events(&fx.db, 1);

    let out = fx.push(&cfg);
    assert_eq!(out.status.code(), Some(3), "{}", stderr_of(&out));
    assert_eq!(hop.hits().len(), 0, "a 303 must not leak a bodyless signed GET to the redirect target");
    assert_eq!(primary.post_count(), 1);
    assert_eq!(cursor_of(&fx.db), 0);
}

// ---------------------------------------------------------------------------
// D3: the report must NAME the redirect as the target's standing error, so
// the operator can repoint the url EXPLICITLY. Measured on the pre-fix
// tree: the report was the healthy shape (`delivered: 1`, no error) for an
// endpoint that redirected every delivery away.
// ---------------------------------------------------------------------------

#[test]
fn d3_the_report_names_the_redirect_as_the_block() {
    let fx = Fixture::new("d3");
    let primary = Recorder::start();
    let hop = Recorder::start();
    primary.set_answer(301);
    primary.set_location(Some(hop.url_path("sink")));

    let cfg = fx.cfg_for("cfg.json", target("t1", &primary.url()));
    seed_events(&fx.db, 1);
    let out = fx.push(&cfg);
    assert_eq!(out.status.code(), Some(3), "{}", stderr_of(&out));
    let rep = json_of(&out);
    let row = &rep["targets"][0];
    assert_eq!(row["blocked"].as_bool(), Some(true));
    let reason = row["error"].as_str().unwrap_or("");
    assert!(
        reason.contains("30"),
        "the block reason must name the redirect status, got: {reason}"
    );
    assert!(
        reason.contains("redirect") || reason.contains("3xx"),
        "the block reason must name the mechanism (redirect), got: {reason}"
    );

    // The same story on the polled surface, without a new delivery pass.
    let status = run(
        &fx.dir,
        &[
            "webhook",
            "status",
            "--db",
            fx.db.to_str().unwrap(),
            "--config",
            cfg.to_str().unwrap(),
        ],
    );
    let srep = json_of(&status);
    let srow = &srep["targets"][0];
    let last = srow["last_error"].as_str().unwrap_or("");
    assert!(
        last.contains("redirect") || last.contains("30"),
        "`webhook status` must name the redirect block, got: {last}"
    );
}

// ---------------------------------------------------------------------------
// D4: the configured url is the ONLY url bytes go to, per attempt. Even
// when the endpoint answers 302 pointing back at ITSELF (a loop), the
// binary must stop after the one request — measured on the pre-fix tree
// the loop was followed up to ureq's own 10-hop bound before the answer
// of the last hop was recorded (a loop of redirects is a live endpoint
// misbehaving; it is an outage, not something to walk into).
// ---------------------------------------------------------------------------

#[test]
fn d4_a_redirect_loop_gets_one_request_not_ten() {
    let fx = Fixture::new("d4");
    let primary = Recorder::start();
    primary.set_answer(302);
    primary.set_location(Some(primary.url_path("hook"))); // points at itself

    let cfg = fx.cfg_for("cfg.json", target("t1", &primary.url()));
    seed_events(&fx.db, 1);
    let out = fx.push(&cfg);
    assert_eq!(out.status.code(), Some(3), "{}", stderr_of(&out));
    // EVERY request counts, not only POSTs: a 302 downgrades the method to
    // GET, so a followed loop shows up as requests that are not POSTs.
    assert!(
        primary.hits().len() <= 1,
        "a redirect loop must receive ONE request per attempt, got {}: {:?}",
        primary.hits().len(),
        primary.hits()
    );
    assert_eq!(primary.post_count(), 1);
}

// ---------------------------------------------------------------------------
// G1: a plain 200 sink delivers normally — the rule must not turn the
// normal case into an outage.
// ---------------------------------------------------------------------------

#[test]
fn g1_a_200_sink_still_delivers_normally() {
    let fx = Fixture::new("g1");
    let primary = Recorder::start();
    let cfg = fx.cfg_for("cfg.json", target("t1", &primary.url()));
    seed_events(&fx.db, 2);
    let out = fx.push(&cfg);
    assert_eq!(out.status.code(), Some(0), "{}", stderr_of(&out));
    let rep = json_of(&out);
    assert_eq!(rep["targets"][0]["delivered"].as_i64(), Some(2));
    assert_eq!(primary.post_count(), 2);
    assert_eq!(cursor_of(&fx.db), 2);
    assert_eq!(journal_delivered_count(&fx.db), 2);
}

// ---------------------------------------------------------------------------
// G2: a 404 sink is still a REFUSED verdict (4xx classification), the
// queue blocks with the refusal named — unchanged.
// ---------------------------------------------------------------------------

#[test]
fn g2_a_404_sink_is_still_refused_and_named() {
    let fx = Fixture::new("g2");
    let primary = Recorder::start();
    primary.set_answer(404);
    let cfg = fx.cfg_for("cfg.json", target("t1", &primary.url()));
    seed_events(&fx.db, 1);
    let out = fx.push(&cfg);
    assert_eq!(out.status.code(), Some(3), "{}", stderr_of(&out));
    let rep = json_of(&out);
    let row = &rep["targets"][0];
    assert_eq!(row["error"].as_str(), Some("refused with http 404"));
    assert_eq!(first_attempt_status(&fx.db), Some(404));
    assert_eq!(primary.post_count(), 1);
    assert_eq!(cursor_of(&fx.db), 0);
}

// ---------------------------------------------------------------------------
// G3: a 500 sink is still a TRANSIENT outage (5xx classification) —
// unchanged, and the attempt records the status.
// ---------------------------------------------------------------------------

#[test]
fn g3_a_500_sink_is_still_transient_and_named() {
    let fx = Fixture::new("g3");
    let primary = Recorder::start();
    primary.set_answer(500);
    let cfg = fx.cfg_for("cfg.json", target("t1", &primary.url()));
    seed_events(&fx.db, 1);
    let out = fx.push(&cfg);
    assert_eq!(out.status.code(), Some(3), "{}", stderr_of(&out));
    let rep = json_of(&out);
    assert_eq!(rep["targets"][0]["error"].as_str(), Some("http 500"));
    // A transient attempt records status NULL by design (the error text is
    // the carry) — assert the attempt exists with that text, not a status.
    assert_eq!(
        first_attempt_error(&fx.db),
        Some("http 500".to_string()),
        "the attempt must carry the transient text"
    );
    assert_eq!(primary.post_count(), 1);
    assert_eq!(cursor_of(&fx.db), 0);
}

// ---------------------------------------------------------------------------
// G4: a 429 sink is still transient — the retry-worthy classes are the
// same as before; a redirect is ADDED to them, not swapped in.
// ---------------------------------------------------------------------------

#[test]
fn g4_a_429_sink_is_still_transient() {
    let fx = Fixture::new("g4");
    let primary = Recorder::start();
    primary.set_answer(429);
    let cfg = fx.cfg_for("cfg.json", target("t1", &primary.url()));
    seed_events(&fx.db, 1);
    let out = fx.push(&cfg);
    assert_eq!(out.status.code(), Some(3), "{}", stderr_of(&out));
    let rep = json_of(&out);
    assert_eq!(rep["targets"][0]["error"].as_str(), Some("http 429"));
    assert_eq!(
        first_attempt_error(&fx.db),
        Some("http 429".to_string()),
        "the attempt must carry the transient text"
    );
    assert_eq!(primary.post_count(), 1);
}
