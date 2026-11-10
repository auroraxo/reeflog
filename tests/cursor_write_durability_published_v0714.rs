//! Pins the measurement of the PUBLISHED v0.7.14 release asset: the same
//! 4 defect faces + 4 guards, measured against the published v0.7.13
//! baseline and against the published v0.7.14 asset (whose archive sha256
//! equals the digest the Release API publishes).
//!
//! Why this exists (2026-11-10): the fix was measured on the tree first
//! (research/cursor-write-durability-2026-11-10.json), but a fix that only
//! lives on the branch is the defect class this repository has measured
//! repeatedly — so the RELEASE itself is measured, not the build, not the
//! checkout. The defect lived IN the release: the published v0.7.13 binary
//! truncated the cursor file to zero bytes on a failed write, wedging every
//! later run.
//!
//! What is pinned:
//!
//!   * both binaries are named as PUBLISHED release assets with their
//!     digests — v0.7.13 (4a0355f9…) and v0.7.14 (9b6c3034…);
//!   * the published v0.7.13 baseline VIOLATES all four defect cases
//!     (cursor truncated / empty file created / rewind wedge);
//!   * the published v0.7.14 asset refuses all four (old cursor kept,
//!     no file created, retry redelivers) with the guards unchanged —
//!   * the fix is on the TAG a stranger installs;
//!   * the artifact carries no host path.
//!
//! Falsified by construction: change any count, claim another version
//! measured, or inject a machine path, and these tests fail.

use std::fs;
use std::path::PathBuf;

fn artifact() -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("research")
        .join("cursor-write-durability-published-v0.7.14-2026-11-10.json");
    fs::read_to_string(&path)
        .expect("research/cursor-write-durability-published-v0.7.14-2026-11-10.json is readable")
}

fn assert_contains(text: &str, needle: &str, why: &str) {
    assert!(text.contains(needle), "the artifact must contain `{needle}` — {why}");
}

#[test]
fn the_artifact_names_the_binaries_it_measured() {
    let text = artifact();
    assert_contains(
        &text,
        "\"tool\": \"measure-cursor-write-durability\"",
        "the artifact declares which tool produced it",
    );
    assert_contains(
        &text,
        "\"tag\": \"v0.7.13\"",
        "the baseline is the PUBLISHED v0.7.13 release tag",
    );
    assert_contains(
        &text,
        "\"archiveSha256\": \"4a0355f9536e30217b3b0ab2cb1510df1ab1583eafe0e0e8fa0c1cc577399eb6\"",
        "the baseline archive sha256 equals the Release-API digest",
    );
    assert_contains(
        &text,
        "\"tag\": \"v0.7.14\"",
        "the fixed side is the PUBLISHED v0.7.14 release tag, not the tree",
    );
    assert_contains(
        &text,
        "\"archiveSha256\": \"9b6c30348f854af2acbd78ccbb474ae0837db1f34bd806626a7e2796bfe2611e\"",
        "the published v0.7.14 archive sha256 equals the Release-API digest",
    );
    assert_contains(
        &text,
        "\"source\": \"published v0.7.14 release asset (sha256 9b6c30348f854af2acbd78ccbb474ae0837db1f34bd806626a7e2796bfe2611e)\"",
        "the fixed seat must be labelled as the published asset it was, not 'this tree'",
    );
}

#[test]
fn the_published_v0713_asset_violates_every_defect_case() {
    let text = artifact();
    assert_contains(
        &text,
        "\"baselineShowsDefect\": 4",
        "the published v0.7.13 asset must show all four defect faces (a measurement whose baseline is clean certifies nothing)",
    );
    assert_contains(
        &text,
        "\"baselineDefectCases\": 4",
        "exactly four defect cases are defined",
    );
    // Each defect case on the baseline side: the write FAILED and the old
    // cursor was destroyed (empty file / truncated position).
    let d1 = case_block(&text, "D1");
    assert_contains(&d1, "\"cursorAfter\": \"\"", "D1 baseline: the cursor file was truncated to zero bytes");
    let d3 = case_block(&text, "D3");
    assert_contains(&d3, "\"fileExists\": true", "D3 baseline: an empty file APPEARED at the cursor path (the wedge)");
    assert_contains(&d3, "\"fileSize\": 0", "D3 baseline: that file was zero bytes");
    let d4 = case_block(&text, "D4");
    assert_contains(&d4, "\"retryExit\": 1", "D4 baseline: the next run was wedged at exit 1");
    assert_contains(&d4, "\"retryRows\": 0", "D4 baseline: and delivered nothing — the documented repair is a full rewind");
}

#[test]
fn the_published_v0714_asset_keeps_the_cursor_and_the_guards() {
    let text = artifact();
    assert_contains(
        &text,
        "\"fixedDefectViolations\": 0",
        "the published v0.7.14 asset violates none of the four defect cases",
    );
    assert_contains(
        &text,
        "\"guardViolations\": 0",
        "no guard regressed on either side",
    );
    assert_contains(&text, "\"verdict\": \"fixed\"", "the artifact's verdict");
    let d1 = case_block(&text, "D1");
    assert_contains(&d1, "\"cursorAfter\": \"0\\n\"", "D1 fixed: the old cursor survived the failed write byte-exactly");
    assert_contains(
        &d1,
        "the cursor file is UNCHANGED",
        "D1 fixed: the refusal carries the at-least-once redeliver contract",
    );
    let d3 = case_block(&text, "D3");
    assert_contains(&d3, "\"fileExists\": false", "D3 fixed: no file was created at the cursor path");
    let d4 = case_block(&text, "D4");
    assert_contains(&d4, "\"retryExit\": 0", "D4 fixed: a retry after the failed write succeeds");
    assert_contains(&d4, "\"retryRows\": 5", "D4 fixed: and redelivers the same five rows (at-least-once)");
}

#[test]
fn the_artifact_carries_no_host_path() {
    let text = artifact();
    for forbidden in ["/home/", "/Users/", "/tmp/"] {
        assert!(
            !text.contains(forbidden),
            "the committed artifact must not contain the machine path fragment `{forbidden}`"
        );
    }
}

/// The per-case JSON block for one id, keyed off the `"id": "<D1>"` marker
/// and cut at the next `"id":` (the artifact is pretty-printed, so blocks
/// are separated lines — a plain `contains` would match across cases).
fn case_block(text: &str, id: &str) -> String {
    let start = text
        .find(&format!("\"id\": \"{id}\""))
        .unwrap_or_else(|| panic!("the artifact must contain a case `{id}`"));
    let rest = &text[start..];
    let end = rest
        .find("\n      \"id\": ")
        .unwrap_or(rest.len());
    rest[..end].to_string()
}
