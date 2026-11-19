//! The LaserStream #115 / PR #114 two-rule matrix must stay a MEASUREMENT.
//!
//! Why this test exists (2026-11-19): issue #115 proposed a fix direction
//! ("move the cursor only on delivered payload slots") and PR #114
//! implements it as `replay_cursor_slot`. The matrix records what a
//! COMPLIANT SERVER (one that resumes at the requested `from_slot`,
//! inclusive) would actually deliver to the consumer under each rule —
//! i.e. it is the user-zero evidence for whether the fix reaches the faces
//! #115 reports. If it drifts, reeflog's README row about the fix silently
//! stops matching the evidence, and a maintainer would act on a number
//! nobody re-checked.
//!
//! The two claims worth keeping true:
//!
//!   * PR #114 fixes the face its tests target (C: internal-only tick) and
//!     the block-footer advance (D) — but it does NOT fix A and B, the two
//!     faces #115 is about: a USER slot update at live head (with or without
//!     `filter_by_commitment`) still advances the cursor to 122, and a
//!     compliant server then resumes at 122, so slots 92..121 are never
//!     delivered. Both cells read `verdict = loss-30-slots`.
//!   * On MAIN the block-footer advance skips too (D), and a footer-only
//!     subscription (E) loses its whole delivered range; under PR #114 E
//!     keeps at-least-once but the cursor never advances (duplicate
//!     forever). The control cell must stay `ok`-class on both rules — a
//!     control reported as broken would mean the measurement invents
//!     defects.
//!
//! Falsified by construction: flip a verdict string and the test goes red;
//! swap a loss cell for the PR #114 fixed cell and it goes red; make the
//! PR114 A cell read `ok` (the wrong claim) and it goes red; inject a host
//! path and it goes red.

use std::fs;
use std::path::PathBuf;

const MATRIX: &str = "research/laserstream-115-pr114-two-rule-matrix-2026-11-19.json";

fn matrix_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(MATRIX)
}

fn matrix_text() -> String {
    fs::read_to_string(matrix_path()).unwrap_or_else(|e| {
        panic!("{MATRIX} is readable: {e}")
    })
}

/// Plain-text read of `"key": value` — the crate has no JSON dependency and
/// this test does not need one.
fn field(text: &str, key: &str) -> String {
    let needle = format!("\"{key}\"");
    let start = text
        .find(&needle)
        .unwrap_or_else(|| panic!("the matrix must carry the field {key}"))
        + needle.len();
    let rest = &text[start..];
    let colon = rest.find(':').expect("the field is followed by a colon");
    let rest = &rest[colon + 1..];
    let end = rest
        .find(|c: char| [',', '}', '\n'].contains(&c))
        .unwrap_or(rest.len());
    rest[..end].trim().trim_matches('"').to_string()
}

/// Extract the single cell object for a (rule, script) pair by splitting the
/// matrix text on the cell boundaries. Cells are objects with keys
/// "rule", "script", ... "verdict", "explanation" and are separated by `}`.
fn cell(text: &str, rule: &str, script: &str) -> String {
    let mut rest = text;
    loop {
        let rule_at = rest
            .find(&format!("\"rule\": \"{rule}\""))
            .unwrap_or_else(|| panic!("the matrix must carry a {rule} cell for {script}"));
        let window = &rest[rule_at..];
        let end = window.find('}').expect("the cell object closes");
        let candidate = &window[..=end];
        if candidate.contains(&format!("\"script\": \"{script}\"")) {
            return candidate.to_string();
        }
        rest = &window[end + 1..];
    }
}

#[test]
fn the_matrix_exists_and_is_one_json_object() {
    let text = matrix_text();
    let trimmed = text.trim();
    assert!(
        trimmed.starts_with('{') && trimmed.ends_with('}'),
        "the matrix must be one JSON object"
    );
    assert_eq!(
        text.matches('{').count(),
        text.matches('}').count(),
        "the matrix must balance its braces"
    );
}

#[test]
fn the_pr114_rule_still_fails_the_faces_115_reports() {
    // The headline claim: PR #114's rule does not reach the user-slot-tick
    // faces, and a compliant server then loses exactly 92..121.
    for rule in ["main", "pr114"] {
        for script in ["variant-a", "variant-b"] {
            let cell = cell(&matrix_text(), rule, script);
            assert_eq!(
                field(&cell, "verdict"),
                "loss-30-slots",
                "the {rule} rule's {script} cell must still record a 30-slot loss"
            );
            assert_eq!(
                field(&cell, "reconnect_from_slot"),
                "122",
                "the {rule} rule's {script} cursor must still jump to 122"
            );
            assert_eq!(
                field(&cell, "cursor_jumped_past_last_delivered"),
                "true",
                "the {rule} rule's {script} must still report the jump"
            );
        }
    }
}

#[test]
fn pr114_fixes_the_internal_tick_face_but_not_by_swapping_cells() {
    let text = matrix_text();
    let c_main = cell(&text, "main", "variant-c");
    let c_pr114 = cell(&text, "pr114", "variant-c");
    assert_eq!(
        field(&c_main, "verdict"),
        "loss-30-slots",
        "main's C cell must still record the loss PR #114 targets"
    );
    assert_eq!(
        field(&c_pr114, "verdict"),
        "ok-duplicate-1",
        "PR #114's C cell must still show the fix: from=91, one duplicate"
    );
    assert_eq!(
        field(&c_pr114, "reconnect_from_slot"),
        "91",
        "PR #114's C cell must still resume at the last delivered slot"
    );
    // D (block footer) moves to the same class only because PR #114 omits
    // BlockFooter from its rule — not because the cell was renamed.
    let d_pr114 = cell(&text, "pr114", "variant-d");
    assert_eq!(
        field(&d_pr114, "verdict"),
        "ok-duplicate-1",
        "PR #114's D cell must still show the side-effect fix"
    );
    let d_main = cell(&text, "main", "variant-d");
    assert_eq!(
        field(&d_main, "verdict"),
        "loss-30-slots",
        "main's D cell must still record the footer-advance loss"
    );
}

#[test]
fn the_footer_only_face_keeps_its_honest_two_sided_verdict() {
    let text = matrix_text();
    let e_main = cell(&text, "main", "variant-e");
    assert_eq!(
        field(&e_main, "verdict"),
        "loss-32-slots",
        "main's E cell must still record the footer-only loss"
    );
    let e_pr114 = cell(&text, "pr114", "variant-e");
    assert_eq!(
        field(&e_pr114, "verdict"),
        "duplicates-forever",
        "PR #114's E cell must stay the honest duplicate-only verdict, not \"fixed\""
    );
    assert_eq!(
        field(&e_pr114, "reconnect_from_slot"),
        "null",
        "PR #114's E cell must still show the cursor that never advances"
    );
}

#[test]
fn the_controls_stay_ok_on_both_rules() {
    let text = matrix_text();
    for rule in ["main", "pr114"] {
        let c = cell(&text, rule, "control");
        let verdict = field(&c, "verdict");
        assert!(
            !verdict.starts_with("loss"),
            "the {rule} control must not be reported as losing events"
        );
        if rule == "main" {
            assert_eq!(
                field(&c, "reconnect_from_slot"),
                "null",
                "the main control must not fabricate a cursor"
            );
        } else {
            // Under PR #114's rule the payload deliveries themselves advance
            // the cursor — the control resuming at the last delivered slot
            // (91, one duplicate) IS the fix working, not a defect.
            assert_eq!(
                field(&c, "reconnect_from_slot"),
                "91",
                "the pr114 control must resume at its last delivered slot"
            );
            assert_eq!(
                verdict, "ok-duplicate-1",
                "the pr114 control must stay the duplicate-only verdict"
            );
        }
    }
}

#[test]
fn every_cell_carries_its_own_explanation_and_no_host_path() {
    let text = matrix_text();
    let mut labels: Vec<String> = Vec::new();
    for rule in ["main", "pr114"] {
        for script in ["control", "variant-a", "variant-b", "variant-c", "variant-d", "variant-e"] {
            let cell = cell(&text, rule, script);
            let explanation = field(&cell, "explanation");
            assert!(
                explanation.len() > 10,
                "the {rule} {script} cell must explain its verdict"
            );
            labels.push(format!("{rule} {script}: {}", field(&cell, "verdict")));
        }
    }
    let mut unique = labels.clone();
    unique.sort();
    unique.dedup();
    assert_eq!(
        unique.len(),
        labels.len(),
        "every cell must carry its own rule/script verdict"
    );
    for needle in ["/home/", "/Users/", "C:\\\\", "/tmp/"] {
        assert!(
            !text.contains(needle),
            "the matrix carries the host path {needle}"
        );
    }
}
