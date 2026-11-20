//! The drain measurement, pinned on both sides.
//!
//! PULS BA, 2026-11-20. `tools/measure-seal-pass-backlog-drain.mjs` — two
//! real binaries against one local chain sink that answers every status:
//!
//! * The BASELINE side is the PUBLISHED v0.7.41 linux-amd64 release asset
//!   (downloaded from the GitHub release API, archive digest `0db3083d…`,
//!   unpacked binary sha256 `4b990ae2…` = `reeflog 0.7.41`) — i.e. the bytes
//!   a stranger installs. Verdict `seal-pass-backlog-drained` with 3 DEFECT
//!   FACES: the seal pass asked for the oldest 150 pending rows every time,
//!   so a 1,000-row backlog drained 300 rows in one daemon run (700
//!   unsealed at exit 0), the CLI sealed 150 of 1,000 and exited 0, and 150
//!   undecidable head rows left all 150 decidable rows behind them unasked
//!   (0 sealed).
//!
//! * The FIXED side is this tree (0 defect faces): the pass walks the
//!   pending set with a seq cursor instead of a window, so every row is
//!   asked exactly once per pass, and the pass reports whether it actually
//!   reached the end.
//!
//! The pin holds the MEASURED numbers on both sides — the baseline side
//! must still carry the exact stall it recorded AND the exact published
//! bytes it was measured on (sha256 + version), and the fixed side must
//! still carry the full drains it claims and describe the release this
//! tree actually is.

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

const ARTIFACT: &str = "seal-pass-backlog-drain-2026-11-20.json";

/// The UNPACKED binary inside the PUBLISHED v0.7.41 linux-amd64 archive
/// (the archive's own Release API digest is `sha256:0db3083d…`, verified
/// before unpacking).
const PUBLISHED_V0741_SHA: &str =
    "4b990ae266b1321d827c0e6314b4c11791003090a230a20ca1845c435a6d7204";

#[test]
fn the_baseline_side_names_the_published_bytes_it_measured() {
    let v: serde_json::Value =
        serde_json::from_str(&artifact(ARTIFACT)).expect("the artifact is JSON");
    let baseline = &v["trees"]["baseline"];
    assert_eq!(
        baseline["binSha256"].as_str(),
        Some(PUBLISHED_V0741_SHA),
        "the baseline side must be the PUBLISHED v0.7.41 binary, named by sha256"
    );
    assert_eq!(
        baseline["binVersion"].as_str(),
        Some("reeflog 0.7.41"),
        "the baseline version must be read from that binary"
    );
    let source = baseline["source"].as_str().expect("the baseline names its source");
    assert!(
        source.contains("PUBLISHED v0.7.41"),
        "the baseline must say it measured the published asset, not a local rebuild"
    );
}

#[test]
fn the_fixed_side_describes_the_release_this_tree_is() {
    let v: serde_json::Value =
        serde_json::from_str(&artifact(ARTIFACT)).expect("the artifact is JSON");
    let fixed = &v["trees"]["fixed"];
    let cargo = std::fs::read_to_string(
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml"),
    )
    .expect("Cargo.toml is readable");
    let crate_version = cargo
        .lines()
        .find(|l| l.starts_with("version = "))
        .and_then(|l| l.split('"').nth(1))
        .expect("Cargo.toml names a version");
    assert_eq!(
        fixed["binVersion"].as_str(),
        Some(format!("reeflog {crate_version}").as_str()),
        "the fixed side must be the binary of THIS tree's version"
    );
    assert_ne!(
        fixed["binSha256"].as_str(),
        Some(PUBLISHED_V0741_SHA),
        "the fixed build must be a different binary from the defect carrier"
    );
}

#[test]
fn the_artifact_names_the_verdict_and_both_sides() {
    let text = artifact(ARTIFACT);
    assert_contains(
        &text,
        "\"verdict\": \"seal-pass-backlog-drained\"",
        "the verdict",
    );
    assert_contains(&text, "\"baselineDefectFaces\": 3", "the baseline defect count");
    assert_contains(&text, "\"fixedDefectFaces\": 0", "the fixed defect count");
    assert_contains(&text, "\"baselineGuards\": \"all pass\"", "the baseline guards");
    assert_contains(&text, "\"fixedGuards\": \"all pass\"", "the fixed guards");
}

#[test]
fn the_baseline_side_still_carries_the_measured_stall() {
    let text = artifact(ARTIFACT);
    // D1: 1,000 pending, one daemon run → 300 sealed, 700 unsealed, exit 0.
    assert_contains(&text, "\"still_unsealed\": 700", "the 700 rows the run left behind");
    assert_contains(&text, "\"sealed\": 300", "the 300 rows the run managed");
    // D2: the CLI sealed 150 of 1,000 and exited 0.
    assert_contains(&text, "\"still_unsealed\": 850", "the 850 rows the CLI never asked");
    // D3: the undecidable head blocked the decidable tail — 0 sealed.
    assert_contains(&text, "\"sealed\": 0", "the tail the head blocked");
    assert_contains(&text, "\"undecidable_head\": 150", "the size of the blocking head");
}

#[test]
fn the_fixed_side_still_carries_the_full_drains() {
    let text = artifact(ARTIFACT);
    assert_contains(&text, "\"still_unsealed\": 0", "the drains that reached zero");
    // The CLI walk: 1,000 asked in 7 batches and the pass says it finished.
    assert_contains(&text, "\"asked\": 1000", "the full backlog was asked");
    assert_contains(&text, "\"complete\": true", "the pass reports itself finished");
    // D3: the rows behind an undecidable head got their verdicts.
    assert_contains(&text, "\"sealed\": 150", "the tail the fixed pass reached");
    assert_contains(&text, "\"missing\": 0", "the two-source rule held over the head");
}

#[test]
fn both_sides_agree_that_the_two_source_rule_never_moved() {
    let text = artifact(ARTIFACT);
    assert_contains(
        &text,
        "\"G2_two_source_holds\"",
        "the two-source guard on both trees",
    );
}

#[test]
fn the_artifact_has_no_host_paths() {
    let text = artifact(ARTIFACT);
    assert!(!text.contains("/home/"), "host paths must not leak into the artifact");
    assert!(!text.contains("/tmp/"), "host paths must not leak into the artifact");
}
