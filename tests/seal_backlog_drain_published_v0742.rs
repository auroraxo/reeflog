//! Pins the measurement of the PUBLISHED v0.7.42 asset: the release that
//! carries the seal-pass backlog-drain fix.
//!
//! Why this exists (2026-11-20): the fix (b8941cd) was measured on a local
//! build, and a release that ships a fix must be measured on the BYTES it
//! ships — a rebuild from the same source is not the asset a stranger
//! installs. This test pins the published measurement:
//!
//! * the BASELINE side is the PUBLISHED v0.7.41 linux-amd64 asset
//!   (archive digest `sha256:0db3083d…`, unpacked binary `4b990ae2…` =
//!   `reeflog 0.7.41`) — the defect carrier, and it must STILL carry the
//!   defect (3 faces): tags are immutable, so v0.7.42 is the only fixed
//!   install path;
//! * the FIXED side is the PUBLISHED v0.7.42 linux-amd64 asset (archive
//!   digest `sha256:4c183352…`, unpacked binary `6f20fa89…` =
//!   `reeflog 0.7.42`) — it must hold every face (0 defects) and keep the
//!   guards;
//! * both sides are THIRD binaries, distinct from each other and from the
//!   local fix build — an artifact that measured the wrong bytes cannot
//!   pass as the one it claims;
//! * no host path may leak into the artifact.
//!
//! Falsified by construction: every number is compared with `assert_eq!`
//! against a literal, so an edited artifact cannot re-bless itself.

use std::path::PathBuf;

fn artifact(name: &str) -> serde_json::Value {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("research")
        .join(name);
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    serde_json::from_str(&text).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

/// sha256 of the UNPACKED binary inside the published linux-amd64 archive
/// (the archive's own Release API digest is `sha256:0db3083d…` for v0.7.41
/// and `sha256:4c183352…` for v0.7.42 — both verified before unpacking).
const PUBLISHED_V0741_SHA: &str =
    "4b990ae266b1321d827c0e6314b4c11791003090a230a20ca1845c435a6d7204";
const PUBLISHED_V0742_SHA: &str =
    "6f20fa8988af4231eac87d555c97a077c0e420157cec51773217a9d0bb991c72";

const ARTIFACT: &str = "seal-pass-backlog-drain-published-v0742-2026-11-20.json";

#[test]
fn the_artifact_names_the_verdict_and_both_sides() {
    let v = artifact(ARTIFACT);
    assert_eq!(
        v["verdict"].as_str(),
        Some("seal-pass-backlog-drained"),
        "the verdict"
    );
    assert_eq!(v["baselineDefectFaces"].as_i64(), Some(3), "the baseline defect count");
    assert_eq!(v["fixedDefectFaces"].as_i64(), Some(0), "the fixed defect count");
    assert_eq!(v["baselineGuards"].as_str(), Some("all pass"), "the baseline guards");
    assert_eq!(v["fixedGuards"].as_str(), Some("all pass"), "the fixed guards");
}

#[test]
fn the_baseline_side_is_the_published_v0741_bytes() {
    let v = artifact(ARTIFACT);
    let b = &v["trees"]["baseline"];
    assert_eq!(
        b["binSha256"].as_str(),
        Some(PUBLISHED_V0741_SHA),
        "the baseline must be the PUBLISHED v0.7.41 binary, named by sha256"
    );
    assert_eq!(
        b["binVersion"].as_str(),
        Some("reeflog 0.7.41"),
        "the baseline version must be read from that binary"
    );
    assert!(
        b["source"].as_str().unwrap_or("").contains("PUBLISHED v0.7.41"),
        "the baseline must say it measured the published asset, not a local rebuild"
    );
}

#[test]
fn the_fixed_side_is_the_published_v0742_bytes() {
    let v = artifact(ARTIFACT);
    let f = &v["trees"]["fixed"];
    assert_eq!(
        f["binSha256"].as_str(),
        Some(PUBLISHED_V0742_SHA),
        "the fixed side must be the PUBLISHED v0.7.42 binary, named by sha256"
    );
    assert_eq!(
        f["binVersion"].as_str(),
        Some("reeflog 0.7.42"),
        "the fixed version must be read from that binary"
    );
    assert!(
        f["source"].as_str().unwrap_or("").contains("PUBLISHED v0.7.42"),
        "the fixed side must say it measured the published asset, not a local rebuild"
    );
}

#[test]
fn the_published_v0741_asset_still_carries_the_measured_stall() {
    let v = artifact(ARTIFACT);
    let faces = &v["trees"]["baseline"]["faces"];
    // D1: 1,000 pending, one daemon run → 300 sealed, 700 unsealed, exit 0.
    assert_eq!(faces["D1_daemon_drain"]["sealed"].as_i64(), Some(300));
    assert_eq!(faces["D1_daemon_drain"]["still_unsealed"].as_i64(), Some(700));
    assert_eq!(faces["D1_daemon_drain"]["exit"].as_i64(), Some(0));
    // D2: the CLI sealed 150 of 1,000 and exited 0.
    assert_eq!(faces["D2_cli_drain"]["sealed"].as_i64(), Some(150));
    assert_eq!(faces["D2_cli_drain"]["still_unsealed"].as_i64(), Some(850));
    // D3: the undecidable head blocked the decidable tail — 0 sealed.
    assert_eq!(faces["D3_head_block"]["undecidable_head"].as_i64(), Some(150));
    assert_eq!(faces["D3_head_block"]["sealed"].as_i64(), Some(0));
    assert_eq!(faces["D3_head_block"]["still_unsealed"].as_i64(), Some(300));
}

#[test]
fn the_published_v0742_asset_drains_the_whole_backlog() {
    let v = artifact(ARTIFACT);
    let faces = &v["trees"]["fixed"]["faces"];
    // D1: one daemon run drains all 1,000.
    assert_eq!(faces["D1_daemon_drain"]["sealed"].as_i64(), Some(1000));
    assert_eq!(faces["D1_daemon_drain"]["still_unsealed"].as_i64(), Some(0));
    // D2: the CLI asked every row and says it finished.
    assert_eq!(faces["D2_cli_drain"]["sealed"].as_i64(), Some(1000));
    assert_eq!(faces["D2_cli_drain"]["still_unsealed"].as_i64(), Some(0));
    assert_eq!(faces["D2_cli_drain"]["seal_pass"]["asked"].as_i64(), Some(1000));
    assert_eq!(faces["D2_cli_drain"]["complete"].as_bool(), Some(true));
    // D3: the rows behind an undecidable head got their verdicts, and the
    // two-source rule held (the row the chain cannot decide is NOT missing).
    assert_eq!(faces["D3_head_block"]["sealed"].as_i64(), Some(150));
    assert_eq!(faces["D3_head_block"]["missing"].as_i64(), Some(0));
}

#[test]
fn the_two_sides_are_different_binaries() {
    let v = artifact(ARTIFACT);
    let b = v["trees"]["baseline"]["binSha256"].as_str().unwrap_or("");
    let f = v["trees"]["fixed"]["binSha256"].as_str().unwrap_or("");
    assert_ne!(b, f, "the published defect carrier and the published fix must differ");
    assert_ne!(b, PUBLISHED_V0742_SHA);
    assert_ne!(f, PUBLISHED_V0741_SHA);
}

#[test]
fn the_artifact_has_no_host_path() {
    let text = std::fs::read_to_string(
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("research")
            .join(ARTIFACT),
    )
    .expect("the artifact is readable");
    for leak in ["/home/", "/tmp/reeflog", "/Users/", "aurora"] {
        assert!(
            !text.contains(leak),
            "the published artifact must not carry the host path {leak}"
        );
    }
}
