//! One disconnect episode leaves ONE healed gap row, not one row per attempt.
//!
//! PULS AQ, 2026-11-18. The class is the one this repo keeps closing: a
//! report that names a problem that does not exist. `gaps_open` is a
//! documented field of `reeflog status` (and of the Node client's
//! `status()`); it is the field an operator reads to decide whether the
//! mailbox is whole. A healed journal that still reports `gaps_open: N`
//! is a permanent false alarm — exactly the defect class v0.7.27 removed
//! from `hole` rows (PULS AJ: `verify` opened one row per SCAN, `repair`
//! closed only the oldest; measured on the published v0.7.26 asset).
//!
//! The NEW face: the daemon opens a fresh `connect_fail` row for EVERY
//! failed connect attempt (`src/main.rs`, the reconnect loop:
//! `open_gap = Some(j.gap_open("connect_fail", ...))` overwrites the handle
//! but never closes or reuses the previous row), and `subscribe_fail` does
//! the same. Only the LAST row of the episode is closed on reconnect
//! (`open_gap.take()`). Measured with the same probe on the pre-fix tree
//! (`tools/measure-gap-episode-dedup.mjs`,
//! `research/gap-episode-dedup-prefix-2026-11-18.json`, verdict
//! `gap-episode-rows-accumulate`, 7 of 8 faces defective) and on the
//! published v0.7.34 asset
//! (`research/gap-episode-dedup-published-v0734-2026-11-18.json`): one
//! episode with 6 refused connect attempts leaves 6 OPEN `connect_fail`
//! rows (`gaps: 6, gaps_open: 6`); a second episode adds rows without ever
//! resolving the first; a healed journal holds `gaps_open: 7` — forever,
//! and a nightly network down-switch reopens the alarm every day.
//!
//! The rule this gate pins (mirrors `tests/gap_ledger.rs`, PULS AJ):
//!   * one episode = ONE row per kind while nothing connects: each failed
//!     attempt accumulates its detail on the already-open row instead of
//!     inserting a duplicate;
//!   * the episode stays OPEN while the endpoint is down — an outage is
//!     loud until the stream returns; `gaps_open` counts it honestly;
//!   * the reconnect heal closes the WHOLE episode: every open
//!     `connect_fail`/`subscribe_fail` row the same episode wrote is
//!     resolved, with the backfill count on the connect row so
//!     `SUM(backfill_count)` still counts fills, not rows;
//!   * the episode stays HISTORY: `gaps` keeps counting resolved episodes;
//!   * a TRUNCATED heal is not a heal: the episode stays open and loud —
//!     alarm beats silence.
//!
//! The fakes: a live sink (real WebSocket upgrade, then the signatures
//! page over plain HTTP — the daemon only resumes after a real upgrade)
//! and a sink whose signature walk answers one FULL page and resets the
//! next call (the truncated-heal face).
//!
//! Guards (green on BOTH trees): no episode at all keeps `gaps: 0,
//! gaps_open: 0`; the boundary restart gap on a journal WITH events keeps
//! its own row (kind `restart`), unaffected by this fix.

use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

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

fn status(dir: &Path) -> serde_json::Value {
    let out = run(dir, &["status", "--db", "reeflog.db"]);
    serde_json::from_str(String::from_utf8_lossy(&out.stdout).trim())
        .expect("status prints a JSON report")
}

#[derive(Debug, Clone)]
struct Gap {
    #[allow(dead_code)]
    id: i64,
    kind: String,
    open: bool,
    detail: String,
}

fn gaps(dir: &Path) -> Vec<Gap> {
    let conn = rusqlite::Connection::open(dir.join("reeflog.db")).expect("open journal");
    let mut stmt = conn
        .prepare("SELECT id, kind, resolved_at IS NULL, detail FROM gaps ORDER BY id")
        .expect("prepare");
    let rows = stmt
        .query_map([], |r| {
            Ok(Gap {
                id: r.get(0)?,
                kind: r.get(1)?,
                open: r.get::<_, i64>(2)? != 0,
                detail: r.get(3)?,
            })
        })
        .expect("query");
    rows.map(|r| r.expect("row")).collect()
}

/// A journal with events, so `start` opens the boundary restart gap
/// (`kind="restart"`) on the first run — the row this fix must leave alone.
/// The 127.0.0.1:9 endpoints refuse every connection: the ONLY other gap
/// rows this run can write are `connect_fail` rows, one per refused attempt
/// pre-fix, one per EPISODE post-fix.
fn planted_dir(tag: &str) -> PathBuf {
    let dir = unique_dir(tag);
    let out = run(&dir, &["init", "--db", "reeflog.db", "--program", VALID_PROGRAM]);
    assert!(out.status.success(), "init must succeed");
    let conn = rusqlite::Connection::open(dir.join("reeflog.db")).expect("open journal");
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_millis() as i64;
    for (sig, slot) in [("sig1", 1), ("sig2", 2)] {
        conn.execute(
            "INSERT INTO events(signature, slot, err, logs, source, first_seen, payload_state)
             VALUES (?1, ?2, NULL, '[\"log\"]', 'ws', ?3, 'held')",
            rusqlite::params![sig, slot, now],
        )
        .expect("plant");
    }
    dir
}

/// Run the daemon against the dead endpoint until the ledger holds at least
/// `attempts` rows of kind `want_kind`, then SIGKILL it.
fn dead_start(dir: &Path, want_kind: &str, attempts: usize) {
    let mut child = Command::new(env!("CARGO_BIN_EXE_reeflog"))
        .args([
            "start",
            "--db",
            "reeflog.db",
            "--program",
            VALID_PROGRAM,
            "--seconds",
            "3600",
            "--ws",
            "ws://127.0.0.1:9",
            "--rpc",
            "http://127.0.0.1:9",
            "--repair-every",
            "0",
            "--seal-every",
            "0",
            "--reorg-every",
            "0",
            "--webhook-every",
            "0",
        ])
        .current_dir(dir)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("daemon spawns");
    // Post-fix the episode is ONE row: the attempts ride on its detail
    // (the first attempt writes the text, every retry appends), so the
    // stop condition counts attempts, not rows.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
    loop {
        let tries: usize = gaps(dir)
            .iter()
            .filter(|g| g.kind == want_kind)
            .map(|g| g.detail.matches("; ws connect").count() + 1)
            .sum();
        if tries >= attempts || std::time::Instant::now() >= deadline {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    let _ = child.kill();
    let _ = child.wait();
}

/// What a test sink does.
#[derive(Clone, Copy)]
enum SinkKind {
    /// Real WebSocket upgrade + the signatures page over plain HTTP.
    Live,
    /// Real WebSocket upgrade; the FIRST signatures call is served (one
    /// full synthetic page), every later call resets without answering —
    /// the walk fills one page, then truncates, and the heal's own proof
    /// walk can never complete.
    FullPageThenFail,
}

/// The sink a healed episode points at. The daemon only resumes after a
/// REAL WebSocket upgrade, so the sink performs one: the daemon's handshake
/// key comes back as the RFC 6455 accept value, the subscription lands, and
/// the signature walk rides on separate plain-HTTP connections. The
/// upgraded socket is held open well past every run window, so a run exits
/// at its own deadline — never through a disconnect it would record.
fn chain_server(kind: SinkKind, signatures: &[&str]) -> (u16, std::thread::JoinHandle<()>) {
    use sha1::{Digest, Sha1};
    use base64::Engine as _;
    const WS_GUID: &str = "258EAFA5-E914-47DA-95CA-C5AB0DC85B11";
    let owned: Vec<String> = signatures.iter().map(|s| s.to_string()).collect();
    // FullPageThenFail: the FIRST signature walk gets one full page, every
    // later call resets — so one walk fills and truncates, and the next walk
    // (the heal's own proof) can never complete.
    let walk_calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().expect("addr").port();
    let handle = std::thread::spawn(move || {
        for stream in listener.incoming() {
            let mut s = match stream {
                Ok(s) => s,
                Err(_) => break,
            };
            // One thread per connection: a held upgraded socket must never
            // block the daemon's NEXT connection — its RPC calls arrive on
            // separate HTTP connects while the WebSocket stays open.
            let owned = owned.clone();
            let walk_calls = walk_calls.clone();
            let _ = std::thread::spawn(move || {
                use std::io::{Read as _, Write as _};
                // Read the whole request: a header OR body can arrive split
                // across TCP segments, and a sink that answers only complete
                // requests keeps the heal from ever proving its claim
                // (intermittent "episode row is resolved" failures came from
                // a single recv() that caught a partial head).
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
                let text = String::from_utf8_lossy(&acc[..head_end]).to_string();
                let head = text.clone();
                let wants_ws = head.lines().any(|l| {
                    l.split_once(':').is_some_and(|(k, v)| {
                        k.trim().eq_ignore_ascii_case("upgrade")
                            && v.trim().eq_ignore_ascii_case("websocket")
                    })
                });
                if wants_ws {
                    let key = head.lines().find_map(|l| {
                        let (k, v) = l.split_once(':')?;
                        if k.trim().eq_ignore_ascii_case("sec-websocket-key") {
                            Some(v.trim().to_string())
                        } else {
                            None
                        }
                    });
                    let Some(key) = key else { return };
                    let mut h = Sha1::new();
                    h.update(key.as_bytes());
                    h.update(WS_GUID.as_bytes());
                    let accept = base64::engine::general_purpose::STANDARD.encode(h.finalize());
                    let resp = format!(
                        "HTTP/1.1 101 Switching Protocols\r\nupgrade: websocket\r\nconnection: upgrade\r\nsec-websocket-accept: {accept}\r\n\r\n"
                    );
                    let _ = s.write_all(resp.as_bytes());
                    let _ = s.flush();
                    // Hold the upgraded socket open past every run window:
                    // the daemon must end its run at its own deadline, not
                    // through a close it would journal as an episode.
                    std::thread::sleep(std::time::Duration::from_secs(30));
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
                let body = String::from_utf8_lossy(&acc[head_end..head_end + content_length]).to_string();
                let parsed: Result<serde_json::Value, _> = serde_json::from_str(&body);
                let Ok(v) = parsed else { return };
                if v["method"] == "getSignaturesForAddress" {
                    // FullPageThenFail: the first walk is served, every
                    // later call (paged or not) resets without answering —
                    // the proof is interrupted mid-claim.
                    if let SinkKind::FullPageThenFail = kind {
                        let n = walk_calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                        if n >= 1 {
                            return; // drop without answering -> the walk records truncation
                        }
                        let rows: Vec<serde_json::Value> = (0..1000)
                            .map(|i| {
                                serde_json::json!({
                                    "signature": format!("page-sig-{i:04}"),
                                    "slot": 10 + i as i64,
                                    "err": serde_json::Value::Null
                                })
                            })
                            .collect();
                        let payload = serde_json::json!({
                            "jsonrpc": "2.0", "id": 1, "result": rows
                        })
                        .to_string();
                        let head = format!(
                            "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
                            payload.len()
                        );
                        let _ = s.write_all(head.as_bytes());
                        let _ = s.write_all(payload.as_bytes());
                        let _ = s.flush();
                        return;
                    }
                    let resp = serde_json::json!({
                        "jsonrpc": "2.0", "id": 1,
                        "result": owned.iter().map(|sig| serde_json::json!({
                            "signature": sig, "slot": 1, "err": serde_json::Value::Null
                        })).collect::<Vec<_>>()
                    });
                    let payload = resp.to_string();
                    let head = format!(
                        "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
                        payload.len()
                    );
                    let _ = s.write_all(head.as_bytes());
                    let _ = s.write_all(payload.as_bytes());
                    let _ = s.flush();
                } else {
                    // seal/reorg touch `getSignaturesForAddress` too; the
                    // daemon's other ticks are disabled, and an unexpected
                    // method gets an empty page rather than a wrong answer.
                    let payload = serde_json::json!({"jsonrpc": "2.0", "id": 1, "result": []})
                        .to_string();
                    let head = format!(
                        "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
                        payload.len()
                    );
                    let _ = s.write_all(head.as_bytes());
                    let _ = s.write_all(payload.as_bytes());
                    let _ = s.flush();
                }
            });
        }
    });
    (port, handle)
}

fn episode_face_counts() -> (usize, usize) {
    // Warm the code so the faces below measure ONE build.
    let dir = unique_dir("gap-episode-warm");
    let out = run(&dir, &["init", "--db", "reeflog.db", "--program", VALID_PROGRAM]);
    assert!(out.status.success(), "init must succeed");
    let port = 9;
    let mut child = Command::new(env!("CARGO_BIN_EXE_reeflog"))
        .args([
            "start",
            "--db",
            "reeflog.db",
            "--program",
            VALID_PROGRAM,
            "--seconds",
            "2",
            "--ws",
            &format!("ws://127.0.0.1:{port}"),
            "--rpc",
            &format!("http://127.0.0.1:{port}"),
            "--repair-every",
            "0",
            "--seal-every",
            "0",
            "--reorg-every",
            "0",
            "--webhook-every",
            "0",
        ])
        .current_dir(&dir)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("daemon spawns");
    let _ = child.wait();

    // EPISODE 1: refused attempts leave the ledger.
    let dir = planted_dir("gap-episode-e1");
    dead_start(&dir, "connect_fail", 6);
    let g = gaps(&dir);
    let episode1: Vec<_> = g.iter().filter(|x| x.kind == "connect_fail").collect();
    assert_eq!(episode1.len(), 1, "one episode = ONE row: {episode1:#?}");
    assert!(episode1[0].open, "the episode row is open while nothing connects");
    assert!(
        episode1[0].detail.contains("ws connect"),
        "the row accumulates the refusal detail: {}",
        episode1[0].detail
    );
    assert!(
        episode1[0].detail.matches("ws connect").count() >= 2,
        "the retries ride on the ONE row: {}",
        episode1[0].detail
    );
    let st = status(&dir);
    assert_eq!(st["gaps_open"].as_i64(), Some(2), "restart + the open episode"); // restart row + episode
    assert_eq!(st["gaps"].as_i64(), Some(2));

    // EPISODE 1 heals: the chain answers, the daemon reconnects.
    let (port, _srv) = chain_server(SinkKind::Live, &["sig1", "sig2"]);
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
            "--repair-every",
            "0",
            "--seal-every",
            "0",
            "--reorg-every",
            "0",
            "--webhook-every",
            "0",
        ],
    );
    assert!(out.status.success(), "daemon on the live sink must exit clean");
    let g = gaps(&dir);
    let episode: Vec<_> = g.iter().filter(|x| x.kind == "connect_fail").collect();
    assert_eq!(
        episode.len(),
        1,
        "the episode stays exactly ONE row after the heal: {episode:#?}"
    );
    assert!(!episode[0].open, "the heal closes the episode row");
    let st = status(&dir);
    assert_eq!(
        st["gaps_open"].as_i64(),
        Some(1),
        "only the unhealed boundary restart row stays open"
    );
    (episode.len(), g.len())
}

/// The honest-alarm face: a heal whose signature walk TRUNCATES does not
/// resolve the episode. A partial heal must not read as a whole one — the
/// alarm beats the silence, and the episode stays open until a COMPLETE
/// walk proves the claim.
#[test]
fn a_truncated_heal_keeps_the_episode_open() {
    let dir = planted_dir("gap-episode-truncated");
    dead_start(&dir, "connect_fail", 2);
    let before = gaps(&dir);
    let episode: Vec<_> = before.iter().filter(|x| x.kind == "connect_fail").collect();
    assert_eq!(episode.len(), 1, "one episode, one row: {episode:#?}");

    // The sink answers ONE full page and then resets every later call: the
    // walk fills a page and truncates, so the heal cannot prove the claim.
    let (port, _srv) = chain_server(SinkKind::FullPageThenFail, &["sig1", "sig2"]);
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
            "--repair-every",
            "0",
            "--seal-every",
            "0",
            "--reorg-every",
            "0",
            "--webhook-every",
            "0",
        ],
    );
    assert!(out.status.success(), "daemon on the live sink must exit clean");
    let after = gaps(&dir);
    let episode: Vec<_> = after.iter().filter(|x| x.kind == "connect_fail").collect();
    assert_eq!(
        episode.len(),
        1,
        "the episode stays ONE row: {episode:#?}"
    );
    assert!(
        episode[0].open,
        "a truncated heal must not resolve the episode: {episode:#?}"
    );
    let st = status(&dir);
    assert!(
        st["gaps_open"].as_i64().unwrap_or(0) >= 1,
        "the alarm stays on: {st}"
    );
}

#[test]
fn one_episode_is_one_row_and_the_heal_closes_it() {
    let (_n_connect, _total) = episode_face_counts();
}

#[test]
fn a_second_episode_heals_without_resurrecting_or_duplicating_the_first() {
    // Episode 1 planted and left open.
    let dir = planted_dir("gap-episode-e2");
    dead_start(&dir, "connect_fail", 3);
    let before = gaps(&dir);
    let first: Vec<_> = before.iter().filter(|x| x.kind == "connect_fail").collect();
    assert_eq!(first.len(), 1, "one episode, one row: {first:#?}");

    // EPISODE 2 on the LIVE sink: the daemon connects inside episode 2's
    // own first attempt, which also heals whatever episode rows are open.
    let (port, _srv) = chain_server(SinkKind::Live, &["sig1", "sig2"]);
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
            "--repair-every",
            "0",
            "--seal-every",
            "0",
            "--reorg-every",
            "0",
            "--webhook-every",
            "0",
        ],
    );
    assert!(out.status.success(), "daemon on the live sink must exit clean");
    let after = gaps(&dir);
    let connect_rows: Vec<_> = after.iter().filter(|x| x.kind == "connect_fail").collect();
    assert_eq!(
        connect_rows.len(),
        1,
        "the healed episode is ONE resolved row, and episode 2 left no duplicate: {connect_rows:#?}"
    );
    assert!(
        !connect_rows[0].open,
        "the episode row is resolved: {connect_rows:#?} | daemon stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let st = status(&dir);
    assert_eq!(
        st["gaps"].as_i64(),
        Some(3),
        "episodes are history: the dead run's boundary + the one episode row + the heal run's own (closed) boundary"
    );
    assert_eq!(
        st["gaps_open"].as_i64(),
        Some(1),
        "only the boundary restart row is open"
    );
    assert!(
        connect_rows[0].detail.contains("ws connect"),
        "the resolved row still names the refusal: {}",
        connect_rows[0].detail
    );
}

/// A killed run can leave a `subscribe_fail` row behind — the wire gave up
/// between connect and subscribe. The NEXT run's heal must resolve it with
/// the episode: the daemon-level sweep across kinds is the face (the unit
/// tests call the helper directly and would not see a daemon routing around
/// it). The row is planted because a sink cannot deterministically make
/// `ws.send` itself fail — that window is a race, and a gate must not be a
/// coin flip.
#[test]
fn a_killed_runs_subscribe_fail_row_heals_with_the_episode() {
    let dir = planted_dir("gap-episode-sub");
    dead_start(&dir, "connect_fail", 1);
    let conn = rusqlite::Connection::open(dir.join("reeflog.db")).expect("open journal");
    conn.execute(
        "INSERT INTO gaps(kind, since_seq, since_slot, since_sig, detail, detected_at)
         VALUES('subscribe_fail', 2, 2, 'sig2',
                'subscribe failed: connection closed before acknowledgment (planted from a killed run)',
                1700000000000)",
        [],
    )
    .expect("plant subscribe_fail row");
    conn.close().expect("close journal");
    assert_eq!(
        gaps(&dir)
            .iter()
            .filter(|g| g.kind == "subscribe_fail" && g.open)
            .count(),
        1,
        "the killed run's open subscribe row is planted"
    );

    // The heal run: the stream comes back, the tail backfill completes, and
    // BOTH kinds of the episode resolve — connect_fail from the dead run and
    // the planted subscribe_fail.
    let (port, _srv) = chain_server(SinkKind::Live, &["sig1", "sig2"]);
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
            "--repair-every",
            "0",
            "--seal-every",
            "0",
            "--reorg-every",
            "0",
            "--webhook-every",
            "0",
        ],
    );
    assert!(out.status.success(), "daemon on the live sink must exit clean");
    let g = gaps(&dir);
    let subs: Vec<_> = g.iter().filter(|x| x.kind == "subscribe_fail").collect();
    assert_eq!(
        subs.len(),
        1,
        "the planted subscribe row is not duplicated: {subs:#?}"
    );
    assert!(
        !subs[0].open,
        "a killed run's subscribe_fail heals with the episode: {subs:#?} | daemon stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let connects: Vec<_> = g.iter().filter(|x| x.kind == "connect_fail").collect();
    assert!(
        !connects.is_empty() && connects.iter().all(|c| !c.open),
        "the dead run's connect episode heals with it: {connects:#?}"
    );
    let st = status(&dir);
    assert_eq!(
        st["gaps_open"].as_i64(),
        Some(1),
        "only the dead run's unhealed boundary restart row stays open: {st}"
    );
}

#[test]
fn guards_green_on_both_trees() {
    // G1: a journal with no episode keeps gaps: 0 / gaps_open: 0.
    let dir = unique_dir("gap-episode-clean");
    let out = run(&dir, &["init", "--db", "reeflog.db", "--program", VALID_PROGRAM]);
    assert!(out.status.success(), "init must succeed");
    let (port, _srv) = chain_server(SinkKind::Live, &[]);
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
            "--repair-every",
            "0",
            "--seal-every",
            "0",
            "--reorg-every",
            "0",
            "--webhook-every",
            "0",
        ],
    );
    assert!(out.status.success(), "clean run exits 0");
    let st = status(&dir);
    assert_eq!(
        st["gaps_open"].as_i64(),
        Some(0),
        "a run that never disconnects owes nothing"
    );
    let g = gaps(&dir);
    assert!(g.is_empty(), "no episode, no rows: {g:#?}");

    // G2: the boundary restart gap on a journal WITH events keeps its own
    // row (kind `restart`) — this fix must not touch it.
    let dir = planted_dir("gap-episode-guard2");
    dead_start(&dir, "connect_fail", 1);
    let kinds: Vec<String> = gaps(&dir).iter().map(|g| g.kind.clone()).collect();
    assert!(
        kinds.contains(&"restart".to_string()),
        "the boundary row keeps its kind: {kinds:#?}"
    );
    assert!(
        kinds.iter().filter(|k| **k == "connect_fail").count() <= 1,
        "one refused attempt leaves ONE episode row (pre-fix: 1 too — the guard holds on both trees): {kinds:#?}"
    );
}
