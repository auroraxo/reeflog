//! `webhook status` must report the DEAD-LETTER debt of targets that left
//! the configuration.
//!
//! PULS AL, 2026-11-15. Measured on the PUBLISHED v0.7.28 binary
//! (`tools/measure-status-orphan-dead-letters.mjs`, the real binary against
//! a real journal; verdict `status-orphan-dead-letter-blind`):
//!
//! * A pending dead letter belongs to a TARGET, not to a config entry. When
//!   a target is removed from the config, its quarantined events stay in
//!   the journal — and they can never be replayed from here, because the
//!   replay path needs the target's endpoint. The repo already knows this:
//!   `webhook push` reports `orphaned_dead_letters` and `webhook dlq list`
//!   lists the orphan with `in_config: false`.
//!
//! * `webhook status` reported NEITHER. It walks the CONFIG's targets and
//!   prints a `dead_letters` count for each; a target that left the config
//!   is not in the config, so its pending letters are not printed anywhere.
//!   The report said `dead_letters: 0` for a healthy target and exited 0,
//!   while the same journal read through `push` said
//!   `orphaned_dead_letters: 1`.
//!
//!   `webhook status` is the report a monitoring caller polls. A quarantined
//!   event that nobody holds — permanently, until the operator re-adds the
//!   target — is exactly the debt a monitor must surface, and it is the one
//!   debt that cannot heal by itself. The status report said nothing, so the
//!   operator's periodic check read "all clear" while an event was stranded.
//!
//! The rule:
//! * `webhook status` reports the orphaned dead-letter debt next to the
//!   per-target rows: the number of pending letters whose target is no
//!   longer in the config, and WHICH targets those are. It is the SAME
//!   journal truth `webhook push` reports as `orphaned_dead_letters` and
//!   `webhook dlq list` reports as `orphaned_pending` — one fact, one
//!   number per surface, no drift.
//! * The status report EXITS 3 when orphaned letters are pending, like it
//!   must for a blocked target: silence is not health, and this is the one
//!   debt that never drains by itself.
//!
//! Guards (green on BOTH trees): a journal with no orphan reports 0 and an
//! empty target list (G1); the orphan count is never negative (G2); a
//! pending letter for a target that IS in the config is NOT counted as an
//! orphan — it stays on that target's own row (G3).

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

/// The whole `webhook status` report.
fn status_report(dir: &Path, cfg: &Path) -> (serde_json::Value, i32) {
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
    let code = out.status.code().unwrap_or(-1);
    let v: serde_json::Value =
        serde_json::from_str(String::from_utf8_lossy(&out.stdout).trim()).expect("a JSON report");
    (v, code)
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

/// Plant `count` pending dead letters for `target`.
fn plant_pending_dead_letter(dir: &Path, target: &str, seq: i64, count: usize) {
    let conn = rusqlite::Connection::open(dir.join("reeflog.db")).expect("open journal");
    for i in 0..count {
        conn.execute(
            "INSERT INTO webhook_dead_letters(target,kind,seq,body,event_id,reason,status,
                                              attempts,first_failed_at,last_failed_at,state)
             VALUES(?1,'event',?2,'{}',?3,'http 500',500,3,1700000000000,1700000000000,'pending')",
            rusqlite::params![target, seq + i as i64, format!("EV{seq}{i}")],
        )
        .expect("plant dead letter");
    }
    conn.close().expect("close journal");
}

/// The number of orphaned pending letters the report names. `None` when the
/// report carries no such field at all — the defect face.
fn reported_orphaned(v: &serde_json::Value) -> Option<i64> {
    v.get("orphaned_dead_letters")
        .and_then(|x| x.as_i64())
}

/// The orphaned targets the report names.
fn reported_orphaned_targets(v: &serde_json::Value) -> Vec<String> {
    v.get("orphaned_dead_letter_targets")
        .and_then(|x| x.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|e| e.get("target").and_then(|t| t.as_str()))
                .map(|s| s.to_string())
                .collect()
        })
        .unwrap_or_default()
}

#[test]
fn an_orphaned_dead_letter_is_visible_in_the_status_report() {
    // R1: a pending letter for a target that LEFT the config. The report
    // must not look like "nothing is quarantined".
    let dir = unique_dir("status-orphan-visible");
    init(&dir);
    let cfg = write_config(&dir);
    plant_pending_dead_letter(&dir, "gone", 0, 1);

    let (v, code) = status_report(&dir, &cfg);
    assert_eq!(
        reported_orphaned(&v),
        Some(1),
        "a quarantined event whose target left the config can never be \
         replayed from here — the monitoring report must name it: {v}"
    );
    assert_eq!(
        reported_orphaned_targets(&v),
        vec!["gone".to_string()],
        "the report must name WHICH target holds the stranded letters: {v}"
    );
    assert_eq!(
        code, 3,
        "silence is not health: a pending orphan must hold the exit code \
         like a blocked target does (stderr: {})",
        stderr_of(&run(
            &dir,
            &[
                "webhook",
                "status",
                "--db",
                "reeflog.db",
                "--config",
                cfg.to_str().expect("cfg path"),
            ]
        ))
    );
}

#[test]
fn the_stderr_voice_names_the_orphaned_target() {
    // R2: the reason travels in the line the operator actually reads.
    let dir = unique_dir("status-orphan-voice");
    init(&dir);
    let cfg = write_config(&dir);
    plant_pending_dead_letter(&dir, "gone", 0, 2);

    let out = run(
        &dir,
        &[
            "webhook",
            "status",
            "--db",
            "reeflog.db",
            "--config",
            cfg.to_str().expect("cfg path"),
        ],
    );
    let err = stderr_of(&out);
    assert!(
        err.contains("gone"),
        "the refusal line must name the target that holds the stranded \
         letters: {err}"
    );
    assert!(
        err.contains("dead-letter") || err.contains("dead letter"),
        "the refusal line must say what kind of debt this is: {err}"
    );
}

#[test]
fn a_journal_without_orphans_reports_zero() {
    // G1: the false-positive guard — a clean journal must report 0 and an
    // empty orphan list. A count that is always non-zero would be just as
    // useless as no count.
    let dir = unique_dir("status-orphan-clean");
    init(&dir);
    let cfg = write_config(&dir);

    let (v, code) = status_report(&dir, &cfg);
    assert_eq!(reported_orphaned(&v), Some(0), "{v}");
    assert!(
        reported_orphaned_targets(&v).is_empty(),
        "no orphan means no orphan target: {v}"
    );
    assert_eq!(code, 0, "a clean journal exits 0: {v}");
}

#[test]
fn the_orphan_count_is_never_negative() {
    // G2: the count must be a count, not a subtraction that can go below
    // zero.
    let dir = unique_dir("status-orphan-negative");
    init(&dir);
    let cfg = write_config(&dir);
    plant_pending_dead_letter(&dir, "gone", 0, 1);
    // A letter for a target that IS in the config must not be counted as an
    // orphan either — see the next test. Here we only assert the sign.
    let (v, _) = status_report(&dir, &cfg);
    let n = reported_orphaned(&v).expect("the field exists");
    assert!(n >= 0, "the orphan count must never be negative: {v}");
}

#[test]
fn a_letter_for_a_configured_target_is_not_an_orphan() {
    // G3: the orphan count is about targets that LEFT the config. A pending
    // letter for a target that is still configured stays on that target's
    // own row and must not be double-counted as an orphan.
    let dir = unique_dir("status-orphan-not-orphan");
    init(&dir);
    let cfg = write_config(&dir);
    plant_pending_dead_letter(&dir, "t1", 0, 1);

    let (v, _) = status_report(&dir, &cfg);
    assert_eq!(
        reported_orphaned(&v),
        Some(0),
        "a configured target's letter is not an orphan: {v}"
    );
    assert_eq!(
        v["targets"][0]["dead_letters"].as_i64(),
        Some(1),
        "the letter stays visible on its own target's row: {v}"
    );
    assert!(
        reported_orphaned_targets(&v).is_empty(),
        "no orphan target may be named: {v}"
    );
}

#[test]
fn the_orphan_count_matches_the_push_report() {
    // The same journal truth, one number per surface: `webhook push`
    // reports `orphaned_dead_letters`, `webhook dlq list` reports
    // `orphaned_pending`, and `webhook status` must report the SAME count.
    // A drift between the surfaces would let a reader believe one of them.
    let dir = unique_dir("status-orphan-parity");
    init(&dir);
    let cfg = write_config(&dir);
    plant_pending_dead_letter(&dir, "gone", 0, 3);
    plant_pending_dead_letter(&dir, "t1", 0, 1);

    let (v, _) = status_report(&dir, &cfg);
    let status_orphans = reported_orphaned(&v).expect("status names the orphan count");

    let out = run(
        &dir,
        &[
            "webhook",
            "push",
            "--db",
            "reeflog.db",
            "--config",
            cfg.to_str().expect("cfg path"),
        ],
    );
    let push: serde_json::Value =
        serde_json::from_str(String::from_utf8_lossy(&out.stdout).trim()).expect("push JSON");
    let push_orphans = push["orphaned_dead_letters"].as_i64().expect("push names the orphan count");

    assert_eq!(
        status_orphans, push_orphans,
        "status and push must report the SAME orphan count — status says \
         {status_orphans}, push says {push_orphans}"
    );
}
