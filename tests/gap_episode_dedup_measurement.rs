//! The gap-episode dedup measurement, pinned on both sides.
//!
//! PULS AQ, 2026-11-18. `tools/measure-gap-episode-dedup.mjs` — the real
//! daemon against a dead TCP port (refused connects) and against a local
//! sink that performs a REAL WebSocket upgrade (the daemon only resumes
//! after a real upgrade, so the heal is honest):
//!
//! * The DEFECT side is the PUBLISHED v0.7.34 release asset (binary sha256
//!   a67ae152…, downloaded from the Release API, verdict
//!   `gap-episode-rows-accumulate`, 7 of 8 faces defective): the reconnect
//!   loop opened a FRESH `connect_fail` row for EVERY refused attempt and
//!   the heal closed only the newest handle — one outage left 6 open rows,
//!   a healed journal reported `gaps_open: 6` forever, and a second
//!   episode raised it to 7. The same permanent false alarm v0.7.27
//!   removed from the hole rows (PULS AJ), now on the connect/subscribe
//!   kinds.
//!
//! * The FIXED side is this tree (verdict `gap-episode-dedup-held`, 8/8
//!   faces): one episode is ONE row per kind while the endpoint is down
//!   (the retries ride on the row's detail — the outage stays loud), the
//!   heal resolves the whole episode as history (`gaps_open: 0` when the
//!   stream is back), the fill count lands on one row, and a truncated
//!   heal keeps the episode open (alarm beats silence).
//!
//! The pin holds the MEASURED numbers, not a summary: the defect artifact
//! must still carry the accumulation it recorded, so a later tree cannot
//! quietly make the baseline look better than it was, and the fixed
//! artifact must still carry the dedup it claims.

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
fn the_published_v0734_artifact_records_the_accumulation() {
    let text = artifact("gap-episode-dedup-published-v0734-2026-11-18.json");

    // The seat is named: the PUBLISHED v0.7.34 asset, not a local tree.
    assert_contains(
        &text,
        "\"label\": \"published-v0734\"",
        "which binary was measured (label)",
    );
    assert_contains(
        &text,
        "\"binVersion\": \"reeflog 0.7.34\"",
        "the measured binary's own version",
    );
    assert_contains(
        &text,
        "\"verdict\": \"gap-episode-rows-accumulate\"",
        "the verdict",
    );
    assert_contains(&text, "\"defects\": 7", "the defect face count");
    // The measured faces: the accumulation and the un-healed heal.
    assert_contains(
        &text,
        "F1 one episode = exactly ONE open connect_fail row",
        "the one-row face (it is a DEFECT here)",
    );
    assert_contains(
        &text,
        "\"open\": 6",
        "six open rows after ONE episode (the accumulation)",
    );
    assert_contains(
        &text,
        "\"gaps_open\": 7",
        "the alarm kept climbing across two episodes",
    );
}

#[test]
fn the_fixed_artifact_records_the_dedup_and_the_heal() {
    let text = artifact("gap-episode-dedup-fixed-2026-11-18.json");

    assert_contains(&text, "\"verdict\": \"gap-episode-dedup-held\"", "the verdict");
    assert_contains(&text, "\"defects\": 0", "no defect faces on the fixed tree");
    // The measured binary must be THIS tree's release — an artifact from an
    // older run would still be green on every other claim (the pin-lag
    // test reads the tree the same way).
    let cargo = std::fs::read_to_string(
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml"),
    )
    .expect("Cargo.toml is readable");
    let crate_version = cargo
        .lines()
        .find(|l| l.starts_with("version = "))
        .and_then(|l| l.split('"').nth(1))
        .expect("Cargo.toml names a version");
    assert_contains(
        &text,
        &format!("\"binVersion\": \"reeflog {crate_version}\""),
        "the measured binary is this tree's release",
    );
    // The episode stays loud while the endpoint is down.
    assert_contains(
        &text,
        "F3 status gaps_open == 1 while the endpoint is down (loud)",
        "the honest-alarm face",
    );
    // The heal resolves the episode as history.
    assert_contains(
        &text,
        "\"resolved\": 2",
        "both episodes stay as resolved history",
    );
    assert_contains(&text, "\"gaps_open\": 0", "the healed journal owes nothing");
}

#[test]
fn the_prefix_tree_artifact_records_the_same_accumulation() {
    // The v0.7.34 tree (commit 0335674) built from source — the same
    // defect the published asset carries, measured from the tree side.
    let text = artifact("gap-episode-dedup-prefix-2026-11-18.json");
    assert_contains(
        &text,
        "\"label\": \"prefix-tree-0335674\"",
        "which tree was measured (label)",
    );
    assert_contains(&text, "\"binVersion\": \"reeflog 0.7.34\"", "the measured binary");
    assert_contains(
        &text,
        "\"verdict\": \"gap-episode-rows-accumulate\"",
        "the verdict",
    );
    assert_contains(&text, "\"defects\": 7", "the defect face count");
    assert_contains(&text, "\"open\": 6", "six open rows after ONE episode");
}

#[test]
fn no_artifact_carries_a_host_path() {
    for name in [
        "gap-episode-dedup-published-v0734-2026-11-18.json",
        "gap-episode-dedup-prefix-2026-11-18.json",
        "gap-episode-dedup-fixed-2026-11-18.json",
    ] {
        let text = artifact(name);
        for marker in ["/home/", "/tmp/", "/Users/", "C:\\"] {
            assert!(
                !text.contains(marker),
                "{name} must not carry a host path (found {marker})"
            );
        }
    }
}
