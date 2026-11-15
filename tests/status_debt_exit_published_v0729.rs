//! Pins the measurement of the PUBLISHED v0.7.29 asset: `webhook status`
//! printed two debts and exited 0 for both.
//!
//! Why this exists (2026-11-16): the fix was measured against the published
//! v0.7.29 linux asset (the archive whose sha256 equals the digest the
//! Release API publishes) and the fix itself sat on main only. A third party
//! who installed the pinned tag got the report that says "all clear" over a
//! journal whose delivery is stopped — the surface a monitoring caller
//! polls. The v0.7.29 asset itself is the artifact that must carry the
//! defect, because that is the released behaviour.
//!
//! What is pinned here:
//!
//!   * the artifact exists, parses as JSON, and names the binary it
//!     measured by sha256 and by the version read from that binary — an
//!     artifact that measured a different binary must not pass as this one;
//!   * the verdict is `status-debt-exit-blind` — the published asset carries
//!     the defect;
//!   * the three defect faces D1–D3 are VIOLATED on the published asset and
//!     the two guard faces G1–G2 are NOT: the report exits 0 over a
//!     configured quarantine (D1) and over a standing failure (D2), status
//!     and push disagree on the same journal (D3), while a clean journal
//!     exits 0 (G1) and an orphaned letter still exits 3 (G2);
//!   * the fixed tree is green on the same five faces — the fix is measured,
//!     not asserted;
//!   * the artifact carries no host path from the machine that produced it.
//!
//! Falsified by construction: change any observed exit code in the artifact
//! and this test goes red; swap the binary sha256 and it goes red; turn a
//! guard face red and it goes red.

use std::fs;
use std::path::PathBuf;

fn artifact(name: &str) -> serde_json::Value {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("research")
        .join(name);
    let text = fs::read_to_string(&path).expect("the measurement artifact is readable");
    serde_json::from_str(&text).expect("the artifact is JSON")
}

const PUBLISHED: &str = "status-debt-exit-published-v0729-2026-11-16.json";
const FIXED: &str = "status-debt-exit-fixed-tree-2026-11-16.json";

fn face<'a>(v: &'a serde_json::Value, id: &str) -> &'a serde_json::Value {
    v["faces"]
        .as_array()
        .expect("the artifact carries a face list")
        .iter()
        .find(|f| f["face"].as_str() == Some(id))
        .unwrap_or_else(|| panic!("the artifact must carry the face {id}"))
}

#[test]
fn the_published_artifact_names_the_binary_it_measured() {
    let v = artifact(PUBLISHED);
    assert_eq!(
        v["binary"]["sha256"].as_str(),
        Some("918984e7cc2b9433f6f3262df7161028fa54c2fc7bf888485a44c11dba06c57d"),
        "the artifact must name the published v0.7.29 binary by sha256"
    );
    assert_eq!(
        v["binary"]["version"].as_str(),
        Some("reeflog 0.7.29"),
        "the version must be read from that binary"
    );
    assert_eq!(
        v["label"].as_str(),
        Some("published-v0.7.29"),
        "the artifact must say it measured the PUBLISHED asset, not the tree"
    );
}

#[test]
fn the_published_v0729_asset_carries_the_defect() {
    let v = artifact(PUBLISHED);
    assert_eq!(
        v["verdict"].as_str(),
        Some("status-debt-exit-blind"),
        "the published asset is the one carrying the defect"
    );
    for id in ["D1", "D2", "D3"] {
        assert_eq!(
            face(&v, id)["ok"].as_bool(),
            Some(false),
            "the defect face {id} must be violated on the published asset"
        );
    }
    for id in ["G1", "G2"] {
        assert_eq!(
            face(&v, id)["ok"].as_bool(),
            Some(true),
            "the guard face {id} must hold on the published asset"
        );
    }
}

#[test]
fn the_published_asset_exits_zero_over_the_debts_it_prints() {
    let v = artifact(PUBLISHED);
    assert_eq!(
        face(&v, "D1")["observed"]["exitCode"].as_i64(),
        Some(0),
        "the published report exits 0 while it prints the pending quarantine"
    );
    assert_eq!(
        face(&v, "D1")["observed"]["printed"].as_i64(),
        Some(1),
        "and it does print the debt it calls health"
    );
    assert_eq!(
        face(&v, "D2")["observed"]["exitCode"].as_i64(),
        Some(0),
        "the published report exits 0 while it prints a standing failure"
    );
    assert_eq!(
        face(&v, "D2")["observed"]["hasLastError"].as_bool(),
        Some(true),
        "and the standing verdict IS printed"
    );
    assert_eq!(
        face(&v, "D3")["observed"]["status"].as_i64(),
        Some(0),
        "status and push disagree on the same journal"
    );
    assert_eq!(
        face(&v, "D3")["observed"]["push"].as_i64(),
        Some(3),
        "push holds the debt while status does not"
    );
}

#[test]
fn the_published_guards_still_hold() {
    let v = artifact(PUBLISHED);
    assert_eq!(
        face(&v, "G1")["observed"]["exitCode"].as_i64(),
        Some(0),
        "a clean journal exits 0 on the published asset too"
    );
    assert_eq!(
        face(&v, "G2")["observed"]["exitCode"].as_i64(),
        Some(3),
        "the v0.7.29 orphan rule holds on the published asset"
    );
}

#[test]
fn the_fixed_tree_holds_every_face() {
    let v = artifact(FIXED);
    assert_eq!(
        v["verdict"].as_str(),
        Some("status-debt-exit-held"),
        "the fixed tree must hold the debt on every face"
    );
    for id in ["D1", "D2", "D3", "G1", "G2"] {
        assert_eq!(
            face(&v, id)["ok"].as_bool(),
            Some(true),
            "the face {id} must hold on the fixed tree"
        );
    }
    assert_eq!(
        face(&v, "D3")["observed"]["status"].as_i64(),
        Some(3),
        "status and push must agree on the fixed tree"
    );
    assert_eq!(
        face(&v, "D3")["observed"]["push"].as_i64(),
        Some(3),
        "status and push must agree on the fixed tree"
    );
}

#[test]
fn no_artifact_carries_a_host_path() {
    for name in [PUBLISHED, FIXED] {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("research")
            .join(name);
        let text = fs::read_to_string(&path).expect("the artifact is readable");
        for marker in ["/home/", "/tmp/", "/Users/", "C:\\"] {
            assert!(
                !text.contains(marker),
                "{name} carries the host path marker {marker}"
            );
        }
    }
}
