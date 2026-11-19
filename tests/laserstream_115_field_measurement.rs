//! The LaserStream field measurement must stay a MEASUREMENT, not a claim.
//!
//! Why this test exists (2026-11-19): the artifact behind reeflog's
//! "field use" row records what ANOTHER product's SDK asked for after a
//! disconnect — helius-labs/laserstream-sdk#115, whose open question is
//! whether a reconnect can skip payloads that were never delivered. The
//! numbers came from a mock geyser run against the SDK's `main` at 379a6ba,
//! and they are about a FOREIGN tree: if they drift, the claim in reeflog's
//! README ("ticks at live head advance the SDK's resume cursor past payloads
//! still being replayed, whatever their commitment status") silently stops
//! matching the evidence, and a third party would cite a number nobody
//! re-checked.
//!
//! So the artifact is pinned here:
//!
//!   * all four artifacts must exist and parse as JSON;
//!   * the three defect variants must still show the reconnect asking for
//!     slot 122 while the last delivered payload was 91 — i.e. the 30 slots
//!     92..121 are the ones a from_slot-honoring server would never
//!     re-deliver;
//!   * the CONTROL must still show NO fabricated cursor (no reconnect
//!     `from_slot` at all): a payload-only stream must not be reported as
//!     broken by this measurement;
//!   * the variant labels must stay distinguishable, so a control can never
//!     be swapped for a defect row;
//!   * no artifact may carry an absolute path from the machine that produced
//!     it (the lesson from sememe's declared-origin artifact, which went red
//!     in CI because it carried the author's host path).
//!
//! Falsified by construction: change one number in an artifact and this
//! test goes red; swap the control for a defect row and it goes red; add a
//! host path and it goes red.

use std::fs;
use std::path::PathBuf;

const VARIANTS: [&str; 4] = [
    "variant-a",
    "variant-b",
    "variant-c",
    "control",
];

fn artifact_path(variant: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("research")
        .join(format!("laserstream-115-{variant}-2026-11-19.json"))
}

fn artifact(variant: &str) -> String {
    let path = artifact_path(variant);
    fs::read_to_string(&path).unwrap_or_else(|e| {
        panic!(
            "research/laserstream-115-{variant}-2026-11-19.json is readable: {e}"
        )
    })
}

/// Plain-text field read: a wrong number anywhere changes the text, which is
/// all this test needs (the crate has no JSON dependency, and adding one for
/// a test is not worth it).
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

fn count_occurrences(text: &str, needle: &str) -> usize {
    text.matches(needle).count()
}

#[test]
fn every_variant_artifact_exists_and_parses_as_json() {
    for variant in VARIANTS {
        let text = artifact(variant);
        let trimmed = text.trim();
        assert!(
            trimmed.starts_with('{') && trimmed.ends_with('}'),
            "research/laserstream-115-{variant}-2026-11-19.json must be one JSON object"
        );
        // A JSON object with balanced braces and quotes is a JSON object this
        // test can trust to carry the fields below.
        assert_eq!(
            count_occurrences(&text, "{"),
            count_occurrences(&text, "}"),
            "research/laserstream-115-{variant}-2026-11-19.json must balance its braces"
        );
    }
}

#[test]
fn the_three_defect_variants_still_skip_the_undelivered_range() {
    for variant in ["variant-a", "variant-b", "variant-c"] {
        let text = artifact(variant);
        assert_eq!(
            field(&text, "defect_present"),
            "true",
            "{variant} must still record the skip, or the README row over-claims"
        );
        assert_eq!(
            field(&text, "reconnect_from_slot"),
            "122",
            "{variant}: the reconnect must still ask for live head 122"
        );
        assert_eq!(
            field(&text, "expected_resume_slot"),
            "92",
            "{variant}: the payload after the last delivered one is still 92"
        );
        assert_eq!(
            field(&text, "skipped_range_size"),
            "30",
            "{variant}: slots 92..121 are still the skipped range"
        );
        // The skipped range must be spelled out, not just counted: the list
        // of slots a from_slot-honoring server would never re-deliver starts
        // at 92 and ends at 121 (the entry before the trailing ]).
        let range = text
            .split("\"slots_a_compliant_server_would_never_redeliver\": [")
            .nth(1)
            .unwrap_or_else(|| panic!("{variant}: the skipped range must be present"));
        let range = &range[..range.find(']').unwrap_or(range.len())];
        let first = range.trim().split(',').next().unwrap_or("").trim();
        let last = range.trim().split(',').last().unwrap_or("").trim();
        assert_eq!(
            first, "92",
            "{variant}: the first skipped slot must still be 92"
        );
        assert_eq!(
            last, "121",
            "{variant}: the last skipped slot must still be 121"
        );
        assert_eq!(
            field(&text, "issue"),
            "helius-labs/laserstream-sdk#115",
            "{variant}: the artifact must stay pinned to the issue it measures"
        );
    }
}

#[test]
fn the_control_still_shows_no_fabricated_cursor() {
    let text = artifact("control");
    assert_eq!(
        field(&text, "defect_present"),
        "false",
        "the control must not be reported as broken"
    );
    assert_eq!(
        field(&text, "reconnect_from_slot"),
        "null",
        "a payload-only stream must not fabricate a reconnect cursor"
    );
    assert!(
        !text.contains("\"92\","),
        "the control must not carry a skipped range"
    );
}

#[test]
fn the_variants_stay_distinguishable() {
    let mut labels: Vec<String> = Vec::new();
    for variant in VARIANTS {
        let text = artifact(variant);
        let label = field(&text, "variant");
        assert!(
            !label.is_empty() && label.len() > 20,
            "{variant}: the variant label must say which script produced the numbers"
        );
        labels.push(label);
    }
    let mut unique = labels.clone();
    unique.sort();
    unique.dedup();
    assert_eq!(
        unique.len(),
        VARIANTS.len(),
        "every artifact must carry its own variant label"
    );
}

#[test]
fn no_artifact_carries_a_host_path() {
    for variant in VARIANTS {
        let text = artifact(variant);
        for needle in ["/home/", "/Users/", "C:\\\\", "/tmp/"] {
            assert!(
                !text.contains(needle),
                "research/laserstream-115-{variant}-2026-11-19.json carries the host path {needle}"
            );
        }
    }
}
