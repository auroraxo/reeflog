//! The fan-out isolation measurement, pinned on both sides.
//!
//! PULS AP, 2026-11-17. `tools/measure-fanout-isolation.mjs` — three live
//! sinks, the binary's own report:
//!
//! * The BLIND side is the v0.7.33 tree (commit 06694b0, binary sha256
//!   412cc1e9…, verdict `fanout-isolation-blind`, 5/8 faces). A webhook
//!   target whose delivery state is bound to ANOTHER endpoint aborted the
//!   whole command: `webhook push` exited 1 with NO report, `webhook status`
//!   exited 1 with NO report, `webhook dlq replay` exited 1 and replayed
//!   nothing. The healthy sink received 3 of 5 bodies — the two events that
//!   were pending while the sibling was repointed never left the machine —
//!   and the new endpoint received 0 bytes (the v0.7.15 rule held; only the
//!   BLAST RADIUS was wrong).
//!
//! * The FIXED side is this tree (verdict `fanout-isolation-held`, 8/8
//!   faces): the refusal is a fact about ONE consumer, so the other
//!   consumers keep running, every surface still prints a report, the
//!   refused target is named, and it holds the exit code (3 = "reported and
//!   held", where 1 = "abort without report" is the pre-fix behaviour).
//!
//! The pin holds the MEASURED numbers, not a summary: the blind artifact
//! must still carry the starvation it recorded, so a later tree cannot
//! quietly make the baseline look better than it was, and the fixed
//! artifact must still carry the isolation it claims.

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

#[test]
fn the_v0733_tree_artifact_records_the_blast_radius() {
    let text = artifact("fanout-isolation-blind-v0733-06694b0-2026-11-17.json");

    // The seat is named: this is the v0.7.33 TREE, not a published asset.
    assert_contains(
        &text,
        "\"label\": \"this-tree-v0733-06694b0\"",
        "which tree was measured (label)",
    );
    assert_contains(&text, "\"binaryVersion\": \"reeflog 0.7.33\"", "the measured binary");
    assert_contains(
        &text,
        "412cc1e9df0efa4983efc245d80380ee9c5138404a63e3c504fec4fb123c3fca",
        "the measured binary's sha256",
    );
    assert_contains(&text, "\"verdict\": \"fanout-isolation-blind\"", "the verdict");

    // The starvation itself: the healthy sink got LESS than the queue held.
    assert_contains(
        &text,
        "\"healthy\": 3",
        "the healthy sink's body count (3 of 5 — the starvation)",
    );
    assert_contains(
        &text,
        "\"moved_new\": 0",
        "the new endpoint received NOTHING (the v0.7.15 rule held)",
    );
    // The reports went dark: exit 1 and NO report at all.
    assert_contains(
        &text,
        "\"repointed_push_report_present\": false",
        "the push printed no report",
    );
    assert_contains(
        &text,
        "\"repointed_status_report_present\": false",
        "the status printed no report",
    );
    assert_contains(&text, "\"repointed_push_exit\": 1", "the push exit code");
    assert_contains(&text, "\"repointed_status_exit\": 1", "the status exit code");
}

#[test]
fn the_fixed_tree_artifact_records_the_isolation() {
    let text = artifact("fanout-isolation-fixed-2026-11-17.json");

    assert_contains(
        &text,
        "\"label\": \"tree-build-post-fix\"",
        "which tree was measured (label)",
    );
    assert_contains(
        &text,
        "\"verdict\": \"fanout-isolation-held\"",
        "the verdict: the fan-out is isolated",
    );

    // Every face passed — no face is skipped or silently dropped.
    for face in [
        "G1_a_healthy_config_delivers_to_every_target",
        "F1_a_repointed_target_does_not_silence_the_healthy_one",
        "F2_the_misconfigured_target_is_named_with_its_reason",
        "F3_status_still_reports_the_healthy_target",
        "F4_status_names_the_misconfigured_target",
        "F5_the_misconfigured_target_is_refused_and_its_state_is_untouched",
        "F6_the_refused_target_holds_the_exit_code",
        "G2_a_single_healthy_target_reports_its_own_numbers",
    ] {
        assert_contains(&text, face, "the measured face is present");
    }
    assert!(
        !text.contains("\"pass\": false"),
        "no face may be recorded as failing in the fixed artifact"
    );

    // The healthy consumer got EVERYTHING; the refused one got nothing.
    assert_contains(
        &text,
        "\"healthy\": 5",
        "the healthy sink received the whole queue (5 bodies)",
    );
    assert_contains(
        &text,
        "\"moved_new\": 0",
        "the repointed endpoint received nothing",
    );
    // The reports are back, and the exit code is the repo's own 3.
    assert_contains(
        &text,
        "\"repointed_push_report_present\": true",
        "the push prints a report again",
    );
    assert_contains(
        &text,
        "\"repointed_status_report_present\": true",
        "the status prints a report again",
    );
    assert_contains(&text, "\"repointed_push_exit\": 3", "the push exit code");
    assert_contains(&text, "\"repointed_status_exit\": 3", "the status exit code");
}

#[test]
fn neither_artifact_carries_a_host_path() {
    for name in [
        "fanout-isolation-blind-v0733-06694b0-2026-11-17.json",
        "fanout-isolation-fixed-2026-11-17.json",
    ] {
        let text = artifact(name);
        for marker in ["/home/", "/tmp/", "/Users/", "C:\\", "github_repos"] {
            assert!(
                !text.contains(marker),
                "{name} carries the host path {marker:?}"
            );
        }
    }
}
