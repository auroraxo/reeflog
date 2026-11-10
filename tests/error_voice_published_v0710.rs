//! Pins the measurement of the PUBLISHED v0.7.10 release asset: the same 8
//! hostile values, measured against the published v0.7.9 baseline and
//! against the published v0.7.10 asset (whose archive sha256 equals the
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
//!     from those binaries (`0.7.9` baseline, `0.7.10` subject);
//!   * the published v0.7.9 baseline refused all 8 hostile values — the
//!     VERDICT was never in question — and still forged 5 log lines and let
//!     7 raw control bytes through;
//!   * the published v0.7.10 asset refuses the same 8 and forges 0 lines
//!     with 0 raw control bytes;
//!   * the artifact carries no host path.
//!
//! Falsified by construction: change any count, claim another version
//! measured, or inject a machine path, and these tests fail.

use std::fs;
use std::path::PathBuf;

fn artifact() -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("research")
        .join("error-voice-published-v0.7.10-2026-11-10.json");
    fs::read_to_string(&path)
        .expect("research/error-voice-published-v0.7.10-2026-11-10.json is readable")
}

fn assert_contains(text: &str, needle: &str) {
    assert!(text.contains(needle), "the artifact must contain `{needle}`");
}

#[test]
fn the_artifact_names_the_binaries_it_measured() {
    let text = artifact();
    assert_contains(&text, "\"binary\": \"published v0.7.9 asset\"");
    assert_contains(&text, "\"version\": \"0.7.9\"");
    assert_contains(&text, "\"binary\": \"published v0.7.10 asset\"");
    assert_contains(&text, "\"version\": \"0.7.10\"");
    assert_contains(&text, "\"artifact\": \"error-voice-before-after\"");
}

#[test]
fn the_published_v0710_asset_carries_no_defect() {
    let text = artifact();
    assert_contains(&text, "\"total\": 8");
    assert_contains(&text, "\"baselineRefused\": 8");
    assert_contains(&text, "\"baselineForgedLines\": 5");
    assert_contains(&text, "\"baselineControlBytes\": 7");
    assert_contains(&text, "\"fixedRefused\": 8");
    assert_contains(&text, "\"fixedForgedLines\": 0");
    assert_contains(&text, "\"fixedControlBytes\": 0");
}

#[test]
fn the_published_v0710_artifact_carries_no_host_path() {
    let text = artifact();
    for marker in ["/home/", "/tmp/", "/Users/", "C:\\"] {
        assert!(
            !text.contains(marker),
            "found machine path marker `{marker}` in the v0.7.10 artifact"
        );
    }
}
