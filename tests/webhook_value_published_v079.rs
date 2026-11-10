//! Pins the measurement of the PUBLISHED v0.7.9 release asset: the same 24
//! values, measured against the published v0.7.8 baseline and against the
//! published v0.7.9 asset (whose archive sha256 equals the digest the
//! Release API publishes).
//!
//! Why this exists (2026-11-10): the fix was measured on the tree first. A
//! fix that only lives on the branch is the defect class this repository has
//! measured three times, so the RELEASE itself is measured — not the build,
//! not the checkout.
//!
//! What is pinned:
//!
//!   * the artifact exists and names both binaries with the versions read
//!     from those binaries (`0.7.8` baseline, `0.7.9` subject);
//!   * 11 of 24 cases are defects in the baseline (accepted, delivery died);
//!   * the published v0.7.9 asset refuses all 15 it must refuse (the 11
//!     defects plus the 3 the baseline already refused, plus the leading
//!     whitespace id the baseline accepted-and-broke) and still delivers
//!     the 9 that always worked;
//!   * the artifact carries no host path.
//!
//! Falsified by construction: change any count, claim another version
//! measured, or inject a machine path, and these tests fail.

use std::fs;
use std::path::PathBuf;

fn artifact() -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("research")
        .join("webhook-value-domain-published-v0.7.9-2026-11-10.json");
    fs::read_to_string(&path)
        .expect("research/webhook-value-domain-published-v0.7.9-2026-11-10.json is readable")
}

#[test]
fn the_artifact_names_the_binaries_it_measured() {
    let text = artifact();
    assert!(
        text.contains("\"binary\": \"published v0.7.8 asset\""),
        "the baseline must be named"
    );
    assert!(
        text.contains("\"version\": \"0.7.8\""),
        "the baseline version is read from the binary"
    );
    assert!(
        text.contains("\"binary\": \"published v0.7.9 asset\""),
        "the subject must be named as the PUBLISHED v0.7.9 asset, not the tree"
    );
    assert!(
        text.contains("\"version\": \"0.7.9\""),
        "the subject version is read from the binary"
    );
}

#[test]
fn the_published_v079_asset_carries_no_defect() {
    let text = artifact();
    assert!(text.contains("\"total\": 24"));
    assert!(
        text.contains("\"baselineAcceptedButFailed\": 11"),
        "the baseline defects must not shrink quietly"
    );
    assert!(
        text.contains("\"baselineRefused\": 3"),
        "the baseline guards must not move"
    );
    assert!(
        text.contains("\"fixedRefused\": 15"),
        "the published v0.7.9 asset must refuse every case the fix refuses"
    );
    assert!(
        text.contains("\"fixedAcceptedAndDelivered\": 9"),
        "the 9 values that always worked must still deliver"
    );
}

#[test]
fn the_published_v079_artifact_carries_no_host_path() {
    let text = artifact();
    for marker in ["/home/", "/tmp/", "/Users/", "C:\\"] {
        assert!(
            !text.contains(marker),
            "found machine path marker `{marker}` in the v0.7.9 artifact"
        );
    }
}
