//! The PUBLISHED v0.7.7 artifact must carry the dead-letter fix.
//!
//! Why this test exists (2026-11-09): the fix for the dead-letter report
//! (commit 498b20e) was measured against the published v0.7.6 asset — five of
//! eight measured cases violated the rule — and the fix itself sat on main
//! only. A third party who installs the pinned tag got the version that
//! answers "nothing is quarantined" with exit 0 for a target id that does
//! not exist. v0.7.7 was cut to carry the fix to the released artifact.
//!
//! So the claim to be pinned is not "this tree behaves" (the existing
//! measurement already pins that) but "the RELEASED artifact behaves". The
//! artifact below was measured against the published v0.7.7 linux asset,
//! whose archive sha256 equals the digest the release API publishes.
//!
//! What is pinned here:
//!
//!   * the artifact exists and parses as text;
//!   * the FIXED tree is the published v0.7.7 asset and says so, with the
//!     archive digest in the source line — an artifact that measured a
//!     different binary must not pass as this one;
//!   * the published asset violates NONE of the measured cases (the fix
//!     reached the artifact, not only the branch);
//!   * every guard is still as specified (the fix did not break the
//!     false-positive side);
//!   * the BASELINE still shows the defect it describes on v0.7.6 — that
//!     number describes a RELEASED artifact and must not drift;
//!   * both trees carry the same case ids, so a case cannot be quietly
//!     deleted from the weak side;
//!   * the artifact carries no host path from the machine that produced it.
//!
//! Falsified by construction: change the fixed tree's violation count and
//! it goes red; rename the source line and it goes red; delete a case id
//! from one tree and it goes red.

use std::fs;
use std::path::PathBuf;

const PUBLISHED_DIGEST: &str = "95b2737ea3abb78a739d2ebc00ecf9c28d0c4da830b062cd5d7329963bbfc753";

fn artifact() -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("research")
        .join("dlq-target-domain-published-v0.7.7-2026-11-09.json");
    fs::read_to_string(&path)
        .expect("research/dlq-target-domain-published-v0.7.7-2026-11-09.json is readable")
}

/// The artifact is JSON, but this test must not depend on a JSON crate.
/// Every field it pins is read as plain text, which is enough: a wrong
/// number anywhere changes the text.
fn field(text: &str, key: &str) -> String {
    let needle = format!("\"{key}\"");
    let start = text
        .find(&needle)
        .unwrap_or_else(|| panic!("the artifact must carry the field {key}"))
        + needle.len();
    let rest = &text[start..];
    let colon = rest.find(':').expect("the field is followed by a colon");
    let rest = &rest[colon + 1..];
    let end = rest
        .find(|c: char| c == ',' || c == '}' || c == '\n')
        .unwrap_or(rest.len());
    rest[..end].trim().trim_matches('"').to_string()
}

/// The subtree of one tree: `"<tree>": { … }` up to the next sibling key.
fn tree_block<'a>(text: &'a str, tree: &str) -> &'a str {
    let needle = format!("\"{tree}\":");
    let start = text
        .find(&needle)
        .unwrap_or_else(|| panic!("the artifact must carry the tree {tree}"));
    let rest = &text[start + needle.len()..];
    const NEXT: [&str; 2] = ["\"fixed\":", "\"note\":"];
    let end = NEXT
        .iter()
        .filter_map(|n| rest.find(n))
        .filter(|i| *i > 0)
        .min()
        .unwrap_or(rest.len());
    &rest[..end]
}

fn tree_field(text: &str, tree: &str, key: &str) -> String {
    field(tree_block(text, tree), key)
}

fn ids(text: &str) -> Vec<&str> {
    text.match_indices("\"id\": \"")
        .map(|(i, _)| {
            let rest = &text[i + 7..];
            let end = rest.find('"').expect("the id is closed");
            &rest[..end]
        })
        .collect()
}

#[test]
fn the_measurement_names_the_published_v0_7_7_asset_and_its_digest() {
    let text = artifact();
    let source = tree_field(&text, "fixed", "source");
    assert!(
        source.contains("published v0.7.7"),
        "the fixed tree must say it measured the PUBLISHED v0.7.7 asset, got: {source}"
    );
    assert!(
        source.contains(PUBLISHED_DIGEST),
        "the fixed tree must carry the archive digest the release API publishes, got: {source}"
    );
}

#[test]
fn the_published_asset_violates_no_measured_case() {
    let text = artifact();
    assert_eq!(
        tree_field(&text, "fixed", "violations"),
        "0",
        "the published v0.7.7 asset must break every measured defect case — the fix must \
         have reached the RELEASED artifact, not only the branch"
    );
    let fixed = tree_block(&text, "fixed");
    for id in ids(fixed) {
        let needle = format!("\"id\": \"{id}\"");
        let start = fixed.find(&needle).expect("the id is present");
        let rest = &fixed[start + needle.len()..];
        let end = rest.find("\"id\": \"").unwrap_or(rest.len());
        let case = &fixed[start..start + end];
        assert_eq!(
            field(case, "verdict"),
            "as_specified",
            "the published v0.7.7 asset must behave as specified on {id}"
        );
    }
}

#[test]
fn the_baseline_still_shows_the_defect_on_the_previous_release() {
    let text = artifact();
    assert_eq!(
        tree_field(&text, "baseline", "violations"),
        "5",
        "the baseline was measured on the published v0.7.6 binary: five of the eight cases \
         violated the rule — this number describes a RELEASED artifact and must not drift"
    );
    assert!(
        tree_field(&text, "baseline", "source").contains("published v0.7.6"),
        "the baseline must still say it was the published v0.7.6 asset"
    );
}

#[test]
fn both_trees_carry_the_same_case_ids() {
    let text = artifact();
    let baseline: Vec<&str> = ids(tree_block(&text, "baseline"));
    let fixed: Vec<&str> = ids(tree_block(&text, "fixed"));
    assert_eq!(
        baseline, fixed,
        "a case must not be quietly deleted from one side of the measurement"
    );
    assert!(
        baseline.len() >= 8,
        "the measurement must carry at least the eight measured cases, got {}",
        baseline.len()
    );
}

#[test]
fn the_artifact_carries_no_host_path() {
    let text = artifact();
    for marker in ["/home/", "/tmp/", "/Users/", "C:\\"] {
        assert!(
            !text.contains(marker),
            "the artifact must not carry the host path marker {marker}"
        );
    }
}
