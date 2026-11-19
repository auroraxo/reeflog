//! The two-queue standing-reason measurement, pinned on both sides.
//!
//! PULS AT, 2026-11-19. `tools/measure-status-two-queue-reason.mjs` — the
//! real binary against a real journal with a live sink (a consumer that
//! refuses retractions with 404 and accepts events with 200):
//!
//! * The BASELINE side is the release binary of v0.7.36 as published
//!   (the linux archive whose sha256 equals the digest the Release API
//!   publishes; verdict `status-two-queue-reason-one-field`, 3 defect
//!   faces): with BOTH queues blocked at once — the event queue held at an
//!   owed payload, the retraction queue refused with 404 — `webhook push`
//!   reported BOTH reasons, while `webhook status` reported ONE: the
//!   retraction's 404. The event queue's repair (`hydrate`) was gone from
//!   the report, and nothing on the row said which channel the 404
//!   belonged to.
//!
//! * The FIXED side is this tree (0 defect faces): each channel's standing
//!   reason lives in its own column, the status row carries the
//!   retraction's reason on its own field `reorg_error` — the same name
//!   `webhook push` uses — and a target blocked on both queues shows both
//!   reasons. The guards (event-only, retraction-only, clean) hold on
//!   BOTH sides unchanged.
//!
//! The pin holds the MEASURED numbers, not a summary: the baseline side of
//! the artifact must still carry the single reason field it recorded, so a
//! later tree cannot quietly make the defect look smaller than it was, and
//! the fixed side must still carry the two-channel report it claims.

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

const ARTIFACT: &str = "status-two-queue-reason-2026-11-19.json";

#[test]
fn the_artifact_names_the_verdict_and_both_sides() {
    let text = artifact(ARTIFACT);
    assert_contains(
        &text,
        "\"verdict\": \"status-two-queue-reason-one-field\"",
        "the verdict",
    );
    assert_contains(&text, "\"baselineDefectFaces\": 3", "the baseline defect count");
    assert_contains(&text, "\"fixedDefectFaces\": 0", "the fixed defect count");
    assert_contains(
        &text,
        "\"binVersion\": \"reeflog 0.7.36\"",
        "the measured binaries' own version",
    );
    // The binaries are named by hash, never by host path.
    assert_contains(
        &text,
        "\"binSha256\": \"12433146",
        "the baseline binary's sha256 (v0.7.36 as published)",
    );
}

#[test]
fn the_artifact_carries_the_baseline_defect_numbers() {
    let text = artifact(ARTIFACT);
    // The baseline kept ONE reason column: the retraction's 404 sat where
    // the event queue's reason belongs. A later tree must not be able to
    // make the defect smaller than it was.
    assert_contains(
        &text,
        "\"last_error\": \"refused with http 404\"",
        "the baseline's single reason column",
    );
    assert_contains(
        &text,
        "\"has_reorg_error_field\": false",
        "the baseline report having no retraction field",
    );
    assert_contains(
        &text,
        "\"column\": false",
        "the baseline journal having no retraction column",
    );
    // The baseline's push report knew BOTH reasons — the defect was in the
    // status report, not in the delivery pass.
    assert_contains(
        &text,
        "\"push_reorg_error\": \"refused with http 404\"",
        "the baseline push report naming the retraction reason",
    );
}

#[test]
fn the_artifact_carries_the_fixed_two_channel_report() {
    let text = artifact(ARTIFACT);
    assert_contains(
        &text,
        "\"reorg_error\": \"refused with http 404\"",
        "the fixed report carrying the retraction reason on its own field",
    );
    assert_contains(
        &text,
        "\"last_reorg_error\": \"refused with http 404\"",
        "the fixed journal carrying the retraction reason in its own column",
    );
    assert_contains(
        &text,
        "\"column\": true",
        "the fixed journal having the retraction column",
    );
    // The event channel's reason survives the retraction pass.
    assert_contains(
        &text,
        "\"last_error\": \"the payload is still owed",
        "the fixed journal keeping the event channel's reason",
    );
}

#[test]
fn the_artifact_carries_no_host_path() {
    let text = artifact(ARTIFACT);
    assert!(
        !text.contains("/home/") && !text.contains("/tmp/rl-") && !text.contains("/Users/"),
        "the artifact must not carry a host path from the machine that produced it"
    );
}
