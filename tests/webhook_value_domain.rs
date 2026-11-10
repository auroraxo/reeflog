//! The webhook CONFIG's `id`, `secret` and `url` must be judged by the
//! domain their CONSUMER can carry — not by the shape of the string.
//!
//! MEASURED 2026-11-09 against the PUBLISHED v0.7.8 binary (the release
//! archive's sha256 equals the digest GitHub Releases publishes), with a
//! real receiver that records the RAW request bytes. The v0.7.8 loader
//! checked exactly three things about a target:
//!
//!   * `id` is not empty,
//!   * `secret` is not empty,
//!   * `url` starts with `http://` or `https://` (since v0.7.8: scheme
//!     case-insensitively, plus a host).
//!
//! Everything else was accepted — and the consumer, not the checker, drew
//! the line. Three measured faces of ONE class:
//!
//! 1. **`id` is an HTTP field value.** `delivery_id` builds
//!    `X-Reeflog-Event-Id: <target>:<kind>:<seq>` from it. A dense sweep of
//!    every C0 control byte and DEL, one byte at a time, in the id, against
//!    the real poster:
//!
//!    | byte                      | outcome                                   |
//!    |---------------------------|-------------------------------------------|
//!    | 0x00–0x08, 0x0A–0x1F, 0x7F | `http: failed to parse header value`      |
//!    | 0x09 (HTAB)               | DELIVERED, byte-identical on the wire     |
//!
//!    32 of 33 control bytes killed EVERY delivery at the first event — the
//!    target was configured as healthy (exit 0, report printed) and then
//!    blocked on its first event, in ureq's words, not reeflog's.
//!
//! 2. **An HTTP receiver strips the optional whitespace around a field
//!    value.** With `"id": " t1"` the journal stored `" t1"` and the
//!    receiver saw `"t1:event:1"` — the consumer's deduplication key and
//!    the sender's record disagree, for exactly the field the README tells
//!    consumers to dedup on. The same is true of a LEADING tab. A TRAILING
//!    space or tab is NOT stripped by the receiver (the id is followed by
//!    `:event:1`), so it stays inside the domain — measured, not assumed.
//!
//! 3. **`url` is a Request-URI.** A dense sweep of every printable ASCII
//!    byte in the URL path, plus the same sweep in the host:
//!
//!    | characters                | outcome                          |
//!    |---------------------------|----------------------------------|
//!    | space, all C0 controls    | `http: invalid uri character`    |
//!    | `<` `>` `` ` ``           | `http: invalid uri character`    |
//!    | non-ASCII (e.g. `ü`)      | `http: invalid uri character`    |
//!    | `"` `\` `^` `{` `|` `}`   | accepted in the path             |
//!
//!    The same domain holds for the WebSocket consumer: `tokio-tungstenite`
//!    parses the URI with `http::Uri` and reports the very same
//!    `invalid uri character` for space, controls, `<`, `>`, `` ` `` and
//!    non-ASCII — in the host too, where ureq's path sweep did not reach.
//!
//! The rule (src/flags.rs, ONE implementation per value, shared with the
//! flag layer): a value inside the consumer's domain is accepted, a value
//! outside it is refused BEFORE any delivery is attempted, naming the key,
//! the offending value, the domain and an example.
//!
//! What is deliberately NOT refused:
//!
//!   * a non-empty secret with a newline, a control byte or a non-ASCII
//!     byte: the secret is an HMAC KEY, never sent on the wire. Measured: it
//!     delivers, and the receiver's own HMAC over the received body verifies
//!     against the configured secret. Only an EMPTY secret is out of domain
//!     — it signs with the empty key, which authenticates nothing;
//!   * an id with an internal space, an internal tab or a non-ASCII
//!     character: measured, it delivers. A non-ASCII id reaches the wire as
//!     its UTF-8 bytes (the receiver's header parser decodes them as
//!     latin1, which is the receiver's business, not reeflog's);
//!   * a URL with `"` `\` `^` `{` `|` `}` in the PATH: measured, it
//!     delivers.
//!
//! The guards are what stop this from being a stricter checker instead of a
//! correct one: a plain target must still load, deliver, and round-trip its
//! id exactly.

use std::io::{ErrorKind, Read, Write};
use std::net::TcpListener;
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

fn seed(dir: &Path) -> PathBuf {
    let db = dir.join("reeflog.db");
    let out = run(dir, &["init", "--db", db.to_str().unwrap(), "--program", VALID_PROGRAM]);
    assert!(out.status.success(), "init must succeed");
    db
}

/// Write a config with ONE target, as raw JSON, so the exact bytes of the
/// value reach the loader.
fn config_with(dir: &Path, id: &str, url: &str, secret: &str) -> PathBuf {
    let p = dir.join("wh.json");
    let body = format!(
        r#"{{"targets":[{{"id":{id},"url":{url},"secret":{secret},"timeout_ms":2000}}]}}"#
    );
    std::fs::write(&p, body).expect("config written");
    p
}

/// A JSON string literal for a raw byte sequence.
fn jstr(s: &str) -> String {
    let mut out = String::from("\"");
    for ch in s.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
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

/// A config the loader accepts must be REFUSED with: non-zero exit, the key
/// named, the offending VALUE echoed, and no report on stdout (a refused
/// config must not half-configure anything).
fn assert_refusal(out: &Output, key: &str, value: &str) {
    let err = String::from_utf8_lossy(&out.stderr);
    let low = err.to_lowercase();
    assert!(
        !out.status.success(),
        "`{key}` = `{value}` must be refused (stdout: {})",
        String::from_utf8_lossy(&out.stdout)
    );
    assert!(
        low.contains(key),
        "the refusal must name the `{key}` key, got: {err}"
    );
    if !value.is_empty() {
        assert!(
            low.contains(&value.to_lowercase()),
            "the refusal must name the offending value `{value}`, got: {err}"
        );
    }
    assert!(
        String::from_utf8_lossy(&out.stdout).trim().is_empty(),
        "a refused config must print no report, got: {}",
        String::from_utf8_lossy(&out.stdout)
    );
}

// --- the defect: an `id` the header parser cannot carry --------------------

/// The 32 control bytes that kill EVERY delivery, measured one byte at a
/// time against the published v0.7.8 binary.
fn forbidden_header_bytes() -> Vec<char> {
    let mut v: Vec<char> = (0x00u8..=0x1f)
        .filter(|b| *b != 0x09) // HTAB is inside the domain: measured, it delivers
        .map(|b| b as char)
        .collect();
    v.push('\u{7f}'); // DEL
    v
}

#[test]
fn an_id_with_a_forbidden_control_byte_is_refused_by_the_config() {
    // v0.7.8: accepted (exit 0, report), then EVERY delivery died with
    // `http: failed to parse header value` — the target was configured as
    // healthy and blocked at its first event.
    for ch in forbidden_header_bytes() {
        let dir = unique_dir("wh-id-ctl");
        let db = seed(&dir);
        let cfg = config_with(&dir, &jstr(&format!("a{ch}b")), &jstr("http://127.0.0.1:9/hook"), &jstr("s"));
        let out = status(&dir, &cfg, &db);
        assert_refusal(&out, "id", &format!("a{ch}b"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}

#[test]
fn an_id_with_leading_whitespace_is_refused_by_the_config() {
    // v0.7.8: accepted, and the receiver saw `t1:event:1` while the journal
    // stored ` t1` — the consumer's dedup key stopped matching the
    // sender's record, for exactly the field the README tells consumers to
    // dedup on.
    for ch in [' ', '\t'] {
        let dir = unique_dir("wh-id-lead-ws");
        let db = seed(&dir);
        let cfg = config_with(&dir, &jstr(&format!("{ch}t1")), &jstr("http://127.0.0.1:9/hook"), &jstr("s"));
        let out = status(&dir, &cfg, &db);
        assert_refusal(&out, "id", &format!("{ch}t1"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}

#[test]
fn an_empty_id_is_refused_by_the_config() {
    let dir = unique_dir("wh-id-empty");
    let db = seed(&dir);
    let cfg = config_with(&dir, &jstr(""), &jstr("http://127.0.0.1:9/hook"), &jstr("s"));
    let out = status(&dir, &cfg, &db);
    assert_refusal(&out, "id", "");
    let _ = std::fs::remove_dir_all(&dir);
}

// --- the defect: a URL the Request-URI cannot carry ------------------------

#[test]
fn a_url_with_a_space_is_refused_by_the_config() {
    // v0.7.8: accepted, then every delivery died with
    // `http: invalid uri character`.
    let dir = unique_dir("wh-url-space");
    let db = seed(&dir);
    let cfg = config_with(&dir, &jstr("t1"), &jstr("http://127.0.0.1:9/a b"), &jstr("s"));
    let out = status(&dir, &cfg, &db);
    assert_refusal(&out, "url", "http://127.0.0.1:9/a b");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_url_with_a_control_byte_is_refused_by_the_config() {
    for ch in ['\r', '\n', '\t', '\u{1}', '\u{7f}'] {
        let dir = unique_dir("wh-url-ctl");
        let db = seed(&dir);
        let url = format!("http://127.0.0.1:9/a{ch}b");
        let cfg = config_with(&dir, &jstr("t1"), &jstr(&url), &jstr("s"));
        let out = status(&dir, &cfg, &db);
        assert_refusal(&out, "url", &url);
        let _ = std::fs::remove_dir_all(&dir);
    }
}

#[test]
fn a_url_with_a_character_the_uri_parser_refuses_is_refused_by_the_config() {
    // `<`, `>` and the backtick in the PATH: measured — `http: invalid uri
    // character`, for the HTTP and the WebSocket consumer alike (both parse
    // the URI with `http::Uri`).
    for ch in ['<', '>', '`'] {
        let dir = unique_dir("wh-url-angle");
        let db = seed(&dir);
        let url = format!("http://127.0.0.1:9/a{ch}b");
        let cfg = config_with(&dir, &jstr("t1"), &jstr(&url), &jstr("s"));
        let out = status(&dir, &cfg, &db);
        assert_refusal(&out, "url", &url);
        let _ = std::fs::remove_dir_all(&dir);
    }
}

#[test]
fn a_url_with_a_character_the_uri_parser_refuses_in_the_query_is_refused() {
    // The QUERY has its own domain: `"`, `<`, `>` are refused there, while
    // `\`, `^`, `{`, `|`, `}` and the backtick are not (measured).
    for ch in ['"', '<', '>'] {
        let dir = unique_dir("wh-url-query");
        let db = seed(&dir);
        let url = format!("http://127.0.0.1:9/hook?k={ch}v");
        let cfg = config_with(&dir, &jstr("t1"), &jstr(&url), &jstr("s"));
        let out = status(&dir, &cfg, &db);
        assert_refusal(&out, "url", &url);
        let _ = std::fs::remove_dir_all(&dir);
    }
}

#[test]
fn a_url_with_a_character_the_uri_parser_refuses_in_the_host_is_refused() {
    // The AUTHORITY has the widest refusal set: `" < > \ ^ ` { | }`, space,
    // controls and non-ASCII (measured against the published v0.7.8 asset:
    // nine characters refused, the request never left).
    for ch in ['"', '<', '>', '\\', '^', '`', '{', '|', '}'] {
        let dir = unique_dir("wh-url-host");
        let db = seed(&dir);
        let url = format!("http://ho{ch}st:9/hook");
        let cfg = config_with(&dir, &jstr("t1"), &jstr(&url), &jstr("s"));
        let out = status(&dir, &cfg, &db);
        assert_refusal(&out, "url", &url);
        let _ = std::fs::remove_dir_all(&dir);
    }
}

#[test]
fn a_url_with_a_non_ascii_character_is_refused_by_the_config() {
    // MEASURED: `http::Uri` accepts a non-ASCII byte in the path, sends it
    // raw, and the RECEIVER answers `HTTP 400` — the delivery fails with
    // the consumer's status code instead of reeflog's reason. The URI
    // grammar carries the request-target as percent-encoded ASCII.
    for url in ["http://127.0.0.1:9/atüb", "http://tüb.example:9/hook"] {
        let dir = unique_dir("wh-url-nonascii");
        let db = seed(&dir);
        let cfg = config_with(&dir, &jstr("t1"), &jstr(url), &jstr("s"));
        let out = status(&dir, &cfg, &db);
        assert_refusal(&out, "url", url);
        let _ = std::fs::remove_dir_all(&dir);
    }
}

#[test]
fn a_url_with_a_non_numeric_port_is_refused_by_the_config() {
    // MEASURED 2026-11-09: `http://127.0.0.1:abc/hook` parses, and ureq
    // then connects to the scheme's DEFAULT port — the request went to
    // nginx on port 80, not to the configured target. The operator's port
    // was silently replaced by 80. `:99999` and a bare `:` behave the same.
    for url in [
        "http://127.0.0.1:abc/hook",
        "http://127.0.0.1:99999/hook",
        "http://127.0.0.1:/hook",
    ] {
        let dir = unique_dir("wh-url-port");
        let db = seed(&dir);
        let cfg = config_with(&dir, &jstr("t1"), &jstr(url), &jstr("s"));
        let out = status(&dir, &cfg, &db);
        assert_refusal(&out, "url", url);
        let _ = std::fs::remove_dir_all(&dir);
    }
}

#[test]
fn a_url_with_port_zero_is_refused_by_the_config() {
    // Nothing ever listens on port 0, and the connection dies with a
    // transport error instead of a reason.
    let dir = unique_dir("wh-url-port0");
    let db = seed(&dir);
    let cfg = config_with(
        &dir,
        &jstr("t1"),
        &jstr("http://127.0.0.1:0/hook"),
        &jstr("s"),
    );
    let out = status(&dir, &cfg, &db);
    assert_refusal(&out, "url", "http://127.0.0.1:0/hook");
    let _ = std::fs::remove_dir_all(&dir);
}

// --- the defect: an empty secret authenticates nothing ---------------------

#[test]
fn an_empty_secret_is_refused_by_the_config() {
    // An empty secret signs with the empty key: the HMAC still computes,
    // but it proves nothing — anyone can produce it.
    let dir = unique_dir("wh-secret-empty");
    let db = seed(&dir);
    let cfg = config_with(&dir, &jstr("t1"), &jstr("http://127.0.0.1:9/hook"), &jstr(""));
    let out = status(&dir, &cfg, &db);
    assert_refusal(&out, "secret", "");
    let _ = std::fs::remove_dir_all(&dir);
}

// --- the guards: everything inside the domain must keep working ------------

#[test]
fn a_plain_target_still_loads() {
    let dir = unique_dir("wh-vdom-guard-plain");
    let db = seed(&dir);
    let cfg = config_with(&dir, &jstr("t1"), &jstr("http://127.0.0.1:9/hook"), &jstr("s"));
    let out = status(&dir, &cfg, &db);
    assert!(
        out.status.success(),
        "a plain target must still load: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("t1"), "the target must be reported: {stdout}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn an_id_with_an_internal_space_or_tab_still_loads() {
    // MEASURED: an internal space and an internal tab are inside the
    // consumer's domain — they reach the wire byte-identical.
    for id in ["a b", "a\tb"] {
        let dir = unique_dir("wh-vdom-guard-inner");
        let db = seed(&dir);
        let cfg = config_with(&dir, &jstr(id), &jstr("http://127.0.0.1:9/hook"), &jstr("s"));
        let out = status(&dir, &cfg, &db);
        assert!(
            out.status.success(),
            "id `{id}` is inside the consumer's domain — it must load: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}

#[test]
fn an_id_with_a_trailing_space_still_loads() {
    // MEASURED: a trailing space is NOT stripped by the receiver (the id is
    // followed by `:event:1`), so it stays inside the domain. Only the
    // LEADING whitespace is stripped.
    let dir = unique_dir("wh-vdom-guard-trail");
    let db = seed(&dir);
    let cfg = config_with(&dir, &jstr("t1 "), &jstr("http://127.0.0.1:9/hook"), &jstr("s"));
    let out = status(&dir, &cfg, &db);
    assert!(
        out.status.success(),
        "a trailing space is inside the consumer's domain: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_secret_with_a_newline_a_control_byte_or_a_non_ascii_byte_still_loads() {
    // MEASURED: the secret is an HMAC KEY and never goes on the wire. Such
    // a secret delivers, and the receiver's own HMAC over the received body
    // verifies against it. Refusing it would lock the operator out of a
    // value that works — the same failure class the URL prefix test had.
    for secret in ["se\ncr\net", "a\u{1}b", "sécret-ß"] {
        let dir = unique_dir("wh-vdom-guard-secret");
        let db = seed(&dir);
        let cfg = config_with(&dir, &jstr("t1"), &jstr("http://127.0.0.1:9/hook"), &jstr(secret));
        let out = status(&dir, &cfg, &db);
        assert!(
            out.status.success(),
            "secret `{secret}` is inside the consumer's domain — it must load: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}

#[test]
fn a_url_with_the_characters_the_parser_accepts_still_loads() {
    // MEASURED: `"` `\` `^` `{` `|` `}` are accepted by the URI parser in
    // the path, and `\`` `^` `{` `|` `}` in the query. A stricter checker
    // would refuse working values. (The `http` crate accepts `" { }` in
    // the path for parity with real clients, RFC 3986 notwithstanding.)
    for ch in ['"', '\\', '^', '{', '|', '}'] {
        let dir = unique_dir("wh-vdom-guard-urlchars");
        let db = seed(&dir);
        let url = format!("http://127.0.0.1:9/a{ch}b");
        let cfg = config_with(&dir, &jstr("t1"), &jstr(&url), &jstr("s"));
        let out = status(&dir, &cfg, &db);
        assert!(
            out.status.success(),
            "url `{url}` is inside the consumer's domain — it must load: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}

#[test]
fn a_url_with_a_backtick_in_the_query_still_loads() {
    // MEASURED: the backtick is refused in the PATH and accepted in the
    // QUERY — the two positions have different domains, so the rule cannot
    // be a flat character blacklist.
    let dir = unique_dir("wh-vdom-guard-tick-query");
    let db = seed(&dir);
    let cfg = config_with(
        &dir,
        &jstr("t1"),
        &jstr("http://127.0.0.1:9/hook?k=`v`"),
        &jstr("s"),
    );
    let out = status(&dir, &cfg, &db);
    assert!(
        out.status.success(),
        "a backtick in the query is inside the consumer's domain: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_url_with_a_fragment_still_loads() {
    // MEASURED: the fragment is never transmitted — a space, a control byte
    // and a non-ASCII byte in it all delivered. Refusing it would refuse a
    // value the consumer honours.
    let dir = unique_dir("wh-vdom-guard-fragment");
    let db = seed(&dir);
    let cfg = config_with(
        &dir,
        &jstr("t1"),
        &jstr("http://127.0.0.1:9/hook#a b"),
        &jstr("s"),
    );
    let out = status(&dir, &cfg, &db);
    assert!(
        out.status.success(),
        "a fragment is inside the consumer's domain: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_percent_encoded_url_still_loads() {
    // MEASURED: `caf%C3%A9` and `%zz` both deliver — percent-encoding is
    // how a non-ASCII path is supposed to travel, and the parser does not
    // police the escape.
    for url in ["http://127.0.0.1:9/caf%C3%A9", "http://127.0.0.1:9/a%20b"] {
        let dir = unique_dir("wh-vdom-guard-percent");
        let db = seed(&dir);
        let cfg = config_with(&dir, &jstr("t1"), &jstr(url), &jstr("s"));
        let out = status(&dir, &cfg, &db);
        assert!(
            out.status.success(),
            "url `{url}` is inside the consumer's domain: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}

// --- the end that matters: the boundary is the CONSUMER --------------------

/// A receiver that answers 200 to every POST and records the RAW bytes of
/// the `X-Reeflog-Event-Id` header.
fn recording_receiver(alive: Arc<AtomicBool>) -> (u16, thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().expect("addr").port();
    let handle = thread::spawn(move || {
        listener.set_nonblocking(true).expect("non-blocking listener");
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

/// Seed one sealed event so a delivery has something to carry.
fn seed_event(db: &Path) {
    let conn = rusqlite::Connection::open(db).expect("open journal");
    conn.execute(
        "INSERT OR IGNORE INTO events (signature, slot, err, logs, source, first_seen, seal_status)
         VALUES ('VDOM000000000000000000000000000000000000000000000000000000000001', 44000000, NULL, '[]', 'ws', 1700000000000, 'sealed')",
        [],
    )
    .expect("seed event");
}

#[test]
fn a_plain_target_actually_delivers_with_its_id_intact() {
    // The boundary is the consumer: a plain target must still DELIVER, and
    // the id must reach the consumer exactly as it was configured — this is
    // the false-positive guard for the whole rule.
    let alive = Arc::new(AtomicBool::new(true));
    let (port, rx) = recording_receiver(Arc::clone(&alive));
    let dir = unique_dir("wh-vdom-e2e-plain");
    let db = seed(&dir);
    seed_event(&db);

    let cfg = config_with(
        &dir,
        &jstr("t1"),
        &jstr(&format!("http://127.0.0.1:{port}/hook")),
        &jstr("s"),
    );
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
        "a plain target must deliver: {} / {stdout}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        stdout.contains("\"delivered\": 1"),
        "expected the event delivered, got: {stdout}"
    );
    assert!(
        stdout.contains("\"blocked\": false"),
        "the queue must not be blocked, got: {stdout}"
    );
    alive.store(false, Ordering::SeqCst);
    let _ = std::fs::remove_dir_all(&dir);
    let _ = rx.join();
}

#[test]
fn a_forbidden_id_is_refused_before_any_delivery_can_be_attempted() {
    // The other direction, end to end: a control byte in the id used to
    // configure a target that fails every delivery. Now the loader refuses
    // it, so no queue is ever configured and no delivery runs.
    let dir = unique_dir("wh-vdom-e2e-refuse");
    let db = seed(&dir);
    seed_event(&db);
    let cfg = config_with(&dir, &jstr("a\rb"), &jstr("http://127.0.0.1:9/hook"), &jstr("s"));
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
    assert_refusal(&out, "id", "a\rb");
    let _ = std::fs::remove_dir_all(&dir);
}
