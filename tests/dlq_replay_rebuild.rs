//! `dlq replay` must answer ONE fact with ONE body — the JOURNAL's current
//! row, the same body `webhook push` builds.
//!
//! PULS AT, 2026-11-16. Measured on the PUBLISHED v0.7.31 asset (the linux
//! archive whose sha256 equals the digest the Release API publishes;
//! `tools/measure-dlq-replay-rebuild.mjs`,
//! `research/dlq-replay-rebuild-published-v0731-2026-11-16.json`, verdict
//! `dlq-replay-rebuild-blind`):
//!
//! * the consumer refused an event once → quarantined with the bytes of
//!   that moment (`seal: "finalized"`);
//! * the chain unmade the transaction — journal `seal_status='reverted'`,
//!   a `reorgs` row written (exactly the two writes `mark_reverted` does);
//! * `webhook push` delivered the RETRACTION to that consumer, and a fresh
//!   second target read the event through `push` with `seal: "reverted"`;
//! * `webhook dlq replay` then re-sent the FROZEN body — the consumer's
//!   last word for that signature claimed `seal: "finalized"`, a claim the
//!   chain had unmade, and the report said `replayed: 1` with nothing
//!   naming the divergence.
//!
//! The quarantine stores a SNAPSHOT of a moment; the journal is the fact.
//! Only `push` ever consulted it (`deliver_events` reads `events_after`);
//! the replay read the freeze, so the same seq reached a consumer two
//! different ways depending on which command delivered it — and the way
//! only the replay could deliver was the stale one.
//!
//! The rule enforced here:
//!
//! * the replay rebuilds the body from `event_by_seq` when the journal
//!   still holds the row, so replay and `push` deliver the SAME bytes for
//!   the SAME seq (D1/D2);
//! * the report names the letters whose body came from the journal rather
//!   than the freeze — `rebuilt`, per target and on top (D3);
//! * an UNCHANGED letter still replays byte-for-byte: the documented
//!   signature guarantee holds where it is true (G1);
//! * a letter with NO journal row (planted, or older than the events
//!   table) still replays its stored bytes and exits 0 — nothing is ever
//!   dropped (G2);
//! * a replay refused again stays pending and exits 3 — the queue keeps
//!   holding it (G3).
//!
//! Falsified by construction: point the replay back at `r.body` and D1/D2
//! go red; drop `rebuilt` from the report and D3 goes red; make the
//! rebuild unconditional (no row → send nothing) and G2 goes red.

use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
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

fn stdout_of(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn stderr_of(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

fn report_of(out: &Output) -> Option<serde_json::Value> {
    serde_json::from_str(stdout_of(&out).trim()).ok()
}

/// A receiver on two paths: `/hook` (t1) and `/hook2` (t2), recording every
/// body it accepted, refusing `/hook` on demand. Non-blocking: the binary
/// under test runs in its own process, but a listener that blocks would
/// still time out on a burst.
fn receiver() -> (
    String,
    Arc<Mutex<Vec<(String, String)>>>,
    Arc<AtomicBool>,
    Arc<AtomicUsize>,
    Arc<AtomicUsize>,
    std::thread::JoinHandle<()>,
) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    listener.set_nonblocking(true).expect("nonblocking");
    let addr = listener.local_addr().expect("addr");
    let bodies = Arc::new(Mutex::new(Vec::<(String, String)>::new()));
    let stop = Arc::new(AtomicBool::new(false));
    let refuse_next = Arc::new(AtomicUsize::new(0));
    let refuse_all = Arc::new(AtomicUsize::new(0));
    let (b2, s2, r2, a2) = (bodies.clone(), stop.clone(), refuse_next.clone(), refuse_all.clone());
    let handle = std::thread::spawn(move || {
        while !s2.load(Ordering::Relaxed) {
            match listener.accept() {
                Ok((mut s, _)) => {
                    let (b3, r3, a3) = (b2.clone(), r2.clone(), a2.clone());
                    std::thread::spawn(move || {
                        s.set_nonblocking(true).expect("nonblocking");
                        let mut buf = [0u8; 65536];
                        let mut got = 0usize;
                        for _ in 0..500 {
                            match s.read(&mut buf[got..]) {
                                Ok(0) => break,
                                Ok(n) => {
                                    got += n;
                                    if buf[..got].windows(4).any(|w| w == b"\r\n\r\n") {
                                        break;
                                    }
                                }
                                Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                                    std::thread::sleep(Duration::from_millis(2));
                                }
                                Err(_) => break,
                            }
                        }
                        let head = String::from_utf8_lossy(&buf[..got]).into_owned();
                        let path = head
                            .split_whitespace()
                            .nth(1)
                            .unwrap_or("/")
                            .to_string();
                        let body = match head.find("\r\n\r\n") {
                            Some(i) => String::from_utf8_lossy(&buf[i + 4..got]).into_owned(),
                            None => String::new(),
                        };
                        let is_hook = path.starts_with("/hook") && !path.starts_with("/hook2");
                        let mut refuse = false;
                        if is_hook {
                            if a3.load(Ordering::Relaxed) == 1 {
                                refuse = true;
                            } else if r3.load(Ordering::Relaxed) > 0 {
                                r3.fetch_sub(1, Ordering::Relaxed);
                                refuse = true;
                            }
                        }
                        let resp: &[u8] = if refuse {
                            b"HTTP/1.1 403 Forbidden\r\ncontent-length: 8\r\n\r\n{\"e\":\"no\"}"
                        } else {
                            b"HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: 2\r\n\r\n{}"
                        };
                        let _ = s.write_all(resp);
                        let _ = s.flush();
                        b3.lock().expect("bodies").push((path, body));
                    });
                }
                Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(5));
                }
                Err(_) => break,
            }
        }
    });
    (
        format!("http://{addr}"),
        bodies,
        stop,
        refuse_next,
        refuse_all,
        handle,
    )
}

fn init(dir: &Path) {
    let out = run(dir, &["init", "--db", "reeflog.db", "--program", VALID_PROGRAM]);
    assert!(out.status.success(), "init: {}", stderr_of(&out));
}

/// One event, sealed finalized, with content — the state at ingest.
fn plant_event(dir: &Path, signature: &str, slot: i64) {
    let conn = rusqlite::Connection::open(dir.join("reeflog.db")).expect("open journal");
    conn.execute(
        "INSERT INTO events(signature, slot, err, logs, source, first_seen, seal_status, payload_state)
         VALUES(?1, ?2, NULL, '[\"log one\"]', 'ws', 1700000000000, 'finalized', 'held')",
        rusqlite::params![signature, slot],
    )
    .expect("plant event");
    conn.close().ok();
}

/// The chain unmakes the transaction: seal flips, reorg record appears —
/// exactly the two writes `mark_reverted` performs.
fn unmake(dir: &Path, signature: &str, slot: i64) {
    let conn = rusqlite::Connection::open(dir.join("reeflog.db")).expect("open journal");
    conn.execute(
        "UPDATE events SET seal_status='reverted', seal_seen=1700000000001 WHERE signature=?1",
        rusqlite::params![signature],
    )
    .expect("flip seal");
    conn.execute(
        "INSERT INTO reorgs(signature, slot, detected_at, status_source, tx_source, detail)
         VALUES(?1, ?2, 1700000000001, 'status', 'tx', 'planted by the domain test')",
        rusqlite::params![signature, slot],
    )
    .expect("plant reorg");
    conn.close().ok();
}

fn write_config(dir: &Path, json: &str) -> PathBuf {
    let cfg = dir.join("wh.json");
    std::fs::write(&cfg, json).expect("write config");
    cfg
}

fn t1(base: &str) -> String {
    format!(
        r#"{{"id":"t1","url":"{base}/hook","secret":"s1","timeout_ms":5000,"dead_letter_after":1}}"#
    )
}

fn t2(base: &str) -> String {
    format!(
        r#"{{"id":"t2","url":"{base}/hook2","secret":"s2","timeout_ms":5000,"dead_letter_after":1}}"#
    )
}

fn push(dir: &Path, cfg: &Path) -> Output {
    run(
        dir,
        &["webhook", "push", "--db", "reeflog.db", "--config", cfg.to_str().expect("cfg")],
    )
}

fn replay(dir: &Path, cfg: &Path) -> Output {
    run(
        dir,
        &[
            "webhook", "dlq", "replay", "--db", "reeflog.db", "--config",
            cfg.to_str().expect("cfg"),
        ],
    )
}

fn journal_seal(dir: &Path, signature: &str) -> Option<String> {
    let conn = rusqlite::Connection::open(dir.join("reeflog.db")).expect("open journal");
    let v = conn
        .query_row(
            "SELECT seal_status FROM events WHERE signature=?1",
            rusqlite::params![signature],
            |r| r.get::<_, Option<String>>(0),
        )
        .ok()?;
    conn.close().ok();
    v
}

fn letter_state(dir: &Path, target: &str) -> Option<String> {
    let conn = rusqlite::Connection::open(dir.join("reeflog.db")).expect("open journal");
    let v = conn
        .query_row(
            "SELECT state FROM webhook_dead_letters WHERE target=?1",
            rusqlite::params![target],
            |r| r.get::<_, String>(0),
        )
        .ok();
    conn.close().ok();
    v
}

/// Wait until the receiver has seen at least `n` bodies (the binary runs in
/// its own process; delivery is written before it exits, but the socket
/// thread may lag by a moment).
fn wait_for(bodies: &Mutex<Vec<(String, String)>>, n: usize) {
    for _ in 0..400 {
        if bodies.lock().expect("bodies").len() >= n {
            return;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

// ------------------------------------------------------------------ defects

/// D1: the body the replay sends must carry the seal the JOURNAL holds.
#[test]
fn d1_the_replayed_body_carries_the_seals_the_journal_holds() {
    let dir = unique_dir("d1-seal");
    init(&dir);
    let (base, bodies, stop, refuse_next, refuse_all, h) = receiver();
    let cfg = write_config(&dir, &format!(r#"{{"targets":[{}]}}"#, t1(&base)));

    plant_event(&dir, "SigDomainRevert111111111111111111111111111", 44000011);
    refuse_next.store(1, Ordering::Relaxed);
    let push1 = push(&dir, &cfg);
    assert_eq!(
        push1.status.code(),
        Some(3),
        "the refusal blocks the pass: {}",
        stderr_of(&push1)
    );
    assert_eq!(letter_state(&dir, "t1").as_deref(), Some("pending"), "quarantined");

    // The chain unmakes the transaction.
    unmake(&dir, "SigDomainRevert111111111111111111111111111", 44000011);
    assert_eq!(
        journal_seal(&dir, "SigDomainRevert111111111111111111111111111").as_deref(),
        Some("reverted"),
        "the journal now says reverted"
    );

    let before = bodies.lock().expect("bodies").len();
    let out = replay(&dir, &cfg);
    wait_for(&bodies, before + 1);
    stop.store(true, Ordering::Relaxed);
    refuse_all.store(0, Ordering::Relaxed);
    let _ = h.join();

    let sent = bodies
        .lock()
        .expect("bodies")
        .iter()
        .skip(before)
        .find(|(p, _)| p == "/hook")
        .map(|(_, b)| b.clone());
    let sent = sent.expect("the replay delivers one body");
    let parsed: serde_json::Value = serde_json::from_str(&sent).expect("the body is JSON");

    assert_eq!(
        out.status.code(),
        Some(0),
        "a full drain exits 0: {}",
        stderr_of(&out)
    );
    assert_eq!(
        parsed["seal"].as_str(),
        Some("reverted"),
        "the replayed body must claim the seal the journal holds, not the one the quarantine froze (body: {sent})"
    );
    assert_eq!(
        letter_state(&dir, "t1").as_deref(),
        Some("replayed"),
        "the letter is drained"
    );
    // The receipt must name the bytes the consumer ACTUALLY saw — a receipt
    // written before the rebuild would audit a body that never left the
    // machine.
    let receipt = {
        let conn = rusqlite::Connection::open(dir.join("reeflog.db")).expect("open journal");
        conn.query_row(
            "SELECT body_sha256 FROM webhook_receipts WHERE target='t1' AND kind='event' AND seq=1",
            [],
            |r| r.get::<_, String>(0),
        )
        .expect("a receipt for the replayed event")
    };
    assert_eq!(
        receipt,
        sha256_hex(&sent),
        "the receipt must name the bytes that were delivered"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// The sha256 of some bytes, hex — the same digest the journal stores.
fn sha256_hex(bytes: &str) -> String {
    use sha2::Digest;
    let mut h = sha2::Sha256::new();
    h.update(bytes.as_bytes());
    hex::encode(h.finalize())
}

/// D2: replay and push must answer the SAME event with the SAME body —
/// one fact, one body, whichever command delivers it.
#[test]
fn d2_replay_and_push_deliver_the_same_bytes_for_the_same_seq() {
    let dir = unique_dir("d2-oneself");
    init(&dir);
    let (base, bodies, stop, refuse_next, refuse_all, h) = receiver();
    let cfg1 = write_config(&dir, &format!(r#"{{"targets":[{}]}}"#, t1(&base)));

    plant_event(&dir, "SigDomainSame1111111111111111111111111111", 44000012);
    refuse_next.store(1, Ordering::Relaxed);
    push(&dir, &cfg1);

    // The chain unmakes it; a FRESH second target now appears.
    unmake(&dir, "SigDomainSame1111111111111111111111111111", 44000012);
    let cfg2 = write_config(
        &dir,
        &format!(r#"{{"targets":[{},{}]}}"#, t1(&base), t2(&base)),
    );
    let before = bodies.lock().expect("bodies").len();
    push(&dir, &cfg2);
    // t2 receives: the event (seq 1) and the retraction.
    wait_for(&bodies, before + 2);

    let pushed = bodies
        .lock()
        .expect("bodies")
        .iter()
        .skip(before)
        .find(|(p, b)| p == "/hook2" && b.contains("\"type\":\"event\""))
        .map(|(_, b)| b.clone());
    let pushed = pushed.expect("push delivers the event to the fresh target");

    let before2 = bodies.lock().expect("bodies").len();
    replay(&dir, &cfg2);
    wait_for(&bodies, before2 + 1);
    stop.store(true, Ordering::Relaxed);
    refuse_all.store(0, Ordering::Relaxed);
    let _ = h.join();

    let replayed = bodies
        .lock()
        .expect("bodies")
        .iter()
        .skip(before2)
        .find(|(p, _)| p == "/hook")
        .map(|(_, b)| b.clone());
    let replayed = replayed.expect("the replay delivers one body");

    assert_eq!(
        replayed, pushed,
        "one journal, two surfaces — the replay must send the bytes `push` sends for the same seq"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// D3: the report must name the letters whose body came from the journal.
#[test]
fn d3_the_report_names_the_rebuilt_letters() {
    let dir = unique_dir("d3-report");
    init(&dir);
    let (base, _bodies, stop, refuse_next, refuse_all, h) = receiver();
    let cfg = write_config(&dir, &format!(r#"{{"targets":[{}]}}"#, t1(&base)));

    plant_event(&dir, "SigDomainReport11111111111111111111111111", 44000013);
    refuse_next.store(1, Ordering::Relaxed);
    push(&dir, &cfg);
    unmake(&dir, "SigDomainReport11111111111111111111111111", 44000013);

    let out = replay(&dir, &cfg);
    stop.store(true, Ordering::Relaxed);
    refuse_all.store(0, Ordering::Relaxed);
    let _ = h.join();

    let rep = report_of(&out).expect("a replay prints its report");
    assert_eq!(
        rep.get("rebuilt").and_then(|v| v.as_i64()),
        Some(1),
        "the report must name the letter whose body came from the journal (report: {rep})"
    );
    assert_eq!(
        rep["targets"][0].get("rebuilt").and_then(|v| v.as_i64()),
        Some(1),
        "per target as well (report: {rep})"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

// ------------------------------------------------------------------- guards

/// G1: an UNCHANGED letter still replays byte-for-byte — the documented
/// signature guarantee holds where it is true — and reports `rebuilt: 0`.
#[test]
fn guard_g1_an_unchanged_letter_still_replays_byte_for_byte() {
    let dir = unique_dir("g1-unchanged");
    init(&dir);
    let (base, bodies, stop, refuse_next, refuse_all, h) = receiver();
    let cfg = write_config(&dir, &format!(r#"{{"targets":[{}]}}"#, t1(&base)));

    plant_event(&dir, "SigDomainClean01111111111111111111111111", 44000021);
    refuse_next.store(1, Ordering::Relaxed);
    push(&dir, &cfg);

    let frozen = {
        let conn = rusqlite::Connection::open(dir.join("reeflog.db")).expect("open journal");
        conn.query_row(
            "SELECT body FROM webhook_dead_letters WHERE target='t1' AND kind='event'",
            [],
            |r| r.get::<_, String>(0),
        )
        .expect("the quarantined body")
    };

    let before = bodies.lock().expect("bodies").len();
    let out = replay(&dir, &cfg);
    wait_for(&bodies, before + 1);
    stop.store(true, Ordering::Relaxed);
    refuse_all.store(0, Ordering::Relaxed);
    let _ = h.join();

    let sent = bodies
        .lock()
        .expect("bodies")
        .iter()
        .skip(before)
        .find(|(p, _)| p == "/hook")
        .map(|(_, b)| b.clone())
        .expect("the replay delivers one body");
    assert_eq!(sent, frozen, "an unchanged row replays byte-for-byte");
    let rep = report_of(&out).expect("a report");
    assert_eq!(
        rep.get("rebuilt").and_then(|v| v.as_i64()),
        Some(0),
        "nothing was rebuilt (report: {rep})"
    );
    assert_eq!(out.status.code(), Some(0), "a full drain exits 0");
    let _ = std::fs::remove_dir_all(&dir);
}

/// G2: a letter with NO journal row replays its STORED bytes and exits 0 —
/// a planted or pre-events letter must never be dropped by the rebuild.
#[test]
fn guard_g2_a_letter_without_a_journal_row_replays_its_stored_bytes() {
    let dir = unique_dir("g2-norow");
    init(&dir);
    let (base, bodies, stop, _refuse_next, refuse_all, h) = receiver();
    let cfg = write_config(&dir, &format!(r#"{{"targets":[{}]}}"#, t1(&base)));

    let planted = r#"{"type":"event","seq":1,"planted":true}"#;
    {
        let conn = rusqlite::Connection::open(dir.join("reeflog.db")).expect("open journal");
        conn.execute(
            "INSERT INTO webhook_dead_letters(target,kind,seq,body,event_id,reason,status,attempts,
                                              first_failed_at,last_failed_at,state)
             VALUES('t1','event',1,?1,'t1:event:1','refused with http 403',403,1,1700000000000,1700000000000,'pending')",
            rusqlite::params![planted],
        )
        .expect("plant letter");
        conn.close().ok();
    }

    let out = replay(&dir, &cfg);
    wait_for(&bodies, 1);
    stop.store(true, Ordering::Relaxed);
    refuse_all.store(0, Ordering::Relaxed);
    let _ = h.join();

    let sent = bodies
        .lock()
        .expect("bodies")
        .iter()
        .find(|(p, _)| p == "/hook")
        .map(|(_, b)| b.clone())
        .expect("the replay delivers one body");
    assert_eq!(sent, planted, "no journal row → the stored bytes, verbatim");
    assert_eq!(out.status.code(), Some(0), "nothing was dropped: {}", stderr_of(&out));
    assert_eq!(letter_state(&dir, "t1").as_deref(), Some("replayed"));
    let rep = report_of(&out).expect("a report");
    assert_eq!(rep.get("rebuilt").and_then(|v| v.as_i64()), Some(0));
    let _ = std::fs::remove_dir_all(&dir);
}

/// G3: a replay refused again stays pending and exits 3 — the queue keeps
/// holding it, and the report names what is left.
#[test]
fn guard_g3_a_refused_replay_stays_pending_and_exits_three() {
    let dir = unique_dir("g3-refused");
    init(&dir);
    let (base, _bodies, stop, refuse_next, refuse_all, h) = receiver();
    let cfg = write_config(&dir, &format!(r#"{{"targets":[{}]}}"#, t1(&base)));

    plant_event(&dir, "SigDomainRefused111111111111111111111111", 44000031);
    refuse_next.store(1, Ordering::Relaxed);
    push(&dir, &cfg);
    assert_eq!(letter_state(&dir, "t1").as_deref(), Some("pending"));

    refuse_all.store(1, Ordering::Relaxed);
    let out = replay(&dir, &cfg);
    stop.store(true, Ordering::Relaxed);
    refuse_all.store(0, Ordering::Relaxed);
    let _ = h.join();

    assert_eq!(
        out.status.code(),
        Some(3),
        "a refusal keeps exit 3: {}",
        stderr_of(&out)
    );
    assert_eq!(letter_state(&dir, "t1").as_deref(), Some("pending"), "still queued");
    let rep = report_of(&out).expect("a report");
    assert_eq!(
        rep.get("failed").and_then(|v| v.as_i64()),
        Some(1),
        "the report counts the refusal (report: {rep})"
    );
    assert_eq!(
        rep.get("remaining_pending").and_then(|v| v.as_i64()),
        Some(1),
        "and what the queue still holds (report: {rep})"
    );
    let _ = std::fs::remove_dir_all(&dir);
}
