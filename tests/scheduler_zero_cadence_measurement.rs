//! The zero-cadence measurement, pinned on both sides.
//!
//! PULS AU, 2026-11-19. `tools/measure-scheduler-zero-cadence.mjs` — the
//! real binaries against a real local chain sink that counts every RPC
//! call the daemon makes and every webhook body it delivers:
//!
//! * The BASELINE side is the release binary of v0.7.37 as published (the
//!   linux archive whose sha256 equals the digest the Release API
//!   publishes; verdict `scheduler-zero-cadence-busy`, 5 defect faces):
//!   with any of the four cadences at 0 (`--seal-every`, `--reorg-every`,
//!   `--repair-every`, `--webhook-every`) the scheduler collapsed the zero
//!   to ONE SECOND and ran the pass every tick — while the SAME run's
//!   shutdown gates read the same value as "never"
//!   (`final_reorg: {skipped: true}`, `final_repair: {skipped: true}`).
//!   One flag value, two contradictory meanings inside one run; the
//!   expensive meaning was the 1 Hz one, and the webhook face made the
//!   machine SEND signed bodies at 1 Hz.
//!
//! * The FIXED side is this tree (0 defect faces): a cadence of 0 means
//!   OFF — nothing scheduled, nothing delivered — and the shutdown gates
//!   read the same value the same way. A positive cadence keeps its old
//!   meaning on every tick and at shutdown.
//!
//! The pin holds the MEASURED numbers, not a summary: the baseline side of
//! the artifact must still carry the 1 Hz schedule it recorded, so a later
//! tree cannot quietly make the defect look smaller than it was, and the
//! fixed side must still carry the zero-pass summaries it claims.

use std::path::PathBuf;

fn artifact(name: &str) -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("research")
        .join(name);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

fn assert_contains(text: &str, needle: &str, what: &str) {
    assert!(
        text.contains(needle),
        "the artifact must state {what}: missing {needle:?}"
    );
}

const ARTIFACT: &str = "scheduler-zero-cadence-2026-11-19.json";

#[test]
fn the_artifact_names_the_verdict_and_both_sides() {
    let text = artifact(ARTIFACT);
    assert_contains(
        &text,
        "\"verdict\": \"scheduler-zero-cadence-busy\"",
        "the verdict",
    );
    assert_contains(
        &text,
        "\"baselineDefectFaces\": 5",
        "the baseline defect count",
    );
    assert_contains(&text, "\"fixedDefectFaces\": 0", "the fixed defect count");
    assert_contains(
        &text,
        "\"binVersion\": \"reeflog 0.7.37\"",
        "the measured binaries' own version",
    );
    // The binaries are named by hash, never by host path.
    assert_contains(
        &text,
        "\"binSha256\": \"ddba9506",
        "the baseline binary's sha256 (v0.7.37 as published)",
    );
}

#[test]
fn the_artifact_carries_the_baseline_defect_numbers() {
    let text = artifact(ARTIFACT);
    // The baseline ran the schedules at a cadence of 0: scheduled passes
    // recorded, bodies delivered, shutdown gates contradicting the
    // schedule in the same summary. A later tree must not be able to make
    // the defect smaller than it was.
    assert_contains(
        &text,
        "\"reorg_passes\": 2",
        "the baseline scheduling reorg passes at a cadence of 0",
    );
    assert_contains(
        &text,
        "\"final_reorg\": {\n          \"skipped\": true\n        }",
        "the baseline's final gate claiming the same zero means never",
    );
    assert_contains(
        &text,
        "\"posts\": 2",
        "the baseline's webhook face delivering signed bodies",
    );
}

#[test]
fn the_artifact_carries_the_fixed_numbers() {
    let text = artifact(ARTIFACT);
    // The fixed side: one meaning everywhere. Nothing scheduled, nothing
    // delivered, and the gates read the same zero as never.
    assert_contains(
        &text,
        "\"reorg_passes\": 0",
        "the fixed tree scheduling no reorg pass at a cadence of 0",
    );
    assert_contains(
        &text,
        "\"repair_passes\": 0",
        "the fixed tree scheduling no repair pass at a cadence of 0",
    );
    assert_contains(
        &text,
        "\"webhook_passes\": 0",
        "the fixed tree scheduling no webhook pass at a cadence of 0",
    );
}

#[test]
fn the_artifact_proves_the_guards_held_on_both_trees() {
    let text = artifact(ARTIFACT);
    // The honest forms must not have moved: a never-due cadence schedules
    // nothing on BOTH trees, a tight positive cadence still runs on BOTH
    // trees, and the final gates keep their positive-cadence meaning.
    assert_contains(
        &text,
        "\"baseline\": true,\n      \"fixed\": true",
        "at least one face holding on both trees",
    );
    assert_contains(
        &text,
        "\"name\": \"G1 a never-due cadence schedules nothing and the final gates keep their meaning (unchanged)\"",
        "the G1 guard",
    );
    assert_contains(
        &text,
        "\"name\": \"G2 a tight positive cadence still runs (unchanged)\"",
        "the G2 guard",
    );
}
