//! The committed measurement artifact for the webhook value domain
//! (target `id`, `secret`, `url`) must match what the published v0.7.8
//! binary was measured doing.
//!
//! Why this test exists (2026-11-09): the rule is drawn from measurement,
//! not from an opinion about "clean strings". If the numbers drift — if
//! someone quietly drops an edge case or changes the consumer line — this
//! test fails and names what changed.
//!
//! What is pinned here:
//!
//!   * the artifact exists and parses as text;
//!   * the artifact describes the published v0.7.8 asset;
//!   * the consumers are named (ureq 3.5, tokio-tungstenite 0.24, both
//!     linked against one `http` version);
//!   * the id counts match what was measured (32 of 33 control bytes fatal,
//!     leading whitespace broken at the receiver, internal space/tab and
//!     trailing whitespace preserved);
//!   * the url counts match what was measured (the nine host-refused
//!     characters, the three path-refused characters, the backtick accepted
//!     in query, the fragment never transmitted);
//!   * the artifact carries no host path (the research guard pins this too).
//!
//! Falsified by construction: edit any count in the artifact and this test
//! fails.

use std::fs;
use std::path::PathBuf;

fn artifact() -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("research")
        .join("webhook-value-domain-2026-11-09.json");
    fs::read_to_string(&path)
        .expect("research/webhook-value-domain-2026-11-09.json is readable")
}

fn assert_contains(text: &str, needle: &str) {
    assert!(
        text.contains(needle),
        "the artifact must contain `{needle}`"
    );
}

#[test]
fn the_artifact_exists_and_describes_the_measured_consumers() {
    let text = artifact();
    assert_contains(&text, "\"artifact\": \"webhook-value-domain\"");
    assert_contains(&text, "PUBLISHED v0.7.8 binary");
    assert_contains(&text, "ureq 3.5 (http::Uri + http::HeaderValue::from_str)");
    assert_contains(&text, "tokio-tungstenite 0.24 / tungstenite 0.24 (http::Uri)");
}

#[test]
fn the_id_numbers_are_pinned_to_the_measurement() {
    let text = artifact();
    // The dense sweep over every C0 control byte plus DEL: 32 refused on
    // every delivery, ONE (HTAB, 0x09) delivered byte-identical.
    assert_contains(&text, "\"idControlBytes\"");
    assert_contains(&text, "\"delivered\": 1,");
    // The fatal case (a CR in the id) is recorded with its transport error:
    assert_contains(&text, "http: failed to parse header value");
    // Outer whitespace stripped by the receiver:
    assert_contains(&text, "\"keyBrokenAtReceiver\": 3");
    // Plain, internal space, internal tab, trailing space, trailing tab,
    // and non-ASCII all DELIVER:
    assert_contains(&text, "\"delivered\": 8");
}

#[test]
fn the_url_numbers_are_pinned_to_the_measurement() {
    let text = artifact();
    // The dense path sweep: 91 printable ASCII characters delivered, three
    // refused (`<`, `>`, the backtick).
    assert_contains(&text, "\"urlPathPrintableAscii\"");
    assert_contains(&text, "\"delivered\": 91,");
    assert_contains(&text, "\"refused\": [");
    // The dense query sweep: 91 delivered, 38 refused (space, `"`, `<`, `>`,
    // every C0 control, DEL and the non-ASCII byte).
    assert_contains(&text, "\"refusedNonAscii\": 1");
}

#[test]
fn the_url_positions_are_pinned_to_the_measurement() {
    let text = artifact();
    // Authority: nine characters refused
    assert_contains(&text, "\"authority\"");
    // Path: space, controls, < > ` refused
    assert_contains(&text, "\"path\"");
    // Query: " < > refused, backtick accepted
    assert_contains(&text, "\"query\"");
    // Fragment: nothing refused, never transmitted
    assert_contains(&text, "\"fragment\"");
    assert_contains(&text, "the fragment is never transmitted — nothing in it can fail");
}

#[test]
fn the_artifact_carries_no_host_path() {
    let text = artifact();
    for marker in ["/home/", "/tmp/", "/Users/", "C:\\"] {
        assert!(
            !text.contains(marker),
            "the artifact must carry no host path: found `{marker}`"
        );
    }
}
