//! Pins the committed measurement of the PROGRAM IDENTITY:
//! research/program-identity-2026-11-10.json.
//!
//! Why this exists: the tree's own green tests say the tree is fine. The
//! artifact says WHICH binary was measured, against WHICH baseline, with
//! WHAT numbers — and if the artifact does not match a fresh run of the
//! tool, the claim is hollow. So the numbers are pinned and the tool is
//! re-runnable.
//!
//! What is pinned:
//!
//!   * the artifact names both binaries with the versions read from the
//!     binaries themselves;
//!   * the published v0.7.10 baseline violates 3 of 6 cases and CHANGED the
//!     journal's identity in 1 of them (the silent `init` re-point);
//!   * this tree violates 0 of 6 and changes identity 0 times;
//!   * the guards (matching program, silent caller, idempotent re-init) are
//!     recorded as cases in the artifact;
//!   * the artifact carries no host path.
//!
//! Falsified by construction: change any count, claim another version
//! measured, or inject a machine path, and these tests fail.

use std::fs;
use std::path::PathBuf;

fn artifact() -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("research")
        .join("program-identity-2026-11-10.json");
    fs::read_to_string(&path).expect("research/program-identity-2026-11-10.json is readable")
}

fn assert_contains(text: &str, needle: &str) {
    assert!(text.contains(needle), "the artifact must contain `{needle}`");
}

#[test]
fn the_artifact_names_the_binaries_it_measured() {
    let text = artifact();
    assert_contains(&text, "\"artifact\": \"program-identity-before-after\"");
    assert_contains(&text, "\"binary\": \"published v0.7.10 asset\"");
    assert_contains(&text, "\"version\": \"0.7.10\"");
    assert_contains(&text, "\"binary\": \"tree build\"");
    assert_contains(&text, "\"version\": \"0.7.10\"");
}

#[test]
fn the_baseline_carries_the_defect_and_the_tree_does_not() {
    let text = artifact();
    assert_contains(&text, "\"total\": 6");
    assert_contains(&text, "\"baselineViolations\": 3");
    assert_contains(&text, "\"fixedViolations\": 0");
    assert_contains(&text, "\"baselineIdentityChanges\": 1");
    assert_contains(&text, "\"fixedIdentityChanges\": 0");
}

#[test]
fn every_face_and_guard_is_a_case() {
    let text = artifact();
    for case in [
        "\"case\": \"init-repoints\"",
        "\"case\": \"verify-ignores\"",
        "\"case\": \"repair-ignores\"",
        "\"case\": \"guard-matching-program\"",
        "\"case\": \"guard-silent-caller\"",
        "\"case\": \"guard-idempotent-init\"",
    ] {
        assert_contains(&text, case);
    }
}

#[test]
fn the_artifact_carries_no_host_path() {
    let text = artifact();
    for marker in ["/home/", "/tmp/", "/Users/", "C:\\"] {
        assert!(
            !text.contains(marker),
            "found machine path marker `{marker}` in the program-identity artifact"
        );
    }
}
