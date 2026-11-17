//! Pins the measurements of the retraction-block artifacts: the published
//! v0.7.32 asset carrying the defect and the published v0.7.33 asset
//! holding the rule.
//!
//! Why this exists (2026-11-17): the fix is only real if it is in the bytes
//! a stranger installs. A third party who installed the pinned v0.7.32 tag
//! got a webhook fan-out that reported a refused RETRACTION on the EVENT
//! channel's line, at a position that did not exist (`at seq None`), with
//! Rust debug formatting in the same line and the retraction's reason
//! arriving on the event channel's `blocked_error` field — the operator
//! could not tell which queue was stuck.
//!
//! What is pinned here:
//!
//!   * both artifacts exist, parse as JSON, and name the binaries they
//!     measured by sha256 and by the version read from those binaries — an
//!     artifact that measured a different binary must not pass as this one;
//!   * the PUBLISHED v0.7.32 asset carries the defect: verdict
//!     `retraction-block-blind`, the defect faces D1–D3 VIOLATED, the guard
//!     faces G1–G3 NOT;
//!   * the PUBLISHED v0.7.33 asset holds the rule: verdict
//!     `retraction-block-named`, every face and guard green;
//!   * the guards are green on BOTH artifacts, so the defect faces are red
//!     on v0.7.32 because of the reporting and not because the fixture was
//!     broken;
//!   * no artifact carries a host path from the machine that produced it.
//!
//! Falsified by construction: change any observed value in an artifact and
//! this test goes red; swap a binary sha256 and it goes red; turn a guard
//! face red and it goes red.

use std::fs;
use std::path::PathBuf;

fn artifact(name: &str) -> serde_json::Value {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("research")
        .join(name);
    let text = fs::read_to_string(&path).expect("the measurement artifact is readable");
    serde_json::from_str(&text).expect("the artifact is JSON")
}

/// The published asset run: measured on the PUBLISHED v0.7.32 bytes and on
/// the fixed tree — `faces[]` carries a `pre` and a `post` side.
const PUBLISHED_DEFECT: &str = "retraction-block-blind-2026-11-17.json";
/// The published FIXED asset run: measured on the PUBLISHED v0.7.33 bytes —
/// `faces[]` carries one `ok` side.
const PUBLISHED_FIXED: &str = "retraction-block-published-v0733-2026-11-17.json";

fn side<'a>(v: &'a serde_json::Value, id: &str, key: &str, field: &str) -> &'a serde_json::Value {
    v[field]
        .as_array()
        .expect("the artifact carries a list")
        .iter()
        .find(|f| f[key].as_str() == Some(id))
        .unwrap_or_else(|| panic!("the artifact must carry {field} entry {id}"))
}

#[test]
fn the_defect_artifact_names_the_binaries_it_measured() {
    let v = artifact(PUBLISHED_DEFECT);
    assert_eq!(
        v["baseline"]["binary"]["sha256"].as_str(),
        Some("534c2ec75cfcae1d0c70b5302e70bab1eb9405603f72c0753cf7e355e6053d97"),
        "the baseline must name the published v0.7.32 linux archive by sha256"
    );
    assert_eq!(
        v["baseline"]["binary"]["version"].as_str(),
        Some("reeflog 0.7.32"),
        "the version must be read from that binary"
    );
    assert_eq!(
        v["baseline"]["tree"].as_str(),
        Some("published v0.7.32 release asset"),
        "the baseline side must say it measured the PUBLISHED asset, not a local checkout"
    );
    assert_eq!(
        v["baseline"]["verdict"].as_str(),
        Some("retraction-block-blind"),
        "the published v0.7.32 asset must be blind to the retraction channel"
    );
    assert_eq!(
        v["baseline"]["faces"].as_str(),
        Some("1/4"),
        "three of four defect faces must be violated on v0.7.32 (D4 is the must-not-move face)"
    );
    assert_eq!(
        v["baseline"]["guards"].as_str(),
        Some("3/3"),
        "all guards must be green on the defect asset — the red faces are the reporting's, not the fixture's"
    );
}

#[test]
fn the_published_v0732_asset_carries_the_defect() {
    let v = artifact(PUBLISHED_DEFECT);

    // D1: the operator's line named the EVENT channel and a position that
    // does not exist.
    let d1 = side(&v, "D1", "face", "faces");
    assert_eq!(d1["pre"]["ok"].as_bool(), Some(false), "D1 must be VIOLATED on v0.7.32");
    assert_eq!(
        d1["pre"]["observed"]["namesRetraction"].as_bool(),
        Some(false),
        "the pre-fix line never says it is a retraction"
    );
    assert_eq!(
        d1["pre"]["observed"]["noPhantomPosition"].as_bool(),
        Some(false),
        "the pre-fix line printed `at seq None` — a position that does not exist"
    );
    assert_eq!(
        d1["pre"]["observed"]["namesReorgId"].as_bool(),
        Some(false),
        "the pre-fix line never named the reorg id"
    );

    // D2: the line leaked Rust debug formatting.
    let d2 = side(&v, "D2", "face", "faces");
    assert_eq!(d2["pre"]["ok"].as_bool(), Some(false), "D2 must be VIOLATED on v0.7.32");
    assert_eq!(
        d2["pre"]["observed"]["noDebugFormatting"].as_bool(),
        Some(false),
        "the pre-fix line printed `at seq Some(1)`"
    );

    // D3: the retraction's reason had no field of its own.
    let d3 = side(&v, "D3", "face", "faces");
    assert_eq!(d3["pre"]["ok"].as_bool(), Some(false), "D3 must be VIOLATED on v0.7.32");
    assert_eq!(
        d3["pre"]["observed"]["reorgError"].as_str(),
        None,
        "the pre-fix report has no `reorg_error`; the reason sat in the shared `blocked_error`"
    );
}

#[test]
fn the_guards_are_green_on_the_published_defect_asset() {
    let v = artifact(PUBLISHED_DEFECT);
    for id in ["G1", "G2", "G3"] {
        assert_eq!(
            side(&v, id, "guard", "guards")["pre"]["ok"].as_bool(),
            Some(true),
            "guard {id} must be green on v0.7.32 — the defect faces are red because of the reporting, not because the fixture was broken"
        );
    }
    // G2 is the design rule that must survive the fix: a refused retraction
    // is never quarantined.
    let g2 = side(&v, "G2", "guard", "guards");
    assert_eq!(
        g2["pre"]["observed"]["deadLetters"].as_array().map(|a| a.len()),
        Some(0),
        "a refused retraction is never quarantined, before the fix"
    );
    assert_eq!(
        g2["post"]["observed"]["deadLetters"].as_array().map(|a| a.len()),
        Some(0),
        "a refused retraction is still never quarantined after the fix"
    );
}

#[test]
fn the_published_v0733_asset_holds_the_rule() {
    let v = artifact(PUBLISHED_FIXED);
    assert_eq!(
        v["binary"]["sha256"].as_str(),
        Some("e660b9ecb37d8ba6774ee6ef6d65e08d11b2696d9d603d7edf39c85ae3197d9b"),
        "the artifact must name the published v0.7.33 linux archive by sha256"
    );
    assert_eq!(
        v["binary"]["version"].as_str(),
        Some("reeflog 0.7.33"),
        "the version must be read from that binary"
    );
    assert_eq!(
        v["label"].as_str(),
        Some("published-v0.7.33"),
        "the artifact must say it measured the PUBLISHED asset, not the tree"
    );
    assert_eq!(
        v["verdict"].as_str(),
        Some("retraction-block-named"),
        "the published v0.7.33 asset must hold the rule — the fix is in the bytes a stranger installs"
    );
    for id in ["D1", "D2", "D3", "D4"] {
        assert_eq!(
            side(&v, id, "id", "faces")["ok"].as_bool(),
            Some(true),
            "face {id} must be green on the published v0.7.33 asset"
        );
    }
    for id in ["G1", "G2", "G3"] {
        assert_eq!(
            side(&v, id, "id", "guards")["ok"].as_bool(),
            Some(true),
            "guard {id} must be green on the published v0.7.33 asset"
        );
    }
    // The retraction's reason is on its own field, on the published bytes.
    let d3 = side(&v, "D3", "id", "faces");
    assert_eq!(
        d3["observed"]["reorgError"].as_str(),
        Some("refused with http 404"),
        "the published report carries the retraction's reason on `reorg_error`"
    );
    assert_eq!(
        d3["observed"]["eventError"].as_str(),
        None,
        "the event channel's error stays null — one queue's reason is not read as the other's"
    );
}

#[test]
fn the_fixed_side_of_the_two_tree_artifact_holds_the_rule() {
    let v = artifact(PUBLISHED_DEFECT);
    assert_eq!(
        v["fixed"]["verdict"].as_str(),
        Some("retraction-block-named"),
        "the fixed side must hold the rule"
    );
    for id in ["D1", "D2", "D3", "D4"] {
        assert_eq!(
            side(&v, id, "face", "faces")["post"]["ok"].as_bool(),
            Some(true),
            "face {id} must be green on the fixed side"
        );
    }
    for id in ["G1", "G2", "G3"] {
        assert_eq!(
            side(&v, id, "guard", "guards")["post"]["ok"].as_bool(),
            Some(true),
            "guard {id} must be green on the fixed side"
        );
    }
}

#[test]
fn no_artifact_carries_a_host_path() {
    for name in [PUBLISHED_DEFECT, PUBLISHED_FIXED] {
        let text = fs::read_to_string(
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("research")
                .join(name),
        )
        .expect("the artifact is readable");
        assert!(
            !text.contains("/home/aurora") && !text.contains("/tmp/reeflog"),
            "the artifact {name} must not carry a host path from the machine that produced it"
        );
    }
}
