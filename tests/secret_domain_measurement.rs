//! Pins the secret-domain measurements: the same probe against the PUBLISHED
//! v0.7.15 release asset, its distinct-secret guard, and this tree.
//!
//! Why this exists (2026-11-11): the delivery body a webhook signs carries
//! no target id and no endpoint, and the headers that do are not signed —
//! so with two targets sharing ONE secret on different urls, the pair one
//! receiver accepted verifies at the other (measured on the published
//! v0.7.15 asset, verdict `SIGNATURE-TARGET-AGNOSTIC`, 4 of 4 faces). The
//! fix refuses that config before a byte moves; the artifact for this tree
//! is therefore `REFUSED-UP-FRONT`, 0 faces measurable — because the door
//! is closed, not because nothing was measured.
//!
//! What is pinned:
//!
//!   * the published v0.7.15 asset VIOLATES all four faces (a measurement
//!     whose baseline is clean certifies nothing);
//!   * the published asset is named with its digest (the Release-API
//!     sha256 8f941253…), so the baseline is the release, not the build;
//!   * the distinct-secret GUARD on the same published asset is green —
//!     foreign pairs are refused with 401 by the receivers' own rules
//!     (the false-positive guard the fix must keep working);
//!   * this tree refuses the shared-secret config up front, and the refusal
//!     names the rule;
//!   * the artifacts carry no host path.
//!
//! Falsified by construction: change any count, claim another version was
//! measured, inject a machine path, or swap the verdict — and these fail.

use std::fs;
use std::path::PathBuf;

fn artifact(name: &str) -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("research")
        .join(name);
    fs::read_to_string(&path).unwrap_or_else(|e| panic!("research/{name} is readable: {e}"))
}

fn assert_contains(text: &str, needle: &str, why: &str) {
    assert!(text.contains(needle), "the artifact must contain `{needle}` — {why}");
}

fn assert_not_contains(text: &str, needle: &str, why: &str) {
    assert!(
        !text.contains(needle),
        "the artifact must NOT contain `{needle}` — {why}"
    );
}

#[test]
fn all_three_artifacts_declare_the_tool_and_the_method() {
    for name in [
        "secret-domain-published-v0.7.15-2026-11-11.json",
        "secret-domain-guard-published-v0.7.15-2026-11-11.json",
        "secret-domain-fixed-2026-11-11.json",
        "secret-domain-published-v0.7.16-2026-11-11.json",
        "secret-domain-guard-published-v0.7.16-2026-11-11.json",
    ] {
        let text = artifact(name);
        assert_contains(
            &text,
            "\"artifact\": \"secret-domain",
            "the artifact declares which measurement it is",
        );
        assert_contains(&text, "\"method\":", "the artifact declares how it was measured");
    }
}

#[test]
fn the_published_v0715_asset_violates_every_face() {
    let text = artifact("secret-domain-published-v0.7.15-2026-11-11.json");
    assert_contains(
        &text,
        "\"source\": \"published v0.7.15 release asset",
        "the baseline is the PUBLISHED v0.7.15 release asset, not the tree",
    );
    assert_contains(
        &text,
        "8f941253e82f5c1aa594c8b5253d6c603d29ffbf2f5ec7e25f8fbf94d16f5f35",
        "the published v0.7.15 archive sha256 equals the Release-API digest",
    );
    assert_contains(
        &text,
        "\"verdict\": \"SIGNATURE-TARGET-AGNOSTIC\"",
        "the published asset's verdict is the measured defect, not a clean run",
    );
    assert_contains(&text, "\"violations\": 4", "all four faces are violated");
    for face in [
        "F1_sharedSecretHmacValidatesAtBothTargets",
        "F2_deliveryIdHeaderOutsideSignature",
        "F3_crossTargetReplayAccepted200",
        "F4_seqHeaderLiesUnderValidSignature",
    ] {
        assert_contains(
            &text,
            &format!("\"{face}\""),
            "every face of the shared-secret defect is named in the artifact",
        );
    }
    // Every face was a receiver's OWN verdict: the receivers verified, and
    // a 200 for a forged pair means the receiver's rule said yes.
    assert_contains(
        &text,
        "\"violated\": true",
        "each face records its own violated verdict",
    );
    assert_not_contains(
        &text,
        "/home/",
        "the artifact carries no host path (a host path would make the numbers machine-local)",
    );
}

#[test]
fn the_distinct_secret_guard_on_the_published_asset_is_green() {
    let text = artifact("secret-domain-guard-published-v0.7.15-2026-11-11.json");
    assert_contains(
        &text,
        "\"source\": \"published v0.7.15 release asset",
        "the guard runs against the same published release",
    );
    assert_contains(
        &text,
        "\"rejected\": true",
        "with distinct secrets, a foreign pair is refused by the receivers' own rule",
    );
    assert_contains(
        &text,
        "\"status\": 401",
        "the refusal is the receiver's own verdict (401), not silence",
    );
    assert_contains(
        &text,
        "\"ok\": true",
        "the legitimate delivery with its own secret still verifies (no over-refusal)",
    );
    assert_contains(
        &text,
        "\"pushExit\": 0",
        "a config with distinct secrets still delivers — the guard is the false-positive guard",
    );
    assert_not_contains(&text, "/home/", "no host path");
}

#[test]
fn this_tree_refuses_the_shared_secret_config_up_front() {
    let text = artifact("secret-domain-fixed-2026-11-11.json");
    assert_contains(
        &text,
        "\"verdict\": \"REFUSED-UP-FRONT\"",
        "this tree closes the door before a byte moves",
    );
    assert_contains(
        &text,
        "\"refusedUpFront\": true",
        "the probe records WHY no face was measurable: the refusal",
    );
    assert_contains(
        &text,
        "\"violations\": 0",
        "no face can be violated when the config never loads",
    );
    assert_contains(
        &text,
        "share one secret",
        "the refusal names the rule in the artifact itself",
    );
    // The refused tree must be labelled as a tree build, never as a release.
    assert_contains(
        &text,
        "\"source\": \"tree build",
        "the fixed side is labelled as the tree build it was, not as a release",
    );
    assert_not_contains(&text, "/home/", "no host path");
}

#[test]
fn the_published_v0716_asset_refuses_the_shared_secret_config() {
    // The fix must reach the PIN: v0.7.16 is measured at the published
    // release asset, not at the branch build (a fix that only lives on the
    // branch is the defect class this repository has measured repeatedly).
    let text = artifact("secret-domain-published-v0.7.16-2026-11-11.json");
    assert_contains(
        &text,
        "\"source\": \"published v0.7.16 release asset",
        "the measured seat is the PUBLISHED v0.7.16 release asset, not the tree",
    );
    assert_contains(
        &text,
        "3b688f06c2a60a50ee2a0960ade6f52e4bdb5093e3e1d4ba32de893bf0d538ff",
        "the published v0.7.16 archive sha256 equals the Release-API digest",
    );
    assert_contains(
        &text,
        "\"binaryVersion\": \"reeflog 0.7.16\"",
        "the measured binary's version matches the tag",
    );
    assert_contains(
        &text,
        "\"verdict\": \"REFUSED-UP-FRONT\"",
        "the published v0.7.16 asset refuses the shared-secret config the v0.7.15 asset accepted",
    );
    assert_contains(
        &text,
        "\"violations\": 0",
        "no face is measurable on the fixed release: the door is closed",
    );
    assert_contains(
        &text,
        "share one secret",
        "the refusal line is carried in the artifact itself",
    );
    assert_not_contains(&text, "/home/", "no host path");
}

#[test]
fn the_published_v0716_asset_keeps_distinct_secrets_working() {
    // Over-refusal guard AT THE PIN: the same release that refuses the
    // shared secret must still deliver to two targets with their own
    // secrets, and their receivers' rules must stay sharp.
    let text = artifact("secret-domain-guard-published-v0.7.16-2026-11-11.json");
    assert_contains(
        &text,
        "\"source\": \"published v0.7.16 release asset",
        "the guard runs against the same published release",
    );
    assert_contains(
        &text,
        "\"pushExit\": 0",
        "a config with distinct secrets still delivers on v0.7.16",
    );
    assert_contains(
        &text,
        "\"ok\": true",
        "the legitimate delivery verifies (no over-refusal at the pin)",
    );
    assert_contains(
        &text,
        "\"rejected\": true",
        "foreign pairs are still refused by the receivers' own rules",
    );
    assert_contains(&text, "\"status\": 401", "the refusal is 401, the receiver's own verdict");
    assert_not_contains(&text, "/home/", "no host path");
}
