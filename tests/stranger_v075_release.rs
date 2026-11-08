//! The v0.7.5 stranger artifact must stay a MEASUREMENT, not a claim.
//!
//! Why this test exists (2026-11-08): the fix this work shipped is only
//! worth something if a STRANGER — someone who installs the published
//! release, not someone who runs the checkout — gets it. The artifact
//! records that journey: the release assets against the digests the
//! GitHub API publishes, the empty-app `npm install` + `bootstrap()`, and
//! the defect cases run against the DOWNLOADED binary.
//!
//! So the artifact is pinned here:
//!
//!   * it must exist and parse;
//!   * both asset digests must match the digests the release API
//!     published — a download that does not match the API is a corrupted
//!     download, not a stranger's install;
//!   * the linux asset must be the one that says `reeflog 0.7.5`;
//!   * the macOS asset must be integrity-checked ONLY (this host is
//!     linux-x64) — a test that claims the arm64 binary was executed
//!     would be a claim, not a measurement;
//!   * the published asset must refuse every out-of-domain value and the
//!     published v0.7.4 must refuse NONE of them — that difference IS
//!     the delivery;
//!   * the refusal must name the key AND the exact value, so the value
//!     must appear in the recorded stderr without f64 rounding;
//!   * the artifact must not contain a host path (the lesson from sememe's
//!     declared-origin artifact, which went red in CI because it carried
//!     the author's host path).
//!
//! Falsified by construction: change one digest and this test goes red;
//! delete the macOS "not executed" note and it goes red; add a host path
//! and it goes red.

use std::fs;
use std::path::PathBuf;

fn artifact() -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("research")
        .join("stranger-v0.7.5-release-2026-11-08.json");
    fs::read_to_string(&path).expect("research/stranger-v0.7.5-release-2026-11-08.json is readable")
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
fn both_release_assets_match_the_digests_the_api_published() {
    let text = artifact();
    // The digests the GitHub release API published for v0.7.5.
    for digest in [
        "8d38b9a8aef36501120903b6215fa27ee93d4b103f1d9ffb214a06d9e770b09f",
        "f5420c9c6e2e3eb697200c6bbb8b994160bd4d97b642fe19365425e6b20253b5",
    ] {
        assert!(
            text.contains(digest),
            "the artifact must carry the published digest {digest}"
        );
    }
    assert_eq!(
        field(&text, "digestMatchesApi"),
        "true",
        "the downloaded asset must match the digest the release API published"
    );
}

#[test]
fn the_linux_asset_is_the_version_under_claim() {
    let text = artifact();
    assert!(
        text.contains("reeflog 0.7.5"),
        "the stranger must have installed the version under claim"
    );
}

#[test]
fn the_macos_asset_is_integrity_checked_only() {
    let text = artifact();
    assert!(
        text.contains("\"executed\": false"),
        "the macOS asset was NOT executed on this host — the artifact must say so"
    );
    assert!(
        text.contains("Mach-O 64-bit arm64"),
        "the macOS asset must be recorded as an arm64 Mach-O binary"
    );
}

#[test]
fn the_published_asset_refuses_what_the_published_v074_accepted() {
    let text = artifact();
    assert!(
        text.contains("0/11 refusals"),
        "the baseline (published v0.7.4) must still refuse none of the cases"
    );
    assert!(
        text.contains("11/11 refusals"),
        "the published v0.7.5 asset must refuse every case"
    );
    assert!(
        text.contains("0 killed"),
        "no case may hang on the published asset"
    );
}

#[test]
fn the_refusal_names_the_key_and_the_exact_value() {
    let text = artifact();
    assert!(
        text.contains("timeout_ms must not exceed"),
        "the recorded refusal must name the key and its domain"
    );
    assert!(
        text.contains("18446744073709551615"),
        "the recorded refusal must echo the value EXACTLY as written"
    );
    // The rounded form must not appear: a measurement that lost the value
    // is the defect this whole work fixed.
    for rounded in ["18446744073709552000", "1.8446744073709552e+19"] {
        assert!(
            !text.contains(rounded),
            "the artifact must not carry the f64-rounded form {rounded}"
        );
    }
}

#[test]
fn the_artifact_carries_no_host_path() {
    let text = artifact();
    for needle in ["/home/", "/Users/", "C:\\", "/tmp/"] {
        assert!(
            !text.contains(needle),
            "a committed measurement must not carry the path of the machine that produced it ({needle})"
        );
    }
}
