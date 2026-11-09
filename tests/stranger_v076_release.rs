//! The v0.7.6 stranger artifact must stay a MEASUREMENT, not a claim.
//!
//! Why this test exists (2026-11-09): the defect this release fixes was
//! itself silent — a `--ws`/`--rpc` value outside what the consumer can
//! speak was accepted, and the run looked like a normal one. A stranger
//! measurement about such a defect can rot the same way: if the artifact is
//! edited by hand, or the release is re-created without re-measuring, the
//! numbers keep looking authoritative while describing nothing.
//!
//! So the artifact is pinned here:
//!
//!   * it must exist;
//!   * both release digests must still be the ones the GitHub release API
//!     published for v0.7.6 — a digest that drifted means the asset the
//!     measurement describes is no longer the asset a stranger downloads;
//!   * the linux asset must be the one that says `reeflog 0.7.6`;
//!   * the BASELINE numbers must still say 0/11 on the published v0.7.5
//!     binary — that is a fact about a RELEASED artifact and must not drift;
//!   * the PUBLISHED-ASSET numbers must say every case was refused;
//!   * the empty-app refusal must still name the flag AND the exact value,
//!     and must still leave no journal behind;
//!   * the macOS asset must stay marked as NOT executed — this host cannot
//!     run it, and an artifact that claims otherwise is lying;
//!   * no host path may appear.
//!
//! Falsified by construction: change a digest and it goes red; change 0/11
//! to something else and it goes red; flip `executed` to true and it goes
//! red; add a host path and it goes red.

use std::fs;
use std::path::PathBuf;

fn artifact() -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("research")
        .join("stranger-v0.7.6-release-2026-11-09.json");
    fs::read_to_string(&path)
        .expect("research/stranger-v0.7.6-release-2026-11-09.json is readable")
}

fn field(text: &str, key: &str) -> String {
    let needle = format!("\"{key}\"");
    let start = text
        .find(&needle)
        .unwrap_or_else(|| panic!("the artifact must carry the field {key}"))
        + needle.len();
    let rest = &text[start..];
    let colon = rest.find(':').expect("the field is followed by a colon");
    let rest = &rest[colon + 1..];
    let end = rest
        .find(|c: char| c == ',' || c == '}' || c == '\n')
        .unwrap_or(rest.len());
    rest[..end].trim().trim_matches('"').to_string()
}

#[test]
fn the_artifact_names_the_tag_it_measured() {
    let text = artifact();
    assert_eq!(field(&text, "tag"), "v0.7.6");
    assert_eq!(
        field(&text, "measuredAt"),
        "2026-11-09",
        "the measurement date must not drift"
    );
}

#[test]
fn both_release_digests_still_match_the_github_release_api() {
    // The digests GitHub published for v0.7.6. If a re-release changes an
    // asset, these go red — which is the point: the measurement describes
    // THESE bytes, and a stranger downloads THESE bytes.
    let text = artifact();
    assert!(text.contains("cbe2653416cc71e12ac3df3ed56b8c7cb270270baac60304d65d5af561a4d1e1"),
        "the linux asset digest recorded at release time must not drift");
    assert!(text.contains("c907288ab76bd240e85accfd15877ef3baf82640209ccf4865eb73c2e14d2624"),
        "the macOS asset digest recorded at release time must not drift");
    assert_eq!(
        text.matches("digestMatchesApi\": true").count(),
        2,
        "both assets must be checked against the API digest"
    );
}

#[test]
fn the_linux_asset_reports_the_released_version() {
    let text = artifact();
    assert_eq!(field(&text, "version"), "reeflog 0.7.6");
}

#[test]
fn the_baseline_still_says_the_published_v075_binary_refused_nothing() {
    // The defect, pinned: on the published v0.7.5 binary the URL flags had
    // no domain, so NO case was refused by the parser. If this number
    // changes, the artifact no longer describes the released baseline.
    let text = artifact();
    assert!(
        text.contains("0/11 refusals"),
        "the v0.7.5 baseline refused 0/11 — that is the defect: {text}"
    );
}

#[test]
fn the_published_asset_refused_every_measured_case() {
    let text = artifact();
    assert!(
        text.contains("11/11 refusals"),
        "the published v0.7.6 asset must refuse every measured case"
    );
    assert!(
        text.contains("6/6 guards"),
        "every value the consumers CAN speak must still be accepted by the published asset"
    );
    assert!(
        text.contains("0 killed"),
        "no case may hang on the published asset"
    );
}

#[test]
fn the_empty_app_refusal_names_the_flag_and_the_exact_value() {
    // The stranger's own install: the refusal must be reeflog's words, not
    // the transport's, and the value must be echoed as written.
    let text = artifact();
    assert!(
        text.contains("flag --ws expects a ws or wss URL"),
        "the refusal must come from the parser and name the flag"
    );
    assert!(
        text.contains("got `https://api.mainnet-beta.solana.com`"),
        "the refusal must echo the exact value the stranger typed"
    );
    assert_eq!(
        field(&text, "journalLeftBehind"),
        "false",
        "a refused URL must not leave a journal behind"
    );
    assert_eq!(
        field(&text, "installedVersion"),
        "reeflog 0.7.6",
        "the empty-app install must have produced the released version"
    );
}

#[test]
fn the_macos_asset_was_integrity_checked_but_not_executed() {
    // This host is linux-x64. An artifact claiming the darwin asset ran
    // would be describing a measurement that never happened.
    let text = artifact();
    let start = text.find("\"darwin\":").expect("the artifact has a darwin block");
    let darwin = &text[start..];
    assert!(
        darwin.contains("\"executed\": false"),
        "the macOS asset cannot have been executed on this host: {darwin}"
    );
    assert!(
        darwin.contains("Mach-O 64-bit arm64 executable"),
        "the macOS asset must be identified as a Mach-O arm64 binary"
    );
    assert!(
        darwin.contains("\"containsVersion\": \"0.7.6\""),
        "the macOS asset must carry the released version string"
    );
}

#[test]
fn the_artifact_carries_no_host_path() {
    let text = artifact();
    for needle in ["/home/", "/tmp/", "/Users/", "C:\\"] {
        assert!(
            !text.contains(needle),
            "a committed measurement must not carry the path of the machine that produced it ({needle})"
        );
    }
    assert!(
        text.contains("\"hostPathInArtifact\": false"),
        "the artifact must record that it carries no host path"
    );
}
