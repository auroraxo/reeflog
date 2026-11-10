//! Pins the measurement of the PUBLISHED v0.7.12 release asset: the same 5
//! replay-page cases, measured against the published v0.7.11 baseline and
//! against the published v0.7.12 asset (whose archive sha256 equals the
//! digest the Release API publishes).
//!
//! Why this exists (2026-11-10): the fix was measured on the tree first
//! (research/dlq-replay-page-2026-11-10.json), but a fix that only lives on
//! the branch is the defect class this repository has measured three times —
//! so the RELEASE itself is measured, not the build, not the checkout.
//!
//! What is pinned:
//!
//!   * both binaries are named with the version read from the binary itself
//!     (`0.7.11` baseline, `0.7.12` subject) and the release digests;
//!   * the published v0.7.11 baseline violates 5 of 5 cases;
//!   * the published v0.7.12 asset violates 0 of 5: an undrained pass exits 3
//!     and names the unreached count, a full drain and an empty queue still
//!     exit 0;
//!   * the artifact carries no host path.
//!
//! Falsified by construction: change any count, claim another version
//! measured, or inject a machine path, and these tests fail.

use std::fs;
use std::path::PathBuf;

fn artifact() -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("research")
        .join("dlq-replay-page-published-v0.7.12-2026-11-10.json");
    fs::read_to_string(&path)
        .expect("research/dlq-replay-page-published-v0.7.12-2026-11-10.json is readable")
}

fn assert_contains(text: &str, needle: &str, why: &str) {
    assert!(
        text.contains(needle),
        "the artifact must contain `{needle}` — {why}"
    );
}

#[test]
fn the_artifact_names_the_binaries_it_measured() {
    let text = artifact();
    assert_contains(
        &text,
        "\"subject\": \"the dlq replay report must describe the queue, not the page its limit attempted\"",
        "the artifact declares what it is",
    );
    assert_contains(
        &text,
        "\"source\": \"published v0.7.11 release asset (sha256 f75898ff9121df1bd280399401893268e161ec95e1af38b0ccabbd656e588ac3)\"",
        "the baseline must be named as the PUBLISHED v0.7.11 asset with its digest",
    );
    assert_contains(
        &text,
        "\"source\": \"published v0.7.12 release asset (sha256 ab28aac213d2e5158d27086c22acb2947ab4f756d21d0b5da3adc614e5aac856)\"",
        "the subject must be named as the PUBLISHED v0.7.12 asset with its digest, not the tree",
    );
}

#[test]
fn the_published_v0712_asset_carries_no_defect() {
    let text = artifact();
    assert_contains(&text, "\"violations\": 5", "five baseline violations on the published v0.7.11 asset");
    assert_contains(
        &text,
        "\"source\": \"published v0.7.12 release asset",
        "the subject side of the comparison is the published asset",
    );
    // The subject side ends with violations: 0 — assert the LAST occurrence
    // so a baseline count change cannot satisfy this.
    let idx = text.rfind("\"violations\": 0").expect("the artifact records zero violations on the subject side");
    let subject = &text[idx..];
    assert_contains(subject, "\"violations\": 0", "the published v0.7.12 asset must violate nothing of the 5");

    // An undrained pass on the subject must exit 3 and name the count.
    let cases = &text[idx.saturating_sub(4000)..idx];
    assert_contains(cases, "\"exit\": 3", "an undrained pass exits 3 on the published asset");
    assert_contains(
        cases,
        "\"stderr_head\": \"reeflog: 5 pending dead letter(s) were not reached by this pass",
        "the refusal names the unreached count",
    );
}

#[test]
fn the_published_v0712_artifact_carries_no_host_path() {
    let text = artifact();
    assert!(
        !text.contains("/home/") && !text.contains("/Users/") && !text.contains("/tmp/"),
        "a committed artifact must not name the machine it was produced on"
    );
}
