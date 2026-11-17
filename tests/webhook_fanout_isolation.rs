//! ONE misconfigured target must not silence the WHOLE fan-out.
//!
//! PULS AP, 2026-11-17. Measured on the tree at v0.7.33 (06694b0) with
//! `tools/measure-fanout-isolation.mjs` (three live sinks, the binary's own
//! report; verdict `fanout-isolation-blind`, 5/8 faces):
//!
//! * F1 `webhook push` on a config with a REPOINTED target and a healthy
//!   sibling: exit 1, **no report at all** (`repointed_push_report_present:
//!   false`), and the healthy consumer received **3 of 5** bodies — the two
//!   events that were pending while the sibling was misconfigured never
//!   left the machine. The abort happens at the first failing target in
//!   CONFIG ORDER, so a healthy target listed after the broken one starves
//!   as well.
//!
//! * F3 `webhook status` — the surface a monitoring caller polls — printed
//!   NOTHING for the same config: exit 1, no JSON. Not even the healthy
//!   target's own numbers.
//!
//! * F6 the refusal holds exit **1** (an abort), not the exit **3** this
//!   repo uses for "a target is blocked and the report says so". So the
//!   operator gets a bare error line and no report to read.
//!
//! The refusal itself is RIGHT: the v0.7.15 endpoint-binding rule refuses a
//! repointed target (a cursor is a promise to ONE endpoint — the measured
//! starvation, 2026-11-11) and no byte may reach the new endpoint (F5 holds
//! on the pre-fix tree and must keep holding). The defect is the BLAST
//! RADIUS: a misconfiguration is a fact about ONE consumer, and the other
//! consumers in the list must keep running and keep being reported.
//!
//! THE RULE NOW ENFORCED:
//!   * a target whose delivery state is bound to a different endpoint is
//!     REFUSED per target: the pass continues for every other target in
//!     the config, and the refusal is named IN THE REPORT (not only on
//!     stderr) so the operator learns which consumer is misconfigured;
//!   * the refused target holds the exit code (3), like any other blocked
//!     target — silence is not health, and an abort that prints no report
//!     is not a status a caller can act on;
//!   * the refused target's own state row is untouched and no byte reaches
//!     the new endpoint (the v0.7.15 rule survives the isolation).
//!
//! Guards (green on BOTH trees): a config where every target is healthy
//! delivers to all of them and exits 0 (G1); a config with a single healthy
//! target reports its own numbers and exits 0 (G2).

use std::io::{Read, Write};
use std::net::TcpListener;
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
        "reeflog-fanout-isolation-{}-{}-{}",
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

fn json_of(out: &Output) -> serde_json::Value {
    serde_json::from_str(&stdout_of(out)).unwrap_or_else(|e| {
        panic!(
            "the command must PRINT a report (a caller polls it): {e}\nstdout: {}\nstderr: {}",
            stdout_of(out),
            stderr_of(out)
        )
    })
}

/// Seed `n` events with their content, so the payload-debt rule does not
/// hold the queue: this gate is about the FAN-OUT, not about payloads.
/// Every call gets its own signature prefix (`events.signature` is UNIQUE).
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
  d.exec(`INSERT INTO events(signature, slot, err, logs, source, first_seen, seal_status, payload_state)
          VALUES ('F${{nonce}}_${{i}}${{'x'.repeat(40)}}', ${{44000000 + i}}, NULL, '[]', 'ws', 1700000000000, 'sealed', 'held')`);
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
    assert!(out.status.success(), "seeding failed: {}", stderr_of(&out));
}

fn write_config(dir: &Path, name: &str, targets: &[serde_json::Value]) -> PathBuf {
    let path = dir.join(name);
    std::fs::write(
        &path,
        serde_json::to_string(&serde_json::json!({ "targets": targets })).unwrap(),
    )
    .unwrap();
    path
}

/// A target entry. One secret per target identity (since v0.7.16 one shared
/// secret on two different endpoints is refused).
fn target(id: &str, url: &str) -> serde_json::Value {
    serde_json::json!({ "id": id, "url": url, "secret": format!("s-{id}"), "timeout_ms": 2000, "dead_letter_after": 0 })
}

/// A receiver that answers 200 to everything and counts what arrived.
struct Receiver {
    port: u16,
    hits: Arc<AtomicU64>,
    stop: Arc<AtomicBool>,
    handle: Option<std::thread::JoinHandle<()>>,
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
                                if total > 1_000_000 || done {
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
        Receiver {
            port,
            hits,
            handle: Some(handle),
            stop,
        }
    }
}

impl Drop for Receiver {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        let _ = self.handle.take();
    }
}

fn url(r: &Receiver) -> String {
    format!("http://127.0.0.1:{}/hook", r.port)
}

/// The fixture: a journal with a healthy consumer and a second consumer
/// that is later REPOINTED to a new endpoint.
struct Fixture {
    dir: PathBuf,
    db: PathBuf,
    healthy: Receiver,
    moved_old: Receiver,
    moved_new: Receiver,
}

impl Fixture {
    fn new(tag: &str) -> Fixture {
        let dir = unique_dir(tag);
        let db = dir.join("j.db");
        let out = run(&dir, &["init", "--db", db.to_str().unwrap(), "--program", PROGRAM]);
        assert_eq!(out.status.code(), Some(0), "{}", stderr_of(&out));
        Fixture {
            dir,
            db,
            healthy: Receiver::start(),
            moved_old: Receiver::start(),
            moved_new: Receiver::start(),
        }
    }

    /// The config both targets are healthy in: `moved` points at its old
    /// endpoint.
    fn cfg_both(&self) -> PathBuf {
        write_config(
            &self.dir,
            "cfg-both.json",
            &[
                target("healthy", &url(&self.healthy)),
                target("moved", &url(&self.moved_old)),
            ],
        )
    }

    /// The same config with `moved` REPOINTED, and the moved target listed
    /// FIRST: the abort happens at the first failing target in config order,
    /// so this ordering is the worst case for the healthy consumer.
    fn cfg_repointed(&self) -> PathBuf {
        write_config(
            &self.dir,
            "cfg-repointed.json",
            &[
                target("moved", &url(&self.moved_new)),
                target("healthy", &url(&self.healthy)),
            ],
        )
    }

    fn cfg_only_healthy(&self) -> PathBuf {
        write_config(
            &self.dir,
            "cfg-only-healthy.json",
            &[target("healthy", &url(&self.healthy))],
        )
    }

    fn push(&self, cfg: &Path) -> Output {
        run(
            &self.dir,
            &[
                "webhook",
                "push",
                "--db",
                self.db.to_str().unwrap(),
                "--config",
                cfg.to_str().unwrap(),
            ],
        )
    }

    fn status(&self, cfg: &Path) -> Output {
        run(
            &self.dir,
            &[
                "webhook",
                "status",
                "--db",
                self.db.to_str().unwrap(),
                "--config",
                cfg.to_str().unwrap(),
            ],
        )
    }
}

// ---------------------------------------------------------------------------
// D1: the fan-out must survive one misconfigured target.
// ---------------------------------------------------------------------------
#[test]
fn d1_a_repointed_target_does_not_stop_the_healthy_one_from_being_served() {
    let f = Fixture::new("d1");
    seed_events(&f.db, 3);

    // Control: both targets healthy, both receive the three events.
    let control = f.push(&f.cfg_both());
    assert_eq!(control.status.code(), Some(0), "{}", stderr_of(&control));
    assert_eq!(f.healthy.hits.load(Ordering::SeqCst), 3, "the healthy sink got 3");
    assert_eq!(f.moved_old.hits.load(Ordering::SeqCst), 3, "the moved sink got 3");

    // Two more events become pending, then `moved` is repointed.
    seed_events(&f.db, 2);
    let push = f.push(&f.cfg_repointed());

    // The report MUST exist: a caller polls it, and the pre-fix tree
    // aborted with exit 1 and printed nothing.
    let rep = json_of(&push);
    let targets = rep["targets"].as_array().expect("targets array");
    let healthy_row = targets
        .iter()
        .find(|t| t["target"] == "healthy")
        .expect("the healthy target is reported");
    let moved_row = targets
        .iter()
        .find(|t| t["target"] == "moved")
        .expect("the misconfigured target is reported");

    // The healthy consumer was SERVED: its two pending events went out.
    assert_eq!(
        f.healthy.hits.load(Ordering::SeqCst),
        5,
        "the healthy consumer must receive the events that were pending while its sibling was misconfigured"
    );
    assert_eq!(
        healthy_row["delivered"], 2,
        "the report names what the healthy target received: {healthy_row}"
    );

    // The misconfigured target is named IN THE REPORT, with its reason.
    let moved_text = moved_row.to_string();
    assert!(
        moved_text.contains("bound to endpoint") || moved_text.contains("own target id"),
        "the report must name the misconfigured target's reason: {moved_text}"
    );
    assert!(
        moved_text.contains("moved"),
        "the report must name WHICH target is misconfigured: {moved_text}"
    );

    // The refusal holds the exit code — the same "a debt holds the code"
    // rule the rest of this repo follows. Exit 1 (an abort that prints no
    // report) is the pre-fix behaviour and must not survive.
    assert_eq!(
        push.status.code(),
        Some(3),
        "a refused target is a debt: the report is printed AND the exit code holds it: {}",
        stdout_of(&push)
    );

    // F5 (guard): not one byte reaches the new endpoint.
    assert_eq!(
        f.moved_new.hits.load(Ordering::SeqCst),
        0,
        "the repointed endpoint must receive nothing"
    );
    std::fs::remove_dir_all(&f.dir).ok();
}

// ---------------------------------------------------------------------------
// D2: `webhook status` — the surface a monitoring caller polls — must still
// report the healthy target.
// ---------------------------------------------------------------------------
#[test]
fn d2_status_still_reports_the_healthy_target_and_names_the_misconfigured_one() {
    let f = Fixture::new("d2");
    seed_events(&f.db, 3);
    let control = f.push(&f.cfg_both());
    assert_eq!(control.status.code(), Some(0), "{}", stderr_of(&control));

    seed_events(&f.db, 2);
    let status = f.status(&f.cfg_repointed());

    let rep = json_of(&status);
    let targets = rep["targets"].as_array().expect("targets array");
    let healthy_row = targets
        .iter()
        .find(|t| t["target"] == "healthy")
        .expect("the healthy target is reported");
    let moved_row = targets
        .iter()
        .find(|t| t["target"] == "moved")
        .expect("the misconfigured target is reported");

    // The healthy target's OWN numbers: its cursor is where the journal says
    // it is, and the two events that are still owed to it are counted as
    // owed (this is a status pass, not a push — nothing was delivered).
    assert_eq!(
        healthy_row["next_seq"], 3,
        "the healthy target's own cursor is reported: {healthy_row}"
    );
    assert_eq!(
        healthy_row["pending_events"].as_u64(),
        Some(2),
        "the healthy target's queue is reported: {healthy_row}"
    );
    let moved_text = moved_row.to_string();
    assert!(
        moved_text.contains("bound to endpoint") || moved_text.contains("own target id"),
        "the report must name the misconfigured target's reason: {moved_text}"
    );
    assert_eq!(
        status.status.code(),
        Some(3),
        "a refused target holds the exit code: {}",
        stdout_of(&status)
    );
    std::fs::remove_dir_all(&f.dir).ok();
}

// ---------------------------------------------------------------------------
// D3: the dead-letter replay is a delivery path too — the healthy target's
// quarantined event must still be replayed.
// ---------------------------------------------------------------------------
#[test]
fn d3_dlq_replay_still_replays_for_the_healthy_target() {
    let f = Fixture::new("d3");
    seed_events(&f.db, 3);
    let control = f.push(&f.cfg_both());
    assert_eq!(control.status.code(), Some(0), "{}", stderr_of(&control));

    // One quarantined letter per target, under the endpoint each target
    // was written for.
    let js = r#"
const { DatabaseSync } = require('node:sqlite');
const d = new DatabaseSync(process.argv[1]);
const old = process.argv[2];
const healthy = process.argv[3];
d.exec(`INSERT INTO webhook_dead_letters
        (target,kind,seq,body,event_id,reason,status,attempts,first_failed_at,last_failed_at,state)
        VALUES ('healthy','event',9,'{"type":"event","seq":9}','healthy:event:9','refused with http 404',404,5,1,1,'pending')`);
d.exec(`INSERT INTO webhook_dead_letters
        (target,kind,seq,body,event_id,reason,status,attempts,first_failed_at,last_failed_at,state)
        VALUES ('moved','event',9,'{"type":"event","seq":9}','moved:event:9','refused with http 404',404,5,1,1,'pending')`);
"#;
    let out = Command::new("node")
        .args(["-e", js])
        .arg(&f.db)
        .arg(url(&f.moved_old))
        .arg(url(&f.healthy))
        .output()
        .expect("node seeds dead letters");
    assert!(out.status.success(), "{}", stderr_of(&out));

    let before_healthy = f.healthy.hits.load(Ordering::SeqCst);
    let replay = run(
        &f.dir,
        &[
            "webhook",
            "dlq",
            "replay",
            "--db",
            f.db.to_str().unwrap(),
            "--config",
            f.cfg_repointed().to_str().unwrap(),
        ],
    );
    let rep = json_of(&replay);
    assert!(
        f.healthy.hits.load(Ordering::SeqCst) > before_healthy,
        "the healthy target's quarantined event must still be replayed: {}",
        stdout_of(&replay)
    );
    assert!(
        rep["replayed"].as_i64().unwrap_or(0) >= 1,
        "the report names the replayed letter: {rep}"
    );
    assert_eq!(
        f.moved_new.hits.load(Ordering::SeqCst),
        0,
        "the repointed endpoint must receive no quarantined body"
    );
    std::fs::remove_dir_all(&f.dir).ok();
}

// ---------------------------------------------------------------------------
// G1 (guard, green on BOTH trees): a config where every target is healthy
// delivers to all of them and exits 0.
// ---------------------------------------------------------------------------
#[test]
fn g1_a_healthy_config_delivers_to_every_target() {
    let f = Fixture::new("g1");
    seed_events(&f.db, 3);
    let push = f.push(&f.cfg_both());
    assert_eq!(push.status.code(), Some(0), "{}", stderr_of(&push));
    let rep = json_of(&push);
    let targets = rep["targets"].as_array().expect("targets array");
    assert_eq!(targets.len(), 2);
    for t in targets {
        assert_eq!(t["delivered"], 3, "each healthy target got its events: {t}");
        assert_eq!(t["blocked"], false, "no healthy target is blocked: {t}");
    }
    assert_eq!(f.healthy.hits.load(Ordering::SeqCst), 3);
    assert_eq!(f.moved_old.hits.load(Ordering::SeqCst), 3);
    std::fs::remove_dir_all(&f.dir).ok();
}

// ---------------------------------------------------------------------------
// G2 (guard, green on BOTH trees): a config with a single healthy target
// reports its own numbers and exits 0.
// ---------------------------------------------------------------------------
#[test]
fn g2_a_single_healthy_target_reports_its_own_numbers() {
    let f = Fixture::new("g2");
    seed_events(&f.db, 3);
    let push = f.push(&f.cfg_only_healthy());
    assert_eq!(push.status.code(), Some(0), "{}", stderr_of(&push));
    let status = f.status(&f.cfg_only_healthy());
    assert_eq!(status.status.code(), Some(0), "{}", stderr_of(&status));
    let rep = json_of(&status);
    let row = &rep["targets"][0];
    assert_eq!(row["target"], "healthy");
    assert_eq!(row["next_seq"], 3);
    assert_eq!(row["pending_events"], 0);
    std::fs::remove_dir_all(&f.dir).ok();
}
