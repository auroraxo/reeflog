//! `--ws` and `--rpc` are URL flags whose values have NO domain in the
//! parser — unlike every numeric flag, and unlike the webhook target URL,
//! which already requires `http://` or `https://` at config load.
//!
//! Measured on the published v0.7.5 binary (git archive of the tag,
//! 2026-11-09), with `SOL` = `675kPX9MHTjS2zt1qfr1NYHuzeLXfQM9H24wFSUt1Mp8`:
//!
//!   reeflog start --db probe.db --program SOL --seconds 3 --ws ftp://x/y
//!                                               -> exit 0, the journal is
//!                                                  CREATED, two
//!                                                  `connect_fail` gap rows
//!                                                  are written, the report
//!                                                  looks like a normal run;
//!   reeflog start ... --ws ''                   -> exit 0, same;
//!   reeflog start ... --ws 'ws:/127.0.0.1:9'    -> exit 0 (single slash);
//!   reeflog verify --db live.db --rpc ftp://x/y -> exit 3, but only AFTER
//!                                                  the journal was opened
//!                                                  and the call attempted:
//!                                                  "bad uri: unknown
//!                                                  scheme: ftp" — ureq's
//!                                                  words, the flag's never;
//!   reeflog verify --db live.db --rpc not-a-url  -> exit 3, "bad uri:
//!                                                  not-a-url is missing
//!                                                  scheme";
//!   reeflog start ... --ws https://127.0.0.1:18099/x against a LISTENING
//!                                                  port -> gap "URL error:
//!                                                  URL scheme not supported"
//!                                                  — tungstenite can never
//!                                                  speak it, yet the run
//!                                                  went ahead and wrote
//!                                                  state;
//!   reeflog verify ... --rpc ws://127.0.0.1:9/x  -> exit 3, "bad uri:
//!                                                  unknown scheme: ws".
//!
//! The consumers' domains are not an opinion, they are their source code:
//! `ureq` resolves only `http`/`https` (ureq-proto's `SchemeExt::default_port`
//! returns `None` for anything else, and ureq's resolver then reports
//! "unknown scheme"); `tokio-tungstenite` accepts only `ws`/`wss`
//! (`tungstenite::client::uri_mode`: `Some("ws") => Plain, Some("wss") =>
//! Tls, _ => Err(UnsupportedUrlScheme)`).
//!
//! So the rule: a URL flag must carry a URL the CONSUMER can speak, with a
//! host, refused by the PARSER — before a journal is opened or created —
//! with the flag AND the value named, exactly like a numeric flag outside
//! its domain.
//!
//! The guards are the reason this file exists at all: a checker that
//! refuses `http://127.0.0.1:9`, `wss://…`, a URL with a path, port and
//! query, or an UPPERCASE scheme (which the consumers DO accept — measured:
//! `--ws WS://127.0.0.1:9/x` reached TCP connect) is worse than the defect.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const VALID_PROGRAM: &str = "675kPX9MHTjS2zt1qfr1NYHuzeLXfQM9H24wFSUt1Mp8";

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
    String::from_utf8_lossy(&out.stderr).to_lowercase()
}

/// The refusal must come from the PARSER: it names the flag, it names the
/// value, and it says what domain was expected. A transport error from deep
/// inside ureq/tungstenite is NOT this refusal — it does not name the flag.
fn assert_parser_refusal(out: &Output, flag: &str, value: &str) {
    let err = String::from_utf8_lossy(&out.stderr);
    let low = err.to_lowercase();
    assert!(
        !out.status.success(),
        "{flag} `{value}` must be refused (stdout: {})",
        String::from_utf8_lossy(&out.stdout)
    );
    assert!(
        low.contains(flag),
        "the refusal must name the flag {flag}, got: {err}"
    );
    assert!(
        low.contains("url") || low.contains("scheme"),
        "the refusal must say what domain it expected, got: {err}"
    );
    if !value.is_empty() {
        assert!(
            low.contains(&value.to_lowercase()),
            "the refusal must name the offending value `{value}`, got: {err}"
        );
    }
    // The transport's own words mean the parser let the value through.
    assert!(
        !low.contains("unknown scheme:") && !low.contains("bad uri:"),
        "the value reached the transport — the parser refused nothing: {err}"
    );
}

// --- the defect: a URL the consumer cannot speak is accepted ---------------

#[test]
fn an_unsupported_ws_scheme_is_refused_before_the_journal_exists() {
    let dir = unique_dir("ws-ftp");
    let out = run(
        &dir,
        &[
            "start",
            "--db",
            "probe.db",
            "--program",
            VALID_PROGRAM,
            "--seconds",
            "1",
            "--ws",
            "ftp://127.0.0.1:9/x",
        ],
    );
    assert_parser_refusal(&out, "--ws", "ftp://127.0.0.1:9/x");
    assert!(
        !dir.join("probe.db").exists(),
        "a refused URL must not leave a journal behind — the daemon never started"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn https_given_to_the_websocket_flag_is_refused() {
    // Measured on v0.7.5 against a LISTENING port: the daemon opened the
    // journal, ran its full duration and recorded gap "URL error: URL scheme
    // not supported" — tungstenite can never speak https, so the whole run
    // was doomed by a value the parser could have caught in one lookup.
    let dir = unique_dir("ws-https");
    let out = run(
        &dir,
        &[
            "start",
            "--db",
            "probe.db",
            "--program",
            VALID_PROGRAM,
            "--seconds",
            "1",
            "--ws",
            "https://127.0.0.1:18099/x",
        ],
    );
    assert_parser_refusal(&out, "--ws", "https://127.0.0.1:18099/x");
    assert!(!dir.join("probe.db").exists(), "no journal behind a refusal");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn an_empty_ws_value_is_refused_instead_of_running_the_daemon() {
    // `--ws ''` was ACCEPTED on v0.7.5: `start` ran for its full --seconds
    // and exited 0. An empty endpoint is not a slow endpoint.
    let dir = unique_dir("ws-empty");
    let out = run(
        &dir,
        &[
            "start",
            "--db",
            "probe.db",
            "--program",
            VALID_PROGRAM,
            "--seconds",
            "1",
            "--ws",
            "",
        ],
    );
    assert_parser_refusal(&out, "--ws", "");
    assert!(!dir.join("probe.db").exists(), "no journal behind a refusal");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_ws_url_without_a_double_slash_is_refused() {
    // v0.7.5: `--ws ws:/127.0.0.1:9` -> exit 0, gap "HTTP format error:
    // invalid format". A malformed URL is refused, not run.
    let dir = unique_dir("ws-single-slash");
    let out = run(
        &dir,
        &[
            "start",
            "--db",
            "probe.db",
            "--program",
            VALID_PROGRAM,
            "--seconds",
            "1",
            "--ws",
            "ws:/127.0.0.1:9",
        ],
    );
    assert_parser_refusal(&out, "--ws", "ws:/127.0.0.1:9");
    assert!(!dir.join("probe.db").exists(), "no journal behind a refusal");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_ws_url_without_a_host_is_refused() {
    let dir = unique_dir("ws-no-host");
    let out = run(
        &dir,
        &[
            "start",
            "--db",
            "probe.db",
            "--program",
            VALID_PROGRAM,
            "--seconds",
            "1",
            "--ws",
            "wss://",
        ],
    );
    assert_parser_refusal(&out, "--ws", "wss://");
    assert!(!dir.join("probe.db").exists(), "no journal behind a refusal");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_non_http_scheme_for_the_rpc_flag_is_refused() {
    // v0.7.5: exit 3 with ureq's "bad uri: unknown scheme: ftp" — AFTER the
    // journal was opened and the call attempted. The parser never spoke.
    let dir = unique_dir("rpc-ftp");
    let out = run(&dir, &["verify", "--db", "reeflog.db", "--rpc", "ftp://127.0.0.1:9/x"]);
    assert_parser_refusal(&out, "--rpc", "ftp://127.0.0.1:9/x");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_websocket_scheme_given_to_the_rpc_flag_is_refused() {
    // v0.7.5: exit 3, "bad uri: unknown scheme: ws". Crossing the two flags
    // is the most natural typo of all — they sit next to each other.
    let dir = unique_dir("rpc-ws");
    let out = run(&dir, &["verify", "--db", "reeflog.db", "--rpc", "ws://127.0.0.1:9/x"]);
    assert_parser_refusal(&out, "--rpc", "ws://127.0.0.1:9/x");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn an_rpc_value_that_is_not_a_url_at_all_is_refused() {
    // v0.7.5: exit 3, "bad uri: not-a-url is missing scheme", deep in a scan.
    let dir = unique_dir("rpc-garbage");
    let out = run(&dir, &["verify", "--db", "reeflog.db", "--rpc", "not-a-url"]);
    assert_parser_refusal(&out, "--rpc", "not-a-url");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn an_empty_rpc_value_is_refused() {
    let dir = unique_dir("rpc-empty");
    let out = run(&dir, &["verify", "--db", "reeflog.db", "--rpc", ""]);
    assert_parser_refusal(&out, "--rpc", "");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn an_rpc_url_without_a_host_is_refused() {
    // v0.7.5: exit 3, "bad uri: http: empty string" — again from the
    // transport, again without the flag's name.
    let dir = unique_dir("rpc-no-host");
    let out = run(&dir, &["verify", "--db", "reeflog.db", "--rpc", "https://"]);
    assert_parser_refusal(&out, "--rpc", "https://");
    let _ = std::fs::remove_dir_all(&dir);
}

// --- guards: everything the consumers DO accept must keep working -----------
//
// These run against the PARSER ONLY (unreachable endpoints, --seconds 0), so
// they pass on both trees by construction of WHAT they assert: no flag
// refusal. They are the false-positive side of the ladder — on the pre-fix
// tree they pass too, and that is exactly right: the pre-fix parser refuses
// nothing, so "nothing is refused" cannot be the guard. The guards pin the
// fix from refusing too much.

/// A refusal for the URL domain, as opposed to any other outcome.
fn url_refusal(err: &str) -> bool {
    let low = err.to_lowercase();
    (low.contains("--ws") || low.contains("--rpc")) && (low.contains("url") || low.contains("scheme"))
}

#[test]
fn an_unreachable_http_rpc_endpoint_is_still_not_a_refusal() {
    // Connection refused is a TRANSPORT outcome (exit 3), not a flag
    // refusal. The domain check must not swallow it.
    let dir = unique_dir("guard-rpc-http");
    let out = run(&dir, &["verify", "--db", "reeflog.db", "--rpc", "http://127.0.0.1:9"]);
    assert!(
        !url_refusal(&stderr_of(&out)),
        "http:// is inside the RPC domain — it must not be refused: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn an_unreachable_https_rpc_endpoint_is_still_not_a_refusal() {
    let dir = unique_dir("guard-rpc-https");
    let out = run(&dir, &["verify", "--db", "reeflog.db", "--rpc", "https://127.0.0.1:9/x"]);
    assert!(
        !url_refusal(&stderr_of(&out)),
        "https:// is inside the RPC domain: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn an_rpc_url_with_port_path_and_query_is_still_accepted() {
    let dir = unique_dir("guard-rpc-full");
    let out = run(
        &dir,
        &["verify", "--db", "reeflog.db", "--rpc", "http://127.0.0.1:18899/rpc?commitment=x"],
    );
    assert!(
        !url_refusal(&stderr_of(&out)),
        "port, path and query are legal URL parts: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_plain_ws_endpoint_still_starts() {
    // The shape every e2e harness and the fork tool pass.
    let dir = unique_dir("guard-ws");
    let out = run(
        &dir,
        &[
            "start",
            "--db",
            "probe.db",
            "--program",
            VALID_PROGRAM,
            "--seconds",
            "0",
            "--ws",
            "ws://127.0.0.1:9",
            "--rpc",
            "http://127.0.0.1:9",
        ],
    );
    assert!(
        !url_refusal(&stderr_of(&out)),
        "ws://127.0.0.1:9 and http://127.0.0.1:9 are both inside their domains: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_wss_endpoint_is_still_accepted() {
    // The default endpoint of the product is wss:// — refusing it would be
    // refusing the product's own configuration.
    let dir = unique_dir("guard-wss");
    let out = run(
        &dir,
        &[
            "start",
            "--db",
            "probe.db",
            "--program",
            VALID_PROGRAM,
            "--seconds",
            "0",
            "--ws",
            "wss://api.mainnet-beta.solana.com",
        ],
    );
    assert!(
        !url_refusal(&stderr_of(&out)),
        "wss:// is the documented default: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn an_uppercase_scheme_is_still_accepted() {
    // MEASURED on v0.7.5: `--ws WS://127.0.0.1:9/x` reached TCP connect
    // ("Connection refused") — the consumers compare schemes
    // case-insensitively, so a checker that refuses `WS://` would be
    // stricter than the thing it checks.
    let dir = unique_dir("guard-upper");
    let out = run(
        &dir,
        &[
            "start",
            "--db",
            "probe.db",
            "--program",
            VALID_PROGRAM,
            "--seconds",
            "0",
            "--ws",
            "WS://127.0.0.1:9",
        ],
    );
    assert!(
        !url_refusal(&stderr_of(&out)),
        "an uppercase scheme is accepted by the consumer, so it must be accepted here: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn the_inline_form_of_a_url_flag_is_checked_too() {
    let dir = unique_dir("ws-inline");
    let out = run(
        &dir,
        &[
            "start",
            "--db",
            "probe.db",
            "--program",
            VALID_PROGRAM,
            "--seconds",
            "1",
            "--ws=ftp://127.0.0.1:9/x",
        ],
    );
    assert_parser_refusal(&out, "--ws", "ftp://127.0.0.1:9/x");
    assert!(!dir.join("probe.db").exists(), "no journal behind a refusal");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_refused_url_does_not_stop_a_valid_one_from_being_used_in_the_same_run() {
    // Both flags checked independently: --ws garbage next to a valid --rpc
    // still refuses (the invalid flag wins), and the message names the flag
    // that is wrong, not the one that is right.
    let dir = unique_dir("ws-one-of-two");
    let out = run(
        &dir,
        &[
            "start",
            "--db",
            "probe.db",
            "--program",
            VALID_PROGRAM,
            "--seconds",
            "1",
            "--ws",
            "nonsense",
            "--rpc",
            "http://127.0.0.1:9",
        ],
    );
    assert_parser_refusal(&out, "--ws", "nonsense");
    assert!(
        !String::from_utf8_lossy(&out.stderr).to_lowercase().contains("--rpc"),
        "the valid --rpc must not be blamed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let _ = std::fs::remove_dir_all(&dir);
}
