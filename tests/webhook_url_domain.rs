//! The webhook CONFIG's `url` must be judged by the domain its CONSUMER can
//! deliver to — not by a string prefix.
//!
//! Measured 2026-11-09 on the published v0.7.7 tree (HEAD 9d534e2, the tag a
//! stranger installs): `load_targets` decided the whole URL question with
//! `url.starts_with("http://") || url.starts_with("https://")`. That is the
//! FORM of a URL, and the form and the meaning disagree in BOTH directions —
//! the same two-faced defect the flag parser closed one layer down (v0.7.6,
//! `tests/url_flag_domain.rs`), left standing one layer up:
//!
//!   `"url": "HTTP://127.0.0.1:8794/hook"`
//!     -> REFUSED: `target #0: "url" must be http:// or https://`. But the
//!        consumer that posts to this URL ACCEPTS it: measured with `ureq`
//!        3.5.0 (the poster's own transport), `HTTP://…` and `HtTp://…`
//!        both answer HTTP 200 from a live receiver, because ureq lowercases
//!        the scheme before resolving it. The checker refused a value the
//!        product can deliver to — and the message says "must be http://"
//!        about a URL that IS http://.
//!
//!   `"url": "http://"`
//!     -> ACCEPTED (exit 0, report printed, queue configured), and then every
//!        delivery dies in the transport with ureq's own words:
//!        `http: empty string`. The target is blocked from then on. The flag
//!        layer refuses exactly this value ("expects a URL with a host") —
//!        the config layer waved it through.
//!
//!   `"url": "http:///x"`  -> same class: accepted, `http: invalid format`.
//!   `"url": "https://"`   -> accepted, `http: empty string`.
//!
//! THE RULE (src/flags.rs, ONE implementation for both layers —
//! `check_url_domain`): an absolute URL, a scheme the consumer speaks,
//! compared case-insensitively because the consumer does, plus a non-empty
//! authority. The refusal names WHO is complaining (`target #i url`), the
//! offending value, the expected domain and an example — before any
//! connection is attempted.
//!
//! The guards are what stop this from being a stricter checker instead of a
//! correct one: a plain lowercase URL with path/port/query must still load,
//! an uppercase one must now load AND deliver, and the refusals that already
//! worked (`ftp://…`, a typo'd key, a duplicate id) must not move.

use std::io::{ErrorKind, Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
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

fn run(dir: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_reeflog"))
        .args(args)
        .current_dir(dir)
        .output()
        .expect("reeflog binary runs")
}

/// A journal for `webhook status`/`push`, created the documented way.
fn seed(dir: &Path) -> PathBuf {
    let db = dir.join("reeflog.db");
    let out = run(dir, &["init", "--db", db.to_str().unwrap(), "--program", VALID_PROGRAM]);
    assert!(out.status.success(), "init must succeed");
    db
}

/// Write a config with ONE target whose url is the given value, as raw JSON.
fn config(dir: &Path, url: &str) -> PathBuf {
    let p = dir.join("wh.json");
    let body = format!(
        r#"{{"targets":[{{"id":"t1","url":"{url}","secret":"s","timeout_ms":2000}}]}}"#
    );
    std::fs::write(&p, body).expect("config written");
    p
}

fn status(dir: &Path, cfg: &Path, db: &Path) -> Output {
    run(
        dir,
        &[
            "webhook",
            "status",
            "--db",
            db.to_str().unwrap(),
            "--config",
            cfg.to_str().unwrap(),
        ],
    )
}

/// A config the loader accepts must be REFUSED with: non-zero exit, the
/// target and the `url` key named, the offending VALUE echoed, no report on
/// stdout (a refused config must not half-configure anything), and no
/// connection attempted.
fn assert_config_refusal(out: &Output, url: &str) {
    let err = String::from_utf8_lossy(&out.stderr);
    let low = err.to_lowercase();
    assert!(
        !out.status.success(),
        "url `{url}` must be refused (stdout: {})",
        String::from_utf8_lossy(&out.stdout)
    );
    assert!(
        low.contains("url"),
        "the refusal must name the `url` key, got: {err}"
    );
    assert!(
        low.contains(&url.to_lowercase()),
        "the refusal must name the offending value `{url}`, got: {err}"
    );
    // No report: a refused config must not be half-applied.
    assert!(
        String::from_utf8_lossy(&out.stdout).trim().is_empty(),
        "a refused config must print no report, got: {}",
        String::from_utf8_lossy(&out.stdout)
    );
    // The transport's own words mean the config loader let the value through.
    assert!(
        !low.contains("http: empty string") && !low.contains("invalid format"),
        "the value reached the transport — the loader refused nothing: {err}"
    );
}

/// The WEAKER guard form: the value must be refused, but the message is not
/// judged. Used for values that were already refused before the fix — a
/// guard must be green on BOTH trees, otherwise it is a defect case wearing
/// a guard's name.
fn assert_config_refused_only(out: &Output, url: &str) {
    assert!(
        !out.status.success(),
        "url `{url}` must be refused (stdout: {})",
        String::from_utf8_lossy(&out.stdout)
    );
}

// --- the defect: the prefix test accepted what the consumer cannot use ------

#[test]
fn a_scheme_with_no_host_is_refused_by_the_config() {
    // v0.7.7: accepted (exit 0, report), then `http: empty string` on every
    // delivery — a target that can never deliver, configured successfully.
    let dir = unique_dir("wh-url-nohost");
    let db = seed(&dir);
    let cfg = config(&dir, "http://");
    let out = status(&dir, &cfg, &db);
    assert_config_refusal(&out, "http://");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn an_https_scheme_with_no_host_is_refused_by_the_config() {
    let dir = unique_dir("wh-url-nohost-https");
    let db = seed(&dir);
    let cfg = config(&dir, "https://");
    let out = status(&dir, &cfg, &db);
    assert_config_refusal(&out, "https://");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn an_empty_authority_is_refused_by_the_config() {
    // `http:///x` starts with "http://" — the prefix test could not tell the
    // empty authority from a real one. The consumer calls it
    // "http: invalid format".
    let dir = unique_dir("wh-url-empty-auth");
    let db = seed(&dir);
    let cfg = config(&dir, "http:///x");
    let out = status(&dir, &cfg, &db);
    assert_config_refusal(&out, "http:///x");
    let _ = std::fs::remove_dir_all(&dir);
}

// --- the defect: the prefix test refused what the consumer CAN use ----------

#[test]
fn an_uppercase_scheme_is_accepted_because_the_consumer_speaks_it() {
    // v0.7.7: REFUSED with `must be http:// or https://` — about a URL that
    // IS http://. The consumer (ureq) lowercases the scheme: measured, it
    // answers 200 to `HTTP://` and `HtTp://`.
    let dir = unique_dir("wh-url-upper");
    let db = seed(&dir);
    let cfg = config(&dir, "HTTP://127.0.0.1:9/hook");
    let out = status(&dir, &cfg, &db);
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "an uppercase scheme is inside the consumer's domain — it must load: {err}"
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("t1"),
        "the target must appear in the report, got: {stdout}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

// --- the guards: everything that worked must keep working -------------------

#[test]
fn a_plain_lowercase_url_still_loads() {
    let dir = unique_dir("wh-url-guard");
    let db = seed(&dir);
    let cfg = config(&dir, "http://127.0.0.1:9/hook");
    let out = status(&dir, &cfg, &db);
    assert!(
        out.status.success(),
        "a documented url must still load: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_non_http_scheme_is_still_refused() {
    // A GUARD: `ftp://` was refused before the fix and must stay refused.
    // Only the MESSAGE changed (it now names the value and the domain).
    let dir = unique_dir("wh-url-ftp");
    let db = seed(&dir);
    let cfg = config(&dir, "ftp://x/y");
    let out = status(&dir, &cfg, &db);
    assert_config_refused_only(&out, "ftp://x/y");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn an_unknown_config_key_is_still_refused() {
    let dir = unique_dir("wh-url-typo");
    let db = seed(&dir);
    let cfg = dir.join("wh.json");
    std::fs::write(
        &cfg,
        r#"{"targets":[{"id":"t1","url":"http://x/y","secret":"s","timeout":5}]}"#,
    )
    .unwrap();
    let out = status(&dir, &cfg, &db);
    assert!(!out.status.success(), "a typo'd key must be refused");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_duplicate_target_id_is_still_refused() {
    let dir = unique_dir("wh-url-dupe");
    let db = seed(&dir);
    let cfg = dir.join("wh.json");
    std::fs::write(
        &cfg,
        r#"{"targets":[{"id":"a","url":"http://x/y","secret":"s"},{"id":"a","url":"http://x/z","secret":"s"}]}"#,
    )
    .unwrap();
    let out = status(&dir, &cfg, &db);
    assert!(!out.status.success(), "a duplicate id must be refused");
    let _ = std::fs::remove_dir_all(&dir);
}

// --- the end that matters: the refused/accepted boundary is the CONSUMER ----

/// A receiver that answers 200 to every POST. The listener is closed when
/// `alive` goes false — the thread must not keep the test binary alive.
fn accepting_receiver(alive: Arc<AtomicBool>) -> (u16, thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().expect("addr").port();
    let handle = thread::spawn(move || {
        listener
            .set_nonblocking(true)
            .expect("non-blocking listener");
        while alive.load(Ordering::SeqCst) {
            match listener.accept() {
                Ok((mut s, _)) => {
                    thread::spawn(move || {
                        let mut buf = [0u8; 4096];
                        loop {
                            match s.read(&mut buf) {
                                Ok(0) | Err(_) => break,
                                Ok(n) => {
                                    let head = String::from_utf8_lossy(&buf[..n]).to_lowercase();
                                    if head.starts_with("post") {
                                        let _ = s.write_all(
                                            b"HTTP/1.1 200 OK\r\ncontent-length:2\r\nconnection:close\r\n\r\n{}",
                                        );
                                        break;
                                    }
                                }
                            }
                        }
                    });
                }
                Err(ref e) if e.kind() == ErrorKind::WouldBlock => {
                    thread::sleep(Duration::from_millis(10));
                }
                Err(_) => break,
            }
        }
    });
    (port, handle)
}

#[test]
fn an_uppercase_scheme_target_actually_delivers() {
    // The boundary is the consumer, not the checker: with the receiver
    // running, the config that v0.7.7 REFUSED must now load and the event
    // must actually be delivered (exit 0, delivered 1, not blocked).
    let alive = Arc::new(AtomicBool::new(true));
    let (port, rx) = accepting_receiver(Arc::clone(&alive));
    let dir = unique_dir("wh-url-upper-deliver");
    let db = seed(&dir);

    // one event to deliver
    let status_out = run(&dir, &["status", "--db", db.to_str().unwrap()]);
    assert!(status_out.status.success(), "journal is healthy");
    let conn = rusqlite_open(&db);
    conn.execute(
        "INSERT OR IGNORE INTO events (signature, slot, err, logs, source, first_seen, seal_status)
         VALUES ('URLD0000000000000000000000000000000000000000000000000000000000001', 44000000, NULL, '[]', 'ws', 1700000000000, 'sealed')",
        [],
    )
    .expect("seed event");
    drop(conn);

    let cfg = config(&dir, &format!("HTTP://127.0.0.1:{port}/hook"));
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
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "the uppercase target must deliver: {} / {stdout}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        stdout.contains("\"delivered\": 1"),
        "expected the event delivered, got: {stdout}"
    );
    assert!(
        stdout.contains("\"blocked\": false"),
        "the queue must not be blocked by a deliverable url, got: {stdout}"
    );

    alive.store(false, Ordering::SeqCst);
    let _ = std::fs::remove_dir_all(&dir);
    let _ = rx.join();
}

/// rusqlite is a dependency of the crate, so the test can open the journal
/// directly instead of inventing events through the network.
fn rusqlite_open(db: &Path) -> rusqlite::Connection {
    rusqlite::Connection::open(db).expect("open journal")
}

#[test]
fn a_no_host_target_is_refused_before_any_delivery_can_be_attempted() {
    // The other direction, end to end: `http://` used to configure a target
    // that fails `http: empty string` on its first delivery. Now the loader
    // refuses it, so no queue is ever configured and no delivery runs.
    let dir = unique_dir("wh-url-nohost-e2e");
    let db = seed(&dir);
    let cfg = config(&dir, "http://");
    let t0 = Instant::now();
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
    assert_config_refusal(&out, "http://");
    assert!(
        t0.elapsed() < Duration::from_secs(10),
        "a refusal must not wait on a delivery: {:?}",
        t0.elapsed()
    );
    let _ = std::fs::remove_dir_all(&dir);
}

// the daemon's own config path (`start --webhook-config`) shares the loader

#[test]
fn the_daemon_refuses_a_no_host_webhook_config_before_starting() {
    let dir = unique_dir("wh-url-start");
    let db = seed(&dir);
    let cfg = config(&dir, "http://");
    let out = run(
        &dir,
        &[
            "start",
            "--db",
            db.to_str().unwrap(),
            "--seconds",
            "1",
            "--webhook-config",
            cfg.to_str().unwrap(),
        ],
    );
    assert_config_refusal(&out, "http://");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn the_daemon_accepts_an_uppercase_webhook_config() {
    let dir = unique_dir("wh-url-start-upper");
    let db = seed(&dir);
    // unreachable port, but the CONFIG must load; the run may then fail for
    // transport reasons (exit 3) — what must not happen is a config refusal.
    let cfg = config(&dir, "HTTP://127.0.0.1:9/hook");
    let out = run(
        &dir,
        &[
            "start",
            "--db",
            db.to_str().unwrap(),
            "--seconds",
            "1",
            "--webhook-config",
            cfg.to_str().unwrap(),
        ],
    );
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        !err.to_lowercase().contains("must be http"),
        "the config layer must not refuse a url the consumer speaks: {err}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}
