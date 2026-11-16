//! Pins the measurements of the PUBLISHED payload-debt assets: v0.7.30
//! carrying the defect and v0.7.31 holding the rule.
//!
//! Why this exists (2026-11-16): the fix is only real if it is in the bytes
//! a stranger installs. A third party who installed the pinned v0.7.30 tag
//! got a `status` that printed `payload_pending: 1` and exited 0 — the
//! report a monitoring caller polls calling an owed journal healthy — while
//! the default payload-bearing `tail` on the same journal held and exited 3.
//!
//! What is pinned here:
//!
//!   * both artifacts exist, parse as JSON, and name the binary they
//!     measured by sha256 and by the version read from that binary — an
//!     artifact that measured a different binary must not pass as this one;
//!   * the PUBLISHED v0.7.30 asset carries the defect: verdict
//!     `status-payload-debt-exit-blind`, the three defect faces D1–D3
//!     VIOLATED, the three guard faces G1–G3 NOT;
//!   * the PUBLISHED v0.7.31 asset holds the rule: verdict
//!     `status-payload-debt-exit-held`, every face green — the fix is in
//!     the released bytes, not only on main;
//!   * the guards are green on BOTH published assets, so the defect faces
//!     are red on v0.7.30 because of the exit code and not because the
//!     fixture was broken;
//!   * no artifact carries a host path from the machine that produced it.
//!
//! Falsified by construction: change any observed exit code in an artifact
//! and this test goes red; swap a binary sha256 and it goes red; turn a
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

const PUBLISHED_DEFECT: &str = "status-payload-debt-exit-published-v0730-2026-11-16.json";
const PUBLISHED_FIXED: &str = "status-payload-debt-exit-published-v0731-2026-11-16.json";

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
        Some("b1eac6c03d3b4aa3cf0e3a31d4bc6e746afd31b3b835dfdbcdb80629552742fa"),
        "the artifact must name the published v0.7.30 binary by sha256"
    );
    assert_eq!(
        v["binary"]["version"].as_str(),
        Some("reeflog 0.7.30"),
        "the version must be read from that binary"
    );
    assert_eq!(
        v["label"].as_str(),
        Some("published-v0.7.30"),
        "the artifact must say it measured the PUBLISHED asset, not the tree"
    );
}

#[test]
fn the_published_v0730_asset_carries_the_defect() {
    let v = artifact(PUBLISHED_DEFECT);
    assert_eq!(
        v["verdict"].as_str(),
        Some("status-payload-debt-exit-blind"),
        "the published v0.7.30 asset must be blind to the payload debt"
    );

    // D1: the report prints the debt and exits 0.
    let d1 = face(&v, "D1");
    assert_eq!(d1["ok"], false, "D1 must be VIOLATED on the published asset");
    assert_eq!(d1["observed"]["exitCode"], 0, "the published status exits 0");
    assert_eq!(d1["observed"]["printed"], 1, "while printing the debt");

    // D2: status and the payload-bearing tail DISAGREE on the same journal.
    let d2 = face(&v, "D2");
    assert_eq!(d2["ok"], false, "D2 must be VIOLATED on the published asset");
    assert_eq!(d2["observed"]["status"], 0);
    assert_eq!(d2["observed"]["tail"], 3, "the tail DOES hold");

    // D3: the verdict does not track the debt.
    let d3 = face(&v, "D3");
    assert_eq!(d3["ok"], false, "D3 must be VIOLATED on the published asset");
    assert_eq!(d3["observed"]["before"], 0);
    assert_eq!(d3["observed"]["after"], 0, "paying the debt changed nothing");
}

#[test]
fn the_guards_are_green_on_the_published_defect_asset() {
    // The defect faces must be red BECAUSE OF THE EXIT CODE, not because
    // the fixture was broken: the guards on the same published binary are
    // green. G1/G2/G3 on the pre-fix asset, D1/D2/D3 plus the guards on the
    // fixed one — so the guard faces are NOT pinned to a verdict.
    let v = artifact(PUBLISHED_DEFECT);
    for id in ["G1", "G2", "G3"] {
        assert_eq!(
            face(&v, id)["ok"],
            true,
            "the guard {id} must be green on the published v0.7.30 asset"
        );
    }
    assert_eq!(face(&v, "G1")["observed"]["exitCode"], 0);
    assert_eq!(face(&v, "G3")["observed"]["accountsMissing"], 5);
}

#[test]
fn the_published_fixed_artifact_names_the_binary_it_measured() {
    let v = artifact(PUBLISHED_FIXED);
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
    let sha = v["binary"]["sha256"].as_str().expect("a sha256");
    assert_eq!(sha.len(), 64, "a hex sha256");
    assert_ne!(
        sha,
        "b1eac6c03d3b4aa3cf0e3a31d4bc6e746afd31b3b835dfdbcdb80629552742fa",
        "the fixed artifact must measure the NEW release, not the defective one"
    );
}

#[test]
fn the_published_v0731_asset_holds_the_rule() {
    let v = artifact(PUBLISHED_FIXED);
    assert_eq!(
        v["verdict"].as_str(),
        Some("status-payload-debt-exit-held"),
        "the fix must be in the bytes a stranger installs"
    );

    for id in ["D1", "D2", "D3", "G1", "G2", "G3"] {
        assert_eq!(
            face(&v, id)["ok"],
            true,
            "the face {id} must hold on the published v0.7.31 asset"
        );
    }

    let d1 = face(&v, "D1");
    assert_eq!(d1["observed"]["exitCode"], 3, "the debt holds the exit code");
    let d2 = face(&v, "D2");
    assert_eq!(
        d2["observed"]["status"], d2["observed"]["tail"],
        "one fact, one verdict across the two surfaces"
    );
    let d3 = face(&v, "D3");
    assert_eq!(d3["observed"]["before"], 3);
    assert_eq!(d3["observed"]["after"], 0, "the verdict comes back down");
}

#[test]
fn no_published_artifact_carries_a_host_path() {
    for name in [PUBLISHED_DEFECT, PUBLISHED_FIXED] {
        let text = fs::read_to_string(
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("research")
                .join(name),
        )
        .expect("the artifact is readable");
        assert!(
            !text.contains("/tmp/") && !text.contains("/home/"),
            "{name}: a host path would pin the claim to the machine that measured it"
        );
    }
}
