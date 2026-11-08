//! The cursor-state measurement must stay a MEASUREMENT, not a claim.
//!
//! Why this test exists (2026-11-08): the defect this work fixed was itself
//! a silent one — an existing cursor file that does not carry a cursor
//! became cursor 0, and the run replayed the whole journal with exit 0,
//! looking exactly like a successful first run. A measurement about such a
//! defect can rot the same way: if the artifact is edited by hand, or
//! regenerated from a tree that no longer has the defect, the numbers keep
//! looking authoritative while describing nothing.
//!
//! So the artifact is pinned here:
//!
//!   * it must exist and parse;
//!   * the BASELINE numbers must still say what was measured on the
//!     published v0.7.3 binary — 5 of 5 defect cases behaved as specified
//!     there, and every one of them silently replayed all 20 rows with
//!     exit 0. Those numbers are about a RELEASED artifact and must not
//!     drift;
//!   * the FIXED numbers must say the tree behaves as specified: every
//!     defect case refused with 0 rows delivered, every guard unchanged;
//!   * both trees must carry the same case ids, so a case cannot be quietly
//!     deleted from one side;
//!   * a refused case must have left the cursor file untouched — the
//!     refusal must not destroy the evidence;
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
        .join("cursor-state-2026-11-08.json");
    fs::read_to_string(&path).expect("research/cursor-state-2026-11-08.json is readable")
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
        .find(|c: char| [',', '}', '\n'].contains(&c))
        .unwrap_or(rest.len());
    rest[..end].trim().trim_matches('"').to_string()
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

/// The block of one tree for one case, so a case's numbers can be read
/// without a JSON parser: the block starts at the case id and ends at the
/// next case id.
fn case_block(text: &str, id: &str, tree: &str) -> String {
    let needle = format!("\"id\": \"{id}\"");
    let start = text
        .find(&needle)
        .unwrap_or_else(|| panic!("the artifact must carry the case {id}"));
    // Search for the NEXT id AFTER this one — the needle itself is at the
    // start of the block.
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
    let block = &block[tstart..];
    // The tree block ends where the next top-level key of the case starts.
    let end = block
        .find("\"verdict\"")
        .map(|i| i + 1)
        .unwrap_or(block.len());
    block[..end].to_string()
}

#[test]
fn the_baseline_numbers_still_describe_the_published_v073_binary() {
    let text = artifact();
    assert_eq!(
        field(&text, "rows"),
        "20",
        "the baseline was measured with 20 planted rows and must not drift"
    );
    // The baseline's own verdict fields: 5 defect cases, each of which
    // replayed the whole journal with exit 0.
    for id in [
        "C1-corrupt-content",
        "C2-beyond-i64",
        "C3-negative",
        "C4-empty-file",
        "C5-float",
    ] {
        let block = case_block(&text, id, "baseline");
        assert!(
            block.contains("\"exit\": 0"),
            "the published v0.7.3 binary exited 0 on {id} and must not drift: {block}"
        );
        assert!(
            block.contains("\"rows\": 20"),
            "the published v0.7.3 binary replayed all 20 rows on {id} and must not drift: {block}"
        );
        assert!(
            block.contains("\"refused\": false"),
            "the published v0.7.3 binary did NOT refuse {id} and must not drift: {block}"
        );
    }
}

#[test]
fn the_fixed_numbers_say_the_tree_behaves_as_specified() {
    let text = artifact();
    assert_eq!(
        field(&text, "failures"),
        "0",
        "no measured case may fail on this tree"
    );
    for id in [
        "C1-corrupt-content",
        "C2-beyond-i64",
        "C3-negative",
        "C4-empty-file",
        "C5-float",
    ] {
        let block = case_block(&text, id, "fixed");
        assert!(
            block.contains("\"exit\": 1"),
            "{id} must be refused on this tree: {block}"
        );
        assert!(
            block.contains("\"rows\": 0"),
            "{id} must deliver no row at all: {block}"
        );
        assert!(
            block.contains("\"refused\": true"),
            "{id} must be refused on this tree: {block}"
        );
    }
}

#[test]
fn a_refusal_never_rewrote_the_cursor_file() {
    // The refusal must leave the evidence in place: a refusal that overwrote
    // the file would destroy what the operator needs to see.
    let text = artifact();
    for (id, content) in [
        ("C1-corrupt-content", "abc\\n"),
        ("C2-beyond-i64", "9223372036854775808\\n"),
        ("C3-negative", "-1\\n"),
        ("C5-float", "15.0\\n"),
    ] {
        let block = case_block(&text, id, "fixed");
        assert!(
            block.contains(&format!("\"fileAfter\": \"{content}\"")),
            "the refusal on {id} must leave the cursor file untouched: {block}"
        );
    }
}

#[test]
fn the_guards_are_unchanged_on_both_trees() {
    // A fix that refuses more than it must is a different defect: the first
    // run, the deliberate rewind and the exactly-once resume have to work
    // the same way as before.
    let text = artifact();
    for (id, rows) in [
        ("C6-missing-file-first-run", 20),
        ("G1-zero-in-file", 20),
        ("G2-valid-resume", 5),
        ("G3-write-cursor-file-form", 16),
        ("G4-ahead-of-tip", 0),
        ("G5-reorg-channel", 0),
    ] {
        for tree in ["baseline", "fixed"] {
            let block = case_block(&text, id, tree);
            assert!(
                block.contains(&format!("\"rows\": {rows}")),
                "the guard {id} must deliver {rows} rows on the {tree} tree: {block}"
            );
            assert!(
                block.contains("\"exit\": 0"),
                "the guard {id} must exit 0 on the {tree} tree: {block}"
            );
        }
    }
}

#[test]
fn both_trees_measure_the_same_cases() {
    let text = artifact();
    // Every case must carry BOTH trees. A case that only exists on one side
    // would let a verdict look better by deleting the inconvenient half.
    let all = ids(&text);
    assert!(
        all.len() >= 11,
        "the artifact must carry every case id, saw {} ids",
        all.len()
    );
    let unique: BTreeSet<&&str> = all.iter().collect();
    assert_eq!(
        unique.len(),
        all.len(),
        "the case ids must be unique — a duplicated id would hide a case"
    );
    for id in all {
        for tree in ["baseline", "fixed"] {
            let block = case_block(&text, id, tree);
            assert!(
                !block.is_empty(),
                "the case {id} must carry the tree {tree}"
            );
        }
    }
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
