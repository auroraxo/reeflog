//! `webhook status` must report the RETRACTION queue as its own truth.
//!
//! PULS AK, 2026-11-15. Measured on the PUBLISHED v0.7.27 tree
//! (`tools/measure-status-retraction-pending.mjs`, the real binary against a
//! real journal; verdict `status-retraction-queue-blind`, 0/3 faces):
//!
//! * R3 `webhook status` printed `pending_events: 0` — the event queue is
//!   drained — and NOTHING at all about the retraction queue, while the
//!   journal held 2 recorded reorgs the target had never received. The
//!   report named `next_reorg_id: 0` (a bare cursor value) but never said
//!   how many retractions were still behind it. A monitoring caller polling
//!   `webhook status` therefore concluded the consumer was caught up while
//!   the events the chain UNMADE had never reached it.
//!
//!   The same journal state read through `webhook push` says it plainly:
//!   `remaining_reorgs_behind_cursor: 2`. v0.7.26 established that rule for
//!   the push report ("the REPORT describes the QUEUE, not the PAGE"); the
//!   status report still names only one of the two queues.
//!
//! * R1 with BOTH queues behind, the status report named `pending_events: 2`
//!   and had no field for the 2 pending retractions — a reader could not
//!   tell "the target is two events behind" from "the target is two events
//!   and two retractions behind".
//!
//! * R2 (guard, both trees) a fully drained target must report 0 for both
//!   queues — the count must not be a number that is always non-zero.
//!
//! The rule:
//! * `webhook status` reports `pending_retractions` per target: the number
//!   of recorded reorgs the target has not received yet. It is the SAME
//!   queue truth `webhook push` reports as `remaining_reorgs_behind_cursor`
//!   — one fact, one name per surface, no drift.
//! * The retraction channel is separate ON PURPOSE (a retraction must still
//!   reach a consumer whose event queue is blocked), so the two counts are
//!   reported separately and neither stands in for the other.
//!
//! Guards (green on BOTH trees): the drained target reports 0/0 (G1); the
//! event queue and the retraction queue are counted independently, so a
//! target with a drained event queue and a pending retraction is visible
//! (G2); `pending_retractions` never goes negative (G3).

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

fn stderr_of(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

/// `webhook status` for the single target `t1`.
fn status_of(dir: &Path, cfg: &Path) -> serde_json::Value {
    let out = run(
        dir,
        &[
            "webhook",
            "status",
            "--db",
            "reeflog.db",
            "--config",
            cfg.to_str().expect("cfg path"),
        ],
    );
    assert!(
        out.status.success(),
        "webhook status must not fail: {}",
        stderr_of(&out)
    );
    let v: serde_json::Value =
        serde_json::from_str(String::from_utf8_lossy(&out.stdout).trim()).expect("a JSON report");
    v["targets"][0].clone()
}

fn write_config(dir: &Path) -> PathBuf {
    let cfg = dir.join("wh.json");
    std::fs::write(
        &cfg,
        r#"{"targets":[{"id":"t1","url":"http://127.0.0.1:9/never","secret":"s3cret","timeout_ms":2000,"dead_letter_after":0}]}"#,
    )
    .expect("write config");
    cfg
}

fn init(dir: &Path) {
    let out = run(dir, &["init", "--db", "reeflog.db", "--program", VALID_PROGRAM]);
    assert!(out.status.success(), "init: {}", stderr_of(&out));
}

/// `n` events (payload held, so they are deliverable) and `m` recorded
/// retractions.
fn plant(dir: &Path, n: usize, m: usize) {
    let conn = rusqlite::Connection::open(dir.join("reeflog.db")).expect("open journal");
    for i in 1..=n {
        conn.execute(
            "INSERT INTO events(signature,slot,err,logs,source,first_seen,payload_state)
             VALUES(?1,?2,NULL,?3,'ws',?4,'held')",
            rusqlite::params![
                format!("EV{i}"),
                100 + i as i64,
                format!(r#"["log {i}"]"#),
                1_700_000_000_000i64 + i as i64
            ],
        )
        .expect("plant event");
    }
    for i in 1..=m {
        conn.execute(
            "INSERT INTO reorgs(signature,slot,detected_at,status_source,tx_source,detail)
             VALUES(?1,?2,?3,'known','refused','measured')",
            rusqlite::params![
                format!("EV{i}"),
                100 + i as i64,
                1_700_000_000_000i64 + i as i64
            ],
        )
        .expect("plant reorg");
    }
    conn.close().expect("close journal");
}

/// The target's two cursor lines.
fn set_target(dir: &Path, next_seq: i64, next_reorg_id: i64) {
    let conn = rusqlite::Connection::open(dir.join("reeflog.db")).expect("open journal");
    conn.execute(
        "INSERT INTO webhook_state(target,endpoint,next_seq,next_reorg_id,last_error,updated_at)
         VALUES('t1','',?1,?2,NULL,1700000000000)
         ON CONFLICT(target) DO UPDATE SET next_seq=excluded.next_seq,
                                           next_reorg_id=excluded.next_reorg_id",
        rusqlite::params![next_seq, next_reorg_id],
    )
    .expect("set target");
    conn.close().expect("close journal");
}

/// The number of retractions the target has not received, as the REPORT says
/// it. `None` when the report does not carry the field at all — the defect
/// face.
fn reported_pending_retractions(t: &serde_json::Value) -> Option<i64> {
    t.get("pending_retractions")
        .and_then(|v| v.as_i64())
}

#[test]
fn a_target_with_pending_retractions_is_visible_in_the_status_report() {
    // R3: the event queue is DRAINED and the retraction queue is not. The
    // report must not look like "the consumer is caught up".
    let dir = unique_dir("status-retr-only");
    init(&dir);
    let cfg = write_config(&dir);
    plant(&dir, 2, 2);
    // The event cursor is at the tip (2), the retraction cursor at 0.
    set_target(&dir, 2, 0);

    let t = status_of(&dir, &cfg);
    assert_eq!(
        t["pending_events"].as_i64(),
        Some(0),
        "the event queue is drained in this fixture: {t}"
    );
    assert_eq!(
        reported_pending_retractions(&t),
        Some(2),
        "two recorded retractions the target never received must be visible \
         in the status report — the report otherwise says the consumer is \
         caught up while the events the chain unmade have not reached it: {t}"
    );
}

#[test]
fn both_queues_are_reported_separately() {
    // R1: both queues behind — the report must name both, and neither may
    // stand in for the other.
    let dir = unique_dir("status-retr-both");
    init(&dir);
    let cfg = write_config(&dir);
    plant(&dir, 3, 2);
    set_target(&dir, 0, 0);

    let t = status_of(&dir, &cfg);
    assert_eq!(
        t["pending_events"].as_i64(),
        Some(3),
        "the event queue holds 3 rows: {t}"
    );
    assert_eq!(
        reported_pending_retractions(&t),
        Some(2),
        "the retraction queue holds 2 rows and must be counted on its own \
         line: {t}"
    );
}

#[test]
fn a_drained_target_reports_zero_for_both_queues() {
    // R2 / G1: the false-positive guard — a drained target must report 0/0.
    // A count that is always non-zero would be just as useless as no count.
    let dir = unique_dir("status-retr-drained");
    init(&dir);
    let cfg = write_config(&dir);
    plant(&dir, 2, 2);
    set_target(&dir, 2, 2);

    let t = status_of(&dir, &cfg);
    assert_eq!(t["pending_events"].as_i64(), Some(0), "{t}");
    assert_eq!(
        reported_pending_retractions(&t),
        Some(0),
        "a fully drained target must report zero pending retractions: {t}"
    );
}

#[test]
fn the_two_cursors_are_counted_independently() {
    // G2: the retraction count must not be derived from the event count.
    // A target whose event cursor is at the tip but whose retraction cursor
    // is behind must still show the retraction debt — and a target whose
    // EVENT queue is behind while the retraction queue is drained must show
    // 0 retractions.
    let dir = unique_dir("status-retr-independent");
    init(&dir);
    let cfg = write_config(&dir);
    plant(&dir, 4, 3);

    // Event queue behind, retraction queue drained.
    set_target(&dir, 1, 3);
    let a = status_of(&dir, &cfg);
    assert_eq!(a["pending_events"].as_i64(), Some(3), "{a}");
    assert_eq!(
        reported_pending_retractions(&a),
        Some(0),
        "a drained retraction queue must read 0 even while the event queue \
         is behind: {a}"
    );

    // Event queue drained, retraction queue behind.
    set_target(&dir, 4, 0);
    let b = status_of(&dir, &cfg);
    assert_eq!(b["pending_events"].as_i64(), Some(0), "{b}");
    assert_eq!(
        reported_pending_retractions(&b),
        Some(3),
        "the retraction debt must be visible on its own: {b}"
    );
}

#[test]
fn the_pending_retraction_count_is_never_negative() {
    // G3: a cursor ahead of the journal (a deliberate rewind/resume, which
    // the cursor contract allows) must not produce a negative debt.
    let dir = unique_dir("status-retr-negative");
    init(&dir);
    let cfg = write_config(&dir);
    plant(&dir, 1, 1);
    set_target(&dir, 99, 99);

    let t = status_of(&dir, &cfg);
    let n = reported_pending_retractions(&t).expect("the field exists");
    assert!(
        n >= 0,
        "a cursor ahead of the journal must not report a negative debt: {t}"
    );
    assert_eq!(n, 0, "nothing is behind a cursor past the tip: {t}");
}
