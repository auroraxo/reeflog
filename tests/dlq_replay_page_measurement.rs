//! The dlq-replay measurement must stay a MEASUREMENT, not a claim.
//!
//! Why this test exists (2026-11-10): the defect it describes is a SILENT
//! one — `dlq replay --limit 2` over 7 pending dead letters exited 0 with
//! `replayed: 2, failed: 0` while the journal still held 5 pending, and
//! nothing in the report or the exit code named them. A measurement about
//! such a defect can rot the same way: if the artifact is edited by hand,
//! or regenerated from a tree that no longer has the fix, the numbers keep
//! looking authoritative while describing nothing.
//!
//! So the artifact is pinned here:
//!
//!   * it must exist and parse as text;
//!   * the BASELINE numbers must still say what was measured on the
//!     published v0.7.11 binary — those numbers describe a RELEASED artifact
//!     and must not drift;
//!   * the FIXED numbers must say this tree refuses every measured defect
//!     case and behaves as specified on every measured guard;
//!   * every case must carry the same id in BOTH trees, so a case cannot be
//!     quietly deleted from the weak side;
//!   * the baseline must still show the defect it describes: the truncated
//!     passes exited 0 while the journal held pending letters, and the
//!     report had no field naming them;
//!   * the artifact must name the binaries it measured, and must not carry
//!     an absolute path from the machine that produced it.
//!
//! Falsified by construction: change a number and it goes red; delete a
//! case id from either tree and it goes red; flip a verdict and it goes
//! red.

use std::collections::BTreeSet;
use std::fs;
use std::path::PathBuf;

fn artifact() -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("research")
        .join("dlq-replay-page-2026-11-10.json");
    fs::read_to_string(&path).expect("research/dlq-replay-page-2026-11-10.json is readable")
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

/// The subtree of one tree: `"baseline": { … }` up to the next sibling key.
fn tree_block<'a>(text: &'a str, tree: &str) -> &'a str {
    let needle = format!("\"{tree}\":");
    let start = text
        .find(&needle)
        .unwrap_or_else(|| panic!("the artifact must carry the tree {tree}"));
    let rest = &text[start + needle.len()..];
    let end = rest.find("\"fixed\":").unwrap_or(rest.len());
    &rest[..end]
}

fn tree_field(text: &str, tree: &str, key: &str) -> String {
    field(tree_block(text, tree), key)
}

/// The ids of every case in one tree's block.
fn case_ids(block: &str) -> BTreeSet<String> {
    let mut ids = BTreeSet::new();
    let mut rest = block;
    while let Some(i) = rest.find("\"id\": \"") {
        let after = &rest[i + 7..];
        let end = after.find('"').expect("the id is terminated");
        ids.insert(after[..end].to_string());
        rest = &after[end..];
    }
    ids
}

#[test]
fn the_artifact_exists_and_names_what_it_measured() {
    let text = artifact();
    assert!(
        text.contains("dlq replay"),
        "the artifact must name the surface it measured"
    );
    assert!(
        text.contains("v0.7.11"),
        "the artifact must name the published release it measured against"
    );
    assert!(
        text.contains("sha256"),
        "the artifact must carry the digest of the binary it measured"
    );
}

#[test]
fn the_baseline_still_shows_the_defect() {
    let text = artifact();
    let b = tree_block(&text, "baseline");
    // Every measured case was VIOLATED on the published binary.
    assert_eq!(
        tree_field(&text, "baseline", "violations"),
        "5",
        "the published v0.7.11 binary violated every measured case"
    );
    // The truncated passes exited 0 while the journal held pending letters.
    assert!(
        b.contains("\"exit\": 0"),
        "the baseline's truncated passes exited 0 (artifact: {b})"
    );
    assert!(
        b.contains("\"journal_pending\": 5"),
        "the baseline left 5 pending letters behind (artifact: {b})"
    );
    // The report had no field naming them.
    assert!(
        b.contains("\"reported_remaining\": null"),
        "the baseline's report carried no remaining count (artifact: {b})"
    );
}

#[test]
fn the_fixed_tree_satisfies_every_measured_case() {
    let text = artifact();
    assert_eq!(
        tree_field(&text, "fixed", "violations"),
        "0",
        "this tree must violate no measured case"
    );
    let f = tree_block(&text, "fixed");
    assert!(
        f.contains("\"reported_remaining\": 5"),
        "the fixed tree names the 5 letters the page never reached"
    );
    assert!(
        f.contains("\"reported_remaining\": 1"),
        "the fixed tree names the tail the default page skipped"
    );
}

#[test]
fn both_trees_measured_the_same_cases() {
    let text = artifact();
    let b = case_ids(tree_block(&text, "baseline"));
    let f = case_ids(tree_block(&text, "fixed"));
    assert_eq!(
        b, f,
        "a case must not be quietly deleted from one side of the measurement"
    );
    assert!(
        b.contains("a_truncated_replay_reports_the_queue_not_the_page"),
        "the truncation case must be among the measured cases"
    );
    assert!(
        b.contains("a_full_drain_still_exits_clean"),
        "the guard must be among the measured cases"
    );
}

#[test]
fn the_artifact_carries_no_host_path() {
    let text = artifact();
    for needle in ["/home/", "/tmp/", "/Users/"] {
        assert!(
            !text.contains(needle),
            "the artifact must not carry a host path ({needle}): {text}"
        );
    }
}
