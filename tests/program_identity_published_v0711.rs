//! Pins the measurement of the PUBLISHED v0.7.11 release asset: the same 6
//! program-identity cases, measured against the published v0.7.10 baseline
//! and against the published v0.7.11 asset (whose archive sha256 equals the
//! digest the Release API publishes).
//!
//! Why this exists (2026-11-10): the fix was measured on the tree first. A
//! fix that only lives on the branch is the defect class this repository has
//! measured three times, so the RELEASE itself is measured — not the build,
//! not the checkout.
//!
//! What is pinned:
//!
//!   * the artifact exists and names both binaries with the versions read
//!     from those binaries (`0.7.10` baseline, `0.7.11` subject);
//!   * the published v0.7.10 baseline violates 3 of 6 cases and CHANGED the
//!     journal's identity in 1 of them (the silent `init` re-point);
//!   * the published v0.7.11 asset violates 0 of 6 and changes identity 0
//!     times — the refusal reaches the stranger's install;
//!   * the artifact carries no host path.
//!
//! Falsified by construction: change any count, claim another version
//! measured, or inject a machine path, and these tests fail.

use std::fs;
use std::path::PathBuf;

fn artifact() -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("research")
        .join("program-identity-published-v0.7.11-2026-11-10.json");
    fs::read_to_string(&path)
        .expect("research/program-identity-published-v0.7.11-2026-11-10.json is readable")
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
        "\"artifact\": \"program-identity-before-after\"",
        "the artifact declares what it is",
    );
    assert_contains(
        &text,
        "\"binary\": \"published release asset v0.7.10 (sha256 == Release-API digest 2f6eded2...)\"",
        "the baseline must be named as the PUBLISHED v0.7.10 asset with its digest",
    );
    assert_contains(
        &text,
        "\"version\": \"0.7.10\"",
        "the baseline version is read from the binary itself",
    );
    assert_contains(
        &text,
        "\"binary\": \"published release asset v0.7.11 (sha256 == Release-API digest f75898ff...)\"",
        "the subject must be named as the PUBLISHED v0.7.11 asset with its digest, not the tree",
    );
    assert_contains(
        &text,
        "\"version\": \"0.7.11\"",
        "the subject version is read from the binary itself",
    );
}

#[test]
fn the_published_v0711_asset_carries_no_defect() {
    let text = artifact();
    assert_contains(&text, "\"total\": 6", "six cases were measured on both sides");
    assert_contains(
        &text,
        "\"baselineViolations\": 3",
        "the published v0.7.10 defects must not shrink quietly",
    );
    assert_contains(
        &text,
        "\"baselineIdentityChanges\": 1",
        "the silent identity change must stay visible in the baseline",
    );
    assert_contains(
        &text,
        "\"fixedViolations\": 0",
        "the published v0.7.11 asset must violate nothing of the 6",
    );
    assert_contains(
        &text,
        "\"fixedIdentityChanges\": 0",
        "the published v0.7.11 asset must never repoint a journal",
    );
}

#[test]
fn the_published_v0711_artifact_carries_no_host_path() {
    let text = artifact();
    assert!(
        !text.contains("/home/") && !text.contains("/Users/") && !text.contains("/tmp/"),
        "a committed artifact must not name the machine it was produced on"
    );
}
