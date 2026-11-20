//! A seal pass may only advance past rows it actually asked.
//!
//! PULS BC, 2026-11-20. The seal pass (`src/main.rs` `seal_batch`) asks
//! `getSignatureStatuses` for a batch of pending signatures and reads the
//! per-signature verdicts from `result.value[]`. The reorg module's
//! `status_sources` (src/reorg.rs) defends that contract explicitly —
//! `a.len() == sigs.len()`, anything else is `Source::Failed("bad response
//! shape")` for the whole batch — but the seal pass zipped `pending` with
//! `value` unconditionally: an answer array SHORTER than the request (a
//! truncated tail under load, a proxy cut, a sick endpoint that answers
//! what it has) sealed the rows the answer covered and moved the walk's
//! seq cursor past every row the answer DID NOT cover. Those rows were
//! never asked, and the pass still reported `complete: true` and exited 0 —
//! the exact lie PULS BA removed ("the pass says so when it cannot
//! finish"), reintroduced one answer-shape deeper.
//!
//! THE RULE NOW ENFORCED: `value.len() != pending.len()` is a failed
//! source, not a smaller success. The walk stops with a named
//! transport_error, `complete: false`, and the CLI exits 3 — every row is
//! re-asked by the next pass, no row is consumed without an answer.
//!
//! Faces (RED on the pre-fix tree, GREEN after the fix):
//!   D1 CLI: 10 pending rows, the sink answers floor(n/2) finalized rows —
//!      the pass must NOT exit 0 with `complete: true` while rows it never
//!      asked stay pending (pre-fix: 5 sealed, 5 unasked, exit 0,
//!      complete true);
//!   D2 daemon tick: the same short answer inside `start` — the tick must
//!      not advance the cursor past unanswered rows (pre-fix: 5 sealed, 5
//!      unasked at exit 0; post-fix: 0 sealed, all 10 still pending);
//!   D3 missing verdicts: the same short answer with null entries and a
//!      refusing second source — the rows BEHIND the truncated answer must
//!      not be dropped by a `complete: true` pass that promoted the first
//!      half as `missing`.
//!
//! Guards (GREEN on BOTH trees — the honest full-length forms must not
//! move):
//!   G1 a full-length finalized answer seals every row, exit 0;
//!   G2 a full-length all-null answer with a serving second source decides
//!      nothing (undecided rows stay pending, exit 0, complete true);
//!   G3 a full-length all-null answer with a refusing second source seals
//!      `missing` for every row (the two-source rule, unchanged).

use std::io::Write;
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
        "reeflog-seal-answer-{tag}-{}-{n}",
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
        .expect("reeflog binary runs")
}

fn stdout_of(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn stderr_of(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

fn report_of(out: &Output) -> serde_json::Value {
    serde_json::from_str(&stdout_of(out)).unwrap_or(serde_json::json!({}))
}

/// How the sink answers `getSignatureStatuses`:
/// Full — one entry per requested signature (the honest contract);
/// FloorHalf — only floor(n/2) entries: a truncated answer, the defect shape.
#[derive(Clone, Copy, PartialEq)]
enum AnswerLen {
    Full,
    FloorHalf,
}

/// What the answered entries carry, and what the second source does.
#[derive(Clone, Copy, PartialEq)]
enum Entry {
    Finalized,
    Null,
}
#[derive(Clone, Copy, PartialEq)]
enum SecondSource {
    Refuse,
    Serve,
}

struct SinkConfig {
    len: AnswerLen,
    entry: Entry,
    tx: SecondSource,
}

#[derive(Default)]
struct SinkCount {
    get_signature_statuses_req: usize,
    get_signature_statuses_ans: usize,
    get_transaction: usize,
    get_signatures_for_address: usize,
}

/// One port, the house roles: WS upgrade held open (only the daemon face
/// connects), JSON-RPC over plain HTTP for the seal pass, counted per method.
fn chain_server(cfg: SinkConfig) -> (u16, Arc<Mutex<SinkCount>>) {
    let count = Arc::new(Mutex::new(SinkCount::default()));
    let c2 = count.clone();
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().expect("addr").port();
    std::thread::spawn(move || {
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
                if head
                    .lines()
                    .any(|l| {
                        l.split_once(':').is_some_and(|(k, v)| {
                            k.trim().eq_ignore_ascii_case("upgrade")
                                && v.trim().eq_ignore_ascii_case("websocket")
                        })
                    })
                {
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
                let Ok(v) = serde_json::from_str::<serde_json::Value>(&body) else {
                    return;
                };
                let mut count = count.lock().unwrap();
                let payload = match v["method"].as_str().unwrap_or("") {
                    "getSignatureStatuses" => {
                        let req: Vec<String> = v["params"][0]
                            .as_array()
                            .map(|a| {
                                a.iter()
                                    .map(|x| x.as_str().unwrap_or_default().to_string())
                                    .collect()
                            })
                            .unwrap_or_default();
                        count.get_signature_statuses_req += req.len();
                        let answer = match cfg.len {
                            AnswerLen::Full => req.len(),
                            AnswerLen::FloorHalf => req.len() / 2,
                        };
                        count.get_signature_statuses_ans += answer;
                        let entry = match cfg.entry {
                            Entry::Finalized => serde_json::json!({
                                "confirmationStatus": "finalized",
                                "err": serde_json::Value::Null
                            }),
                            Entry::Null => serde_json::Value::Null,
                        };
                        let values: Vec<serde_json::Value> =
                            (0..answer).map(|_| entry.clone()).collect();
                        serde_json::json!({ "value": values })
                    }
                    "getTransaction" => {
                        count.get_transaction += 1;
                        match cfg.tx {
                            SecondSource::Refuse => serde_json::Value::Null,
                            SecondSource::Serve => serde_json::json!({ "slot": 1000 }),
                        }
                    }
                    "getSignaturesForAddress" => {
                        count.get_signatures_for_address += 1;
                        serde_json::json!([])
                    }
                    _ => serde_json::json!({}),
                };
                let payload =
                    serde_json::json!({"jsonrpc": "2.0", "id": v["id"].clone(), "result": payload})
                        .to_string();
                drop(count);
                let head = format!(
                    "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
                    payload.len()
                );
                let _ = s.write_all(head.as_bytes());
                let _ = s.write_all(payload.as_bytes());
                let _ = s.flush();
            });
        }
    });
    (port, count)
}

/// Plant `n` PENDING rows (seal_status NULL) — the fixture technique every
/// webhook test in this repo uses (a local sqlite write through node).
fn plant_pending(db: &Path, n: usize) {
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
d.exec(`CREATE TABLE IF NOT EXISTS events(seq INTEGER PRIMARY KEY AUTOINCREMENT,signature TEXT NOT NULL UNIQUE,slot INTEGER NOT NULL,err TEXT,logs TEXT,source TEXT NOT NULL DEFAULT 'ws',first_seen INTEGER NOT NULL,seal_status TEXT,seal_seen INTEGER,payload_state TEXT)`);
for (let i = 1; i <= n; i++) {{
  d.prepare(`INSERT INTO events(signature, slot, err, logs, source, first_seen)
             VALUES (?, ?, NULL, '[]', 'ws', 1700000000000)`).run(
    `BC${{nonce}}_${{i}}${{'x'.repeat(60)}}`, 44000000 + i);
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
    assert!(
        out.status.success(),
        "seeding failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[derive(Default, Clone, Copy)]
struct Counts {
    total: i64,
    finalized: i64,
    missing: i64,
    unsealed: i64,
}

fn counts(dir: &Path) -> Counts {
    let js = r#"
const { DatabaseSync } = require('node:sqlite');
const d = new DatabaseSync(process.argv[1], { readOnly: true });
const one = (q) => d.prepare(q).get().n;
console.log(JSON.stringify({
  total: one('SELECT COUNT(*) n FROM events'),
  finalized: one(`SELECT COUNT(*) n FROM events WHERE seal_status='finalized'`),
  missing: one(`SELECT COUNT(*) n FROM events WHERE seal_status='missing'`),
  unsealed: one('SELECT COUNT(*) n FROM events WHERE seal_status IS NULL'),
}));
"#;
    let out = Command::new("node")
        .args(["-e", js])
        .arg(dir.join("reeflog.db"))
        .output()
        .expect("node reads counts");
    assert!(
        out.status.success(),
        "counting failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_str::<serde_json::Value>(&String::from_utf8_lossy(&out.stdout))
        .map(|v| Counts {
            total: v["total"].as_i64().unwrap_or(0),
            finalized: v["finalized"].as_i64().unwrap_or(0),
            missing: v["missing"].as_i64().unwrap_or(0),
            unsealed: v["unsealed"].as_i64().unwrap_or(0),
        })
        .unwrap_or_default()
}

// --- D1: the CLI pass must not exit 0 over rows it never asked -------------
#[test]
fn d1_a_short_answer_is_not_a_complete_pass() {
    let dir = unique_dir("d1");
    plant_pending(&dir.join("reeflog.db"), 10);
    let (port, _c) = chain_server(SinkConfig {
        len: AnswerLen::FloorHalf,
        entry: Entry::Finalized,
        tx: SecondSource::Refuse,
    });
    let out = run(
        &dir,
        &["seal", "--db", "reeflog.db", "--rpc", &format!("http://127.0.0.1:{port}")],
    );
    let c = counts(&dir);
    let report = report_of(&out);
    let complete = report["seal_pass"]["complete"].as_bool();
    // The honest contract, in both directions: a pass that asked EVERY row
    // exits 0 with `complete: true`; a pass that did not exits 3 with
    // `complete: false`. A short answer made the pre-fix tree do NEITHER
    // honestly: it exited 0 with `complete: true` over 5 rows it never
    // asked.
    if complete == Some(true) {
        assert_eq!(
            out.status.code(),
            Some(0),
            "a complete pass exits 0; a pass that asked every row must not exit 3 \
             (stderr: {})",
            stderr_of(&out)
        );
        assert_eq!(
            c.unsealed, 0,
            "a pass that reports complete: true may not leave rows unasked"
        );
    } else {
        assert_eq!(
            out.status.code(),
            Some(3),
            "an incomplete pass exits 3 — the work is unfinished, not failed \
             (exit {:?}, complete {complete:?}, stderr: {})",
            out.status.code(),
            stderr_of(&out)
        );
    }
    // No row may be CONSUMED without an answer: the pass either asks it
    // (and seals it) or leaves it pending for the next pass.
    assert_eq!(
        c.total,
        c.finalized + c.missing + c.unsealed,
        "every row stays accounted for"
    );
}

// --- D2: the daemon tick must not advance past unanswered rows -------------
#[test]
fn d2_the_daemon_tick_does_not_consume_unanswered_rows() {
    let dir = unique_dir("d2");
    plant_pending(&dir.join("reeflog.db"), 10);
    let (port, _c) = chain_server(SinkConfig {
        len: AnswerLen::FloorHalf,
        entry: Entry::Finalized,
        tx: SecondSource::Refuse,
    });
    let rpc = format!("http://127.0.0.1:{port}");
    let ws = format!("ws://127.0.0.1:{port}");
    let out = run(
        &dir,
        &[
            "start",
            "--db",
            "reeflog.db",
            "--program",
            VALID_PROGRAM,
            "--rpc",
            &rpc,
            "--ws",
            &ws,
            "--seconds",
            "3",
            "--seal-every",
            "1",
            "--reorg-every",
            "0",
            "--repair-every",
            "0",
            "--webhook-every",
            "0",
        ],
    );
    let c = counts(&dir);
    // Post-fix the ticks record a transport failure and consume nothing:
    // every row is still pending at exit, and the run SAYS so — the same
    // voice the reorg and repair ticks carry for their incomplete passes.
    assert!(
        stderr_of(&out).contains("seal pass INCOMPLETE"),
        "an incomplete seal tick must be named on stderr (stderr: {})",
        stderr_of(&out)
    );
    assert_eq!(c.finalized, 0, "a truncated answer seals nothing: sealed={}", c.finalized);
    // Pre-fix the first tick sealed the covered half, moved the cursor past
    // the rest, and the run exited 0.
    assert!(
        out.status.code() == Some(0),
        "unasked rows stay pending for the next pass — the run itself is healthy"
    );
}

// --- D3: rows behind a truncated answer must not be dropped silently -------
#[test]
fn d3_missing_verdicts_behind_a_short_answer_are_not_a_complete_pass() {
    let dir = unique_dir("d3");
    plant_pending(&dir.join("reeflog.db"), 10);
    let (port, _c) = chain_server(SinkConfig {
        len: AnswerLen::FloorHalf,
        entry: Entry::Null,
        tx: SecondSource::Refuse,
    });
    let out = run(
        &dir,
        &["seal", "--db", "reeflog.db", "--rpc", &format!("http://127.0.0.1:{port}")],
    );
    let c = counts(&dir);
    let report = report_of(&out);
    let complete = report["seal_pass"]["complete"].as_bool();
    // Pre-fix: the covered half was promoted `missing` (legitimately for
    // those rows), the uncovered half was never asked, and the pass still
    // claimed `complete: true` at exit 0.
    if complete == Some(true) {
        assert_eq!(
            out.status.code(),
            Some(0),
            "a complete pass exits 0"
        );
        assert_eq!(
            c.unsealed, 0,
            "a pass that reports complete: true may not leave rows unasked"
        );
    } else {
        assert_eq!(
            out.status.code(),
            Some(3),
            "an incomplete pass exits 3 (exit {:?}, complete {complete:?}, \
             missing={} unsealed={}, stderr: {})",
            out.status.code(),
            c.missing,
            c.unsealed,
            stderr_of(&out)
        );
    }
}

// --- G1: a full-length answer seals everything (unchanged) -----------------
#[test]
fn g1_a_full_length_answer_still_seals_every_row() {
    let dir = unique_dir("g1");
    plant_pending(&dir.join("reeflog.db"), 10);
    let (port, _c) = chain_server(SinkConfig {
        len: AnswerLen::Full,
        entry: Entry::Finalized,
        tx: SecondSource::Refuse,
    });
    let out = run(
        &dir,
        &["seal", "--db", "reeflog.db", "--rpc", &format!("http://127.0.0.1:{port}")],
    );
    let c = counts(&dir);
    let report = report_of(&out);
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr_of(&out));
    assert_eq!(report["seal_pass"]["complete"].as_bool(), Some(true));
    assert_eq!(c.finalized, 10, "every row sealed");
    assert_eq!(c.unsealed, 0, "nothing left pending");
}

// --- G2: full-length nulls with a serving second source decide nothing -----
#[test]
fn g2_undecided_rows_stay_pending_through_a_full_answer() {
    let dir = unique_dir("g2");
    plant_pending(&dir.join("reeflog.db"), 10);
    let (port, _c) = chain_server(SinkConfig {
        len: AnswerLen::Full,
        entry: Entry::Null,
        tx: SecondSource::Serve,
    });
    let out = run(
        &dir,
        &["seal", "--db", "reeflog.db", "--rpc", &format!("http://127.0.0.1:{port}")],
    );
    let c = counts(&dir);
    let report = report_of(&out);
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr_of(&out));
    assert_eq!(report["seal_pass"]["complete"].as_bool(), Some(true));
    assert_eq!(c.missing, 0, "a served transaction is never a fork-out");
    assert_eq!(c.unsealed, 10, "undecided rows stay pending");
}

// --- G3: full-length nulls with a refusing second source seal missing ------
#[test]
fn g3_the_two_source_missing_path_keeps_working() {
    let dir = unique_dir("g3");
    plant_pending(&dir.join("reeflog.db"), 10);
    let (port, _c) = chain_server(SinkConfig {
        len: AnswerLen::Full,
        entry: Entry::Null,
        tx: SecondSource::Refuse,
    });
    let out = run(
        &dir,
        &["seal", "--db", "reeflog.db", "--rpc", &format!("http://127.0.0.1:{port}")],
    );
    let c = counts(&dir);
    let report = report_of(&out);
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr_of(&out));
    assert_eq!(report["seal_pass"]["complete"].as_bool(), Some(true));
    assert_eq!(c.missing, 10, "both sources refused: a genuine fork-out");
    assert_eq!(c.unsealed, 0);
}
