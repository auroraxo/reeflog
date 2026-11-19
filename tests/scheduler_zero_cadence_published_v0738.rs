//! The published v0.7.38 asset carries the zero-cadence fix.
//!
//! PULS AV, 2026-11-19. Re-measured after the release: both binaries are the
//! RELEASE BYTES a stranger actually installs (the linux archives whose
//! sha256 equals the digest the Release API publishes), not trees. The
//! measurement ran with `tools/measure-scheduler-zero-cadence.mjs` against
//! a local chain sink that counts every RPC call and every webhook body.
//!
//! * baseline — the published v0.7.37 asset, binary `ddba9506…`:
//!   verdict `scheduler-zero-cadence-busy`, 5 defect faces (D1 seal 1 Hz,
//!   D2 reorg 1 Hz + gate skipped, D3 repair 1 Hz + gate skipped,
//!   D4 webhook delivered signed bodies, D5 both meanings in one summary);
//! * fixed — the published v0.7.38 asset, binary `0f937457…`: every one of
//!   the five defect faces holds (0 defect faces), and the guards G1
//!   (a never-due cadence schedules nothing, gates keep their meaning) and
//!   G2 (a tight positive cadence still runs) hold on BOTH trees — the fix
//!   narrowed the zero, it did not move the honest forms.

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

const ARTIFACT: &str = "scheduler-zero-cadence-published-v0738-2026-11-19.json";

#[test]
fn both_sides_are_the_published_release_bytes() {
    let text = artifact(ARTIFACT);
    // The binaries are named by hash, never by host path.
    assert_contains(
        &text,
        "\"binVersion\": \"reeflog 0.7.37\"",
        "the baseline's own version",
    );
    assert_contains(
        &text,
        "\"binSha256\": \"ddba950633d1ffd5e283ae5b477bc720e611ac83bdaaafff84dee0bdd275074e\"",
        "the published v0.7.37 binary a stranger installs",
    );
    assert_contains(
        &text,
        "\"binVersion\": \"reeflog 0.7.38\"",
        "the fixed side's own version",
    );
    assert_contains(
        &text,
        "\"binSha256\": \"0f937457a21e2128318b39fa0fa120b03668b1097be639e7c0f408533d788167\"",
        "the published v0.7.38 binary a stranger installs",
    );
    assert_contains(&text, "\"label\": \"published v0.7.37\"", "the baseline label");
    assert_contains(&text, "\"label\": \"published v0.7.38\"", "the fixed label");
}

#[test]
fn the_baseline_defect_and_the_fixed_verdict_are_recorded() {
    let text = artifact(ARTIFACT);
    assert_contains(
        &text,
        "\"verdict\": \"scheduler-zero-cadence-busy\"",
        "the verdict",
    );
    assert_contains(&text, "\"baselineDefectFaces\": 5", "the baseline defect count");
    assert_contains(&text, "\"fixedDefectFaces\": 0", "the fixed defect count");
    // The baseline's 1 Hz evidence must still be readable: a later edit
    // must not be able to make the defect look smaller than it was.
    assert_contains(&text, "\"seal_passes\": 2", "the baseline seal pass at a cadence of 0");
    assert_contains(&text, "\"posts\": 2", "the baseline webhook face delivering bodies");
    // The fixed side's zero: nothing scheduled at any of the four cadences.
    assert_contains(&text, "\"repair_passes\": 0", "the fixed tree's silenced repair schedule");
}

#[test]
fn no_artifact_carries_a_host_path() {
    let text = artifact(ARTIFACT);
    assert!(
        !text.contains("/home/") && !text.contains("/tmp/") && !text.contains("/Users/"),
        "the claim must not be pinned to one machine"
    );
}
