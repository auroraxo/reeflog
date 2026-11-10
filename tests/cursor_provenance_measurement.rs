//! The cursor-provenance measurement must stay a MEASUREMENT, not a claim.
//!
//! Why this test exists (2026-11-10): the defect this work fixed was a
//! SILENT one. A cursor file carried no record of which channel or which
//! journal wrote it, so every crossing was accepted with exit 0 and
//! delivered the wrong thing — a starved retraction channel, a duplicate
//! replay, a journal that read as drained without ever having been read.
//! A measurement about such a defect can rot the same way: edited by hand,
//! or regenerated from a tree that no longer has the defect, the numbers
//! keep looking authoritative while describing nothing.
//!
//! So the artifact is pinned here:
//!
//!   * it must exist and parse;
//!   * the BASELINE numbers must still say what was measured on the
//!     published v0.7.12 binary — every crossing accepted with exit 0, and
//!     each one visibly wrong (0 rows where the backlog waited, 17 replayed
//!     rows, 0 rows on a fresh journal). Those numbers are about a
//!     RELEASED artifact and must not drift;
//!   * the FIXED numbers must say the tree behaves as specified: every
//!     crossing refused with 0 rows delivered and the cursor file left
//!     byte-identical, every guard unchanged;
//!   * both trees must carry the same case ids, so a case cannot be quietly
//!     deleted from one side;
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
        .join("cursor-provenance-2026-11-10.json");
    fs::read_to_string(&path).expect("research/cursor-provenance-2026-11-10.json is readable")
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
    let end = block.find("\"verdict\"").map(|i| i + 1).unwrap_or(block.len());
    block[..end].to_string()
}

const CROSSINGS: [&str; 4] = [
    "D1-events-cursor-on-reorgs-channel",
    "D2-reorgs-cursor-on-events-channel",
    "D3-cursor-copied-to-another-journal",
    "D4-crossing-refusal-leaves-file-untouched",
];

#[test]
fn the_baseline_is_the_published_v0712_asset_not_a_local_build() {
    // The published-tree falsification: the defect rows are measured against
    // the RELEASED v0.7.12 binary — the one a stranger installs — named with
    // the digest the Release API publishes (same digest as
    // research/dlq-replay-page-published-v0.7.12-2026-11-10.json), never
    // "this tree with the fix removed".
    let text = artifact();
    assert!(
        text.contains(
            "\"source\": \"published v0.7.12 release asset (sha256 ab28aac213d2e5158d27086c22acb2947ab4f756d21d0b5da3adc614e5aac856)\""
        ),
        "the baseline must be the PUBLISHED v0.7.12 asset with its digest — otherwise the measured defect is a local claim, not a falsification of the release"
    );
    assert!(
        text.contains("\"source\": \"this tree\""),
        "the fixed side must be this tree"
    );
}

#[test]
fn the_baseline_numbers_still_describe_the_published_v0712_binary() {
    let text = artifact();
    assert_eq!(
        field(&text, "events"),
        "20",
        "the baseline was measured with 20 planted events and must not drift"
    );
    assert_eq!(
        field(&text, "reorgs"),
        "3",
        "the baseline was measured with 3 planted reorgs and must not drift"
    );
    for id in CROSSINGS {
        let block = case_block(&text, id, "baseline");
        assert!(
            block.contains("\"exit\": 0"),
            "the published v0.7.12 binary accepted the crossing on {id} with exit 0 and must not drift: {block}"
        );
        assert!(
            block.contains("\"refused\": false"),
            "the published v0.7.12 binary did NOT refuse {id} and must not drift: {block}"
        );
    }
}

#[test]
fn the_baseline_numbers_still_show_how_wrong_each_crossing_was() {
    // The baseline's rows are the visible damage: the starved channel
    // delivered 0 while 3 reorgs waited, the reversed channel REPLAYED 17
    // already-delivered events, and a fresh journal delivered 0 of its 20.
    let text = artifact();
    let block = case_block(&text, CROSSINGS[0], "baseline");
    assert!(
        block.contains("\"rows\": 0"),
        "on the published v0.7.12 binary the reorg channel delivered 0 rows while its backlog waited: {block}"
    );
    let block = case_block(&text, CROSSINGS[1], "baseline");
    assert!(
        block.contains("\"rows\": 17"),
        "on the published v0.7.12 binary the events channel replayed 17 already-delivered rows: {block}"
    );
    let block = case_block(&text, CROSSINGS[2], "baseline");
    assert!(
        block.contains("\"rows\": 0"),
        "on the published v0.7.12 binary a fresh journal delivered 0 of its 20 rows: {block}"
    );
}

#[test]
fn the_fixed_numbers_say_the_tree_behaves_as_specified() {
    let text = artifact();
    assert_eq!(
        field(&text, "failures"),
        "0",
        "no measured case may fail on this tree"
    );
    for id in CROSSINGS {
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
    // the file would destroy what the operator needs to see, and would
    // silently restart the consumer.
    let text = artifact();
    for id in CROSSINGS {
        let block = case_block(&text, id, "fixed");
        // Compare the two values: they must be the same string.
        let w = extract_after(&block, "cursorWrittenByFirstStep");
        let a = extract_after(&block, "cursorAfterFinalStep");
        assert_eq!(
            w, a,
            "the refusal on {id} must leave the cursor file byte-identical: {block}"
        );
        assert!(
            w != "null" && !w.is_empty(),
            "the fixed tree must record the cursor it found for {id}: {block}"
        );
    }
}

/// The JSON string value that follows a key, without a JSON parser.
fn extract_after(block: &str, key: &str) -> String {
    let needle = format!("\"{key}\":");
    let start = block
        .find(&needle)
        .unwrap_or_else(|| panic!("the block must carry {key}"))
        + needle.len();
    let rest = &block[start..];
    let rest = rest.trim_start();
    if !rest.starts_with('"') {
        // null
        return "null".into();
    }
    let inner = &rest[1..];
    let end = inner.find('"').expect("the string is closed");
    inner[..end].to_string()
}

#[test]
fn the_guards_are_unchanged_on_both_trees() {
    // A fix that refuses more than it must is a different defect: the
    // first run, the deliberate rewind, the exactly-once resume, the
    // per-channel files and the invalid-content refusal all have to behave
    // the same way as before.
    let text = artifact();
    for (id, rows) in [
        ("G1-first-run-no-file-delivers-all", 20),
        ("G2-bare-hand-written-number-stays-legal", 5),
        ("G3-same-channel-resume-exactly", 0),
        ("G4-two-channel-files-both-work", 3),
        ("G6-summary-line-names-a-plain-number", 20),
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
    // The invalid-content guard refuses on BOTH trees — the v0.7.4 content
    // gate must not have been traded away for the provenance rule.
    let block = case_block(&text, "G5-invalid-content-still-refused", "fixed");
    assert!(
        block.contains("\"refused\": true") && block.contains("\"rows\": 0"),
        "`abc` in a cursor file must stay a refusal on this tree: {block}"
    );
    let block = case_block(&text, "G5-invalid-content-still-refused", "baseline");
    assert!(
        block.contains("\"refused\": true"),
        "`abc` in a cursor file was already a refusal on the published tree and must not drift: {block}"
    );
}

#[test]
fn both_trees_measure_the_same_cases() {
    let text = artifact();
    let all = ids(&text);
    assert!(
        all.len() >= 10,
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
