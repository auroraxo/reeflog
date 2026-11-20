//! Pins the measurement of the PUBLISHED v0.7.40 asset: the envelope
//! completeness predicates never learned `token_state`, so an account
//! envelope WITHOUT the event-time token authority was presented as a
//! WHOLE envelope — the third occurrence of the same class (after the
//! instruction bytes and the account-list provenance).
//!
//! Why this exists (2026-11-20): the fix was measured against the
//! published v0.7.40 linux asset (the archive whose sha256 equals the
//! digest the Release API publishes) and the fix itself sat on main only.
//! The v0.7.40 asset itself is the artifact that must carry the defect,
//! because that is the released behaviour.
//!
//! What is pinned here:
//!
//!   * the artifact exists, parses as JSON, and names the binary it
//!     measured by sha256 and by the version read from that binary — an
//!     artifact that measured a different binary must not pass as this one;
//!   * the verdict is `token-state-completeness-blind` — the published
//!     asset carries the defect;
//!   * the four baseline-visible defect faces D1–D4 are VIOLATED on the
//!     published asset: `status` calls a half envelope whole (D1), the
//!     debt counter agrees (D2), `tail --with-accounts` DELIVERS and
//!     CONSUMES it (D3), and the accounts pass never spends an RPC on
//!     the debt (D4);
//!   * the guard faces G1–G3 are NOT violated on the published asset: a
//!     complete envelope is delivered and consumed, plain `tail` is never
//!     held, and a recorded outcome is skipped rather than held;
//!   * the FIXED tree is green on the same faces — the fix is measured,
//!     not asserted;
//!   * the artifacts carry no host path from the machine that produced
//!     them.
//!
//! Falsified by construction: change any observed exit code in the
//! artifact and this test goes red; swap the binary sha256 and it goes
//! red; turn a guard face red and it goes red.

use std::fs;
use std::path::PathBuf;

fn artifact(name: &str) -> serde_json::Value {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("research")
        .join(name);
    let text = fs::read_to_string(&path).expect("the measurement artifact is readable");
    serde_json::from_str(&text).expect("the artifact is JSON")
}

const PUBLISHED: &str = "token-state-completeness-published-v0740-2026-11-20.json";
const FIXED: &str = "token-state-completeness-fixed-tree-2026-11-20.json";

fn face<'a>(v: &'a serde_json::Value, id: &str) -> &'a serde_json::Value {
    v["faces"]
        .as_array()
        .expect("the artifact carries a face list")
        .iter()
        .find(|f| f["id"].as_str() == Some(id))
        .unwrap_or_else(|| panic!("the artifact must carry the face {id}"))
}

const PUBLISHED_SHA: &str = "a332e39ce7a9de658827fbec49fd4dedc93bd4349bc2abb71dda2001d6fc4f1b";

#[test]
fn the_published_artifact_names_the_binary_it_measured() {
    let v = artifact(PUBLISHED);
    assert_eq!(
        v["binarySha256"].as_str(),
        Some(PUBLISHED_SHA),
        "the artifact must name the published v0.7.40 binary by sha256"
    );
    assert_eq!(
        v["binaryVersion"].as_str(),
        Some("reeflog 0.7.40"),
        "the version must be read from that binary"
    );
    assert_eq!(
        v["tree"].as_str(),
        Some("published-v0.7.40"),
        "the artifact must say it measured the PUBLISHED asset, not the tree"
    );
    assert_eq!(
        v["tool"].as_str(),
        Some("measure-token-state-completeness"),
        "the artifact must name the tool that produced it"
    );
}

#[test]
fn the_published_v0740_asset_carries_the_defect() {
    let v = artifact(PUBLISHED);
    assert_eq!(
        v["verdict"].as_str(),
        Some("token-state-completeness-blind"),
        "the published asset is the one carrying the defect"
    );
    assert_eq!(
        v["summary"]["defectVisible"].as_bool(),
        Some(true),
        "the artifact must record that all four defect faces were visible"
    );
}

#[test]
fn the_published_asset_calls_a_half_envelope_whole() {
    let v = artifact(PUBLISHED);
    // D1 — the health report: a half envelope is NOT counted as partial.
    assert_eq!(
        face(&v, "D1")["accountsPartial"].as_i64(),
        Some(0),
        "the published report counts a token_state-less envelope as whole"
    );
    assert_eq!(
        face(&v, "D1")["accountsHeld"].as_i64(),
        Some(1),
        "and the row IS held — it is only the MARKER it is blind to"
    );
    // D2 — the debt counter: the same blindness in the queue's twin.
    assert_eq!(
        face(&v, "D2")["accountsMissing"].as_i64(),
        Some(0),
        "the published debt counter never notices the missing marker"
    );
}

#[test]
fn the_published_asset_delivers_and_consumes_a_half_envelope() {
    let v = artifact(PUBLISHED);
    let d3 = face(&v, "D3");
    assert_eq!(
        d3["exitCode"].as_i64(),
        Some(0),
        "the published delivery exits 0 over a half envelope"
    );
    assert_eq!(
        d3["cursorPos"].as_i64(),
        Some(2),
        "and the cursor advances PAST it — the row is consumed as delivered"
    );
    assert_eq!(
        d3["namedHydrate"].as_bool(),
        Some(false),
        "and no filling command is named, because nothing is owed"
    );
    let delivered = d3["delivered"].as_array().expect("the delivery list");
    assert_eq!(
        delivered.len(),
        2,
        "both the whole and the half envelope are delivered"
    );
    assert!(
        delivered.iter().any(|s| {
            s.as_str()
                .is_some_and(|s| s.starts_with("SigHalf"))
        }),
        "the half envelope is among them"
    );
}

#[test]
fn the_published_asset_never_pays_for_the_debt() {
    let v = artifact(PUBLISHED);
    let d4 = face(&v, "D4");
    assert_eq!(
        d4["rpcCalls"].as_i64(),
        Some(0),
        "the published accounts pass spends NO RPC — the debt is invisible to it"
    );
    assert_eq!(
        d4["storedHasTokenState"].as_bool(),
        Some(false),
        "and the stored envelope stays without the event-time authority"
    );
}

#[test]
fn the_published_guards_still_hold() {
    let v = artifact(PUBLISHED);
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
        "the half envelope still reaches a plain tail reader — the accounts
        queue is the only place it belongs"
    );
    // G3 — a recorded outcome is skipped, not held.
    let g3 = face(&v, "G3");
    assert_eq!(g3["exitCode"].as_i64(), Some(0));
    assert_eq!(g3["cursorPos"].as_i64(), Some(1));
}

#[test]
fn the_fixed_tree_is_green_on_the_same_faces() {
    let v = artifact(FIXED);
    assert_eq!(
        v["verdict"].as_str(),
        Some("token-state-completeness-held"),
        "the fixed tree must hold every face"
    );
    assert_eq!(v["summary"]["fixedHolds"].as_bool(), Some(true));
    assert_eq!(v["summary"]["guardsGreen"].as_bool(), Some(true));
    assert_eq!(
        v["summary"]["defectVisible"].as_bool(),
        Some(false),
        "no defect face may be visible on the fixed tree"
    );
}

#[test]
fn the_fixed_tree_counts_the_half_envelope_and_refills_it() {
    let v = artifact(FIXED);
    assert_eq!(
        face(&v, "D1")["accountsPartial"].as_i64(),
        Some(1),
        "the fixed report counts a half envelope as partial"
    );
    assert_eq!(
        face(&v, "D2")["accountsMissing"].as_i64(),
        Some(1),
        "the fixed debt counter sees the same debt"
    );
    let d3 = face(&v, "D3");
    assert_eq!(
        d3["exitCode"].as_i64(),
        Some(3),
        "the fixed delivery holds at the half envelope"
    );
    assert_eq!(
        d3["cursorPos"].as_i64(),
        Some(1),
        "and the cursor stays behind it"
    );
    assert_eq!(
        d3["namedHydrate"].as_bool(),
        Some(true),
        "and the filling command is named"
    );
    let d4 = face(&v, "D4");
    assert_eq!(
        d4["rpcCalls"].as_i64(),
        Some(1),
        "the fixed accounts pass pays exactly one RPC for the debt"
    );
    assert_eq!(
        d4["storedHasTokenState"].as_bool(),
        Some(true),
        "and the replacement carries the event-time authority"
    );
}

#[test]
fn the_fixed_tree_does_not_forever_refetch_a_final_half_envelope() {
    let v = artifact(FIXED);
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
fn neither_artifact_carries_a_host_path() {
    for name in [PUBLISHED, FIXED] {
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
