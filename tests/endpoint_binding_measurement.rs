//! Pins the endpoint-binding measurement: the same probe, run against the
//! PUBLISHED v0.7.14 release asset and against this tree.
//!
//! Why this exists (2026-11-11): the delivery state of a webhook target was
//! keyed by the target id alone, so a repointed endpoint inherited the old
//! consumer's cursors and receipts and starved behind a green report
//! (`delivered: 0, blocked: false`, exit 0). The defect lived IN the
//! published release — a fix that only lives on the branch is the defect
//! class this repository has measured repeatedly — so the RELEASE is
//! measured, not the build.
//!
//! What is pinned:
//!
//!   * the published v0.7.14 asset STARVES the repointed consumer: receiver
//!     A holds all three events, receiver B holds none, and the repointed
//!     push still exits 0 (a measurement whose baseline is clean certifies
//!     nothing);
//!   * this tree REFUSES the same pass: exit 1, both endpoints named in the
//!     refusal, and receiver B still holds nothing;
//!   * the fixed side is labelled as the tree build it was — not as a
//!     release;
//!   * the artifacts carry no host path.
//!
//! Falsified by construction: change any count, claim another version was
//! measured, or inject a machine path, and these tests fail.

use std::fs;
use std::path::PathBuf;

fn artifact(name: &str) -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("research")
        .join(name);
    fs::read_to_string(&path).unwrap_or_else(|e| panic!("research/{name} is readable: {e}"))
}

fn assert_contains(text: &str, needle: &str, why: &str) {
    assert!(text.contains(needle), "the artifact must contain `{needle}` — {why}");
}

fn assert_not_contains(text: &str, needle: &str, why: &str) {
    assert!(
        !text.contains(needle),
        "the artifact must NOT contain `{needle}` — {why}"
    );
}

#[test]
fn both_artifacts_declare_the_tool_and_the_method() {
    for name in [
        "endpoint-binding-published-v0.7.14-2026-11-11.json",
        "endpoint-binding-published-v0.7.15-2026-11-11.json",
        "endpoint-binding-fixed-2026-11-11.json",
    ] {
        let text = artifact(name);
        assert_contains(
            &text,
            "\"artifact\": \"endpoint-binding\"",
            "the artifact declares which measurement it is",
        );
        assert_contains(
            &text,
            "\"method\":",
            "the artifact declares how it was measured",
        );
    }
}

#[test]
fn the_published_v0714_asset_starves_the_repointed_consumer() {
    let text = artifact("endpoint-binding-published-v0.7.14-2026-11-11.json");
    assert_contains(
        &text,
        "\"source\": \"published v0.7.14 release asset",
        "the baseline is the PUBLISHED v0.7.14 release asset, not the tree",
    );
    assert_contains(
        &text,
        "9b6c30348f854af2acbd78ccbb474ae0837db1f34bd806626a7e2796bfe2611e",
        "the published v0.7.14 archive sha256 equals the Release-API digest",
    );
    assert_contains(
        &text,
        "\"receiverACount\": 3",
        "receiver A holds all three events",
    );
    assert_contains(
        &text,
        "\"receiverBCount\": 0",
        "receiver B holds NOTHING — the repointed consumer starved",
    );
    // The silence came with a green report: that is the whole defect.
    assert_contains(
        &text,
        "\"exit\": 0",
        "the repointed push exited 0 on the published asset — the starvation was silent",
    );
    assert_contains(
        &text,
        "\"verdict\": \"STARVED\"",
        "the artifact's verdict for the published asset",
    );
}

#[test]
fn the_published_v0715_asset_refuses_the_repointed_consumer() {
    // The fix is on the TAG a stranger installs: the release asset must
    // refuse where the published v0.7.14 asset starved.
    let text = artifact("endpoint-binding-published-v0.7.15-2026-11-11.json");
    assert_contains(
        &text,
        "\"source\": \"published v0.7.15 release asset",
        "the measured seat is the PUBLISHED v0.7.15 release asset, not the tree",
    );
    assert_contains(
        &text,
        "8f941253e82f5c1aa594c8b5253d6c603d29ffbf2f5ec7e25f8fbf94d16f5f35",
        "the published v0.7.15 archive sha256 equals the Release-API digest",
    );
    assert_contains(&text, "\"binaryVersion\": \"reeflog 0.7.15\"", "the measured binary's version");
    assert_contains(
        &text,
        "\"receiverACount\": 3",
        "the first endpoint still received all three events",
    );
    assert_contains(
        &text,
        "\"receiverBCount\": 0",
        "the refused pass sent nothing to the new endpoint",
    );
    assert_contains(
        &text,
        "\"exit\": 1",
        "the repointed push is REFUSED, not silently successful",
    );
    assert_contains(
        &text,
        "is bound to endpoint",
        "the refusal names the recorded endpoint",
    );
    assert_contains(
        &text,
        "give the new endpoint its own target id",
        "the refusal names the repair",
    );
}

#[test]
fn this_tree_refuses_the_repointed_consumer() {
    let text = artifact("endpoint-binding-fixed-2026-11-11.json");
    assert_contains(
        &text,
        "\"source\": \"tree build",
        "the fixed side is labelled as the tree build it was",
    );
    assert_contains(
        &text,
        "\"receiverACount\": 3",
        "the first endpoint still received all three events — the guard holds",
    );
    assert_contains(
        &text,
        "\"receiverBCount\": 0",
        "the refused pass sent nothing to the new endpoint",
    );
    // The refusal is loud, and it names BOTH sides and the target id.
    assert_contains(
        &text,
        "\"exit\": 1",
        "the repointed push is refused with exit 1",
    );
    assert_contains(
        &text,
        "is bound to endpoint",
        "the refusal names the recorded endpoint",
    );
    assert_contains(
        &text,
        "but this config points it at",
        "the refusal names the configured endpoint too",
    );
    assert_contains(
        &text,
        "give the new endpoint its own target id",
        "the refusal names the repair",
    );
}

#[test]
fn neither_artifact_carries_a_host_path() {
    for name in [
        "endpoint-binding-published-v0.7.14-2026-11-11.json",
        "endpoint-binding-published-v0.7.15-2026-11-11.json",
        "endpoint-binding-fixed-2026-11-11.json",
    ] {
        let text = artifact(name);
        assert_not_contains(&text, "/home/", "a machine path must not leak into a committed artifact");
        assert_not_contains(&text, "/tmp/", "a scratch path must not leak into a committed artifact");
        assert_not_contains(&text, "github_repos", "a machine path must not leak into a committed artifact");
    }
}
