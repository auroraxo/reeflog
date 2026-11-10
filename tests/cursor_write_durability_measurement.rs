//! The MEASUREMENT artifact pins itself: `research/cursor-write-durability-2026-11-10.json`
//! is produced by `tools/measure-cursor-write-durability.mjs` — the published
//! v0.7.13 asset (sha256 digest == Release-API digest) must show all four
//! defect faces while this tree refuses all four.
//!
//! Falsified three ways (tools/falsify-cursor-write-pin.sh): a falsified
//! summary number turns the pin RED, a host path smuggled into the artifact
//! turns it RED, and the pin test source pointing at a branch instead of a
//! release digest turns it RED — the tree is restored byte-identical after
//! every falsification.

use std::process::Command;

const ARTIFACT: &str = "research/cursor-write-durability-2026-11-10.json";

fn read_artifact() -> String {
    std::fs::read_to_string(ARTIFACT).expect("the pinned measurement artifact exists (run `OUT=research/cursor-write-durability-2026-11-10.json node tools/measure-cursor-write-durability.mjs` from the repo root)")
}

fn assert_contains(hay: &str, needle: &str, what: &str) {
    assert!(
        hay.contains(needle),
        "{what}: expected `{needle}` in the artifact"
    );
}

/// The verdict and the headline numbers, exactly as measured.
#[test]
fn pin_artifact_verdict_and_numbers() {
    let a = read_artifact();
    assert_contains(&a, "\"verdict\": \"fixed\"", "verdict");
    assert_contains(&a, "\"baselineShowsDefect\": 4", "all four defect faces measured on the published asset");
    assert_contains(&a, "\"fixedDefectViolations\": 0", "the fixed tree violates nothing");
    assert_contains(&a, "\"guardViolations\": 0", "no guard violations");
    assert_contains(&a, "\"baselineDefectCases\": 4", "four defect cases defined");
}

/// The baseline is the PUBLISHED release asset, digest-verified against the
/// Release API — the measurement must never silently run against a local
/// build in the baseline seat.
#[test]
fn pin_baseline_is_the_digest_verified_published_asset() {
    let a = read_artifact();
    // The published v0.7.13 linux asset digest (== Release-API digest).
    assert_contains(
        &a,
        "\"archiveSha256\": \"4a0355f9536e30217b3b0ab2cb1510df1ab1583eafe0e0e8fa0c1cc577399eb6\"",
        "baseline archive digest",
    );
    assert_contains(&a, "\"tag\": \"v0.7.13\"", "baseline tag");
    // digest == apiDigest means the download was verified
    let start = a.find("\"archiveSha256\"").expect("digest key");
    let api = a[start..].find("\"apiDigest\"").expect("api digest key") + start;
    let archive = &a[start + 19..start + 19 + 64];
    let api_digest = &a[api + 15..api + 15 + 64];
    assert_eq!(archive, api_digest, "downloaded archive digest must equal the Release-API digest");
}

/// The artifact speaks about the PUBLISHED binary's behaviour, not about a
/// host: no absolute home or tmp paths anywhere in it.
#[test]
fn pin_artifact_carries_no_host_paths() {
    let a = read_artifact();
    assert!(
        !a.contains("/home/"),
        "artifact must not carry host home paths"
    );
    assert!(
        !a.contains("/tmp/"),
        "artifact must not carry host tmp paths"
    );
}

/// The fixed tree's refusal carries the CONTRACT the fix promises: file
/// unchanged, redelivery, at-least-once. This is the sentence a wedged
/// operator must read instead of a bare errno.
#[test]
fn pin_fixed_refusal_carries_the_redeliver_contract() {
    let a = read_artifact();
    assert_contains(&a, "the cursor file is UNCHANGED", "position-unchanged promise");
    assert_contains(&a, "REDLIVER", "redelivery promise");
    assert_contains(&a, "at-least-once", "delivery contract");
    // And the fixed run keeps the old content byte-exactly:
    assert_contains(&a, "\"cursorAfter\": \"0\\n\"", "old cursor survives the failed write");
    // While the published baseline lost it:
    assert_contains(&a, "\"cursorAfter\": \"\"", "baseline shows the truncation");
}

/// The ladder exists and its mutation filters name real test filters —
/// a pin that cannot be re-derived is decoration, not evidence.
#[test]
fn pin_ladder_exists_and_names_real_filters() {
    let ladder = std::fs::read_to_string("tools/mutation-ladder-cursor-write.sh")
        .expect("the mutation ladder exists");
    for filter in [
        "d1_tail_failed_write_keeps_the_old_cursor",
        "d2_reorg_tail_failed_write_keeps_the_old_cursor",
        "d3_first_batch_failed_write_leaves_no_file_at_the_cursor_path",
        "d4_retry_after_failed_write_redelivers_and_advances",
        "g1_successful_write_still_lands_the_structured_cursor",
    ] {
        assert!(
            ladder.contains(filter),
            "ladder must reference the real test filter {filter}"
        );
    }
    let _ = Command::new("true");
}
