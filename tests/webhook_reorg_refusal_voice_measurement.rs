//! The retraction refusal voice measurement, pinned on both sides.
//!
//! PULS AV, 2026-11-19. `tools/probe-reorg-refusal-voice.mjs` — the real
//! binaries against a real local sink that answers a scripted status and
//! records every request it receives:
//!
//! * The BASELINE side is the release binary of v0.7.38 as published (the
//!   linux archive whose sha256 equals the digest the Release API
//!   publishes; verdict `reorg-refusal-voice-borrowed-flat-wording`, 4
//!   defect faces): with an endpoint that answers 302, the EVENT queue's
//!   reason read "refused with http 302 redirect — … repoint this target's
//!   url explicitly …", while the RETRACTION queue's reason read the flat
//!   "refused with http 302" — in the recorded attempt, in the report's
//!   `reorg_error`, in `webhook status`, and in the operator's blocked
//!   line. Four surfaces, one queue, and the queue whose lateness costs
//!   money read a bare status where the other queue read the repair.
//!
//! * The FIXED side is this tree (0 defect faces): the retraction queue
//!   speaks the same mint — a 3xx names the redirect and the explicit
//!   repoint repair, a 4xx keeps the flat wording it always had, and the
//!   redirect is still never followed.
//!
//! The pin holds the MEASURED numbers, not a summary: the baseline side of
//! the artifact must still carry the flat wording it recorded, so a later
//! tree cannot quietly make the defect look smaller than it was, and the
//! fixed side must still carry the full reason it claims.

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

const ARTIFACT: &str = "reorg-refusal-voice-2026-11-19.json";

#[test]
fn the_artifact_names_the_verdict_and_both_sides() {
    let text = artifact(ARTIFACT);
    assert_contains(
        &text,
        "\"verdict\": \"reorg-refusal-voice-borrowed-flat-wording\"",
        "the verdict",
    );
    assert_contains(&text, "\"binary_version\": \"reeflog 0.7.38\"", "the baseline binary's own version");
    assert_contains(&text, "\"sha256\": \"0f937457", "the baseline binary's sha256 (v0.7.38 as published)");
}

#[test]
fn the_artifact_carries_the_baseline_defect_faces() {
    let text = artifact(ARTIFACT);
    // The baseline's retraction queue read the FLAT wording in four
    // surfaces. A later tree must not be able to make the defect look
    // smaller than it was.
    assert_contains(
        &text,
        "\"value\": \"refused with http 302\"",
        "the baseline's retraction refusal reading the flat 4xx wording",
    );
    assert_contains(
        &text,
        "\"id\": \"D5\"",
        "the polled surface being a measured face of its own",
    );
}

#[test]
fn the_artifact_carries_the_fixed_reason() {
    let text = artifact(ARTIFACT);
    // The fixed side: the retraction queue speaks the same mint as the
    // event queue — the mechanism AND the repair.
    assert_contains(
        &text,
        "repoint this target's url explicitly",
        "the fixed side naming the explicit repair",
    );
}

#[test]
fn the_artifact_carries_the_guards() {
    let text = artifact(ARTIFACT);
    // The normal cases must not move: a 200 sink still delivers, a 404
    // still reads flat, and the redirect is still never followed.
    assert_contains(&text, "\"id\": \"G1\"", "the delivery guard");
    assert_contains(&text, "\"id\": \"G2\"", "the flat 4xx guard");
    assert_contains(&text, "\"id\": \"G3\"", "the block-and-never-follow guard");
    assert_contains(
        &text,
        "\"baseline_value\": \"refused with http 404\"",
        "the 4xx refusal keeping the flat wording",
    );
}
