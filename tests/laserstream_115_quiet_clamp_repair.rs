//! The LaserStream #115 consumer-side clamp repair must stay a MEASUREMENT.
//!
//! Why this test exists (2026-11-20): PULS 4 proved the age-out reservation
//! holds for ANY slot cursor. This pin keeps the NEXT claim true — the open
//! design question the thread left: the SDK's own documented pieces
//! (subscribe_replay_info + intra_slot_updates + an explicit clamped
//! from_slot) ARE a working consumer-side repair for the quiet age-out
//! stall, and the repair's cost is a DECLARED gap, not silence.
//!
//! The claims worth keeping true (scenario: chain head 136 behind retention
//! floor 130; the window evicted 92..129 — 38 slots):
//!
//!   * SILENT cell (default options): the consumer sees 0 errors while the
//!     SDK retries the refused cursor forever — the stall is invisible, so
//!     the repair is UNREACHABLE under default options.
//!   * INTRA cell: with intra_slot_updates the consumer SEES the terminal
//!     OutOfRange — the SDK already owns the switch that connects the stall
//!     to the repair.
//!   * REPAIR cell: one subscribe_replay_info query answers
//!     first_available=130; the consumer re-subscribes with from_slot=130
//!     (the clamp), resumes live delivery 130..136, and DECLARES 92..129
//!     (exactly 38 slots) as a gap.
//!   * CONTROL cell: the same repair WITHOUT the clamp (fresh subscribe at
//!     the stale cursor 122) is refused again and delivers NOTHING — the
//!     clamp is the operative rule, not the re-subscribe alone.
//!
//! Falsified by construction: flip a count, a clamp, or the gap size and the
//! test goes red; make the silent stall visible, make the control deliver,
//! or make the repair refill the unrecoverable range, and it goes red.
//!
//! Scenario naming: the harness cells in this family are named
//! laserstream-115-quiet-clamp-{silent,intra,repair,control}; the summary
//! calls them by those names and this pin reads the summary.

use std::fs;
use std::path::PathBuf;

const SUMMARY: &str = "research/laserstream-115-quiet-clamp-summary-2026-11-20.json";

fn summary_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(SUMMARY)
}

fn summary_text() -> String {
    fs::read_to_string(summary_path())
        .unwrap_or_else(|e| panic!("{SUMMARY} is readable: {e}"))
}

/// Plain-text read of `"key": value` — the crate has no JSON dependency and
/// this test does not need one.
fn field(text: &str, key: &str) -> String {
    let needle = format!("\"{key}\"");
    let start = text
        .find(&needle)
        .unwrap_or_else(|| panic!("the summary must carry the field {key}"))
        + needle.len();
    let rest = &text[start..];
    let colon = rest.find(':').expect("the field is followed by a colon");
    let rest = &rest[colon + 1..];
    let end = rest
        .find(|c: char| [',', '}', '\n'].contains(&c))
        .unwrap_or(rest.len());
    rest[..end].trim().trim_matches('"').to_string()
}

/// Extract the named cell object from the summary text.
fn cell(text: &str, name: &str) -> String {
    let marker = format!("\"{name}\": {{");
    let start = text
        .find(&marker)
        .unwrap_or_else(|| panic!("the summary must carry a {name} cell"));
    let body = &text[start + marker.len()..];
    let end = body.find('}').expect("the cell object closes");
    body[..=end].to_string()
}

fn number_of(cell_text: &str, key: &str) -> u64 {
    field(cell_text, key)
        .parse::<u64>()
        .unwrap_or_else(|e| panic!("{key} must be a number: {e}"))
}

#[test]
fn the_clamp_summary_exists_and_names_four_cells() {
    let text = summary_text();
    for name in ["silent", "intra", "repair", "control"] {
        assert!(text.contains(&format!("\"{name}\": {{")), "a {name} cell must exist");
    }
}

#[test]
fn the_silent_cell_keeps_the_stall_invisible() {
    // The default-options consumer must NOT see the error — that is the
    // defect (the repair is unreachable if the consumer never learns it
    // should repair), and it must stay observed.
    let silent = cell(&summary_text(), "silent");
    assert_eq!(
        number_of(&silent, "consumer_error_count"),
        0,
        "default options: the consumer must see 0 errors (invisible stall)"
    );
    assert!(
        number_of(&silent, "out_of_range_replies") > 0,
        "the server must have refused the cursor (the scenario happened)"
    );
}

#[test]
fn the_intra_cell_keeps_the_error_surfaced() {
    let intra = cell(&summary_text(), "intra");
    assert!(
        number_of(&intra, "consumer_error_count") > 0,
        "intra_slot_updates: the consumer must see the terminal error"
    );
}

#[test]
fn the_repair_cell_clamps_to_first_available_and_declares_the_gap() {
    let repair = cell(&summary_text(), "repair");
    assert_eq!(
        number_of(&repair, "replay_info_queries"),
        1,
        "exactly one subscribe_replay_info query"
    );
    assert_eq!(
        number_of(&repair, "first_available_reported"),
        130,
        "first_available must be the server's floor 130"
    );
    assert_eq!(
        number_of(&repair, "clamp_from_slot"),
        130,
        "the clamp must clamp to 130, not the stale cursor"
    );
    assert_eq!(
        number_of(&repair, "gap_declared_range_size"),
        38,
        "the declared gap must be exactly 92..129 (38 slots)"
    );
    // Delivery resumed at the floor: the artifact records 130..136.
    let artifact = field(&repair, "artifact");
    let raw = fs::read_to_string(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(&artifact))
        .unwrap_or_else(|e| panic!("the repair artifact {artifact} must be readable: {e}"));
    // The array is pretty-printed across lines, so check the values, not the
    // layout: every slot 130..=136 must be present, and nothing else.
    for slot in 130u64..=136 {
        assert!(
            raw.contains(&format!("{slot},")) || raw.contains(&format!("{slot}\n")),
            "delivery must include slot {slot} (the artifact {artifact} must record 130..136)"
        );
    }
    // The DELIVERED set is the array named delivered_after_clamp; the gap
    // range (92..129) is declared separately and must NOT appear in it.
    let delivered_start = raw
        .find("\"delivered_after_clamp\":")
        .expect("the repair artifact must record delivered_after_clamp");
    let delivered_end = raw[delivered_start..]
        .find(']')
        .expect("the delivered array closes")
        + delivered_start;
    let delivered_block = &raw[delivered_start..delivered_end];
    for slot in 92u64..=129 {
        assert!(
            !delivered_block.contains(&format!("{slot},")) && !delivered_block.contains(&format!("{slot}\n")),
            "the repair must not deliver slot {slot} (it is in the declared gap 92..129)"
        );
    }
}

#[test]
fn the_control_cell_proves_the_clamp_is_the_operative_rule() {
    // False-positive guard: re-subscribe WITHOUT the clamp -> refused again,
    // nothing delivered. If the control ever delivers, the "clamp" finding
    // is describing the mock's generosity, not the rule.
    let control = cell(&summary_text(), "control");
    assert!(
        number_of(&control, "out_of_range_replies") > 0,
        "the unclamped re-subscribe must be refused again"
    );
    assert_eq!(
        field(&control, "delivered_after_clamp"),
        "[]",
        "the unclamped control must deliver nothing"
    );
}

#[test]
fn the_summary_must_not_claim_the_gap_is_refilled() {
    // Honesty pin: the repair DECLARES the evicted range; it can never
    // REFILL it. If a future edit claims recovery of 92..129 anywhere in
    // the summary, this test goes red until the claim is re-measured.
    let text = summary_text();
    assert!(
        !text.to_lowercase().contains("refill the unrecoverable"),
        "the summary must not claim the evicted range is refillable"
    );
    assert!(
        text.contains("\"gap_declared_range_size\": 38"),
        "the summary must carry the exact declared gap size"
    );
}

#[test]
fn every_cell_names_its_raw_artifact() {
    // Durability: each summary cell must point at a per-cell artifact that
    // exists in the tree, so the numbers can be re-checked from raw evidence.
    let text = summary_text();
    let mut rest = &text[..];
    let mut counted = 0;
    while let Some(pos) = rest.find("\"artifact\": \"") {
        let rest_after = &rest[pos + "\"artifact\": \"".len()..];
        let end = rest_after.find('"').expect("artifact path closes");
        let path = &rest_after[..end];
        assert!(
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(path).exists(),
            "the artifact named by a summary cell must exist: {path}"
        );
        counted += 1;
        rest = &rest_after[end + 1..];
    }
    assert_eq!(
        counted, 4,
        "the summary must name one raw artifact per cell (4 cells)"
    );
}
