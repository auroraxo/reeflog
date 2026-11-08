//! The numeric-flag DOMAIN measurement must stay a MEASUREMENT, not a claim.
//!
//! Why this test exists (2026-11-08): the defect this work fixed was itself
//! a silent one — a flag VALUE outside the consumer's number space became
//! the DEFAULT, or a panic, or a hang — and the output looked exactly like
//! a successful run. A measurement about such a defect can rot the same way:
//! if the artifact is edited by hand, or regenerated from a tree that no
//! longer has the defect, the numbers keep looking authoritative while
//! describing nothing.
//!
//! So the artifact is pinned here:
//!
//!   * it must exist and parse;
//!   * the BASELINE numbers must still say what was measured on the
//!     published v0.7.2 binary — 4/11 cases behave as specified, 2 of them
//!     silently delivered the DEFAULT limit, 3 panicked and 1 hung. Those
//!     numbers are about a RELEASED artifact and must not drift;
//!   * the FIXED numbers must say the tree behaves as specified (11/11, no
//!     silent default, no hang, no panic);
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
        .join("numeric-flag-domain-2026-11-08.json");
    fs::read_to_string(&path).expect("research/numeric-flag-domain-2026-11-08.json is readable")
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
fn the_baseline_numbers_still_describe_the_published_v072_binary() {
    let text = artifact();
    assert_eq!(
        field(&text, "baselineOk"),
        "4",
        "the baseline was measured on the published v0.7.2 binary and must not drift"
    );
    assert_eq!(
        field(&text, "baselineTotal"),
        "11",
        "the baseline case count must not drift"
    );
    assert_eq!(
        field(&text, "baselineSilentDefaults"),
        "2",
        "the published v0.7.2 binary silently delivered the DEFAULT limit twice"
    );
    assert_eq!(
        field(&text, "baselinePanics"),
        "3",
        "the published v0.7.2 binary panicked on three out-of-domain values"
    );
    assert_eq!(
        field(&text, "baselineHangs"),
        "1",
        "the published v0.7.2 binary hung on one out-of-domain value"
    );
}

#[test]
fn the_fixed_numbers_say_the_tree_behaves_as_specified() {
    let text = artifact();
    assert_eq!(
        field(&text, "fixedOk"),
        "11",
        "every measured case must behave as specified on this tree"
    );
    assert_eq!(
        field(&text, "fixedTotal"),
        "11",
        "the case count must not drift"
    );
    assert_eq!(
        field(&text, "fixedSilentDefaults"),
        "0",
        "no refused command may fall back to a default"
    );
    assert_eq!(
        field(&text, "fixedHangs"),
        "0",
        "no refused command may hang"
    );
    assert_eq!(
        field(&text, "fixedPanics"),
        "0",
        "no refused command may panic"
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
        ids.len() >= 22,
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
