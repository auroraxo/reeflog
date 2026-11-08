//! The numeric-flag measurement must stay a MEASUREMENT, not a claim.
//!
//! Why this test exists (2026-11-08): the defect this work fixed was itself
//! a silent one — a flag VALUE that could not be parsed became the DEFAULT,
//! and the output looked exactly like a successful run. A measurement about
//! such a defect can rot the same way: if the artifact is edited by hand, or
//! if it is regenerated from a tree that no longer has the defect, the
//! numbers keep looking authoritative while describing nothing.
//!
//! So the artifact is pinned here:
//!
//!   * it must exist and parse;
//!   * the BASELINE numbers must still say what was measured on the
//!     published v0.7.1 binary (4/10 cases behave as specified, and 2 of the
//!     "refusals" delivered rows anyway) — those numbers are about a
//!     RELEASED artifact and must not drift;
//!   * the FIXED numbers must say the tree behaves as specified (10/10, 0
//!     rows behind a refusal);
//!   * every case must carry the same id in both trees, so a case cannot be
//!     quietly deleted from one side;
//!   * the artifact must not contain an absolute path from the machine that
//!     produced it (the lesson from sememe's declared-origin artifact, which
//!     went red in CI because it carried the author's host path).
//!
//! Falsified by construction: change one number in the artifact and this
//! test goes red; delete a case id and it goes red; add a host path and it
//! goes red.

use std::collections::BTreeSet;
use std::fs;
use std::path::PathBuf;

fn artifact() -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("research")
        .join("numeric-flag-values-2026-11-08.json");
    fs::read_to_string(&path).expect("research/numeric-flag-values-2026-11-08.json is readable")
}

/// The artifact is JSON, but the test must not depend on a JSON crate (the
/// crate has none, and adding one for a test is not worth it). The fields
/// this test pins are read as plain text, which is enough: a wrong number
/// anywhere in the file changes the text.
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

#[test]
fn the_baseline_numbers_still_describe_the_published_v071_binary() {
    let text = artifact();
    assert_eq!(
        field(&text, "baselineOk"),
        "4",
        "the baseline was measured on the published v0.7.1 binary and must not drift"
    );
    assert_eq!(
        field(&text, "baselineTotal"),
        "10",
        "the baseline case count must not drift"
    );
    assert_eq!(
        field(&text, "baselineDeliveredRowsOnRefusal"),
        "2",
        "the published v0.7.1 binary delivered rows behind two of its refusals"
    );
}

#[test]
fn the_fixed_numbers_say_the_tree_behaves_as_specified() {
    let text = artifact();
    assert_eq!(
        field(&text, "fixedOk"),
        "10",
        "every measured case must behave as specified on this tree"
    );
    assert_eq!(
        field(&text, "fixedTotal"),
        "10",
        "the case count must not drift"
    );
    assert_eq!(
        field(&text, "fixedDeliveredRowsOnRefusal"),
        "0",
        "no refused command may deliver a row"
    );
}

#[test]
fn both_trees_measure_the_same_cases() {
    let text = artifact();
    // The case ids appear twice — once per tree. The id sets must be equal,
    // so a case cannot be dropped from one side to make a verdict look
    // better.
    let ids: Vec<&str> = text
        .match_indices("\"id\": \"")
        .map(|(i, _)| {
            let rest = &text[i + 7..];
            let end = rest.find('"').expect("the id is closed");
            &rest[..end]
        })
        .collect();
    assert!(
        ids.len() >= 20,
        "both trees must carry every case id, saw {} ids",
        ids.len()
    );
    let first = &ids[..ids.len() / 2];
    let second = &ids[ids.len() / 2..];
    let a: BTreeSet<&&str> = first.iter().collect();
    let b: BTreeSet<&&str> = second.iter().collect();
    assert_eq!(
        a, b,
        "the baseline and the fixed tree must measure the SAME cases"
    );
}

#[test]
fn the_artifact_carries_no_host_path() {
    let text = artifact();
    for needle in ["/home/", "/Users/", "C:\\"] {
        assert!(
            !text.contains(needle),
            "a committed measurement must not carry the path of the machine that produced it ({needle})"
        );
    }
}
