//! The retraction-verdict-survival measurement, pinned on both sides.
//!
//! PULS AW, 2026-11-19. `tools/measure-reorg-verdict-survival.mjs` — the
//! real binary against a real journal with a live scripted sink:
//!
//! * The PREFIX side is this tree BEFORE the fix (commit 3d12624, binary
//!   sha256 2272a7c3…; verdict `reorg-verdict-erased-by-idle-pass`, 5/6
//!   defect faces red, 4/4 guards green): a pass whose event phase ran
//!   while the retraction phase examined NO row erased BOTH standing
//!   verdicts on disk. `webhook push --limit 0` — the pass the README
//!   documents as sending zero bytes and moving no cursor — wiped the
//!   retraction queue's recorded refusal, and `webhook status` then said
//!   the target was healthy and exited 0 while the retraction queue was
//!   still stopped. The mirror image held on the event channel: a recorded
//!   payload-hold verdict (`run \`hydrate\``) was erased by the same
//!   zero-byte pass.
//!
//! * The FIXED side is this tree AFTER the fix (verdict
//!   `reorg-verdict-survives`, 6/6 faces, 4/4 guards): a channel's
//!   standing verdict is a fact about THAT channel's most recent phase —
//!   the event phase clears only `last_error`, and only after it actually
//!   read its queue; the retraction phase clears only `last_reorg_error`,
//!   under the same rule. A phase that examines no row leaves the verdict
//!   it finds standing. The through-pass rule is unchanged: a pass that
//!   GETS THROUGH clears both columns and delivers the owed retraction
//!   (no queue progress lost, no verdict frozen forever).
//!
//! The pin holds the MEASURED numbers, not a summary: the prefix side of
//! the artifact must still record the erased verdicts, so a later tree
//! cannot quietly make the defect look smaller than it was, and the fixed
//! side must still carry the surviving verdicts it claims. The binaries
//! are named by hash, never by host path.

use std::path::PathBuf;

fn artifact(name: &str) -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("research")
        .join(name);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

fn assert_contains(text: &str, needle: &str, what: &str) {
    assert!(
        text.contains(needle),
        "the artifact must state {what}: missing {needle:?}"
    );
}

const PREFIX: &str = "reorg-verdict-survival-prefix-2026-11-19.json";
const FIXED: &str = "reorg-verdict-survival-fixed-2026-11-19.json";

#[test]
fn the_prefix_artifact_records_the_erased_verdicts() {
    let text = artifact(PREFIX);
    assert_contains(
        &text,
        "\"verdict\": \"reorg-verdict-erased-by-idle-pass\"",
        "the pre-fix verdict",
    );
    assert_contains(&text, "\"faces_count\": 1", "the pre-fix face count (only the through-pass face is green)");
    assert_contains(&text, "\"F1_verdict_survives_idle_pass\": false", "the drained-queue face red");
    assert_contains(&text, "\"F3_held_verdict_survives_idle_pass\": false", "the held-queue face red");
    assert_contains(&text, "\"F5_event_verdict_survives_idle_pass\": false", "the event-channel mirror face red");
    assert_contains(&text, "\"guards_green\": 4", "the guards green on the prefix side");
    // the verdicts were erased to null — the exact defect, kept as measured
    assert_contains(&text, "\"f1_row\": {\n      \"last_error\": null,\n      \"last_reorg_error\": null", "the erased journal row");
    // the binaries are named by hash, never by host path
    assert_contains(&text, "\"bin_sha256\": \"2272a7c3", "the pre-fix binary's sha256");
    assert!(!text.contains("/tmp/"), "no host path in the committed artifact");
    assert!(!text.contains("/home/"), "no host path in the committed artifact");
}

#[test]
fn the_fixed_artifact_records_the_surviving_verdicts() {
    let text = artifact(FIXED);
    assert_contains(&text, "\"verdict\": \"reorg-verdict-survives\"", "the fixed verdict");
    assert_contains(&text, "\"faces_count\": 6", "all faces green");
    assert_contains(&text, "\"F1_verdict_survives_idle_pass\": true", "the drained-queue face green");
    assert_contains(&text, "\"F2_status_reports_reorg_error_exit3\": true", "the status face green");
    assert_contains(&text, "\"F3_held_verdict_survives_idle_pass\": true", "the held-queue face green");
    assert_contains(&text, "\"F5_event_verdict_survives_idle_pass\": true", "the event-channel mirror face green");
    assert_contains(&text, "\"guards_green\": 4", "the guards green on the fixed side");
    assert_contains(&text, "\"f1_row\": {\n      \"last_error\": null,\n      \"last_reorg_error\": \"refused with http 404\"", "the surviving verdict in the journal");
    assert_contains(&text, "\"bin_sha256\": \"cad7de40", "the fixed binary's sha256");
    assert!(!text.contains("/tmp/"), "no host path in the committed artifact");
    assert!(!text.contains("/home/"), "no host path in the committed artifact");
}

#[test]
fn the_two_sides_measure_different_binaries() {
    let prefix = artifact(PREFIX);
    let fixed = artifact(FIXED);
    let prefix_sha = prefix
        .split("\"bin_sha256\": \"")
        .nth(1)
        .and_then(|s| s.split('"').next())
        .expect("prefix sha")
        .to_string();
    let fixed_sha = fixed
        .split("\"bin_sha256\": \"")
        .nth(1)
        .and_then(|s| s.split('"').next())
        .expect("fixed sha")
        .to_string();
    assert_ne!(
        prefix_sha, fixed_sha,
        "the two sides must be two different binaries, not one binary measured twice"
    );
}

/// The RELEASE verification: the artifact published alongside v0.7.40 must
/// carry the fix, and it must be a DIFFERENT binary than the two tree
/// sides. If the tag shipped something other than what was measured, the
/// release claim would be empty — this pins the shipped bytes to a
/// measurement.
const PUBLISHED_V0740: &str = "reorg-verdict-survival-published-v0740-2026-11-19.json";

#[test]
fn the_published_v0740_asset_carries_the_fix() {
    let text = artifact(PUBLISHED_V0740);
    assert_contains(
        &text,
        "\"verdict\": \"reorg-verdict-survives\"",
        "the published v0.7.40 verdict",
    );
    assert_contains(&text, "\"faces_count\": 6", "all faces green on the shipped asset");
    assert_contains(&text, "\"guards_green\": 4", "the guards green on the shipped asset");
    assert_contains(
        &text,
        "\"f1_row\": {\n      \"last_error\": null,\n      \"last_reorg_error\": \"refused with http 404\"",
        "the surviving verdict in the shipped asset's journal",
    );
    // The label names the release digest the Release API published, so the
    // artifact points at the exact bytes a stranger downloads.
    assert_contains(
        &text,
        "\"label\": \"published-v0.7.40-sha87cebcb4\"",
        "the shipped asset's release digest",
    );
    assert!(!text.contains("/tmp/"), "no host path in the committed artifact");
    assert!(!text.contains("/home/"), "no host path in the committed artifact");
}

#[test]
fn the_shipped_asset_is_a_third_binary() {
    let prefix = artifact(PREFIX);
    let fixed = artifact(FIXED);
    let published = artifact(PUBLISHED_V0740);
    let sha = |t: &str| {
        t.split("\"bin_sha256\": \"")
            .nth(1)
            .and_then(|s| s.split('"').next())
            .expect("sha")
            .to_string()
    };
    let (p, f, s) = (sha(&prefix), sha(&fixed), sha(&published));
    assert_ne!(p, f, "the two tree sides must differ");
    assert_ne!(p, s, "the shipped asset must differ from the pre-fix tree");
    assert_ne!(f, s, "the shipped asset must differ from the local fix build");
}
