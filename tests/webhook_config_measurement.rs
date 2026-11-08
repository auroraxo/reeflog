//! The webhook-config-domain measurement must stay a MEASUREMENT, not a claim.
//!
//! Why this test exists (2026-11-08): the defect this work fixed was itself
//! a silent one — a config value outside the domain its consumer can honour
//! was accepted (or silently re-interpreted), and the run looked like a
//! successful delivery pass. A measurement about such a defect can rot the
//! same way: if the artifact is edited by hand, or if it is regenerated from
//! a tree that no longer has the defect, the numbers keep looking
//! authoritative while describing nothing.
//!
//! So the artifact is pinned here:
//!
//!   * it must exist and parse;
//!   * the BASELINE numbers must still say what was measured on the
//!     published v0.7.4 binary — those numbers are about a RELEASED artifact
//!     and must not drift;
//!   * the FIXED numbers must say the tree behaves as specified (every
//!     refusal refused, every guard green);
//!   * every case must carry the same id in both trees, so a case cannot be
//!     quietly deleted from one side;
//!   * the artifact must not contain an absolute path from the machine that
//!     produced it (the lesson from sememe's declared-origin artifact, which
//!     went red in CI because it carried the author's host path);
//!   * the artifact must not contain a number rounded by the measurement
//!     itself — the values above 2^53 have to appear EXACTLY as they were
//!     written, or the tool is the defect (the trap the first version of
//!     this measurement fell into).
//!
//! Falsified by construction: change one number in the artifact and this
//! test goes red; delete a case id and it goes red; add a host path and it
//! goes red; round a big integer and it goes red.

use std::collections::BTreeSet;
use std::fs;
use std::path::PathBuf;

fn artifact() -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("research")
        .join("webhook-config-domain-2026-11-08.json");
    fs::read_to_string(&path).expect("research/webhook-config-domain-2026-11-08.json is readable")
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

/// The subtree of one tree: `"baseline": { … }` up to the next sibling key.
/// Every count is read from here — `"refusals"` appears once per tree, and a
/// bare `field()` would silently read the baseline's number while claiming to
/// describe this tree.
fn tree_block<'a>(text: &'a str, tree: &str) -> &'a str {
    let needle = format!("\"{tree}\":");
    let start = text
        .find(&needle)
        .unwrap_or_else(|| panic!("the artifact must carry the tree {tree}"));
    let rest = &text[start + needle.len()..];
    // The two tree blocks are the last keys of the artifact, so the next
    // top-level key after this one bounds the block.
    const NEXT: [&str; 1] = ["\"fixed\":"];
    let end = NEXT
        .iter()
        .filter_map(|n| rest.find(n))
        .filter(|i| *i > 0)
        .min()
        .unwrap_or(rest.len());
    &rest[..end]
}

/// A field inside one tree's block.
fn tree_field(text: &str, tree: &str, key: &str) -> String {
    field(tree_block(text, tree), key)
}

/// The block of one tree for one case: the case's row inside that tree.
fn case_block<'a>(text: &'a str, tree: &str, id: &str) -> &'a str {
    let block = tree_block(text, tree);
    let needle = format!("\"id\": \"{id}\"");
    let start = block
        .find(&needle)
        .unwrap_or_else(|| panic!("the {tree} tree must carry the case {id}"));
    let rest = &block[start + needle.len()..];
    let end = rest.find("\"id\": \"").unwrap_or(rest.len());
    &block[start..start + end]
}

/// The ids in the artifact, in order.
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
fn the_baseline_numbers_still_describe_the_published_v074_binary() {
    let text = artifact();
    let baseline = tree_block(&text, "baseline");
    assert_eq!(
        field(baseline, "refusals"),
        "0",
        "the baseline was measured on the published v0.7.4 binary and must not drift"
    );
    assert_eq!(
        field(baseline, "refusalCases"),
        "11",
        "the baseline case count must not drift"
    );
    // The cases that had to be KILLED on the published binary: a timeout_ms
    // that scheduled a deadline nobody will live to see. A measurement that
    // no longer shows the hang has stopped measuring the defect.
    for id in ["timeout-i64-max-plus-one", "timeout-above-ceiling"] {
        let row = case_block(&text, "baseline", id);
        assert!(
            row.contains("\"killed\": true"),
            "the published v0.7.4 binary hung on {id} and that must not drift: {row}"
        );
    }
}

#[test]
fn the_fixed_numbers_say_the_tree_behaves_as_specified() {
    let text = artifact();
    assert_eq!(
        tree_field(&text, "fixed", "refusals"),
        "11",
        "every measured refusal must be refused on this tree"
    );
    assert_eq!(
        tree_field(&text, "fixed", "refusalCases"),
        "11",
        "the case count must not drift"
    );
    assert_eq!(
        tree_field(&text, "fixed", "guards"),
        "5",
        "every measured guard must stay green on this tree"
    );
    assert_eq!(
        tree_field(&text, "fixed", "guardCases"),
        "5",
        "the guard count must not drift"
    );
}

#[test]
fn no_case_on_this_tree_was_killed() {
    // A kill is a HANG on the fixed tree. There must be none: every
    // out-of-domain value is refused before the delivery pass starts.
    let text = artifact();
    let fixed = tree_block(&text, "fixed");
    assert!(
        !fixed.contains("\"killed\": true"),
        "no case may hang on the fixed tree: {fixed}"
    );
}

#[test]
fn both_trees_measure_the_same_cases() {
    let text = artifact();
    let ids = ids(&text);
    assert!(
        ids.len() >= 32,
        "both trees must carry every case id, saw {} ids",
        ids.len()
    );
    let half = ids.len() / 2;
    let a: BTreeSet<&&str> = ids[..half].iter().collect();
    let b: BTreeSet<&&str> = ids[half..].iter().collect();
    assert_eq!(
        a, b,
        "the baseline and the fixed tree must measure the SAME cases"
    );
}

#[test]
fn the_artifact_carries_no_host_path() {
    let text = artifact();
    for needle in ["/home/", "/Users/", "C:\\", "/tmp/reeflog"] {
        assert!(
            !text.contains(needle),
            "a committed measurement must not carry the path of the machine that produced it ({needle})"
        );
    }
}

#[test]
fn the_big_integers_survived_the_measurement() {
    // The trap: JS numbers are f64, so an integer above 2^53 is rounded
    // before it is ever written. If the artifact shows 18446744073709552000
    // where the operator wrote 18446744073709551615, the tool is the defect.
    let text = artifact();
    for exact in [
        "18446744073709551615",
        "18446744073709551616",
        "9223372036854775808",
    ] {
        assert!(
            text.contains(exact),
            "the artifact must carry the integer {exact} exactly as it was written — \
             a rounded form means the measurement tool lost it"
        );
    }
    for rounded in [
        "18446744073709552000",
        "9223372036854776000",
        "1.8446744073709552e+19",
        "9.223372036854776e+18",
    ] {
        assert!(
            !text.contains(rounded),
            "the artifact must not carry the f64-rounded form {rounded} — \
             the tool must not lose the value it is measuring"
        );
    }
}
