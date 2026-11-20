//! Pins the measurement of the PUBLISHED v0.7.43 asset: the release that
//! carries the seal-answer-shape fix (a short answer is not an answer).
//!
//! Why this exists (2026-11-20): the fix (cc913b1) was measured on a local
//! build, and a release that ships a fix must be measured on the BYTES it
//! ships — a rebuild from the same source is not the asset a stranger
//! installs. This test pins the published measurement:
//!
//! * the BASELINE side is the PUBLISHED v0.7.42 linux-amd64 asset
//!   (archive digest `sha256:4c183352…`, unpacked binary `6f20fa89…` =
//!   `reeflog 0.7.42`) — the defect carrier, and it must STILL carry the
//!   defect (3 faces): tags are immutable, so v0.7.43 is the only fixed
//!   install path;
//! * the FIXED side is the PUBLISHED v0.7.43 linux-amd64 asset (archive
//!   digest `sha256:3e7bcc98…`, unpacked binary `816177e3…` =
//!   `reeflog 0.7.43`) — it must hold every face (0 defects) and keep the
//!   guards;
//! * both sides are THIRD binaries, distinct from each other and from the
//!   local fix build (e0ecb02e…) — an artifact that measured the wrong
//!   bytes cannot pass as the one it claims;
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
/// (the archive's own Release API digest is `sha256:4c183352…` for v0.7.42
/// and `sha256:3e7bcc98…` for v0.7.43 — both verified before unpacking).
const PUBLISHED_V0742_SHA: &str =
    "6f20fa8988af4231eac87d555c97a077c0e420157cec51773217a9d0bb991c72";
const PUBLISHED_V0743_SHA: &str =
    "816177e336be4f1a76f7ba996dc5d059a0c88e1460b735eeeb05be4ce10f63af";
/// The local fix build on the release commit — neither measured side.
const LOCAL_FIX_SHA: &str =
    "e0ecb02ee163b8a8e3c4e0006499bcc6eb004816a1d292de970a15b946f14adc";

const ARTIFACT: &str = "seal-answer-shape-published-v0743-2026-11-20.json";

#[test]
fn the_artifact_names_the_verdict_and_both_sides() {
    let v = artifact(ARTIFACT);
    assert_eq!(
        v["verdict"].as_str(),
        Some("seal-answer-covers-request"),
        "the verdict"
    );
    assert_eq!(v["baselineDefectFaces"].as_i64(), Some(3), "the baseline defect count");
    assert_eq!(v["fixedDefectFaces"].as_i64(), Some(0), "the fixed defect count");
    assert_eq!(v["baselineGuards"].as_str(), Some("all pass"), "the baseline guards");
    assert_eq!(v["fixedGuards"].as_str(), Some("all pass"), "the fixed guards");
}

#[test]
fn the_baseline_side_is_the_published_v0742_bytes() {
    let v = artifact(ARTIFACT);
    let b = &v["trees"]["baseline"];
    assert_eq!(
        b["binSha256"].as_str(),
        Some(PUBLISHED_V0742_SHA),
        "the baseline must be the PUBLISHED v0.7.42 binary, named by sha256"
    );
    assert_eq!(
        b["binVersion"].as_str(),
        Some("reeflog 0.7.42"),
        "the baseline version must be read from that binary"
    );
    assert!(
        b["source"].as_str().unwrap_or("").contains("PUBLISHED v0.7.42"),
        "the baseline must say it measured the published asset, not a local rebuild"
    );
    assert!(
        b["source"].as_str().unwrap_or("").contains("4c183352723e04827759947c7e5f34c8211db1252659b14be4aa067b91800807"),
        "the baseline must name the archive digest the Release API publishes"
    );
}

#[test]
fn the_fixed_side_is_the_published_v0743_bytes() {
    let v = artifact(ARTIFACT);
    let f = &v["trees"]["fixed"];
    assert_eq!(
        f["binSha256"].as_str(),
        Some(PUBLISHED_V0743_SHA),
        "the fixed side must be the PUBLISHED v0.7.43 binary, named by sha256"
    );
    assert_eq!(
        f["binVersion"].as_str(),
        Some("reeflog 0.7.43"),
        "the fixed version must be read from that binary"
    );
    assert!(
        f["source"].as_str().unwrap_or("").contains("PUBLISHED v0.7.43"),
        "the fixed side must say it measured the published asset, not a local rebuild"
    );
    assert!(
        f["source"].as_str().unwrap_or("").contains("3e7bcc9890682411059a6dd50d6b5f9db9b8accb1ced81bf93ebb1cf38a5cbab"),
        "the fixed side must name the archive digest the Release API publishes"
    );
}

#[test]
fn the_published_v0742_asset_still_carries_the_measured_defect() {
    let v = artifact(ARTIFACT);
    let faces = &v["trees"]["baseline"]["faces"];
    // D1: 10 pending, short answer → 5 sealed, 5 never asked, exit 0, complete.
    assert_eq!(faces["D1_cli_short_answer"]["sealed"].as_i64(), Some(5));
    assert_eq!(faces["D1_cli_short_answer"]["unsealed"].as_i64(), Some(5));
    assert_eq!(faces["D1_cli_short_answer"]["exit"].as_i64(), Some(0));
    assert_eq!(faces["D1_cli_short_answer"]["complete"].as_bool(), Some(true));
    // D2: the daemon tick sealed rows the truncated answer never covered,
    // without naming the incomplete pass.
    assert!(faces["D2_daemon_short_answer"]["sealed"].as_i64().unwrap_or(0) > 0);
    assert_eq!(
        faces["D2_daemon_short_answer"]["names_incomplete_on_stderr"].as_bool(),
        Some(false)
    );
    // D3: the covered half promoted missing while the rest was dropped.
    assert_eq!(faces["D3_missing_behind_truncation"]["missing"].as_i64(), Some(5));
    assert_eq!(faces["D3_missing_behind_truncation"]["unsealed"].as_i64(), Some(5));
    assert_eq!(faces["D3_missing_behind_truncation"]["exit"].as_i64(), Some(0));
}

#[test]
fn the_published_v0743_asset_refuses_to_look_done() {
    let v = artifact(ARTIFACT);
    let faces = &v["trees"]["fixed"]["faces"];
    // D1: a truncated answer seals nothing, exits 3, complete false.
    assert_eq!(faces["D1_cli_short_answer"]["sealed"].as_i64(), Some(0));
    assert_eq!(faces["D1_cli_short_answer"]["exit"].as_i64(), Some(3));
    assert_eq!(faces["D1_cli_short_answer"]["complete"].as_bool(), Some(false));
    assert_eq!(faces["D1_cli_short_answer"]["defect"].as_bool(), Some(false));
    // D2: the daemon tick names the incomplete pass on stderr.
    assert_eq!(faces["D2_daemon_short_answer"]["sealed"].as_i64(), Some(0));
    assert_eq!(
        faces["D2_daemon_short_answer"]["names_incomplete_on_stderr"].as_bool(),
        Some(true)
    );
    // D3: no half promoted missing over an unanswered truncation.
    assert_eq!(faces["D3_missing_behind_truncation"]["sealed"].as_i64(), Some(0));
    assert_eq!(faces["D3_missing_behind_truncation"]["exit"].as_i64(), Some(3));
}

#[test]
fn both_published_sides_keep_their_guards() {
    let v = artifact(ARTIFACT);
    for tree in ["baseline", "fixed"] {
        let guards = v["trees"][tree]["guards"]
            .as_object()
            .expect("guards object");
        assert_eq!(guards.len(), 4, "{tree}: four guards");
        for (name, g) in guards {
            assert_eq!(
                g["pass"].as_bool(),
                Some(true),
                "{tree}: guard {name} must hold on the published bytes"
            );
        }
    }
}

#[test]
fn the_two_sides_are_different_binaries_neither_is_the_local_build() {
    let v = artifact(ARTIFACT);
    let b = v["trees"]["baseline"]["binSha256"].as_str().unwrap_or("");
    let f = v["trees"]["fixed"]["binSha256"].as_str().unwrap_or("");
    assert_ne!(
        b, f,
        "the published defect carrier and the published fix must differ"
    );
    assert_eq!(b, PUBLISHED_V0742_SHA);
    assert_eq!(f, PUBLISHED_V0743_SHA);
    assert_ne!(
        f, LOCAL_FIX_SHA,
        "the fixed side must be the SHIPPED bytes, not the local rebuild"
    );
    assert_ne!(b, LOCAL_FIX_SHA);
}

#[test]
fn the_artifact_has_no_host_path() {
    let text = std::fs::read_to_string(
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("research")
            .join(ARTIFACT),
    )
    .expect("the artifact is readable");
    for leak in ["/home/", "/tmp/", "/Users/", "aurora"] {
        assert!(
            !text.contains(leak),
            "the published artifact must not carry the host path {leak}"
        );
    }
}
