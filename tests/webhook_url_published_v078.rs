//! The PUBLISHED v0.7.8 artifact must carry the webhook URL fix.
//!
//! Why this test exists (2026-11-09): the webhook URL fix (commit 58335d7)
//! was measured against the published v0.7.7 asset — five of eight measured
//! cases violated the rule — and the fix itself sat on main only. A third
//! party who installed the pinned tag got the version that REFUSES a URL
//! the poster can deliver to (`HTTP://…` — the consumer `ureq` lowercases
//! the scheme and answers 200) and ACCEPTS one that dies on every delivery
//! with ureq's own words (`http: empty string`). v0.7.8 was cut to carry
//! the fix to the released artifact.
//!
//! So the claim to be pinned is not "this tree behaves" (the existing
//! measurement already pins that) but "the RELEASED artifact behaves". The
//! artifact below was measured against the published v0.7.8 linux asset,
//! whose archive sha256 equals the digest the release API publishes.
//!
//! What is pinned here:
//!
//!   * the artifact exists and parses as text;
//!   * the FIXED tree is the published v0.7.8 asset and says so, with the
//!     release tag in the source line — an artifact that measured a
//!     different binary must not pass as this one;
//!   * the published asset violates NONE of the measured cases (the fix
//!     reached the artifact, not only the branch);
//!   * the BASELINE still shows the defect it describes on v0.7.7 — that
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

fn artifact() -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("research")
        .join("webhook-url-domain-published-v0.7.8-2026-11-09.json");
    fs::read_to_string(&path)
        .expect("research/webhook-url-domain-published-v0.7.8-2026-11-09.json is readable")
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
    let rest = rest[colon + 1..].trim_start();
    if let Some(stripped) = rest.strip_prefix('"') {
        // A string value: read to the closing quote.
        let end = stripped.find('"').expect("the string value is closed");
        stripped[..end].to_string()
    } else {
        let end = rest
            .find(|c: char| c == ',' || c == '}' || c == '\n')
            .unwrap_or(rest.len());
        rest[..end].trim().to_string()
    }
}

/// The subtree of one tree: `"<tree>": [ … ]` up to the next sibling key.
fn tree_block<'a>(text: &'a str, tree: &str) -> &'a str {
    let needle = format!("\"{tree}\":");
    let start = text
        .find(&needle)
        .unwrap_or_else(|| panic!("the artifact must carry the tree {tree}"))
        + needle.len();
    let rest = &text[start..];
    let end = rest
        .find(&format!("\"{}\":", if tree == "baseline" { "fixed" } else { "summary" }))
        .unwrap_or(rest.len());
    &rest[..end]
}

/// The ids of every case measured on one side of the artifact, in order.
fn case_ids(text: &str, side: &str) -> Vec<String> {
    tree_block(text, side)
        .match_indices("\"id\": \"")
        .map(|(i, _)| {
            let rest = &tree_block(text, side)[i + 7..];
            let end = rest.find('"').expect("the id is closed");
            rest[..end].to_string()
        })
        .collect()
}

#[test]
fn the_published_v078_asset_violates_none_of_the_measured_cases() {
    let text = artifact();
    assert_eq!(field(&text, "baselineViolations"), "5");
    assert_eq!(field(&text, "fixedViolations"), "0");
    assert_eq!(field(&text, "verdict"), "fixed");
}

#[test]
fn the_fixed_side_is_the_published_v078_asset_not_the_branch() {
    let text = artifact();
    let source = field(&text, "fixedSource");
    assert!(
        source.contains("v0.7.8"),
        "the fixed side must be the published v0.7.8 asset, got: {source}"
    );
    assert!(
        source.contains("digest verified"),
        "the fixed side must be digest verified, got: {source}"
    );
    let baseline_source = field(&text, "baselineSource");
    assert!(
        baseline_source.contains("v0.7.7"),
        "the baseline must be the published v0.7.7 asset, got: {baseline_source}"
    );
}

#[test]
fn the_baseline_still_shows_the_defect_it_describes() {
    let text = artifact();
    assert_eq!(field(&text, "baselineViolations"), "5");
    let baseline = case_ids(&text, "baseline");
    assert_eq!(baseline.len(), 8, "the baseline must carry all 8 cases");
    for id in [
        "nohost-http",
        "nohost-https",
        "empty-auth",
        "uppercase-scheme",
        "mixed-case-scheme",
    ] {
        assert!(
            baseline.iter().any(|b| b == id),
            "the baseline must still measure the defect case {id}"
        );
    }
}

#[test]
fn both_trees_carry_the_same_case_ids() {
    let text = artifact();
    let baseline = case_ids(&text, "baseline");
    let fixed = case_ids(&text, "fixed");
    assert_eq!(baseline, fixed, "a case must not be deleted from one side");
}

#[test]
fn the_artifact_carries_no_host_path() {
    let text = artifact();
    for needle in ["/home/", "/root/", "/Users/", "/tmp/", "\\Users\\"] {
        assert!(
            !text.contains(needle),
            "the committed artifact must not carry the host path {needle}"
        );
    }
}
