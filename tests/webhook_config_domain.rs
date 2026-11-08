//! A webhook CONFIG value must be checked against the domain its consumer
//! can honour — the config file is the layer above the flag parser, and it
//! is exactly where the product's honest limitation ("configuration values
//! are unchecked") lived.
//!
//! Measured 2026-11-08 on the published v0.7.4 tree (HEAD ce8e4f1), against
//! a journal with a planted event and a receiver that accepts the connection
//! and never answers — the shape of a consumer that hung mid-delivery:
//!
//!   webhook push ... "timeout_ms": 18446744073709551615
//!     -> the process NEVER returned: killed at 25 s (the deadline is ~584
//!        million years out). The config key that exists precisely so "a
//!        webhook whose endpoint hangs must not hang the delivery pass" was
//!        itself able to hang the delivery pass.
//!   webhook push ... "timeout_ms": 9223372036854775808
//!     -> same class, same kill: `as_u64` accepts it, `Duration::from_millis`
//!        schedules it, and the deadline never arrives — a "timeout" no
//!        process will ever live to see.
//!   webhook push ... "timeout_ms": 3600001
//!     -> accepted today, waits an hour per attempt against a hung endpoint.
//!   webhook push ... "timeout_ms": 0
//!     -> ACCEPTED, and the poster silently uses `max(1)` = 1 ms: the config
//!        file says 0, the delivery behaves as 1 ms, and nothing says so.
//!        The silent-coercion defect v0.7.2/v0.7.3/v0.7.4 closed for flags
//!        and the cursor file, one layer over, in the config.
//!   webhook push ... "timeout_ms": 1.5 / -1 / "5000"
//!     -> refused, but with `timeout_ms must be a number` — the message does
//!        not name the offending VALUE, so `-1` and `1.5`, which ARE
//!        numbers, are told they are not. The refusal voice the rest of the
//!        family already fixed (name the value, name the reason).
//!
//! The rule this ladder enforces:
//!
//! * `timeout_ms` must be a whole number of milliseconds in 1..=MAX_TIMEOUT_MS;
//! * `0` is a refusal (it would silently become a 1 ms timeout), not a value
//!   the poster gets to re-interpret;
//! * a value above the ceiling is a refusal, not a deadline that never comes;
//!   the ceiling is policy and is documented (an HTTP delivery attempt that
//!   cannot finish within an hour is stuck, not slow);
//! * the refusal names the key AND the offending value, with no report
//!   printed and no connection attempted;
//!
//! The guards matter as much: the documented default (5000 ms) and a
//! configured value must still BOUND a stalled delivery (exit 3 with
//! `timeout: global`, promptly), `dead_letter_after: 1` must still
//! quarantine on the first refusal, `dead_letter_after: 0` must still
//! disable the valve, and a value at the ceiling must still be accepted. A
//! checker that refuses a working config is worse than the defect it fixes.

use std::io::{ErrorKind, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

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

/// A journal with a program and one planted event, so a delivery pass has
/// something to POST and a refusal can be told apart from "nothing to do".
fn with_event(tag: &str) -> PathBuf {
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
    conn.execute(
        "INSERT INTO events(signature, slot, first_seen, source, logs)
         VALUES(?1, ?2, ?3, 'ws', '[\"log\"]')",
        rusqlite::params![format!("Cfg{:043}", 1i64), 44_000_000i64, 1_700_000_000_000i64],
    )
    .expect("plant row");
    conn.close().expect("close journal");
    dir
}

/// A receiver that accepts the connection, reads whatever is sent and NEVER
/// answers, holding the socket open: the shape of a consumer that hung
/// mid-delivery. Holding matters — if the socket closed, the sender would
/// see a connection reset instead of waiting for its timeout.
fn stalling_server() -> (String, Arc<AtomicBool>, thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    listener.set_nonblocking(true).expect("nonblocking");
    let addr = listener.local_addr().expect("addr");
    let stop = Arc::new(AtomicBool::new(false));
    let stop2 = stop.clone();
    let handle = thread::spawn(move || {
        let mut held: Vec<TcpStream> = Vec::new();
        while !stop2.load(Ordering::Relaxed) {
            match listener.accept() {
                Ok((mut s, _)) => {
                    let _ = s.set_nonblocking(true);
                    let mut buf = [0u8; 4096];
                    let _ = s.read(&mut buf);
                    held.push(s);
                }
                Err(ref e) if e.kind() == ErrorKind::WouldBlock => {
                    thread::sleep(Duration::from_millis(20));
                }
                Err(_) => break,
            }
        }
        drop(held);
    });
    (format!("http://{addr}"), stop, handle)
}

/// A receiver that answers every request with 403 — a verdict, not an
/// outage. This is the input `dead_letter_after` counts.
fn refusing_server() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().expect("addr");
    thread::spawn(move || {
        for stream in listener.incoming() {
            match stream {
                Ok(mut s) => {
                    let mut buf = [0u8; 8192];
                    let _ = s.read(&mut buf);
                    let resp = b"HTTP/1.1 403 Forbidden\r\ncontent-type: application/json\r\ncontent-length: 11\r\n\r\n{\"ok\":false}";
                    let _ = s.write_all(resp);
                    let _ = s.flush();
                }
                Err(_) => break,
            }
        }
    });
    format!("http://{addr}")
}

/// Run `webhook push` with a hard deadline. The pre-fix tree does not fail
/// loudly for an out-of-domain timeout: it waits for a deadline that never
/// arrives. A test that waited for that would hang the suite, so the run is
/// killed and reported as a timeout — which is a failure, not a pass.
fn push(dir: &Path, cfg: &Path, secs: u64) -> Option<Output> {
    let mut child = Command::new(env!("CARGO_BIN_EXE_reeflog"))
        .args([
            "webhook",
            "push",
            "--db",
            "reeflog.db",
            "--config",
            cfg.to_str().expect("config path"),
        ])
        .current_dir(dir)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("reeflog runs");
    let deadline = Instant::now() + Duration::from_secs(secs);
    loop {
        match child.try_wait().expect("wait") {
            Some(_) => return Some(child.wait_with_output().expect("collect output")),
            None => {
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    return None;
                }
                thread::sleep(Duration::from_millis(25));
            }
        }
    }
}

fn stderr_of(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).to_lowercase()
}

fn stdout_of(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn report_of(out: &Output) -> Option<serde_json::Value> {
    serde_json::from_str(&stdout_of(out)).ok()
}

/// A config value outside the consumer's domain must be REFUSED: the run
/// finishes promptly with a non-zero exit, the key AND the offending value
/// named on stderr, and no report — and it must not WAIT for a deadline
/// nobody will live to see (on the pre-fix tree the timeout cases are
/// killed at the deadline instead of returning).
fn assert_config_refusal(tag: &str, extra: &str, value_token: &str) {
    let (base, stop, handle) = stalling_server();
    let dir = with_event(tag);
    let body = format!(
        r#"{{"targets":[{{"id":"t1","url":"{base}/hook","secret":"s",{extra}}}]}}"#
    );
    let cfg = dir.join("wh.json");
    std::fs::write(&cfg, body).expect("write config");
    let out = push(&dir, &cfg, 12);
    stop.store(true, Ordering::Relaxed);
    let _ = handle.join();
    let out = match out {
        Some(o) => o,
        None => panic!(
            "config value {value_token} must be REFUSED promptly, but the run had to be \
             killed after 12 s — waiting for a deadline that never comes is the defect \
             this ladder closes"
        ),
    };
    assert!(
        !out.status.success(),
        "config value {value_token} must not run (stdout: {})",
        stdout_of(&out)
    );
    assert!(
        report_of(&out).is_none(),
        "a refused config must print no report (stdout: {})",
        stdout_of(&out)
    );
    let err = stderr_of(&out);
    assert!(
        err.contains("timeout_ms") || err.contains("dead_letter_after"),
        "the refusal must name the key, got: {err}"
    );
    assert!(
        err.contains(&value_token.to_lowercase()),
        "the refusal must name the offending value {value_token}, got: {err}"
    );
}

// ---------------------------------------------------------------- refusals

/// `timeout_ms: 0` is not a timeout: the poster would silently turn it into
/// 1 ms and every delivery would fail as a timeout. Refuse it, do not
/// re-interpret it.
#[test]
fn config_timeout_zero_is_refused() {
    assert_config_refusal("wh-zero", r#""timeout_ms": 0"#, "0");
}

/// A timeout no process will ever live to see must be refused, not waited
/// for. On the pre-fix tree this case HAD TO BE KILLED — that is the defect.
#[test]
fn config_timeout_out_of_domain_is_refused_not_hung() {
    assert_config_refusal(
        "wh-oomax",
        r#""timeout_ms": 18446744073709551615"#,
        "18446744073709551615",
    );
}

/// One above i64, still a legal u64: the same class, the same refusal.
#[test]
fn config_timeout_i64_max_plus_one_is_refused() {
    assert_config_refusal("wh-i64p", r#""timeout_ms": 9223372036854775808"#, "9223372036854775808");
}

/// Just above the documented ceiling: policy, and it must be stated as a
/// refusal rather than accepted into a deadline that will not come.
#[test]
fn config_timeout_above_ceiling_is_refused() {
    assert_config_refusal("wh-above", r#""timeout_ms": 3600001"#, "3600001");
}

/// The refusal must name the VALUE. `1.5` and `-1` are numbers; telling the
/// operator "must be a number" without echoing what they wrote leaves them
/// guessing which value was wrong.
#[test]
fn config_timeout_refusal_names_the_value() {
    assert_config_refusal("wh-frac", r#""timeout_ms": 1.5"#, "1.5");
    assert_config_refusal("wh-neg", r#""timeout_ms": -1"#, "-1");
    assert_config_refusal("wh-str", r#""timeout_ms": "5000""#, "5000");
    assert_config_refusal(
        "wh-big",
        r#""timeout_ms": 18446744073709551616"#,
        "18446744073709551616",
    );
}

/// A `dead_letter_after` outside the i64 the refusal count is compared
/// against, or negative, must be refused with the value named. `0` stays
/// legal (documented: it disables the valve) and is covered by the guards.
#[test]
fn config_dead_letter_out_of_domain_is_refused() {
    assert_config_refusal(
        "wh-dlq",
        r#""dead_letter_after": 9223372036854775808"#,
        "9223372036854775808",
    );
    assert_config_refusal("wh-dlqneg", r#""dead_letter_after": -1"#, "-1");
    assert_config_refusal("wh-dlqfrac", r#""dead_letter_after": 1.5"#, "1.5");
}

// ----------------------------------------------------------------- guards

/// The documented default must still BOUND a stalled delivery: the run
/// finishes on its own with `timeout: global`, promptly, exit 3.
#[test]
fn guard_default_timeout_bounds_a_stalled_delivery() {
    let (base, stop, handle) = stalling_server();
    let dir = with_event("wh-gdef");
    let body = format!(r#"{{"targets":[{{"id":"t1","url":"{base}/hook","secret":"s"}}]}}"#);
    let cfg = dir.join("wh.json");
    std::fs::write(&cfg, body).expect("config");
    let out = push(&dir, &cfg, 15);
    stop.store(true, Ordering::Relaxed);
    let _ = handle.join();
    let out = out.expect("the default timeout must fire");
    assert_eq!(
        out.status.code(),
        Some(3),
        "a stalled delivery blocks the queue (stderr: {})",
        String::from_utf8_lossy(&out.stderr)
    );
    let rep = report_of(&out).expect("report");
    let t = &rep["targets"][0];
    assert_eq!(t["delivered"], 0, "nothing was delivered");
    assert!(
        t["error"].as_str().unwrap_or("").contains("timeout"),
        "the report must say it was a timeout, got: {}",
        t["error"]
    );
}

/// A configured timeout must be honoured as written — 1000 ms, not the
/// default, and not silently re-interpreted.
#[test]
fn guard_configured_timeout_bounds_a_stalled_delivery() {
    let (base, stop, handle) = stalling_server();
    let dir = with_event("wh-gcfg");
    let body = format!(
        r#"{{"targets":[{{"id":"t1","url":"{base}/hook","secret":"s","timeout_ms":1000}}]}}"#
    );
    let cfg = dir.join("wh.json");
    std::fs::write(&cfg, body).expect("config");
    let t0 = Instant::now();
    let out = push(&dir, &cfg, 15);
    let elapsed = t0.elapsed();
    stop.store(true, Ordering::Relaxed);
    let _ = handle.join();
    let out = out.expect("the configured timeout must fire");
    assert_eq!(
        out.status.code(),
        Some(3),
        "a stalled delivery blocks the queue (stderr: {})",
        String::from_utf8_lossy(&out.stderr)
    );
    let rep = report_of(&out).expect("report");
    assert!(
        rep["targets"][0]["error"]
            .as_str()
            .unwrap_or("")
            .contains("timeout"),
        "got: {}",
        rep["targets"][0]["error"]
    );
    assert!(
        elapsed < Duration::from_secs(9),
        "timeout_ms 1000 must bound the wait near 1 s, took {elapsed:?}"
    );
}

/// A value AT the ceiling is still accepted: the checker must not refuse a
/// config the product documents as valid. The receiver drops the connection
/// immediately so the pass completes at once and "accepted" is observable
/// as a report instead of a refusal.
#[test]
fn guard_ceiling_value_is_accepted() {
    let dir = with_event("wh-ceil");
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().expect("addr");
    thread::spawn(move || {
        for stream in listener.incoming() {
            drop(stream);
        }
    });
    let body = format!(
        r#"{{"targets":[{{"id":"t1","url":"http://{addr}/hook","secret":"s","timeout_ms":3600000}}]}}"#
    );
    let cfg = dir.join("wh.json");
    std::fs::write(&cfg, body).expect("config");
    let out = push(&dir, &cfg, 12).expect("the run must finish");
    assert_eq!(
        out.status.code(),
        Some(3),
        "timeout_ms at the ceiling is a valid config; the delivery fails for its own \
         reason (stdout: {}, stderr: {})",
        stdout_of(&out),
        String::from_utf8_lossy(&out.stderr)
    );
    let rep = report_of(&out).expect("a valid config still reports");
    assert_eq!(rep["targets"][0]["target"], "t1");
}

/// `dead_letter_after: 1` must quarantine on the first refusal — the valve
/// the config controls must still work after the checker landed.
#[test]
fn guard_dead_letter_one_quarantines() {
    let base = refusing_server();
    let dir = with_event("wh-dlq1");
    let body = format!(
        r#"{{"targets":[{{"id":"t1","url":"{base}/hook","secret":"s","dead_letter_after":1}}]}}"#
    );
    let cfg = dir.join("wh.json");
    std::fs::write(&cfg, body).expect("config");
    let out = push(&dir, &cfg, 12).expect("the run must finish");
    let rep = report_of(&out).expect("report");
    assert_eq!(
        rep["targets"][0]["dead_lettered"], 1,
        "one refusal with dead_letter_after 1 must quarantine (got: {rep})"
    );
}

/// `dead_letter_after: 0` is documented as "disables it" and must stay
/// legal: the queue blocks instead of quarantining.
#[test]
fn guard_dead_letter_zero_disables() {
    let base = refusing_server();
    let dir = with_event("wh-dlq0");
    let body = format!(
        r#"{{"targets":[{{"id":"t1","url":"{base}/hook","secret":"s","dead_letter_after":0}}]}}"#
    );
    let cfg = dir.join("wh.json");
    std::fs::write(&cfg, body).expect("config");
    let out = push(&dir, &cfg, 12).expect("the run must finish");
    assert_eq!(
        out.status.code(),
        Some(3),
        "a refusal blocks the queue (stderr: {})",
        String::from_utf8_lossy(&out.stderr)
    );
    let rep = report_of(&out).expect("report");
    assert_eq!(
        rep["targets"][0]["dead_lettered"], 0,
        "dead_letter_after 0 disables the valve (got: {rep})"
    );
    assert_eq!(rep["targets"][0]["blocked"], true, "the refusal must block");
}
