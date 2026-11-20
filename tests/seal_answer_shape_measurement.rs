//! The seal-answer-shape measurement, pinned on both sides.
//!
//! PULS BC, 2026-11-20. `tools/measure-seal-answer-shape.mjs` — two real
//! binaries against one local chain sink that answers floor(n/2) of every
//! `getSignatureStatuses` batch:
//!
//! * The BASELINE side is the PUBLISHED v0.7.42 linux-amd64 release asset
//!   (downloaded from the GitHub release API, archive digest
//!   `4c183352723e04827759947c7e5f34c8211db1252659b14be4aa067b91800807`,
//!   unpacked binary sha256 `6f20fa89…` = `reeflog 0.7.42`) — i.e. the bytes
//!   a stranger installs. Verdict `seal-answer-covers-request` with 3 DEFECT
//!   FACES: the seal pass zipped the pending rows with the answer
//!   unconditionally, so a truncated answer sealed the rows it covered
//!   (5 of 10) and moved the walk cursor past the 5 rows it never asked —
//!   and the pass still reported `complete: true` at exit 0. The daemon
//!   tick sealed 7 of 10 and exited 0 without naming the incomplete pass.
//!
//! * The FIXED side is this tree (0 defect faces): a short answer is a
//!   failed source — nothing is sealed, nothing is consumed, the pass
//!   reports `complete: false` and exits 3, and the daemon tick names the
//!   incomplete pass on stderr.
//!
//! The pin holds the MEASURED numbers on both sides — the baseline side
//! must still carry the exact stall it recorded AND the exact published
//! bytes it was measured on (sha256 + version), and the fixed side must
//! still carry the honest refusal it claims.

use std::path::PathBuf;

fn artifact(name: &str) -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("research")
        .join(name);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

fn json(name: &str) -> serde_json::Value {
    serde_json::from_str(&artifact(name)).expect("the artifact is JSON")
}

const ARTIFACT: &str = "seal-answer-shape-2026-11-20.json";

/// The UNPACKED binary inside the PUBLISHED v0.7.42 linux-amd64 archive
/// (the archive's own Release API digest is `sha256:4c183352…`, verified
/// before unpacking).
const PUBLISHED_V0742_SHA: &str =
    "6f20fa8988af4231eac87d555c97a077c0e420157cec51773217a9d0bb991c72";

#[test]
fn the_baseline_side_names_the_published_bytes_it_measured() {
    let v = json(ARTIFACT);
    let baseline = &v["trees"]["baseline"];
    assert_eq!(
        baseline["binSha256"].as_str(),
        Some(PUBLISHED_V0742_SHA),
        "the baseline side must be the PUBLISHED v0.7.42 binary, named by sha256"
    );
    assert_eq!(
        baseline["binVersion"].as_str(),
        Some("reeflog 0.7.42"),
        "the baseline version must be read from that binary"
    );
    let source = baseline["source"].as_str().expect("the baseline names its source");
    assert!(
        source.contains("PUBLISHED v0.7.42"),
        "the baseline must say it measured the published asset, not a local rebuild"
    );
    assert!(
        source.contains("4c183352723e04827759947c7e5f34c8211db1252659b14be4aa067b91800807"),
        "the baseline must name the archive digest the Release API publishes"
    );
}

#[test]
fn the_baseline_side_still_carries_the_measured_stall() {
    let v = json(ARTIFACT);
    let baseline = &v["trees"]["baseline"];
    let d1 = &baseline["faces"]["D1_cli_short_answer"];
    assert_eq!(d1["defect"].as_bool(), Some(true), "D1 must stay a defect face");
    assert_eq!(
        d1["sealed"].as_i64(),
        Some(5),
        "the published binary sealed the 5 rows the truncated answer covered"
    );
    assert_eq!(
        d1["unsealed"].as_i64(),
        Some(5),
        "and left the 5 rows it never asked pending"
    );
    assert_eq!(
        d1["exit"].as_i64(),
        Some(0),
        "while exiting 0 over them"
    );
    assert_eq!(
        d1["complete"].as_bool(),
        Some(true),
        "while claiming the pass was complete"
    );

    let d2 = &baseline["faces"]["D2_daemon_short_answer"];
    assert_eq!(d2["defect"].as_bool(), Some(true));
    assert!(
        d2["sealed"].as_i64().unwrap_or(0) > 0,
        "the published daemon tick sealed rows a truncated answer never covered"
    );
    assert_eq!(
        d2["names_incomplete_on_stderr"].as_bool(),
        Some(false),
        "and it did not name the incomplete pass"
    );

    let d3 = &baseline["faces"]["D3_missing_behind_truncation"];
    assert_eq!(d3["defect"].as_bool(), Some(true));
    assert_eq!(
        d3["missing"].as_i64(),
        Some(5),
        "the covered half was promoted `missing` while the rest was dropped"
    );
    assert_eq!(d3["unsealed"].as_i64(), Some(5));
    assert_eq!(d3["exit"].as_i64(), Some(0));
}

#[test]
fn the_fixed_side_refuses_to_look_done() {
    let v = json(ARTIFACT);
    let fixed = &v["trees"]["fixed"];
    for (face, key) in [
        ("D1_cli_short_answer", "D1_cli_short_answer"),
        ("D3_missing_behind_truncation", "D3_missing_behind_truncation"),
    ] {
        let f = &fixed["faces"][key];
        assert_eq!(
            f["defect"].as_bool(),
            Some(false),
            "{face} must not be a defect face after the fix"
        );
        assert_eq!(
            f["sealed"].as_i64(),
            Some(0),
            "{face}: a truncated answer seals nothing"
        );
        assert_eq!(
            f["exit"].as_i64(),
            Some(3),
            "{face}: an unfinished pass exits 3"
        );
        assert_eq!(
            f["complete"].as_bool(),
            Some(false),
            "{face}: and says so"
        );
    }
    let d2 = &fixed["faces"]["D2_daemon_short_answer"];
    assert_eq!(d2["defect"].as_bool(), Some(false));
    assert_eq!(d2["sealed"].as_i64(), Some(0));
    assert_eq!(
        d2["names_incomplete_on_stderr"].as_bool(),
        Some(true),
        "the daemon tick names the incomplete pass"
    );
}

#[test]
fn both_trees_keep_their_guards() {
    let v = json(ARTIFACT);
    for tree in ["baseline", "fixed"] {
        let t = &v["trees"][tree];
        let guards = t["guards"].as_object().expect("guards");
        assert_eq!(guards.len(), 4, "{tree}: four guards");
        for (name, g) in guards {
            assert_eq!(
                g["pass"].as_bool(),
                Some(true),
                "{tree}: guard {name} must hold on both trees"
            );
        }
    }
}

#[test]
fn the_fixed_side_describes_the_release_this_tree_is() {
    let v = json(ARTIFACT);
    let fixed = &v["trees"]["fixed"];
    let cargo = std::fs::read_to_string(
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml"),
    )
    .expect("Cargo.toml");
    let version = cargo
        .lines()
        .find_map(|l| l.strip_prefix("version = \""))
        .and_then(|l| l.split('"').next())
        .expect("the crate version");
    assert_eq!(
        fixed["binVersion"].as_str(),
        Some(format!("reeflog {version}").as_str()),
        "the fixed side must be the binary of THIS tree (the crate version)"
    );
    let source = fixed["source"].as_str().expect("the fixed side names its source");
    assert!(
        source.contains("this tree"),
        "the fixed side must say it measured this tree"
    );
}

#[test]
fn the_artifact_names_the_verdict_and_both_digests() {
    let v = json(ARTIFACT);
    assert_eq!(
        v["verdict"].as_str(),
        Some("seal-answer-covers-request"),
        "the verdict must be the one the tool computes"
    );
    assert_eq!(v["baselineDefectFaces"].as_i64(), Some(3));
    assert_eq!(v["fixedDefectFaces"].as_i64(), Some(0));
    assert_eq!(v["baselineGuards"].as_str(), Some("all pass"));
    assert_eq!(v["fixedGuards"].as_str(), Some("all pass"));
    let defect = v["defect"].as_str().expect("the artifact states the defect");
    assert!(
        defect.contains("complete: true"),
        "the defect statement must name the lie the pass told"
    );
}

#[test]
fn the_artifact_carries_no_host_paths() {
    let text = artifact(ARTIFACT);
    for needle in ["/home/", "/tmp/", "C:\\\\", "/Users/"] {
        assert!(
            !text.contains(needle),
            "the artifact must not carry the host path {needle:?}"
        );
    }
}
