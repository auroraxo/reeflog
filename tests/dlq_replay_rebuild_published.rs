//! Pins the measurements of the PUBLISHED replay-rebuild artifacts: v0.7.31
//! carrying the defect and the fixed tree holding the rule.
//!
//! Why this exists (2026-11-16): the fix is only real if it is in the bytes
//! a stranger installs. A third party who installed the pinned v0.7.31 tag
//! got a `dlq replay` that re-sent the body frozen into the quarantine —
//! the same event `push` delivered with a different body, and a body
//! claiming `seal: "finalized"` for a transaction the chain had unmade.
//!
//! What is pinned here:
//!
//!   * both artifacts exist, parse as JSON, and name the binary they
//!     measured by sha256 and by the version read from that binary — an
//!     artifact that measured a different binary must not pass as this one;
//!   * the PUBLISHED v0.7.31 asset carries the defect: verdict
//!     `dlq-replay-rebuild-blind`, the three defect faces D1–D3 VIOLATED,
//!     the three guard faces G1–G3 NOT;
//!   * the fixed tree holds the rule: verdict `dlq-replay-rebuild-held`,
//!     every face green — the fix is in the bytes, not only in the source;
//!   * the guards are green on BOTH artifacts, so the defect faces are red
//!     on v0.7.31 because of the replay and not because the fixture was
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

const PUBLISHED_DEFECT: &str = "dlq-replay-rebuild-published-v0731-2026-11-16.json";
const PUBLISHED_FIXED: &str = "dlq-replay-rebuild-published-v0732-2026-11-16.json";
const FIXED: &str = "dlq-replay-rebuild-fixed-2026-11-16.json";

fn face<'a>(v: &'a serde_json::Value, id: &str) -> &'a serde_json::Value {
    v["faces"]
        .as_array()
        .expect("the artifact carries a face list")
        .iter()
        .find(|f| f["face"].as_str() == Some(id))
        .unwrap_or_else(|| panic!("the artifact must carry the face {id}"))
}

#[test]
fn the_published_defect_artifact_names_the_binary_it_measured() {
    let v = artifact(PUBLISHED_DEFECT);
    assert_eq!(
        v["binary"]["sha256"].as_str(),
        Some("19ea6fe54159084b982fc50910284e8961000620bc70e08969d8efab146ab1a4"),
        "the artifact must name the published v0.7.31 binary by sha256"
    );
    assert_eq!(
        v["binary"]["version"].as_str(),
        Some("reeflog 0.7.31"),
        "the version must be read from that binary"
    );
    assert_eq!(
        v["label"].as_str(),
        Some("published-v0.7.31"),
        "the artifact must say it measured the PUBLISHED asset, not the tree"
    );
}

#[test]
fn the_published_v0731_asset_carries_the_defect() {
    let v = artifact(PUBLISHED_DEFECT);
    assert_eq!(
        v["verdict"].as_str(),
        Some("dlq-replay-rebuild-blind"),
        "the published v0.7.31 asset must be blind to the journal's seal"
    );

    // D1: the replayed body claims the seal the QUARANTINE froze, while the
    // journal already says the chain unmade the transaction.
    let d1 = face(&v, "D1");
    assert_eq!(d1["ok"], false, "D1 must be VIOLATED on the published asset");
    assert_eq!(
        d1["observed"]["journalSeal"].as_str(),
        Some("reverted"),
        "the journal holds the reverted seal"
    );
    assert_eq!(
        d1["observed"]["replayedSeal"].as_str(),
        Some("finalized"),
        "the published replay re-sent the frozen claim"
    );

    // D2: push and replay disagree about the same seq.
    let d2 = face(&v, "D2");
    assert_eq!(d2["ok"], false, "D2 must be VIOLATED on the published asset");
    assert_eq!(d2["observed"]["push"].as_str(), Some("reverted"));
    assert_eq!(d2["observed"]["replay"].as_str(), Some("finalized"));
    assert_eq!(
        d2["observed"]["sameBytes"].as_bool(),
        Some(false),
        "the two surfaces sent different bodies for the same seq"
    );

    // D3: the report says nothing about the divergence.
    let d3 = face(&v, "D3");
    assert_eq!(d3["ok"], false, "D3 must be VIOLATED on the published asset");
    assert_eq!(
        d3["observed"]["rebuilt"].as_u64(),
        None,
        "the published report has no `rebuilt` field at all"
    );
}

#[test]
fn the_guards_are_green_on_the_published_defect_asset() {
    // The defect faces must be red BECAUSE OF THE REPLAY, not because the
    // fixture was broken: the guards on the same published binary are green.
    let v = artifact(PUBLISHED_DEFECT);
    for id in ["G1", "G2", "G3"] {
        assert_eq!(
            face(&v, id)["ok"],
            true,
            "the guard {id} must be green on the published v0.7.31 asset"
        );
    }
    assert_eq!(face(&v, "G1")["observed"]["byteIdentical"].as_bool(), Some(true));
    assert_eq!(face(&v, "G2")["observed"]["sentVerbatim"].as_bool(), Some(true));
    assert_eq!(face(&v, "G3")["observed"]["exitCode"].as_i64(), Some(3));
}

#[test]
fn the_fixed_artifact_holds_the_rule() {
    let v = artifact(FIXED);
    assert_eq!(
        v["verdict"].as_str(),
        Some("dlq-replay-rebuild-held"),
        "the fixed tree must hold the rule on every face"
    );
    for id in ["D1", "D2", "D3", "G1", "G2", "G3"] {
        assert_eq!(
            face(&v, id)["ok"],
            true,
            "the face {id} must be green on the fixed tree"
        );
    }
    assert_eq!(
        face(&v, "D1")["observed"]["replayedSeal"].as_str(),
        Some("reverted"),
        "the replayed body carries the journal's seal"
    );
    assert_eq!(
        face(&v, "D2")["observed"]["sameBytes"].as_bool(),
        Some(true),
        "push and replay send the same bytes for the same seq"
    );
    assert_eq!(
        face(&v, "D3")["observed"]["rebuilt"].as_i64(),
        Some(1),
        "the report names the rebuilt letter"
    );
    assert_eq!(
        face(&v, "G1")["observed"]["rebuilt"].as_i64(),
        Some(0),
        "an unchanged letter is not a rebuild"
    );
}

#[test]
fn the_published_v0732_asset_holds_the_rule() {
    // The fix is only real if it is in the bytes a stranger installs: the
    // published v0.7.32 archive (sha256 == the digest the Release API
    // publishes) must hold every face.
    let v = artifact(PUBLISHED_FIXED);
    assert_eq!(
        v["binary"]["sha256"].as_str(),
        Some("534c2ec75cfcae1d0c70b5302e70bab1eb9405603f72c0753cf7e355e6053d97"),
        "the artifact must name the published v0.7.32 binary by sha256"
    );
    assert_eq!(v["binary"]["version"].as_str(), Some("reeflog 0.7.32"));
    assert_eq!(v["label"].as_str(), Some("published-v0.7.32"));
    assert_eq!(
        v["verdict"].as_str(),
        Some("dlq-replay-rebuild-held"),
        "the published v0.7.32 asset must hold the rule"
    );
    for id in ["D1", "D2", "D3", "G1", "G2", "G3"] {
        assert_eq!(
            face(&v, id)["ok"],
            true,
            "the face {id} must be green on the published v0.7.32 asset"
        );
    }
    assert_eq!(
        face(&v, "D1")["observed"]["replayedSeal"].as_str(),
        Some("reverted")
    );
    assert_eq!(face(&v, "D3")["observed"]["rebuilt"].as_i64(), Some(1));
}

#[test]
fn no_artifact_carries_a_host_path() {
    for name in [PUBLISHED_DEFECT, PUBLISHED_FIXED, FIXED] {
        let text = fs::read_to_string(
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("research")
                .join(name),
        )
        .expect("readable");
        for marker in ["/home/", "/tmp/", "/Users/", "C:\\"] {
            assert!(
                !text.contains(marker),
                "the artifact {name} carries a host path marker: {marker}"
            );
        }
    }
}
