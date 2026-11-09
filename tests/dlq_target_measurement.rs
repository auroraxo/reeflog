//! The dead-letter measurement must stay a MEASUREMENT, not a claim.
//!
//! Why this test exists (2026-11-09): the defect it describes was a SILENT
//! one — a `--target` the config does not know answered "nothing is
//! quarantined" with exit 0, and `pending` counted the rows a limit
//! happened to show instead of the letters that were pending. A measurement
//! about such a defect can rot the same way: if the artifact is edited by
//! hand, or regenerated from a tree that no longer has the fix, the numbers
//! keep looking authoritative while describing nothing.
//!
//! So the artifact is pinned here:
//!
//!   * it must exist and parse as text;
//!   * the BASELINE numbers must still say what was measured on the
//!     published v0.7.6 binary — those numbers describe a RELEASED artifact
//!     and must not drift;
//!   * the FIXED numbers must say this tree refuses every measured defect
//!     case and behaves as specified on every measured guard;
//!   * every case must carry the same id in BOTH trees, so a case cannot be
//!     quietly deleted from the weak side;
//!   * the baseline must still show the defect it describes — the unknown
//!     target answered with exit 0 and no refusal, and the pending count
//!     was the row count, not the queue size;
//!   * the artifact must not contain an absolute path from the machine that
//!     produced it (the directory-wide guard in research_artifacts.rs backs
//!     this up, but the measurement is pinned on its own too).
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
        .join("dlq-target-domain-2026-11-09.json");
    fs::read_to_string(&path).expect("research/dlq-target-domain-2026-11-09.json is readable")
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
fn the_baseline_still_shows_the_defect_it_describes() {
    let text = artifact();
    let baseline = tree_block(&text, "baseline");
    assert_eq!(
        field(baseline, "violations"),
        "5",
        "the baseline was measured on the published v0.7.6 binary: five of the eight cases \
         violated the rule — this number must not drift"
    );
    assert_eq!(
        tree_field(&text, "baseline", "source").contains("published v0.7.6"),
        true,
        "the baseline must still say it was the published v0.7.6 asset"
    );
}

#[test]
fn the_baseline_unknown_target_answered_successfully_with_an_empty_report() {
    let text = artifact();
    let case = case_block(&text, "baseline", "unknown_target_is_refused");
    assert_eq!(
        field(case, "exit"),
        "0",
        "on the published binary the unknown --target exited 0 — that is the defect"
    );
    assert_eq!(
        field(case, "report_printed"),
        "true",
        "the published binary PRINTED a report (\"nothing pending\") for a target that does \
         not exist — the silent answer is the defect"
    );
    assert_eq!(
        field(case, "reported_pending_total"),
        "0",
        "it reported zero pending while the journal held three"
    );
    assert_eq!(
        field(case, "journal_pending"),
        "3",
        "the journal held three pending letters"
    );
    assert_eq!(
        field(case, "refusal_names_flag"),
        "false",
        "the published binary named no flag — it never refused"
    );
}

#[test]
fn the_baseline_pending_count_was_the_row_count_not_the_queue_size() {
    let text = artifact();
    let case = case_block(&text, "baseline", "pending_counts_the_queue_not_the_page");
    assert_eq!(
        field(case, "journal_pending"),
        "7",
        "the journal held seven pending letters"
    );
    assert_eq!(
        field(case, "reported_pending_total"),
        "2",
        "the published binary reported 2 — it counted the rows the limit showed, not the \
         queue; this number describes a RELEASED artifact and must not drift"
    );
}

#[test]
fn the_fixed_tree_breaks_every_measured_case() {
    let text = artifact();
    assert_eq!(
        tree_field(&text, "fixed", "violations"),
        "0",
        "this tree must break every measured defect case and keep every guard"
    );
    for id in ids(&tree_block(&text, "fixed")) {
        let case = case_block(&text, "fixed", id);
        assert_eq!(
            field(case, "verdict"),
            "as_specified",
            "the fixed tree must behave as specified on {id}"
        );
    }
}

#[test]
fn the_fixed_tree_refuses_the_unknown_target_by_name() {
    let text = artifact();
    let case = case_block(&text, "fixed", "unknown_target_is_refused");
    assert_eq!(field(case, "exit"), "1", "the refusal must be a non-zero exit");
    assert_eq!(
        field(case, "report_printed"),
        "false",
        "a refusal must print no report"
    );
    assert_eq!(
        field(case, "refusal_names_flag"),
        "true",
        "the refusal must name the flag"
    );
    assert_eq!(
        field(case, "refusal_names_value"),
        "true",
        "the refusal must name the offending value"
    );
}

#[test]
fn the_fixed_tree_reports_the_queue_size() {
    let text = artifact();
    let case = case_block(&text, "fixed", "pending_counts_the_queue_not_the_page");
    assert_eq!(
        field(case, "reported_pending_total"),
        "7",
        "the fixed tree must report the letters that are pending, not the rows it showed"
    );
    assert_eq!(
        field(case, "journal_pending"),
        "7",
        "the journal still holds seven — the measurement must describe the same queue"
    );
}

#[test]
fn both_trees_carry_the_same_cases() {
    let text = artifact();
    let baseline: BTreeSet<&str> = ids(tree_block(&text, "baseline")).into_iter().collect();
    let fixed: BTreeSet<&str> = ids(tree_block(&text, "fixed")).into_iter().collect();
    assert_eq!(
        baseline, fixed,
        "a case deleted from one tree would make the comparison a lie"
    );
    assert_eq!(
        baseline.len(),
        8,
        "the measurement must keep all eight cases"
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
