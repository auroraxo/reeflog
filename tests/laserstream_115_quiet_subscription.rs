//! The LaserStream #115 quiet-subscription age-out matrix must stay a
//! MEASUREMENT.
//!
//! Why this test exists (2026-11-20): the thread's open half-question was
//! whether a payload-only cursor (PR #114's direction) survives a quiet
//! subscription aging out of the replay window. This pin keeps the measured
//! answers true: a quiet subscription with ONE internal-tracker tick at the
//! live head, then silence, measured as rule (main / PR #114) × window
//! (aged floor 130 / held floor 0) × options (default / intra_slot_updates).
//!
//! The claims worth keeping true:
//!
//!   * MAIN's cursor is NOT payload-only on a quiet subscription: it rides the
//!     SDK's internal slot tracker to the live head (from_slot 122), and in
//!     the HELD window it therefore loses 92..121 (the #115 skip in quiet
//!     form).
//!   * PR #114's payload-only cursor stops at the newest DELIVERED payload
//!     (91) and recovers 91..122 with zero loss in the held window — but at
//!     age-out it is refused EXACTLY like main (and would be refused earlier:
//!     91 lags 122). The reservation holds for the proposed fix too.
//!   * With DEFAULT options the OUT_OF_RANGE stall is invisible: 0 consumer
//!     errors while the SDK silently retries. With intra_slot_updates the
//!     consumer sees the terminal error.
//!
//! Falsified by construction: flip a from_slot, a verdict, or a consumer-error
//! count and the test goes red; make main look payload-only, or make the
//! aged stall visible under default options, and it goes red.
//!
//! Note: this pin reads the SUMMARY cells; the per-cell artifacts in
//! research/laserstream-115-quiet-{aged,resume}-rule-*-2026-11-20.json are
//! the raw evidence and are pinned by this test via the summary's
//! `artifact` paths (which the summary must keep pointing at).

use std::fs;
use std::path::PathBuf;

const SUMMARY: &str = "research/laserstream-115-quiet-subscription-matrix-2026-11-20.json";

fn summary_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(SUMMARY)
}

fn summary_text() -> String {
    fs::read_to_string(summary_path()).unwrap_or_else(|e| {
        panic!("{SUMMARY} is readable: {e}")
    })
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

/// Extract the single cell object for a (rule, window, options) triple by
/// splitting the summary text on the cell boundaries.
fn cell(text: &str, rule: &str, window: &str, options: &str) -> String {
    let mut rest = text;
    loop {
        let rule_at = rest
            .find(&format!("\"rule\": \"{rule}\""))
            .unwrap_or_else(|| panic!("the summary must carry a {rule} cell for {window} ({options})"));
        let window_at = &rest[rule_at..];
        let end = window_at.find('}').expect("the cell object closes");
        let candidate = &window_at[..=end];
        if candidate.contains(&format!("\"window\": \"{window}\""))
            && candidate.contains(&format!("\"consumer_options\": \"{options}\""))
        {
            return candidate.to_string();
        }
        rest = &window_at[end + 1..];
    }
}

fn consumer_errors_of(cell_text: &str) -> u64 {
    field(cell_text, "consumer_errors_count")
        .parse::<u64>()
        .expect("consumer_errors_count must be a number")
}

#[test]
fn the_quiet_matrix_exists_and_is_one_json_object() {
    let text = summary_text();
    let trimmed = text.trim();
    assert!(
        trimmed.starts_with('{') && trimmed.ends_with('}'),
        "the summary must be one JSON object"
    );
    assert_eq!(
        text.matches('{').count(),
        text.matches('}').count(),
        "the summary must balance its braces"
    );
}

#[test]
fn main_rides_the_internal_tracker_not_the_payloads() {
    // The headline claim: on a quiet subscription main's cursor is NOT
    // payload-only — it rides the SDK's internal slot tracker to the live
    // head (122), even though the user's subscription delivered nothing new.
    for window in ["aged", "held"] {
        let cell = cell(&summary_text(), "main", window, "default-options");
        assert_eq!(
            field(&cell, "reconnect_from_slot"),
            "122",
            "main's quiet cursor must ride the tracker to the live head"
        );
    }
}

#[test]
fn pr114_payload_only_cursor_stops_at_the_last_delivered_payload() {
    // The payload-only rule does NOT ride the tracker: the cursor stops at
    // the newest delivered payload (91). This is the claim that makes the
    // age-out reservation possible at all.
    for window in ["aged", "held"] {
        let cell = cell(&summary_text(), "pr114", window, "default-options");
        assert_eq!(
            field(&cell, "reconnect_from_slot"),
            "91",
            "PR #114's quiet cursor must stop at the last delivered payload"
        );
    }
}

#[test]
fn age_out_refuses_both_rules_and_the_stall_is_invisible_by_default() {
    // The reservation holds: at the aged window (floor 130) BOTH rules are
    // refused with OUT_OF_RANGE — PR #114 does not survive age-out either.
    for rule in ["main", "pr114"] {
        let aged = cell(&summary_text(), rule, "aged", "default-options");
        assert_eq!(
            field(&aged, "consumer_options"),
            "default-options",
            "the default-options aged cell must be the first cell found"
        );
        assert!(
            aged.contains("\"out_of_range_replies\": 2"),
            "the aged {rule} cell must record the refused attempts"
        );
        assert_eq!(
            consumer_errors_of(&aged),
            0,
            "the aged {rule} cell under DEFAULT options must surface NO error to the consumer (silent stall)"
        );
    }
    // And with intra_slot_updates the consumer DOES see the terminal error.
    for rule in ["main", "pr114"] {
        let intra = cell(&summary_text(), rule, "aged", "intra-slot-updates");
        assert!(
            intra.contains("\"consumer_options\": \"intra-slot-updates\""),
            "the {rule} intra-slot-updates cell must exist"
        );
        assert!(
            consumer_errors_of(&intra) > 0,
            "the aged {rule} cell with intra_slot_updates must surface the error to the consumer"
        );
    }
}

#[test]
fn the_held_window_shows_the_real_tradeoff() {
    // False-positive guard: in the held window (floor 0) nothing is refused,
    // main recovers only the live head (losing 92..121 — the #115 skip in
    // quiet form), and PR #114 recovers 91..122 with zero loss. If the held
    // window refused either rule, or main recovered the range, the matrix
    // would be describing the mock instead of the tradeoff.
    for rule in ["main", "pr114"] {
        let held = cell(&summary_text(), rule, "held", "default-options");
        assert!(
            held.contains("\"out_of_range_replies\": 0"),
            "the held {rule} cell must not be refused"
        );
    }
    let main_held = cell(&summary_text(), "main", "held", "default-options");
    assert!(
        main_held.contains("\"payloads_delivered_after_reconnect\": 1"),
        "main in the held window recovers only the live head (loses 92..121)"
    );
    let pr114_held = cell(&summary_text(), "pr114", "held", "default-options");
    assert!(
        pr114_held.contains("\"payloads_delivered_after_reconnect\": 5"),
        "PR #114 in the held window recovers 91..122 with zero loss"
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
        counted, 8,
        "the summary must name one raw artifact per cell (8 cells)"
    );
}
