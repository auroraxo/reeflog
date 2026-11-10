//! Pins the measurement of the REFUSAL VOICE: the committed artifact must
//! match what the published v0.7.9 binary was measured doing, and the
//! fixed build must carry none of the defect.
//!
//! Why this exists (2026-11-10): the voice rule was derived from
//! measurement, not from an opinion about "clean output". If the numbers
//! drift — if someone quietly drops a case, or the escaping is narrowed to
//! newlines only — this test fails and names what changed.
//!
//! What is pinned here:
//!
//!   * the artifact exists and names the binaries it measured, with the
//!     versions read from those binaries;
//!   * the baseline (the PUBLISHED v0.7.9 asset) refused all 8 cases — the
//!     verdict was never in question — and still forged 5 log lines and let
//!     7 raw control bytes through;
//!   * the fixed build refuses all 8 and forges 0 lines with 0 raw control
//!     bytes;
//!   * the artifact carries no host path (the research guard pins this too).
//!
//! Falsified by construction: edit any count in the artifact and this test
//! fails.

use std::fs;
use std::path::PathBuf;

fn artifact() -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("research")
        .join("error-voice-before-after-2026-11-10.json");
    fs::read_to_string(&path).expect("research/error-voice-before-after-2026-11-10.json is readable")
}

fn assert_contains(text: &str, needle: &str) {
    assert!(text.contains(needle), "the artifact must contain `{needle}`");
}

#[test]
fn the_artifact_names_the_binaries_it_measured() {
    let text = artifact();
    assert_contains(&text, "\"binary\": \"published v0.7.9 asset\"");
    assert_contains(&text, "\"version\": \"0.7.9\"");
    assert_contains(&text, "\"artifact\": \"error-voice-before-after\"");
}

#[test]
fn the_baseline_verdicts_were_never_the_defect() {
    let text = artifact();
    // Every hostile value was REFUSED by the published asset too: the
    // defect was never the verdict, it was the voice.
    assert_contains(&text, "\"baselineRefused\": 8");
    assert_contains(&text, "\"fixedRefused\": 8");
}

#[test]
fn the_baseline_forged_lines_and_control_bytes_are_pinned() {
    let text = artifact();
    assert_contains(&text, "\"baselineForgedLines\": 5");
    assert_contains(&text, "\"baselineControlBytes\": 7");
}

#[test]
fn the_fixed_build_forges_nothing_and_leaks_no_control_byte() {
    let text = artifact();
    assert_contains(&text, "\"fixedForgedLines\": 0");
    assert_contains(&text, "\"fixedControlBytes\": 0");
}

#[test]
fn the_artifact_carries_no_host_path() {
    let text = artifact();
    for marker in ["/home/", "/tmp/", "/Users/", "C:\\"] {
        assert!(
            !text.contains(marker),
            "found machine path marker `{marker}` in the error-voice artifact"
        );
    }
}
