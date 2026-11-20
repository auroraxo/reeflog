//! Pins the measurement of the PUBLISHED v0.7.41 asset: the release that
//! ships the `token_state` completeness fix (5a11643).
//!
//! Why this exists (2026-11-20): v0.7.40 carried the defect — every
//! completeness predicate read a `token_state`-less envelope as a whole
//! one. The fix lived on main only until this release, so the claim that
//! a stranger's install path is fixed must be pinned to THE BYTES a
//! stranger downloads: the linux-amd64 archive whose sha256 equals the
//! digest the Release API publishes (`sha256:0db3083d…`), unpacked and
//! measured with the same instrument as the baseline.
//!
//! What is pinned here:
//!
//!   * the artifact exists, parses as JSON, and names the binary it
//!     measured by sha256 and by the version read from that binary — an
//!     artifact that measured a different binary must not pass as this one;
//!   * the verdict is `token-state-completeness-held` — the published
//!     asset carries the FIX;
//!   * the five baseline-visible defect faces D1–D5 are now
//!     defect-GUARDS on the published asset: `status` counts the half
//!     envelope partial (D1) and the debt counter sees it (D2),
//!     `tail --with-accounts` HOLDS at the row with the repair named
//!     (D3), the accounts pass pays exactly one RPC and stores the
//!     authority (D4), and a recorded outcome leaves the queue while the
//!     halfness stays visible (D5);
//!   * the guard faces G1–G3 still hold: a complete envelope is
//!     delivered, plain `tail` is never held by the accounts queue, and a
//!     recorded outcome is skipped rather than held;
//!   * the shipped binary is a THIRD binary — distinct from the
//!     published v0.7.40 asset (the defect carrier) and from the local
//!     fix-tree build, so the release claim is about the released bytes;
//!   * the artifacts carry no host path from the machine that produced
//!     them.
//!
//! Falsified by construction: change any observed exit code in the
//! artifact and this test goes red; swap the binary sha256 and it goes
//! red; hand the published-v0.7.40 artifact in place of this one and the
//! verdict face goes red.

use std::fs;
use std::path::PathBuf;

fn artifact(name: &str) -> serde_json::Value {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("research")
        .join(name);
    let text = fs::read_to_string(&path).expect("the measurement artifact is readable");
    serde_json::from_str(&text).expect("the artifact is JSON")
}

fn face<'a>(v: &'a serde_json::Value, id: &str) -> &'a serde_json::Value {
    v["faces"]
        .as_array()
        .expect("the artifact carries a face list")
        .iter()
        .find(|f| f["id"].as_str() == Some(id))
        .unwrap_or_else(|| panic!("the artifact must carry the face {id}"))
}

const PUBLISHED_V0741: &str = "token-state-completeness-published-v0741-2026-11-20.json";
const PUBLISHED_V0740: &str = "token-state-completeness-published-v0740-2026-11-20.json";
const FIXED: &str = "token-state-completeness-fixed-tree-2026-11-20.json";

/// sha256 of the UNPACKED binary inside the published linux-amd64
/// archive (the Release API digest of the ARCHIVE is `sha256:0db3083d…`,
/// verified before unpacking).
const PUBLISHED_V0741_SHA: &str =
    "4b990ae266b1321d827c0e6314b4c11791003090a230a20ca1845c435a6d7204";

#[test]
fn the_published_v0741_artifact_names_the_binary_it_measured() {
    let v = artifact(PUBLISHED_V0741);
    assert_eq!(
        v["binarySha256"].as_str(),
        Some(PUBLISHED_V0741_SHA),
        "the artifact must name the published v0.7.41 binary by sha256"
    );
    assert_eq!(
        v["binaryVersion"].as_str(),
        Some("reeflog 0.7.41"),
        "the version must be read from that binary"
    );
    assert_eq!(
        v["tree"].as_str(),
        Some("published-v0.7.41"),
        "the artifact must say it measured the PUBLISHED asset, not the tree"
    );
    assert_eq!(
        v["tool"].as_str(),
        Some("measure-token-state-completeness"),
        "the artifact must name the tool that produced it"
    );
}

#[test]
fn the_published_v0741_asset_carries_the_fix() {
    let v = artifact(PUBLISHED_V0741);
    assert_eq!(
        v["verdict"].as_str(),
        Some("token-state-completeness-held"),
        "the published asset is the one carrying the fix"
    );
    assert_eq!(
        v["summary"]["defectVisible"].as_bool(),
        Some(false),
        "no defect face may be visible on the released bytes"
    );
    assert_eq!(v["summary"]["defectGone"].as_bool(), Some(true));
    assert_eq!(v["summary"]["fixedHolds"].as_bool(), Some(true));
    assert_eq!(v["summary"]["guardsGreen"].as_bool(), Some(true));
    let faces = v["faces"].as_array().expect("face list");
    assert_eq!(faces.len(), 8, "five defect-guards and three guards");
    let guards = faces
        .iter()
        .filter(|f| f["kind"].as_str() == Some("defect-guard"))
        .count();
    assert_eq!(guards, 5, "every baseline defect face is a guard now");
}

#[test]
fn the_published_asset_counts_the_half_envelope() {
    let v = artifact(PUBLISHED_V0741);
    // D1 — the health report.
    assert_eq!(
        face(&v, "D1")["accountsPartial"].as_i64(),
        Some(1),
        "the published report counts a token_state-less envelope as partial"
    );
    assert_eq!(
        face(&v, "D1")["accountsHeld"].as_i64(),
        Some(1),
        "and the row IS held"
    );
    // D2 — the debt counter sees the same debt.
    assert_eq!(
        face(&v, "D2")["accountsMissing"].as_i64(),
        Some(1),
        "the published debt counter notices the missing marker"
    );
}

#[test]
fn the_published_asset_holds_and_names_the_repair() {
    let v = artifact(PUBLISHED_V0741);
    let d3 = face(&v, "D3");
    assert_eq!(
        d3["exitCode"].as_i64(),
        Some(3),
        "the published delivery HOLDS at the half envelope"
    );
    assert_eq!(
        d3["cursorPos"].as_i64(),
        Some(1),
        "and the cursor stays behind it — nothing after it is consumed"
    );
    assert_eq!(
        d3["namedHydrate"].as_bool(),
        Some(true),
        "and the filling command is named"
    );
    let delivered = d3["delivered"].as_array().expect("the delivery list");
    assert_eq!(
        delivered.len(),
        1,
        "only the whole envelope is delivered; the half one is held"
    );
    assert!(
        delivered.iter().all(|s| s.as_str().is_some_and(|s| s.starts_with("SigWhole"))),
        "the half envelope is NOT among the delivered rows"
    );
}

#[test]
fn the_published_asset_pays_for_the_debt_and_stores_the_authority() {
    let v = artifact(PUBLISHED_V0741);
    let d4 = face(&v, "D4");
    assert_eq!(
        d4["rpcCalls"].as_i64(),
        Some(1),
        "the published accounts pass pays exactly one RPC for the debt"
    );
    assert_eq!(
        d4["storedHasTokenState"].as_bool(),
        Some(true),
        "and the replacement carries the event-time authority"
    );
}

#[test]
fn the_published_asset_lets_a_recorded_outcome_leave_the_queue() {
    let v = artifact(PUBLISHED_V0741);
    let d5 = face(&v, "D5");
    assert_eq!(
        d5["accountsMissing"].as_i64(),
        Some(0),
        "a recorded outcome over a half envelope leaves the debt queue"
    );
    assert_eq!(
        d5["accountsPartial"].as_i64(),
        Some(1),
        "but the halfness stays visible in the report — it is not hidden"
    );
}

#[test]
fn the_published_guards_still_hold() {
    let v = artifact(PUBLISHED_V0741);
    // G1 — a complete envelope is delivered and consumed.
    let g1 = face(&v, "G1");
    assert_eq!(g1["exitCode"].as_i64(), Some(0));
    assert_eq!(g1["cursorPos"].as_i64(), Some(1));
    // G2 — plain tail is never held by the accounts queue.
    let g2 = face(&v, "G2");
    assert_eq!(g2["exitCode"].as_i64(), Some(0));
    assert_eq!(
        g2["delivered"].as_array().map(|a| a.len()),
        Some(1),
        "the half envelope still reaches a plain tail reader"
    );
    // G3 — a recorded outcome is skipped, not held.
    let g3 = face(&v, "G3");
    assert_eq!(g3["exitCode"].as_i64(), Some(0));
    assert_eq!(g3["cursorPos"].as_i64(), Some(1));
}

#[test]
fn the_shipped_asset_is_a_third_binary() {
    let defect = artifact(PUBLISHED_V0740);
    let fixed = artifact(FIXED);
    let shipped = artifact(PUBLISHED_V0741);
    let sha = |v: &serde_json::Value| {
        v["binarySha256"]
            .as_str()
            .expect("the artifact names its binary by sha256")
            .to_string()
    };
    let (d, f, s) = (sha(&defect), sha(&fixed), sha(&shipped));
    assert_ne!(d, f, "the defect carrier and the local fix build differ");
    assert_ne!(d, s, "the shipped asset must differ from the defect carrier");
    assert_ne!(f, s, "the shipped asset must differ from the local fix build");
}

#[test]
fn neither_artifact_carries_a_host_path() {
    for name in [PUBLISHED_V0741, FIXED] {
        let text = fs::read_to_string(
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("research")
                .join(name),
        )
        .expect("the artifact is readable");
        for leak in ["/home/", "/tmp/reeflog", "aurora"] {
            assert!(
                !text.contains(leak),
                "the artifact {name} must not carry the host path {leak}"
            );
        }
    }
}
