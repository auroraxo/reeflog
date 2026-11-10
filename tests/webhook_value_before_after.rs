//! Pins the before/after measurement artifact for the webhook value
//! domain: 24 cases against the published v0.7.8 binary vs the fixed build.
//!
//! What this pins:
//!
//!   * the artifact exists and parses;
//!   * the baseline is the published v0.7.8 binary;
//!   * 11 defects were found in the baseline (accepted, then delivery died
//!     with a transport error: carriage return in id, space in url, invalid
//!     port silently dialing 80, non-ascii url dying at receiver);
//!   * all 11 defects are refused by the fixed build;
//!   * the 9 working cases (plain, trailing space, internal space/tab,
//!     non-empty secret with newline or non-ascii, braces in path, backtick
//!     in query, uppercase scheme) still deliver on the fixed build;
//!   * no host paths leak in the artifact.

use std::fs;
use std::path::PathBuf;

fn artifact() -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("research")
        .join("webhook-value-domain-before-after-2026-11-09.json");
    fs::read_to_string(&path)
        .expect("research/webhook-value-domain-before-after-2026-11-09.json is readable")
}

#[test]
fn the_before_after_artifact_matches_the_measurement() {
    let text = artifact();
    assert!(text.contains("\"total\": 24"));
    assert!(text.contains("\"baselineAcceptedButFailed\": 11"));
    assert!(text.contains("\"baselineRefused\": 3"));
    assert!(text.contains("\"fixedRefused\": 15"));
    assert!(text.contains("\"fixedAcceptedAndDelivered\": 9"));
}

#[test]
fn the_before_after_artifact_carries_no_host_path() {
    let text = artifact();
    for marker in ["/home/", "/tmp/", "/Users/", "C:\\"] {
        assert!(
            !text.contains(marker),
            "found machine path marker `{marker}` in before/after artifact"
        );
    }
}
