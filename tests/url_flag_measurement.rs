//! The URL-flag-domain measurement must stay a MEASUREMENT, not a claim.
//!
//! Why this test exists (2026-11-09): the defect it describes was itself a
//! silent one — a `--ws`/`--rpc` value outside what the consumer can speak
//! was accepted by the parser, and the run looked like a normal one (on
//! v0.7.5, `start --ws ftp://x/y` exited 0 with a journal it had created).
//! A measurement about such a defect can rot the same way: if the artifact
//! is edited by hand, or regenerated from a tree that no longer has the
//! fix, the numbers keep looking authoritative while describing nothing.
//!
//! So the artifact is pinned here:
//!
//!   * it must exist and parse as text;
//!   * the BASELINE numbers must still say what was measured on the
//!     published v0.7.5 binary — those numbers describe a RELEASED artifact
//!     and must not drift;
//!   * the FIXED numbers must say this tree refuses every measured case and
//!     guards every measured guard;
//!   * every case must carry the same id in BOTH trees, so a case cannot be
//!     quietly deleted from the weak side;
//!   * no baseline case may be recorded as a journal-creating refusal —
//!     on v0.7.5 the `--ws` refusals did not exist, and each of those cases
//!     CREATED a journal. If that stops being true, the defect being
//!     described has changed;
//!   * the artifact must not contain an absolute path from the machine that
//!     produced it (the directory-wide guard in research_artifacts.rs backs
//!     this up, but the measurement is pinned on its own too).
//!
//! Falsified by construction: change a number and it goes red; delete a
//! case id from either tree and it goes red; add a host path and it goes
//! red.

use std::collections::BTreeSet;
use std::fs;
use std::path::PathBuf;

fn artifact() -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("research")
        .join("url-flag-domain-2026-11-09.json");
    fs::read_to_string(&path)
        .expect("research/url-flag-domain-2026-11-09.json is readable")
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
/// Every count is read from here — `refusals` appears once per tree, and a
/// bare `field()` would silently read the baseline's number while claiming
/// to describe this tree.
fn tree_block<'a>(text: &'a str, tree: &str) -> &'a str {
    let needle = format!("\"{tree}\":");
    let start = text
        .find(&needle)
        .unwrap_or_else(|| panic!("the artifact must carry the tree {tree}"));
    let rest = &text[start + needle.len()..];
    // `fixed` is the last top-level tree block, so the next top-level key
    // after `baseline` bounds it.
    const NEXT: [&str; 1] = ["\"fixed\":"];
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

/// The block of one tree for one case.
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
fn the_baseline_numbers_still_describe_the_published_v075_binary() {
    let text = artifact();
    let baseline = tree_block(&text, "baseline");
    assert_eq!(
        field(baseline, "refusals"),
        "0",
        "the baseline was measured on the published v0.7.5 binary, whose URL flags had no domain at all — this number must not drift"
    );
    assert_eq!(
        field(baseline, "refusalCases"),
        "11",
        "the baseline case count must not drift"
    );
    assert_eq!(
        field(baseline, "guards"),
        "6",
        "the guards passed on the baseline too — the pre-fix parser refused nothing, which is exactly the defect"
    );
}

#[test]
fn the_fixed_numbers_say_the_tree_behaves_as_specified() {
    let text = artifact();
    assert_eq!(
        tree_field(&text, "fixed", "refusals"),
        "11",
        "every measured out-of-domain URL must be refused by the parser on this tree"
    );
    assert_eq!(
        tree_field(&text, "fixed", "refusalCases"),
        "11",
        "the case count must not drift"
    );
    assert_eq!(
        tree_field(&text, "fixed", "guards"),
        "6",
        "every value the consumers CAN speak must still be accepted on this tree"
    );
    assert_eq!(
        tree_field(&text, "fixed", "guardCases"),
        "6",
        "the guard count must not drift"
    );
}

#[test]
fn no_fixed_case_was_killed_or_created_a_journal() {
    let text = artifact();
    let fixed = tree_block(&text, "fixed");
    assert!(
        !fixed.contains("\"killed\": true"),
        "no case may hang on the fixed tree: {fixed}"
    );
    for id in [
        "ws-scheme-ftp",
        "ws-scheme-https",
        "ws-empty",
        "ws-single-slash",
        "ws-no-host",
        "ws-no-scheme",
        "rpc-scheme-ftp",
        "rpc-scheme-ws",
        "rpc-no-scheme",
        "rpc-empty",
        "rpc-no-host",
    ] {
        let row = case_block(&text, "fixed", id);
        assert!(
            !row.contains("\"journalCreated\": true"),
            "{id} must be refused BEFORE anything is created — the refusal left a journal behind: {row}"
        );
    }
}

#[test]
fn the_baseline_did_create_journals_for_the_cases_it_accepted() {
    // The defect, pinned: on the published binary every `--ws` refusal case
    // ran instead, and a run that starts creates a journal. If a baseline
    // row stops saying so, the artifact no longer describes the published
    // binary.
    let text = artifact();
    for id in [
        "ws-scheme-ftp",
        "ws-scheme-https",
        "ws-empty",
        "ws-single-slash",
        "ws-no-host",
        "ws-no-scheme",
    ] {
        let row = case_block(&text, "baseline", id);
        assert!(
            row.contains("\"journalCreated\": true"),
            "on v0.7.5, {id} was ACCEPTED and created a journal — that is the defect: {row}"
        );
        assert!(
            row.contains("\"exit\": 0"),
            "on v0.7.5, {id} exited 0 like a healthy run: {row}"
        );
    }
}

#[test]
fn both_trees_measure_the_same_cases() {
    let text = artifact();
    let ids = ids(&text);
    assert!(
        ids.len() >= 34,
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
    for needle in ["/home/", "/tmp/", "/Users/", "C:\\"] {
        assert!(
            !text.contains(needle),
            "a committed measurement must not carry the path of the machine that produced it ({needle})"
        );
    }
}

#[test]
fn the_artifact_records_where_each_binary_came_from() {
    // A baseline that is "the current tree with the fix removed" would
    // prove nothing about what a stranger installs; the artifact must name
    // the released tag it measured.
    let text = artifact();
    assert_eq!(field(&text, "baselineBinary"), "git archive v0.7.5");
    assert_eq!(field(&text, "fixedBinary"), "this tree");
}
