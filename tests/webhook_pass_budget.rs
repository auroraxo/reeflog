//! THE WEBHOOK PASS STAYS INSIDE THE RUN THAT SCHEDULED IT (PULS BF).
//!
//! The finding (2026-11-21, `tools/measure-webhook-pass-overrun.mjs` against
//! the PUBLISHED v0.7.44 asset, binary sha256 ead75ad9…): the webhook
//! delivery pass is bounded per POST by the target's `timeout_ms` but has NO
//! budget of its own. `deliver_events` + `deliver_reorgs` loop over up to
//! `--webhook-limit` rows synchronously INSIDE the scheduler tick, so the
//! run loop cannot check its `--seconds` deadline again until the pass hands
//! control back: a 30 s run with a 200-row backlog against a 250 ms/POST
//! endpoint ended 29 s late. Every signature walk was deadline-bounded in
//! PULS BD; the webhook pass was the one scheduler consumer left.
//!
//! THE RULE NOW ENFORCED: the daemon grants each webhook pass the time that
//! is left until the run's own deadline. A pass that runs out of TIME is a
//! time-slice, not a failure — the rows it did not examine stay owed, the
//! stop is named in the run summary (`budget_stops`, `rows_owed`), delivery
//! continues on the next pass, and every exit voice is untouched. The
//! budget must not starve delivery either: a pass that stops having
//! delivered nothing would hide the queue behind a green exit.
//!
//! Faces (this file, one live daemon run against a 150 ms/POST sink):
//!   D1 the run honours `--seconds` while the queue owes far more than the
//!      run can pay (80 rows x 150 ms = 12 s of delivery work on a 6 s run):
//!      the wall clock stays near the deadline — on the unfixed tree this
//!      run pays the whole queue past its deadline;
//!   N1 the pass actually ran (the cadence went due — an instrument that
//!      never fires proves nothing);
//!   N2 the time-slice is named: `budget_stops >= 1` and `rows_owed >= 1`
//!      in the run summary (a pre-fix binary does not carry the fields);
//!   N3 the budget does not starve delivery: at least one body was handed
//!      over before the slice ended, and the sink saw every body the
//!      summary claims;
//!   N4 a slow endpoint is not a crash: exit 0.
//!
//! `webhook push` is a DRAIN command and stays unbudgeted — pinned by the
//! two-tree measurement (`research/webhook-pass-overrun-*.json`, G3), not
//! here: this file pins the scheduler's contract, not the drain's.

use std::io::Write;
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

const VALID_PROGRAM: &str = "whirLbMiicVdio4qvUfM5KAg6Ct8VwpYzGff3uctyCc";
const WS_GUID: &str = "258EAFA5-E914-47DA-95CA-C5AB0DC85B11";

/// The run is 6 s; the sink delays every POST by 150 ms; the backlog owes
/// 80 x 150 ms = 12 s. The unfixed tree pays the whole queue past the
/// deadline (~13-15 s wall); the fixed tree stops the pass AT the deadline
/// (~6-8 s wall). The bound sits between the two with margin for load.
const RUN_SECONDS: u64 = 6;
const BACKLOG_ROWS: usize = 80;
const DELAY_MS: u64 = 150;
/// D1's bound: deadline + shutdown budget + one in-flight POST + exit slack.
const WALL_BUDGET_MS: u64 = (RUN_SECONDS + 4) * 1000;

fn unique_dir(tag: &str) -> PathBuf {
    static N: AtomicU64 = AtomicU64::new(0);
    let n = N.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!(
        "reeflog-webhook-pass-budget-{tag}-{}-{n}",
        std::process::id()
    ));
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

/// One port, three roles: WS upgrade (held open so the run ends at its own
/// deadline, not through a disconnect), instant JSON-RPC answers (the
/// defect is isolated to the webhook pass — the network must not be the
/// thing that is slow), and webhook POSTs answered after exactly
/// `delay_ms`. Counts every webhook POST it received.
fn sink_with_webhook_delay(delay_ms: u64) -> (u16, Arc<AtomicU64>) {
    let posts = Arc::new(AtomicU64::new(0));
    let p2 = posts.clone();
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().expect("addr").port();
    let handle = std::thread::spawn(move || {
        for stream in listener.incoming() {
            let mut s = match stream {
                Ok(s) => s,
                Err(_) => break,
            };
            let posts = p2.clone();
            let _ = std::thread::spawn(move || {
                use std::io::Read as _;
                let mut acc: Vec<u8> = Vec::new();
                let mut buf = [0u8; 8192];
                let head_end = loop {
                    let n = match s.read(&mut buf) {
                        Ok(0) | Err(_) => return,
                        Ok(n) => n,
                    };
                    acc.extend_from_slice(&buf[..n]);
                    if let Some(p) = acc.windows(4).position(|w| w == b"\r\n\r\n") {
                        break p;
                    }
                    if acc.len() > 1 << 20 {
                        return;
                    }
                };
                let head = String::from_utf8_lossy(&acc[..head_end]).to_string();
                if head.lines().any(|l| {
                    l.split_once(':').is_some_and(|(k, v)| {
                        k.trim().eq_ignore_ascii_case("upgrade")
                            && v.trim().eq_ignore_ascii_case("websocket")
                    })
                }) {
                    // The ingest socket: real RFC 6455 upgrade, then hold it
                    // open past every run window.
                    let Some(key) = head.lines().find_map(|l| {
                        let (k, v) = l.split_once(':')?;
                        k.trim()
                            .eq_ignore_ascii_case("sec-websocket-key")
                            .then(|| v.trim().to_string())
                    }) else {
                        return;
                    };
                    use base64::Engine as _;
                    use sha1::{Digest, Sha1};
                    let mut h = Sha1::new();
                    h.update(key.as_bytes());
                    h.update(WS_GUID.as_bytes());
                    let accept = base64::engine::general_purpose::STANDARD.encode(h.finalize());
                    let resp = format!(
                        "HTTP/1.1 101 Switching Protocols\r\nupgrade: websocket\r\nconnection: upgrade\r\nsec-websocket-accept: {accept}\r\n\r\n"
                    );
                    let _ = s.write_all(resp.as_bytes());
                    let _ = s.flush();
                    std::thread::sleep(Duration::from_secs(30));
                    return;
                }
                // Plain HTTP: read the full body.
                let head_end = head_end + 4;
                let content_length: usize = head
                    .lines()
                    .find_map(|l| {
                        let (k, v) = l.split_once(':')?;
                        k.trim()
                            .eq_ignore_ascii_case("content-length")
                            .then(|| v.trim().parse::<usize>().ok())
                            .flatten()
                    })
                    .unwrap_or(0);
                while acc.len() < head_end + content_length {
                    let n = match s.read(&mut buf) {
                        Ok(0) | Err(_) => return,
                        Ok(n) => n,
                    };
                    acc.extend_from_slice(&buf[..n]);
                }
                let body =
                    String::from_utf8_lossy(&acc[head_end..head_end + content_length]).to_string();
                let parsed: Result<serde_json::Value, _> = serde_json::from_str(&body);
                let is_rpc = parsed
                    .as_ref()
                    .ok()
                    .and_then(|v| v.get("method"))
                    .and_then(|m| m.as_str())
                    .is_some();
                let respond = |s: &mut std::net::TcpStream, payload: &str| {
                    let head = format!(
                        "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
                        payload.len()
                    );
                    let _ = s.write_all(head.as_bytes());
                    let _ = s.write_all(payload.as_bytes());
                    let _ = s.flush();
                };
                if !is_rpc {
                    // A webhook delivery: the ONLY thing in this harness
                    // that is slow.
                    posts.fetch_add(1, Ordering::SeqCst);
                    std::thread::sleep(Duration::from_millis(delay_ms));
                    respond(&mut s, "{\"ok\":true}");
                    return;
                }
                let Ok(v) = parsed else { return };
                match v["method"].as_str().unwrap_or("") {
                    "getSignatureStatuses" => {
                        let req_sigs = v["params"][0].as_array().map(|a| a.len()).unwrap_or(0);
                        let values: Vec<serde_json::Value> =
                            (0..req_sigs).map(|_| serde_json::Value::Null).collect();
                        let payload = serde_json::json!({
                            "jsonrpc": "2.0", "id": 1, "result": { "value": values }
                        })
                        .to_string();
                        respond(&mut s, &payload);
                    }
                    "getSignaturesForAddress" => {
                        respond(&mut s, r#"{"jsonrpc":"2.0","id":1,"result":[]}"#);
                    }
                    _ => {
                        respond(
                            &mut s,
                            r#"{"jsonrpc":"2.0","id":1,"result":null}"#,
                        );
                    }
                }
            });
        }
    });
    std::mem::forget(handle);
    (port, posts)
}

/// A journal whose webhook queue owes `n` deliverable rows: every row has
/// its logs (no payload debt — the debt rule must not be what stops this
/// queue, the BUDGET must be) and no receipt, so each one costs one POST.
fn planted_dir(tag: &str, n: usize) -> PathBuf {
    let dir = unique_dir(tag);
    let out = run(
        &dir,
        &["init", "--db", "reeflog.db", "--program", VALID_PROGRAM],
    );
    assert!(out.status.success(), "init must succeed");
    let conn = rusqlite::Connection::open(dir.join("reeflog.db")).expect("open journal");
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_millis() as i64;
    for i in 0..n {
        conn.execute(
            "INSERT INTO events(signature, slot, err, logs, source, first_seen, seal_status, payload_state)
             VALUES (?1, ?2, NULL, '[\"a\"]', 'ws', ?3, 'finalized', NULL)",
            rusqlite::params![
                format!("PassBudget{i:06}111111111111111111111111111"),
                46000001 - i as i64,
                now
            ],
        )
        .expect("plant deliverable row");
    }
    conn.close().expect("close");
    dir
}

#[test]
fn the_webhook_pass_stays_inside_the_run_and_names_its_slice() {
    let dir = planted_dir("run", BACKLOG_ROWS);
    let (port, posts) = sink_with_webhook_delay(DELAY_MS);
    let cfg = dir.join("webhooks.json");
    std::fs::write(
        &cfg,
        serde_json::json!({
            "targets": [{
                "id": "slice",
                "url": format!("http://127.0.0.1:{port}/hook"),
                "secret": "whsec_pass_budget_probe",
                "timeout_ms": 5000
            }]
        })
        .to_string(),
    )
    .expect("write webhook config");

    let t0 = Instant::now();
    let out = run(
        &dir,
        &[
            "start",
            "--db",
            "reeflog.db",
            "--seconds",
            &RUN_SECONDS.to_string(),
            "--ws",
            &format!("ws://127.0.0.1:{port}"),
            "--rpc",
            &format!("http://127.0.0.1:{port}"),
            "--webhook-config",
            cfg.to_str().expect("cfg path"),
            "--webhook-every",
            "1",
            "--webhook-limit",
            "200",
        ],
    );
    let wall = t0.elapsed();
    assert!(
        out.status.success(),
        "a slow endpoint is not a crash (N4): exit {:?}\nstderr: {}",
        out.status.code(),
        stderr_of(&out)
    );

    let summary: serde_json::Value = serde_json::from_str(&stdout_of(&out)).unwrap_or_else(|e| {
        panic!("the run must print its summary JSON: {e}\nstdout: {}", stdout_of(&out))
    });
    let wh = &summary["webhook"];

    // N1 — the cadence went due and a pass ran (an instrument that never
    // fires proves nothing; falsified once already on this defect).
    let passes = wh["passes"].as_i64().unwrap_or_else(|| {
        panic!("the summary must report webhook passes: {}", stdout_of(&out))
    });
    assert!(passes >= 1, "the webhook tick must have run at least once");

    // D1 — the run honours its seconds: the queue owes
    // 80 x 150 ms = 12 s, more than the whole 6 s run. The unfixed tree
    // pays the whole queue past the deadline; the fixed tree stops the
    // pass AT the deadline and pays only one in-flight POST past it.
    assert!(
        wall <= Duration::from_millis(WALL_BUDGET_MS),
        "the run must end near its --seconds even when the queue owes more \
         than the run can pay: wall {:?} vs budget {}ms (queue owes {}ms)",
        wall,
        WALL_BUDGET_MS,
        BACKLOG_ROWS as u64 * DELAY_MS
    );

    // N2 — the time-slice is named, not silent: the summary counts the
    // passes the budget ended and the rows that stayed owed.
    let budget_stops = wh["budget_stops"].as_i64().unwrap_or_else(|| {
        panic!(
            "the summary must name budget stops (a pre-fix binary does not \
             carry the field): webhook = {wh}"
        )
    });
    let rows_owed = wh["rows_owed"].as_i64().unwrap_or_else(|| {
        panic!(
            "the summary must name the rows the slice left owed: webhook = {wh}"
        )
    });
    assert!(
        budget_stops >= 1,
        "the pass must confess it ended on the budget, not on the queue"
    );
    assert!(
        rows_owed >= 1,
        "the summary must name the rows the slice left owed"
    );

    // N3 — the budget does not starve delivery: at least one body left the
    // machine before the slice ended, and the sink saw every body the
    // summary claims was delivered.
    let delivered = wh["delivered"].as_i64().unwrap_or_else(|| {
        panic!("the summary must report deliveries: webhook = {wh}")
    });
    assert!(
        delivered >= 1,
        "a budget that delivers NOTHING is a defect of its own"
    );
    let seen = posts.load(Ordering::SeqCst) as i64;
    assert!(
        seen >= delivered,
        "the sink must have seen every body the summary claims: sink {seen} vs delivered {delivered}"
    );

    // The rows behind the slice are still owed IN THE JOURNAL: the next
    // pass (or `webhook push`) can pay them — nothing was dropped.
    let conn = rusqlite::Connection::open(dir.join("reeflog.db")).expect("open journal");
    let cursor: i64 = conn
        .query_row(
            "SELECT COALESCE(MAX(next_seq), 0) FROM webhook_state WHERE target='slice'",
            [],
            |r| r.get(0),
        )
        .unwrap_or(0);
    let events: i64 = conn
        .query_row("SELECT COUNT(*) FROM events", [], |r| r.get(0))
        .expect("count events");
    assert_eq!(
        events, BACKLOG_ROWS as i64,
        "the backlog must be intact"
    );
    assert!(
        cursor >= 1 && cursor < events,
        "the cursor must have advanced past the delivered rows and stop short \
         of the queue the budget did not reach: cursor {cursor} of {events}"
    );
}
