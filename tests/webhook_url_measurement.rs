//! Pins `research/webhook-url-domain-2026-11-09.json` against drift.
//!
//! The measurement tool (`tools/measure-webhook-url-domain.mjs`) measures
//! two binaries on the same 8 cases:
//!
//!   BASELINE  the published v0.7.7 binary, downloaded from GitHub Releases
//!              and verified against the digest the release API publishes;
//!   FIXED     the binary built from this tree.
//!
//! The defect it measures is a two-faced one: the config loader judged the
//! target URL with a string PREFIX, so it refused a URL the consumer can
//! deliver to (`HTTP://…`, which ureq lowercases and answers 200 to) and
//! accepted a URL the consumer cannot deliver to (`http://`, which dies with
//! ureq's "http: empty string" on the first delivery).
//!
//! A measurement about such a defect can rot the same way the defect did: if
//! the artifact is edited by hand, or regenerated from a tree that no longer
//! has the fix, the numbers keep looking authoritative while describing
//! nothing. So the artifact is pinned here:
//!
//!   * it must exist and parse as text;
//!   * no host path from the machine that produced it may appear in it;
//!   * the BASELINE numbers must still say what was measured on the released
//!     binary — 5 violations of 5 defect cases — and must not drift;
//!   * the FIXED numbers must say this tree violates nothing;
//!   * every case must carry the same id in BOTH trees, so a case cannot be
//!     quietly deleted from the weak side;
//!   * the verdict must be "fixed".
//!
//! Falsified by construction: change a number and it goes red; delete a
//! case id from either tree and it goes red; flip the verdict and it goes
//! red.

use std::collections::BTreeSet;
use std::fs;
use std::path::PathBuf;

fn artifact() -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("research")
        .join("webhook-url-domain-2026-11-09.json");
    fs::read_to_string(&path).expect("research/webhook-url-domain-2026-11-09.json is readable")
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

/// The case ids of one tree: every `"id": "…"` inside the tree's block.
fn case_ids(text: &str, tree: &str) -> BTreeSet<String> {
    let needle = format!("\"{tree}\":");
    let start = text
        .find(&needle)
        .unwrap_or_else(|| panic!("the artifact must carry the tree {tree}"))
        + needle.len();
    let rest = &text[start..];
    let end = rest
        .find("\"fixed\":")
        .filter(|e| *e > 0)
        .unwrap_or(rest.len());
    let block = &rest[..end];
    let mut ids = BTreeSet::new();
    let mut cursor = 0usize;
    while let Some(pos) = block[cursor..].find("\"id\":") {
        let abs = cursor + pos + "\"id\":".len();
        let rest2 = &block[abs..];
        let quote = rest2.find('"').expect("an id value is quoted");
        let rest3 = &rest2[quote + 1..];
        let close = rest3.find('"').expect("an id value is closed");
        ids.insert(rest3[..close].to_string());
        cursor = abs + quote + 1 + close;
    }
    ids
}

/// The number of `"violated": true` entries in one tree's block.
fn violated_count(text: &str, tree: &str) -> usize {
    let needle = format!("\"{tree}\":");
    let start = text
        .find(&needle)
        .unwrap_or_else(|| panic!("the artifact must carry the tree {tree}"))
        + needle.len();
    let rest = &text[start..];
    let end = rest.find("\"fixed\":").filter(|e| *e > 0).unwrap_or(rest.len());
    rest[..end].matches("\"violated\": true").count()
}

#[test]
fn the_artifact_records_the_measured_defect_and_its_fix() {
    let text = artifact();

    // No host path from the machine that produced it.
    for leak in ["/home/", "/tmp/", "/Users/", "C:\\"] {
        assert!(
            !text.contains(leak),
            "the artifact must not carry the host path `{leak}`"
        );
    }

    // The verdict and the counts.
    assert_eq!(field(&text, "verdict"), "fixed");
    assert_eq!(field(&text, "totalCases"), "8");
    assert_eq!(field(&text, "defectCases"), "5");
    assert_eq!(field(&text, "guardCases"), "3");
    assert_eq!(field(&text, "baselineViolations"), "5");
    assert_eq!(field(&text, "fixedViolations"), "0");

    // The baseline must still show the defect: 5 of the 8 cases violated on
    // the released binary.
    assert_eq!(
        violated_count(&text, "baseline"),
        5,
        "the baseline must still show 5 violations on the published binary"
    );
    assert_eq!(
        violated_count(&text, "fixed"),
        0,
        "this tree must violate nothing"
    );
}

#[test]
fn both_trees_carry_the_same_cases() {
    let text = artifact();
    let baseline = case_ids(&text, "baseline");
    let fixed = case_ids(&text, "fixed");
    assert_eq!(baseline.len(), 8, "the baseline must carry all 8 cases");
    assert_eq!(
        baseline, fixed,
        "a case may not be quietly deleted from one tree"
    );

    // The measured defect cases must be present by name: they are what the
    // artifact is about.
    for id in [
        "nohost-http",
        "nohost-https",
        "empty-auth",
        "uppercase-scheme",
        "mixed-case-scheme",
    ] {
        assert!(baseline.contains(id), "the case {id} must be measured");
    }
}
