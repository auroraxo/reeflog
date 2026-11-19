//! A zero cadence means OFF, not "every second".
//!
//! PULS AU, 2026-11-19. Measured on the PUBLISHED v0.7.37 asset (the linux
//! archive whose sha256 equals the digest the Release API publishes, binary
//! sha256 ddba9506…; `tools/measure-scheduler-zero-cadence.mjs` against a
//! real local chain sink that counts every RPC call the daemon makes):
//!
//! `reeflog start --seal-every 0 --reorg-every 0 --repair-every 0
//! --webhook-every 0` RAN every periodic pass — the scheduler next-due is
//! seeded `now + max(cadence, 1)` and re-seeded the same way, so a cadence
//! of 0 collapses to 1 s and the passes fire every second. The same run's
//! shutdown gate read the very same value as "never": the final reorg check
//! and final repair pass were skipped (`reorg_passes: 1, repair_passes: 1`
//! — the startup pass only), so ONE flag value carried TWO contradictory
//! meanings inside one run. A 10 s run with all four cadences at 0 cost 17
//! RPC calls; the same run at cadence 3600 cost 5.
//!
//! The zero is legal by design everywhere else in the flag domain
//! (`flags.rs` NUMERIC keeps 0 BECAUSE "several flags document it as
//! 'disable this pass'"; `--payload-limit 0` and `--repair-pages 0` are the
//! documented spellings of "off" in the same command). The scheduler is the
//! one layer that re-interpreted that zero into a busy schedule — and its
//! own shutdown pass refused to follow.
//!
//! THE RULE NOW ENFORCED: for `--seal-every`, `--reorg-every`,
//! `--repair-every` and `--webhook-every`, 0 means the pass never runs —
//! same value, same meaning, on every tick and at shutdown. Any other
//! cadence is unchanged.
//!
//! Faces (each asserted on BOTH trees by the two-tree measurement tool):
//!   D1 `--seal-every 0` runs the seal pass (defect: at least once besides
//!      the shutdown seal, which is unconditional);
//!   D2 `--reorg-every 0` runs the reorg pass AND its shutdown final check
//!      (defect: the final check claims `skipped: true` while the schedule
//!      ran the same work every second);
//!   D3 `--repair-every 0` runs the repair pass AND its shutdown final pass
//!      (defect: same contradiction, repair side);
//!   D4 `--webhook-every 0` with a configured target POSTs signed bodies
//!      (defect: a delivery cadence of 0 delivers, at 1 Hz, forever);
//!   D5 the summary confesses the contradiction: with all cadences at 0 the
//!      run reports `reorg_passes >= 2` while `final_reorg` says
//!      `skipped` (and the repair pair likewise) — one value, two meanings.
//!
//! Guards (green on BOTH trees — the honest forms must not move):
//!   G1 an effectively-never cadence (3600) within a short run schedules
//!      nothing but the startup-boundary work: no seal pass, no webhook
//!      pass, and the shutdown gates read `skipped`;
//!   G2 a tight positive cadence still runs (the schedule itself keeps
//!      working — 1 s cadence, several passes);
//!   G3 the existing infinite-lookback guard stays green: a huge cadence
//!      value is still refused by the flag domain (checked against the
//!      checker's own refusal, unchanged).

use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

const VALID_PROGRAM: &str = "whirLbMiicVdio4qvUfM5KAg6Ct8VwpYzGff3uctyCc";
const WS_GUID: &str = "258EAFA5-E914-47DA-95CA-C5AB0DC85B11";

fn unique_dir(tag: &str) -> PathBuf {
    static N: AtomicU64 = AtomicU64::new(0);
    let n = N.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!(
        "reeflog-zero-cadence-{tag}-{}-{n}",
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

fn report_of(out: &Output) -> serde_json::Value {
    serde_json::from_str(&stdout_of(out)).unwrap_or(serde_json::json!({}))
}

/// What the chain sink counted, per RPC method, plus every webhook body it
/// received. One port speaks both protocols: a WS upgrade handshake for the
/// ingest socket, plain JSON-RPC over separate HTTP connections for the
/// periodic passes, and webhook deliveries are counted by an HTTP POST
/// whose body is not a JSON-RPC request.
#[derive(Clone)]
struct ChainCount {
    get_signature_statuses: usize,
    get_transaction: usize,
    get_signatures_for_address: usize,
    webhook_posts: usize,
}

/// One port, three roles: WS upgrade (held open), JSON-RPC answers, webhook
/// POST counter. `serve_sealed_rows` decides whether the RPC side answers
/// `getSignatureStatuses` with a finalized row (which the seal pass
/// promotes) or with nulls (which keeps rows unsealed so the pass keeps
/// finding work every tick — the loudest possible shape for D1).
fn chain_server(serve_sealed_rows: bool) -> (u16, Arc<Mutex<ChainCount>>) {
    let count = Arc::new(Mutex::new(ChainCount {
        get_signature_statuses: 0,
        get_transaction: 0,
        get_signatures_for_address: 0,
        webhook_posts: 0,
    }));
    let c2 = count.clone();
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().expect("addr").port();
    let handle = std::thread::spawn(move || {
        for stream in listener.incoming() {
            let mut s = match stream {
                Ok(s) => s,
                Err(_) => break,
            };
            let count = c2.clone();
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
                    // The ingest socket: perform the real RFC 6455 upgrade
                    // and hold it open past every run window, so the run
                    // ends at its own deadline instead of through a
                    // disconnect it would journal as an episode.
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
                // Plain HTTP from here: read the full body.
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
                // A webhook delivery is a JSON body with NO JSON-RPC
                // envelope: count it as a POST, not as an RPC call, and
                // answer 200 so a delivery can actually succeed.
                let parsed: Result<serde_json::Value, _> = serde_json::from_str(&body);
                let is_rpc = parsed
                    .as_ref()
                    .ok()
                    .and_then(|v| v.get("method"))
                    .and_then(|m| m.as_str())
                    .is_some();
                if !is_rpc {
                    count.lock().unwrap().webhook_posts += 1;
                    let resp =
                        "HTTP/1.1 200 OK\r\ncontent-length: 2\r\nconnection: close\r\n\r\nok";
                    let _ = s.write_all(resp.as_bytes());
                    let _ = s.flush();
                    return;
                }
                let Ok(v) = parsed else { return };
                let mut count = count.lock().unwrap();
                match v["method"].as_str().unwrap_or("") {
                    "getSignatureStatuses" => {
                        count.get_signature_statuses += 1;
                        let req_sigs = v["params"][0].as_array().map(|a| a.len()).unwrap_or(0);
                        let values: Vec<serde_json::Value> = (0..req_sigs)
                            .map(|_| {
                                if serve_sealed_rows {
                                    serde_json::json!({
                                        "confirmationStatus": "finalized",
                                        "err": serde_json::Value::Null
                                    })
                                } else {
                                    serde_json::Value::Null
                                }
                            })
                            .collect();
                        let payload = serde_json::json!({
                            "jsonrpc": "2.0", "id": 1, "result": { "value": values }
                        })
                        .to_string();
                        drop(count);
                        let head = format!(
                            "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
                            payload.len()
                        );
                        let _ = s.write_all(head.as_bytes());
                        let _ = s.write_all(payload.as_bytes());
                        let _ = s.flush();
                    }
                    "getTransaction" => {
                        count.get_transaction += 1;
                        let payload = serde_json::json!({
                            "jsonrpc": "2.0", "id": 1, "result": serde_json::Value::Null
                        })
                        .to_string();
                        drop(count);
                        let head = format!(
                            "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
                            payload.len()
                        );
                        let _ = s.write_all(head.as_bytes());
                        let _ = s.write_all(payload.as_bytes());
                        let _ = s.flush();
                    }
                    "getSignaturesForAddress" => {
                        count.get_signatures_for_address += 1;
                        let payload = serde_json::json!({
                            "jsonrpc": "2.0", "id": 1, "result": []
                        })
                        .to_string();
                        drop(count);
                        let head = format!(
                            "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
                            payload.len()
                        );
                        let _ = s.write_all(head.as_bytes());
                        let _ = s.write_all(payload.as_bytes());
                        let _ = s.flush();
                    }
                    _ => {}
                }
            });
        }
    });
    std::mem::forget(handle);
    (port, count)
}

/// A journal with ONE unsealed row (the seal pass has work to find on every
/// pass when the sink answers nulls) and one sealed row (the reorg window
/// has a row to consider). No rows are owed payloads, so the webhook pass
/// is not blocked by the debt rule.
fn planted_dir(tag: &str) -> PathBuf {
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
    conn.execute(
        "INSERT INTO events(signature, slot, err, logs, source, first_seen, seal_status, payload_state)
         VALUES (?1, 100, NULL, '[\"a\"]', 'ws', ?2, 'finalized', 'held')",
        rusqlite::params!["Sealed1", now],
    )
    .expect("plant sealed");
    conn.execute(
        "INSERT INTO events(signature, slot, err, logs, source, first_seen, seal_status, payload_state)
         VALUES (?1, 101, NULL, '[\"b\"]', 'ws', ?2, NULL, 'held')",
        rusqlite::params!["Unsealed1", now],
    )
    .expect("plant unsealed");
    conn.close().expect("close");
    dir
}

fn webhook_config(dir: &Path) -> PathBuf {
    let cfg = dir.join("webhooks.json");
    std::fs::write(
        &cfg,
        serde_json::json!({
            "targets": [{
                "id": "t1",
                "url": "PLACEHOLDER",
                "secret": "whsec_zero_cadence_probe",
                "timeout_ms": 3000,
                "dead_letter_after": 0
            }]
        })
        .to_string(),
    )
    .expect("write config");
    cfg
}

fn all_zero() -> [(&'static str, &'static str); 4] {
    [
        ("--seal-every", "0"),
        ("--reorg-every", "0"),
        ("--repair-every", "0"),
        ("--webhook-every", "0"),
    ]
}

// ---------------------------------------------------------------------------
// D1: `--seal-every 0` runs the seal pass. With the sink answering nulls,
// every seal pass leaves the row unsealed, so ANY pass beyond the
// unconditional shutdown seal proves the schedule ran. The shutdown seal is
// NOT a schedule pass — it runs unconditionally — so the assertion is "more
// seal work than the shutdown pass alone": two or more
// getSignatureStatuses calls.
// ---------------------------------------------------------------------------
#[test]
fn a_zero_seal_cadence_runs_the_seal_pass() {
    let dir = planted_dir("seal-zero");
    let (port, count) = chain_server(false);
    let ws_url = format!("ws://127.0.0.1:{port}");
    let rpc_url = format!("http://127.0.0.1:{port}");
    let mut args: Vec<&str> = vec![
        "start",
        "--db",
        "reeflog.db",
        "--program",
        VALID_PROGRAM,
        "--seconds",
        "6",
        "--ws",
        &ws_url,
        "--rpc",
        &rpc_url,
    ];
    for (f, v) in all_zero() {
        args.push(f);
        args.push(v);
    }
    let out = run(&dir, &args);
    assert!(
        out.status.success(),
        "the run must exit clean: {}",
        stderr_of(&out)
    );
    let c = {
        let g = count.lock().unwrap();
        ChainCount {
            get_signature_statuses: g.get_signature_statuses,
            get_transaction: g.get_transaction,
            get_signatures_for_address: g.get_signatures_for_address,
            webhook_posts: g.webhook_posts,
        }
    };
    // The shutdown seal is NOT a schedule pass: it runs unconditionally and
    // costs exactly ONE getSignatureStatuses call. A schedule that ran
    // would cost one more per second of the window.
    assert_eq!(
        c.get_signature_statuses, 1,
        "--seal-every 0 must disable the periodic seal pass (the shutdown seal \
         alone costs one getSignatureStatuses call); measured {} — the schedule RAN instead",
        c.get_signature_statuses
    );
}

// ---------------------------------------------------------------------------
// D2: `--reorg-every 0` runs the reorg pass — and its own shutdown gate
// claims the same value means "never" (final_reorg.skipped). One value, two
// meanings inside one run.
// ---------------------------------------------------------------------------
#[test]
fn a_zero_reorg_cadence_runs_the_reorg_pass_but_skips_the_final_check() {
    let dir = planted_dir("reorg-zero");
    let (port, _count) = chain_server(true);
    let ws_url = format!("ws://127.0.0.1:{port}");
    let rpc_url = format!("http://127.0.0.1:{port}");
    let mut args: Vec<&str> = vec![
        "start",
        "--db",
        "reeflog.db",
        "--program",
        VALID_PROGRAM,
        "--seconds",
        "6",
        "--ws",
        &ws_url,
        "--rpc",
        &rpc_url,
        // The seal and repair schedules stay off by the honest spelling:
        // a cadence that cannot come due within the run. The reorg cadence
        // is the value under test.
        "--seal-every",
        "3600",
        "--repair-every",
        "3600",
        "--webhook-every",
        "3600",
        "--reorg-every",
        "0",
    ];
    let out = run(&dir, &args);
    assert!(out.status.success(), "the run must exit clean");
    let s = report_of(&out);
    // ONE meaning for the zero: the pass never runs — neither on the tick
    // nor at the shutdown gate.
    assert_eq!(
        s["reorg_passes"].as_i64(),
        Some(0),
        "--reorg-every 0 must disable the reorg pass; the summary reports \
         reorg_passes = {} — the schedule RAN instead",
        s["reorg_passes"]
    );
    assert_eq!(
        s["final_reorg"]["skipped"].as_bool(),
        Some(true),
        "--reorg-every 0 means 'never' at the shutdown gate too"
    );
}

// ---------------------------------------------------------------------------
// D3: `--repair-every 0` runs the repair pass while the final repair pass
// claims `skipped: true`.
// ---------------------------------------------------------------------------
#[test]
fn a_zero_repair_cadence_runs_the_repair_pass_but_skips_the_final_pass() {
    let dir = planted_dir("repair-zero");
    let (port, _count) = chain_server(false);
    let ws_url = format!("ws://127.0.0.1:{port}");
    let rpc_url = format!("http://127.0.0.1:{port}");
    let mut args: Vec<&str> = vec![
        "start",
        "--db",
        "reeflog.db",
        "--program",
        VALID_PROGRAM,
        "--seconds",
        "6",
        "--ws",
        &ws_url,
        "--rpc",
        &rpc_url,
        "--seal-every",
        "3600",
        "--reorg-every",
        "3600",
        "--webhook-every",
        "3600",
        "--repair-every",
        "0",
    ];
    let out = run(&dir, &args);
    assert!(out.status.success(), "the run must exit clean");
    let s = report_of(&out);
    assert_eq!(
        s["repair_passes"].as_i64(),
        Some(0),
        "--repair-every 0 must disable the repair pass; repair_passes = {}",
        s["repair_passes"]
    );
    assert_eq!(
        s["final_repair"]["skipped"].as_bool(),
        Some(true),
        "--repair-every 0 means 'never' at the shutdown gate too"
    );
}

// ---------------------------------------------------------------------------
// D4: `--webhook-every 0` delivers. A delivery cadence of 0 is not "deliver
// every second" — no operator who wrote 0 asked for a 1 Hz stream of
// signed bodies, and the same flag value in the same run would still have
// meant "never" to the shutdown gates.
// ---------------------------------------------------------------------------
#[test]
fn a_zero_webhook_cadence_does_not_deliver() {
    let dir = planted_dir("webhook-zero");
    let cfg = webhook_config(&dir);
    // Point the target at the SAME sink port; the sink counts non-RPC
    // bodies as webhook posts. The receiver answers 200.
    let (port, count) = chain_server(true);
    let cfg_text = std::fs::read_to_string(&cfg).unwrap();
    let cfg_text = cfg_text.replace("PLACEHOLDER", &format!("http://127.0.0.1:{port}/hook"));
    std::fs::write(&cfg, cfg_text).unwrap();

    let ws_url = format!("ws://127.0.0.1:{port}");
    let rpc_url = format!("http://127.0.0.1:{port}");
    let mut args: Vec<&str> = vec![
        "start",
        "--db",
        "reeflog.db",
        "--program",
        VALID_PROGRAM,
        "--seconds",
        "6",
        "--ws",
        &ws_url,
        "--rpc",
        &rpc_url,
        "--webhook-config",
        cfg.to_str().unwrap(),
        "--seal-every",
        "3600",
        "--reorg-every",
        "3600",
        "--repair-every",
        "3600",
        "--webhook-every",
        "0",
    ];
    let out = run(&dir, &args);
    assert!(out.status.success(), "the run must exit clean");
    let c = {
        let g = count.lock().unwrap();
        ChainCount {
            get_signature_statuses: g.get_signature_statuses,
            get_transaction: g.get_transaction,
            get_signatures_for_address: g.get_signatures_for_address,
            webhook_posts: g.webhook_posts,
        }
    };
    assert_eq!(
        c.webhook_posts, 0,
        "--webhook-every 0 must disable the webhook pass; the sink received \
         {} signed POST(s) — the schedule DELIVERED instead",
        c.webhook_posts
    );
}

// ---------------------------------------------------------------------------
// D5: the summary itself confesses the contradiction when all cadences are
// zero: schedule passes ran (>= 2 of each) while the final gates say
// "skipped". This is the two-meanings defect in one JSON document.
// ---------------------------------------------------------------------------
#[test]
fn the_summary_shows_the_two_meanings_of_zero() {
    let dir = planted_dir("summary-zero");
    let (port, _count) = chain_server(false);
    let ws_url = format!("ws://127.0.0.1:{port}");
    let rpc_url = format!("http://127.0.0.1:{port}");
    let mut args: Vec<&str> = vec![
        "start",
        "--db",
        "reeflog.db",
        "--program",
        VALID_PROGRAM,
        "--seconds",
        "6",
        "--ws",
        &ws_url,
        "--rpc",
        &rpc_url,
    ];
    for (f, v) in all_zero() {
        args.push(f);
        args.push(v);
    }
    let out = run(&dir, &args);
    assert!(out.status.success(), "the run must exit clean");
    let s = report_of(&out);
    // ONE meaning, everywhere: with every cadence at 0 nothing is
    // scheduled and both final gates say so.
    assert_eq!(
        s["reorg_passes"].as_i64(),
        Some(0),
        "no reorg pass may run when its cadence is 0: reorg_passes = {}",
        s["reorg_passes"]
    );
    assert_eq!(
        s["repair_passes"].as_i64(),
        Some(0),
        "no repair pass may run when its cadence is 0: repair_passes = {}",
        s["repair_passes"]
    );
    assert_eq!(
        s["final_reorg"]["skipped"].as_bool(),
        Some(true),
        "the final gate must read the same zero as 'never'"
    );
    assert_eq!(
        s["final_repair"]["skipped"].as_bool(),
        Some(true),
        "the final gate must read the same zero as 'never'"
    );
}

// ---------------------------------------------------------------------------
// G1: an effectively-never cadence (3600) in a short run schedules nothing
// extra: no seal pass, no webhook delivery, and the shutdown gates read
// `skipped`. The honest spelling stays honest.
// ---------------------------------------------------------------------------
#[test]
fn a_never_cadence_schedules_nothing_and_the_gates_read_skipped() {
    let dir = planted_dir("never-cadence");
    let cfg = webhook_config(&dir);
    let (port, count) = chain_server(true);
    let cfg_text = std::fs::read_to_string(&cfg).unwrap();
    let cfg_text = cfg_text.replace("PLACEHOLDER", &format!("http://127.0.0.1:{port}/hook"));
    std::fs::write(&cfg, cfg_text).unwrap();

    let out = run(
        &dir,
        &[
            "start",
            "--db",
            "reeflog.db",
            "--program",
            VALID_PROGRAM,
            "--seconds",
            "6",
            "--ws",
            &format!("ws://127.0.0.1:{port}"),
            "--rpc",
            &format!("http://127.0.0.1:{port}"),
            "--webhook-config",
            cfg.to_str().unwrap(),
            "--seal-every",
            "3600",
            "--reorg-every",
            "3600",
            "--repair-every",
            "3600",
            "--webhook-every",
            "3600",
        ],
    );
    assert!(out.status.success(), "the run must exit clean");
    let s = report_of(&out);
    assert_eq!(
        s["webhook"]["passes"].as_i64(),
        Some(0),
        "no webhook pass may run within the window"
    );
    assert_eq!(
        count.lock().unwrap().webhook_posts,
        0,
        "no signed body may leave the machine"
    );
    // The shutdown gates are NOT schedules: a positive cadence means the
    // final pass runs once at shutdown (the run may have ended early). Only
    // a cadence of 0 means "never" — on the tick AND at the gate.
    assert!(
        s["final_reorg"].get("skipped").is_none(),
        "a positive cadence must still run its final reorg check at shutdown: {}",
        s["final_reorg"]
    );
    assert!(
        s["final_repair"].get("skipped").is_none(),
        "a positive cadence must still run its final repair pass at shutdown: {}",
        s["final_repair"]
    );
    // The startup-boundary work (restart gap, one final seal at shutdown)
    // is unchanged — the point is that the SCHEDULES added nothing.
    assert!(
        s["seal_passes"].as_i64().unwrap_or(0) == 0,
        "no scheduled seal pass may run within the window: {}",
        s["seal_passes"]
    );
}

// ---------------------------------------------------------------------------
// G2: a tight POSITIVE cadence still runs — the schedule keeps working for
// everyone who actually asked for it.
// ---------------------------------------------------------------------------
#[test]
fn a_tight_positive_cadence_still_runs() {
    let dir = planted_dir("tight-cadence");
    let (port, _count) = chain_server(true);
    let out = run(
        &dir,
        &[
            "start",
            "--db",
            "reeflog.db",
            "--program",
            VALID_PROGRAM,
            "--seconds",
            "8",
            "--ws",
            &format!("ws://127.0.0.1:{port}"),
            "--rpc",
            &format!("http://127.0.0.1:{port}"),
            "--seal-every",
            "1",
            "--reorg-every",
            "1",
            "--repair-every",
            "1",
            "--webhook-every",
            "1",
        ],
    );
    assert!(out.status.success(), "the run must exit clean");
    let s = report_of(&out);
    assert!(
        s["seal_passes"].as_i64().unwrap_or(0) >= 2,
        "a 1 s seal cadence must run several passes: {}",
        s["seal_passes"]
    );
    assert!(
        s["reorg_passes"].as_i64().unwrap_or(0) >= 2,
        "a 1 s reorg cadence must run several passes: {}",
        s["reorg_passes"]
    );
    assert!(
        s["final_reorg"].get("skipped").is_none(),
        "a reorg schedule that ran must not claim skipped at shutdown"
    );
}

// ---------------------------------------------------------------------------
// G3: the flag domain keeps refusing an unschedulable cadence — the zero
// rule narrows one corner of the domain, it must not widen another.
// ---------------------------------------------------------------------------
#[test]
fn an_unschedulable_cadence_is_still_refused() {
    let dir = unique_dir("unschedulable");
    let out = run(
        &dir,
        &[
            "start",
            "--db",
            "reeflog.db",
            "--program",
            VALID_PROGRAM,
            "--seconds",
            "1",
            "--repair-every",
            "18446744073709551615",
        ],
    );
    assert!(
        !out.status.success(),
        "a cadence the clock cannot schedule must still be refused"
    );
    assert!(
        stderr_of(&out).contains("--repair-every"),
        "the refusal must name the flag"
    );
}
