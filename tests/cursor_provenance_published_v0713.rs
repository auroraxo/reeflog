//! Pins the measurement of the PUBLISHED v0.7.13 release asset: the same
//! 4 crossings + 6 guards, measured against the published v0.7.12 baseline
//! and against the published v0.7.13 asset (whose archive sha256 equals the
//! digest the Release API publishes).
//!
//! Why this exists (2026-11-10): the fix was measured on the tree first
//! (research/cursor-provenance-2026-11-10.json), but a fix that only lives
//! on the branch is the defect class this repository has measured four times
//! — so the RELEASE itself is measured, not the build, not the checkout.
//! The defect lived IN the release: every consumer running the published
//! v0.7.12 binary accepted all four crossings with exit 0.
//!
//! What is pinned:
//!
//!   * both binaries are named as PUBLISHED release assets with their
//!     digests — v0.7.12 (ab28aac2…) and v0.7.13 (4a0355f9…);
//!   * the published v0.7.12 baseline ACCEPTED all four crossings (exit 0,
//!     refused: false) and each acceptance was visibly wrong: D1 delivered 0
//!     rows while the backlog waited, D2 replayed 17 delivered events, D3
//!     read a fresh journal as drained;
//!   * the published v0.7.13 asset REFUSES all four crossings (exit 1, 0
//!     rows, cursor file byte-identical) and keeps all six guards behaving
//!     as before — the fix is on the TAG a stranger installs;
//!   * the artifact carries no host path.
//!
//! Falsified by construction: change any count, claim another version
//! measured, or inject a machine path, and these tests fail.

use std::fs;
use std::path::PathBuf;

fn artifact() -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("research")
        .join("cursor-provenance-published-v0.7.13-2026-11-10.json");
    fs::read_to_string(&path)
        .expect("research/cursor-provenance-published-v0.7.13-2026-11-10.json is readable")
}

fn assert_contains(text: &str, needle: &str, why: &str) {
    assert!(text.contains(needle), "the artifact must contain `{needle}` — {why}");
}

#[test]
fn the_artifact_names_the_binaries_it_measured() {
    let text = artifact();
    assert_contains(
        &text,
        "\"tool\": \"measure-cursor-provenance\"",
        "the artifact declares which tool produced it",
    );
    assert_contains(
        &text,
        "\"source\": \"published v0.7.12 release asset (sha256 ab28aac213d2e5158d27086c22acb2947ab4f756d21d0b5da3adc614e5aac856)\"",
        "the baseline must be named as the PUBLISHED v0.7.12 asset with its digest",
    );
    assert_contains(
        &text,
        "\"source\": \"published v0.7.13 release asset (sha256 4a0355f9536e30217b3b0ab2cb1510df1ab1583eafe0e0e8fa0c1cc577399eb6)\"",
        "the subject must be named as the PUBLISHED v0.7.13 asset with its digest, not the tree",
    );
    assert_contains(
        &text,
        "\"bin\": \"published-reeflog\"",
        "the fixed side must be labelled as the published asset it was",
    );
}

#[test]
fn the_published_v0712_asset_accepted_every_crossing() {
    let text = artifact();
    for id in [
        "D1-events-cursor-on-reorgs-channel",
        "D2-reorgs-cursor-on-events-channel",
        "D3-cursor-copied-to-another-journal",
        "D4-crossing-refusal-leaves-file-untouched",
    ] {
        let block = case_block(&text, id, "baseline");
        assert_contains(&block, "\"exit\": 0", &format!("the published v0.7.12 asset accepted {id}"));
        assert_contains(&block, "\"refused\": false", &format!("v0.7.12 did not refuse {id}"));
    }
    // the visible wrongness of the acceptance must not drift either
    let d1 = case_block(&text, "D1-events-cursor-on-reorgs-channel", "baseline");
    assert_contains(&d1, "\"rows\": 0", "the published v0.7.12 starved the reorg channel: 3 reorgs waited, 0 were delivered");
    let d2 = case_block(&text, "D2-reorgs-cursor-on-events-channel", "baseline");
    assert_contains(&d2, "\"rows\": 17", "the published v0.7.12 replayed 17 already-delivered events");
    let d3 = case_block(&text, "D3-cursor-copied-to-another-journal", "baseline");
    assert_contains(&d3, "\"rows\": 0", "the published v0.7.12 read a fresh journal as drained");
}

#[test]
fn the_published_v0713_asset_refuses_every_crossing_and_keeps_every_guard() {
    let text = artifact();
    assert_contains(
        &text,
        "\"failures\": 0",
        "the published v0.7.13 asset must show zero measured failures",
    );
    for id in [
        "D1-events-cursor-on-reorgs-channel",
        "D2-reorgs-cursor-on-events-channel",
        "D3-cursor-copied-to-another-journal",
        "D4-crossing-refusal-leaves-file-untouched",
    ] {
        let block = case_block(&text, id, "fixed");
        assert_contains(&block, "\"exit\": 1", &format!("the published v0.7.13 asset must refuse {id}"));
        assert_contains(&block, "\"rows\": 0", &format!("a refusal on v0.7.13 delivers nothing ({id})"));
        assert_contains(&block, "\"refused\": true", &format!("{id} must be refused on the published v0.7.13 asset"));
    }
    for (id, rows) in [
        ("G1-first-run-no-file-delivers-all", 20),
        ("G2-bare-hand-written-number-stays-legal", 5),
        ("G3-same-channel-resume-exactly", 0),
        ("G4-two-channel-files-both-work", 3),
        ("G6-summary-line-names-a-plain-number", 20),
    ] {
        let block = case_block(&text, id, "fixed");
        assert_contains(
            &block,
            &format!("\"rows\": {rows}"),
            &format!("the guard {id} must behave the same on the published v0.7.13 asset"),
        );
    }
    let g5 = case_block(&text, "G5-invalid-content-still-refused", "fixed");
    assert_contains(&g5, "\"refused\": true", "`abc` must stay a refusal on the published v0.7.13 asset");
}

/// The block of one tree for one case: starts at the case id, ends at the
/// next case id.
fn case_block(text: &str, id: &str, tree: &str) -> String {
    let needle = format!("\"id\": \"{id}\"");
    let start = text
        .find(&needle)
        .unwrap_or_else(|| panic!("the artifact must carry the case {id}"));
    let rest = &text[start + needle.len()..];
    let end = rest
        .find("\"id\": \"")
        .map(|i| i + needle.len())
        .unwrap_or(rest.len());
    let block = &text[start..start + end];
    let tneedle = format!("\"{tree}\"");
    let tstart = block
        .find(&tneedle)
        .unwrap_or_else(|| panic!("the case {id} must carry the tree {tree}"));
    block[tstart..].to_string()
}

#[test]
fn the_artifact_carries_no_host_path() {
    let text = artifact();
    for needle in ["/home/", "/Users/", "/tmp/", "C:\\"] {
        assert!(
            !text.contains(needle),
            "a committed measurement must not carry the path of the machine that produced it ({needle})"
        );
    }
}
