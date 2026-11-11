//! A webhook target's delivery state belongs to its ENDPOINT, not to its id.
//!
//! MEASURED 2026-11-11 against this tree (v0.7.14, 0f32da4) with two live
//! receivers and the binary's own report:
//!
//!   * `t1` -> receiver A, `webhook push` -> A received 3 of 3, exit 0;
//!   * SAME id `t1`, same secret, url repointed to receiver B,
//!     `webhook push` -> B received **0**, exit **0**,
//!     report `delivered: 0, blocked: false`, and `webhook status` then
//!     printed `delivered: 3` next to B's url while `pending_events: 0`.
//!
//! The journal holds one `webhook_state` row per target id and one receipt
//! table keyed by (target, kind, seq). Nothing records WHICH ENDPOINT those
//! cursors and receipts belong to. So a consumer that moves (new port after
//! a restart, new host, second environment behind the same id) is reported
//! healthy while it holds nothing — the same class v0.7.13 closed for cursor
//! FILES (`journal_id` in provenance): a position that does not say where it
//! applies reads as "already delivered" somewhere it never was.
//!
//! THE RULE NOW ENFORCED:
//!
//!   * the state row carries the url it was written for; a config whose url
//!     differs from the recorded one is REFUSED (exit 1) naming both urls and
//!     the repair (give the new endpoint its own target id) — before any
//!     cursor is read, so no bytes go out and the journal is untouched;
//!   * receipts are recorded per (target, endpoint): a receipt written for A
//!     can never silence a delivery to B;
//!   * a state row with no endpoint recorded (a journal written before this
//!     rule) is ADOPTED by the config's url on the first run and refused on
//!     any later change — adoption happens once and is reported.
//!
//! GUARDS (must pass on the pre-fix tree too, they describe old behaviour
//! that stays correct): the same url keeps delivering across passes with no
//! duplicates; two ids are independent; a blocked target still blocks with
//! exit 3.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

const PROGRAM: &str = "whirLbMiicVdio4qvUfM5KAg6Ct8VwpYzGff3uctyCc";

fn unique_dir(tag: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let dir = std::env::temp_dir().join(format!(
        "reeflog-endpoint-binding-{}-{}-{}",
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

fn stdout_of(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn stderr_of(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

/// Seed `n` sealed events with a local sqlite write (the same fixture
/// technique every webhook test in this repo uses). Each invocation gets its
/// own signature prefix: `events.signature` is UNIQUE, so a second seed in
/// the same journal must not collide with the first.
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
  d.exec(`INSERT INTO events(signature, slot, err, logs, source, first_seen, seal_status)
          VALUES ('SIG${{nonce}}_${{i}}${{'x'.repeat(60)}}', ${{44000000 + i}}, NULL, '[]', 'ws', 1700000000000, 'sealed')`);
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
        stderr_of(&out)
    );
}

fn write_config(dir: &Path, name: &str, targets: &[(&str, &str)]) -> PathBuf {
    let list: Vec<serde_json::Value> = targets
        .iter()
        .map(|(id, url)| {
            serde_json::json!({ "id": id, "url": url, "secret": "s" })
        })
        .collect();
    let path = dir.join(name);
    std::fs::write(
        &path,
        serde_json::to_string(&serde_json::json!({ "targets": list })).unwrap(),
    )
    .unwrap();
    path
}

/// A receiver that answers 200 to everything and counts what arrived.
struct Receiver {
    port: u16,
    hits: Arc<AtomicU64>,
    handle: Option<std::thread::JoinHandle<()>>,
    listener: Option<TcpListener>,
    stop: Arc<AtomicBool>,
}

impl Receiver {
    fn start() -> Receiver {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind receiver");
        let port = listener.local_addr().expect("addr").port();
        let hits = Arc::new(AtomicU64::new(0));
        let stop = Arc::new(AtomicBool::new(false));
        let h = hits.clone();
        let s = stop.clone();
        listener.set_nonblocking(true).ok();
        let handle = std::thread::spawn(move || loop {
            if s.load(Ordering::SeqCst) {
                break;
            }
            match listener.accept() {
                Ok((mut sock, _)) => {
                    sock.set_read_timeout(Some(Duration::from_secs(5))).ok();
                    let mut buf = [0u8; 65536];
                    let mut total = 0usize;
                    loop {
                        match sock.read(&mut buf) {
                            Ok(0) => break,
                            Ok(n) => {
                                total += n;
                                let head = String::from_utf8_lossy(&buf[..n.min(4096)]).to_string();
                                let done = head.contains("\r\n\r\n");
                                if total > 1_000_000 {
                                    break;
                                }
                                if done && !head.contains("chunked") {
                                    break;
                                }
                                if done {
                                    // naive: headers seen; the test bodies are
                                    // small enough to arrive in one read
                                    break;
                                }
                            }
                            Err(_) => break,
                        }
                    }
                    h.fetch_add(1, Ordering::SeqCst);
                    let _ = sock.write_all(
                        b"HTTP/1.1 200 OK\r\ncontent-length: 2\r\nconnection: close\r\n\r\nok",
                    );
                }
                Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(5));
                }
                Err(_) => break,
            }
        });
        // The listener is moved into the thread; keep only the port and stop
        // flag for the owner.
        Receiver {
            port,
            hits,
            handle: Some(handle),
            listener: None,
            stop,
        }
    }
}

impl Drop for Receiver {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        // Accept loop polls every 5ms; the thread exits on its own.
        let _ = self.handle.take();
    }
}

fn url(r: &Receiver) -> String {
    format!("http://127.0.0.1:{}/hook", r.port)
}

// ---------------------------------------------------------------------------
// D1: the measured defect — a repointed url must not inherit A's cursors.
// ---------------------------------------------------------------------------
#[test]
fn d1_repainted_url_is_refused_instead_of_starved() {
    let dir = unique_dir("d1");
    let db = dir.join("j.db");
    let out = run(&dir, &["init", "--db", db.to_str().unwrap(), "--program", PROGRAM]);
    assert_eq!(out.status.code(), Some(0), "{}", stderr_of(&out));
    seed_events(&db, 3);

    let a = Receiver::start();
    let b = Receiver::start();
    let cfg_a = write_config(&dir, "cfg-a.json", &[("t1", url(&a).as_str())]);
    let cfg_b = write_config(&dir, "cfg-b.json", &[("t1", url(&b).as_str())]);

    let push_a = run(
        &dir,
        &[
            "webhook",
            "push",
            "--db",
            db.to_str().unwrap(),
            "--config",
            cfg_a.to_str().unwrap(),
        ],
    );
    assert_eq!(push_a.status.code(), Some(0), "{}", stderr_of(&push_a));
    assert_eq!(a.hits.load(Ordering::SeqCst), 3, "A received all three");

    // Same id, same secret, new endpoint. The old tree delivered NOTHING to
    // B and exited 0; the new rule refuses before a single byte moves.
    let push_b = run(
        &dir,
        &[
            "webhook",
            "push",
            "--db",
            db.to_str().unwrap(),
            "--config",
            cfg_b.to_str().unwrap(),
        ],
    );
    assert_ne!(
        push_b.status.code(),
        Some(0),
        "a repointed endpoint must not report success: {}",
        stdout_of(&push_b)
    );
    let err = stderr_of(&push_b);
    assert!(
        err.contains(url(&a).as_str()) && err.contains(url(&b).as_str()),
        "the refusal must name BOTH endpoints (recorded and configured): {err}"
    );
    assert!(
        err.contains("t1"),
        "the refusal must name the target id: {err}"
    );
    assert!(
        err.contains("give the new endpoint its own target id"),
        "the refusal must name the repair: {err}"
    );
    assert_eq!(
        b.hits.load(Ordering::SeqCst),
        0,
        "the refused pass must not deliver to the new endpoint either"
    );
    // The journal is untouched: A's cursor still says 3 and A is still owed
    // nothing.
    let st = run(
        &dir,
        &[
            "webhook",
            "status",
            "--db",
            db.to_str().unwrap(),
            "--config",
            cfg_a.to_str().unwrap(),
        ],
    );
    let v: serde_json::Value = serde_json::from_str(&stdout_of(&st)).expect("status json");
    assert_eq!(
        v["targets"][0]["next_seq"], 3,
        "A's cursor must survive the refused pass unchanged"
    );
    std::fs::remove_dir_all(&dir).ok();
}

// ---------------------------------------------------------------------------
// D2: `webhook status` printed another endpoint's delivery record next to the
// configured url. It must refuse the same way push does.
// ---------------------------------------------------------------------------
#[test]
fn d2_status_refuses_a_repointed_url() {
    let dir = unique_dir("d2");
    let db = dir.join("j.db");
    let out = run(&dir, &["init", "--db", db.to_str().unwrap(), "--program", PROGRAM]);
    assert_eq!(out.status.code(), Some(0), "{}", stderr_of(&out));
    seed_events(&db, 3);

    let a = Receiver::start();
    let b = Receiver::start();
    let cfg_a = write_config(&dir, "cfg-a.json", &[("t1", url(&a).as_str())]);
    let cfg_b = write_config(&dir, "cfg-b.json", &[("t1", url(&b).as_str())]);

    let push_a = run(
        &dir,
        &[
            "webhook",
            "push",
            "--db",
            db.to_str().unwrap(),
            "--config",
            cfg_a.to_str().unwrap(),
        ],
    );
    assert_eq!(push_a.status.code(), Some(0), "{}", stderr_of(&push_a));

    let st = run(
        &dir,
        &[
            "webhook",
            "status",
            "--db",
            db.to_str().unwrap(),
            "--config",
            cfg_b.to_str().unwrap(),
        ],
    );
    assert_ne!(
        st.status.code(),
        Some(0),
        "status must not print A's deliveries next to B's url: {}",
        stdout_of(&st)
    );
    let err = stderr_of(&st);
    assert!(
        err.contains(url(&a).as_str()) && err.contains(url(&b).as_str()),
        "the refusal must name both endpoints: {err}"
    );
    std::fs::remove_dir_all(&dir).ok();
}

// ---------------------------------------------------------------------------
// D3: the dead-letter replay is a delivery path too — the stored bytes must
// not follow a repointed url under the same id.
// ---------------------------------------------------------------------------
#[test]
fn d3_dlq_replay_refuses_a_repointed_url() {
    let dir = unique_dir("d3");
    let db = dir.join("j.db");
    let out = run(&dir, &["init", "--db", db.to_str().unwrap(), "--program", PROGRAM]);
    assert_eq!(out.status.code(), Some(0), "{}", stderr_of(&out));
    seed_events(&db, 1);

    let a = Receiver::start();
    let b = Receiver::start();
    let cfg_a = write_config(&dir, "cfg-a.json", &[("t1", url(&a).as_str())]);
    let cfg_b = write_config(&dir, "cfg-b.json", &[("t1", url(&b).as_str())]);

    let push_a = run(
        &dir,
        &[
            "webhook",
            "push",
            "--db",
            db.to_str().unwrap(),
            "--config",
            cfg_a.to_str().unwrap(),
        ],
    );
    assert_eq!(push_a.status.code(), Some(0), "{}", stderr_of(&push_a));

    // Quarantine one letter under A's endpoint via the journal directly (the
    // same fixture technique dlq tests use), so the replay has something to
    // send.
    let js = r#"
const { DatabaseSync } = require('node:sqlite');
const d = new DatabaseSync(process.argv[1]);
d.exec(`INSERT INTO webhook_dead_letters
        (target,kind,seq,body,event_id,reason,status,attempts,first_failed_at,last_failed_at,state)
        VALUES ('t1','event',9,'{"type":"event","seq":9}','t1:event:9','refused with http 404',404,5,1,1,'pending')`);
"#;
    let out = Command::new("node")
        .args(["-e", js])
        .arg(&db)
        .output()
        .expect("node seeds a dead letter");
    assert!(out.status.success(), "{}", stderr_of(&out));

    let replay = run(
        &dir,
        &[
            "webhook",
            "dlq",
            "replay",
            "--db",
            db.to_str().unwrap(),
            "--config",
            cfg_b.to_str().unwrap(),
        ],
    );
    assert_ne!(
        replay.status.code(),
        Some(0),
        "replay must refuse a repointed endpoint: {}",
        stdout_of(&replay)
    );
    assert_eq!(
        b.hits.load(Ordering::SeqCst),
        0,
        "no byte of the quarantined body may reach the new endpoint"
    );
    // The letter stays pending — nothing was dropped.
    let js = r#"
const { DatabaseSync } = require('node:sqlite');
const d = new DatabaseSync(process.argv[1]);
const r = d.prepare("SELECT state FROM webhook_dead_letters WHERE target='t1' AND seq=9").get();
console.log(r.state);
"#;
    let out = Command::new("node")
        .args(["-e", js])
        .arg(&db)
        .output()
        .expect("node reads the letter");
    assert_eq!(
        String::from_utf8_lossy(&out.stdout).trim(),
        "pending",
        "a refused replay must leave the letter pending"
    );
    std::fs::remove_dir_all(&dir).ok();
}

// ---------------------------------------------------------------------------
// D4: legacy adoption — a journal written before this rule (state row with
// an empty endpoint) must KEEP WORKING on the url it was written for, with
// its cursors intact and no re-delivery, and refuse a repoint after.
// ---------------------------------------------------------------------------
#[test]
fn d4_a_legacy_journal_is_adopted_by_its_url_then_refused_on_repoint() {
    let dir = unique_dir("d4");
    let db = dir.join("j.db");
    let out = run(&dir, &["init", "--db", db.to_str().unwrap(), "--program", PROGRAM]);
    assert_eq!(out.status.code(), Some(0), "{}", stderr_of(&out));
    seed_events(&db, 3);

    let a = Receiver::start();
    let b = Receiver::start();
    let cfg_a = write_config(&dir, "cfg-a.json", &[("t1", url(&a).as_str())]);
    let cfg_b = write_config(&dir, "cfg-b.json", &[("t1", url(&b).as_str())]);

    // Forge the legacy shape exactly as a v0.7.14 journal holds it: a state
    // row with cursor 0 and NO endpoint (the column itself arrives with
    // v0.7.15's ALTER — empty is the legacy seat), plus ONE endpoint-less
    // receipt for event 1. The receipt is the real test of adoption: an
    // adopted receipt must silence event 1 (no re-delivery), an adoption
    // that stamps only the row would re-send it.
    let js = r#"
const { DatabaseSync } = require('node:sqlite');
const d = new DatabaseSync(process.argv[1]);
d.exec(`DELETE FROM webhook_state WHERE target='t1'`);
d.exec(`INSERT INTO webhook_state(target,endpoint,next_seq,next_reorg_id,updated_at)
        VALUES ('t1','',0,0,1700000000000)`);
d.exec(`DELETE FROM webhook_receipts WHERE target='t1'`);
d.exec(`INSERT INTO webhook_receipts(target,endpoint,kind,seq,body_sha256,delivered_at)
        VALUES ('t1','','event',1,'deadbeef',1700000000000)`);
"#;
    let out = Command::new("node")
        .args(["-e", js])
        .arg(&db)
        .output()
        .expect("node forges the legacy shape");
    assert!(out.status.success(), "{}", stderr_of(&out));

    // The same url: adoption, not refusal — and the remaining two events
    // are delivered exactly once (the receipt was adopted, not forgotten).
    let push_a2 = run(
        &dir,
        &[
            "webhook",
            "push",
            "--db",
            db.to_str().unwrap(),
            "--config",
            cfg_a.to_str().unwrap(),
        ],
    );
    assert_eq!(
        push_a2.status.code(),
        Some(0),
        "a legacy journal must keep working on its own url: {}",
        stderr_of(&push_a2)
    );
    let rep: serde_json::Value = serde_json::from_str(&stdout_of(&push_a2)).expect("push json");
    assert_eq!(
        rep["targets"][0]["delivered"], 2,
        "the adopted receipt silences event 1; events 2 and 3 are delivered once"
    );
    assert_eq!(
        rep["targets"][0]["next_seq"], 3,
        "the legacy cursor carried over through adoption"
    );
    let stamped = run(
        &dir,
        &[
            "webhook",
            "status",
            "--db",
            db.to_str().unwrap(),
            "--config",
            cfg_a.to_str().unwrap(),
        ],
    );
    let sv: serde_json::Value = serde_json::from_str(&stdout_of(&stamped)).expect("status json");
    assert_eq!(
        sv["targets"][0]["endpoint_recorded"], url(&a).as_str(),
        "the legacy row now names the url that adopted it"
    );

    // A different url against the SAME id is now a refusal: the adoption
    // happened once, and the old single-endpoint forgiveness is over.
    let push_b = run(
        &dir,
        &[
            "webhook",
            "push",
            "--db",
            db.to_str().unwrap(),
            "--config",
            cfg_b.to_str().unwrap(),
        ],
    );
    assert_ne!(
        push_b.status.code(),
        Some(0),
        "after adoption a repoint must be refused"
    );
    assert_eq!(
        b.hits.load(Ordering::SeqCst),
        0,
        "the refused repoint delivered nothing"
    );
    std::fs::remove_dir_all(&dir).ok();
}

// ---------------------------------------------------------------------------
// G1: the SAME url keeps working across passes — no duplicate delivery, no
// false refusal. (Must be green on the pre-fix tree: it is old behaviour
// that stays correct.)
// ---------------------------------------------------------------------------
#[test]
fn g1_same_url_delivers_exactly_once() {
    let dir = unique_dir("g1");
    let db = dir.join("j.db");
    let out = run(&dir, &["init", "--db", db.to_str().unwrap(), "--program", PROGRAM]);
    assert_eq!(out.status.code(), Some(0), "{}", stderr_of(&out));
    seed_events(&db, 3);

    let a = Receiver::start();
    let cfg = write_config(&dir, "cfg.json", &[("t1", url(&a).as_str())]);
    let push1 = run(
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
    assert_eq!(push1.status.code(), Some(0), "{}", stderr_of(&push1));
    let push2 = run(
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
    assert_eq!(push2.status.code(), Some(0), "{}", stderr_of(&push2));
    assert_eq!(
        a.hits.load(Ordering::SeqCst),
        3,
        "the second pass over a caught-up queue delivers nothing new"
    );
    // Two more events -> only the new ones go out.
    seed_events(&db, 5);
    let push3 = run(
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
    assert_eq!(push3.status.code(), Some(0), "{}", stderr_of(&push3));
    assert_eq!(
        a.hits.load(Ordering::SeqCst),
        8,
        "the five new events are delivered, the three already held are not re-sent"
    );
    std::fs::remove_dir_all(&dir).ok();
}

// ---------------------------------------------------------------------------
// G2: two target ids are two independent consumers, each with its own state.
// ---------------------------------------------------------------------------
#[test]
fn g2_two_ids_are_independent() {
    let dir = unique_dir("g2");
    let db = dir.join("j.db");
    let out = run(&dir, &["init", "--db", db.to_str().unwrap(), "--program", PROGRAM]);
    assert_eq!(out.status.code(), Some(0), "{}", stderr_of(&out));
    seed_events(&db, 3);

    let a = Receiver::start();
    let b = Receiver::start();
    let cfg = write_config(
        &dir,
        "cfg.json",
        &[("t1", url(&a).as_str()), ("t2", url(&b).as_str())],
    );
    let push = run(
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
    assert_eq!(push.status.code(), Some(0), "{}", stderr_of(&push));
    assert_eq!(a.hits.load(Ordering::SeqCst), 3);
    assert_eq!(b.hits.load(Ordering::SeqCst), 3);
    std::fs::remove_dir_all(&dir).ok();
}

// ---------------------------------------------------------------------------
// G3: a blocked target still blocks with exit 3 — the refusal on repointing
// must not replace the blocking contract.
// ---------------------------------------------------------------------------
#[test]
fn g3_blocked_target_still_blocks() {
    use std::net::TcpListener as L;
    let dir = unique_dir("g3");
    let db = dir.join("j.db");
    let out = run(&dir, &["init", "--db", db.to_str().unwrap(), "--program", PROGRAM]);
    assert_eq!(out.status.code(), Some(0), "{}", stderr_of(&out));
    seed_events(&db, 3);

    let listener = L::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().expect("addr").port();
    let stop = Arc::new(AtomicBool::new(false));
    let s = stop.clone();
    std::thread::spawn(move || {
        for _ in listener.incoming() {
            if s.load(Ordering::SeqCst) {
                break;
            }
            // Accept and answer 403 to everything.
            let mut accepted: Option<TcpStream> = None;
            while accepted.is_none() {
                match listener.accept() {
                    Ok((sock, _)) => accepted = Some(sock),
                    Err(_) => std::thread::sleep(Duration::from_millis(5)),
                }
            }
            let mut sock = accepted.unwrap();
            let mut buf = [0u8; 8192];
            let _ = sock.read(&mut buf);
            let _ = sock.write_all(
                b"HTTP/1.1 403 Forbidden\r\ncontent-length: 0\r\nconnection: close\r\n\r\n",
            );
        }
    });
    let refusing = format!("http://127.0.0.1:{}/hook", port);
    let cfg = write_config(&dir, "cfg.json", &[("t1", refusing.as_str())]);
    let push = run(
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
    stop.store(true, Ordering::SeqCst);
    assert_eq!(
        push.status.code(),
        Some(3),
        "a refusing endpoint must still block with exit 3: {}",
        stdout_of(&push)
    );
    std::fs::remove_dir_all(&dir).ok();
}
