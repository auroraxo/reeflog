//! The webhook redirect measurement, pinned on both sides.
//!
//! PULS AS, 2026-11-19. `tools/measure-webhook-redirect.mjs` — one live
//! two-sink probe (the configured endpoint answers 302 pointing at a second
//! local sink; both record every request with its method, body bytes and
//! signature header):
//!
//! * The BASELINE side is the release binary of v0.7.35 as shipped (binary
//!   sha256 df2ea627dbd03a7d…, verdict
//!   `webhook-redirect-followed-and-recorded-delivered`, 5 defect faces):
//!   the configured endpoint answered 302, the binary FOLLOWED the
//!   redirect — the hop received a bodyless GET still carrying the
//!   signature header, the journal recorded the event delivered (attempt
//!   status 200 = the hop's own answer), the cursor advanced past the seq
//!   and the exit code was 0. The configured consumer never received the
//!   event and it could never be re-delivered.
//!
//! * The FIXED side is this tree (verdict
//!   `webhook-redirect-refused-at-the-configured-endpoint`, 0 defect
//!   faces): the endpoint receives exactly one request, the redirect is
//!   never followed, the attempt records the endpoint's OWN 302, the
//!   report names the mechanism and the repair, the cursor stays behind
//!   the event, and the exit code holds (3). The guards (200 delivers /
//!   404 refused / 500 transient) hold on BOTH sides unchanged.
//!
//! The pin holds the MEASURED numbers, not a summary: the baseline side of
//! the artifact must still carry the redirect-following it recorded, so a
//! later tree cannot quietly make the defect look smaller than it was, and
//! the fixed side must still carry the refusal it claims.

use std::path::PathBuf;

fn artifact(name: &str) -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("research")
        .join(name);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

fn assert_contains(text: &str, needle: &str, what: &str) {
    assert!(
        text.contains(needle),
        "the artifact must state {what}: missing {needle:?}"
    );
}

#[test]
fn the_artifact_names_the_verdict_and_both_sides() {
    let text = artifact("webhook-redirect-two-tree-2026-11-19.json");
    assert_contains(
        &text,
        "\"verdict\": \"webhook-redirect-refused-at-the-configured-endpoint\"",
        "the verdict",
    );
    assert_contains(&text, "\"baselineDefectFaces\": 5", "the baseline defect count");
    assert_contains(&text, "\"fixedDefectFaces\": 0", "the fixed defect count");
    assert_contains(
        &text,
        "\"binVersion\": \"reeflog 0.7.35\"",
        "the measured binaries' own version",
    );
    // The binaries are named by hash, never by host path.
    assert_contains(
        &text,
        "\"binSha256\": \"df2ea627",
        "the baseline binary's sha256 (v0.7.35 as released)",
    );
}

#[test]
fn the_artifact_carries_the_baseline_defect_numbers() {
    let text = artifact("webhook-redirect-two-tree-2026-11-19.json");
    // The baseline FOLLOWED the redirect: the hop was hit, the delivery was
    // recorded, the cursor moved. A later tree must not be able to make the
    // defect smaller than it was.
    assert_contains(
        &text,
        "\"reportDelivered\": 1",
        "the baseline recording a redirect as a delivery",
    );
    assert_contains(
        &text,
        "\"attemptStatus\": 200",
        "the baseline recording the hop's own answer as the verdict",
    );
    assert_contains(
        &text,
        "\"body_bytes\": 0",
        "the baseline sending a bodyless request to the hop",
    );
    assert_contains(
        &text,
        "\"signature_header\": true",
        "the signature header riding on the redirect hop",
    );
    assert_contains(&text, "\"cursor\": 1", "the baseline advancing the cursor past the undelivered event");
    assert_contains(&text, "\"exitCode\": 0", "the baseline calling the delivery healthy");
}

#[test]
fn the_artifact_carries_the_fixed_side_refusal() {
    let text = artifact("webhook-redirect-two-tree-2026-11-19.json");
    assert_contains(
        &text,
        "\"reportError\": \"refused with http 302 redirect",
        "the fixed side naming the mechanism",
    );
    assert_contains(
        &text,
        "repoint this target's url explicitly",
        "the fixed side naming the repair",
    );
}

#[test]
fn the_artifact_pins_the_guards_unchanged() {
    let text = artifact("webhook-redirect-two-tree-2026-11-19.json");
    assert_contains(
        &text,
        "\"error\": \"refused with http 404\"",
        "the 404 wording unchanged",
    );
    assert_contains(&text, "\"error\": \"http 500\"", "the 500 wording unchanged");
    // The guard rows must exist for BOTH sides (each guard object appears
    // twice: baselineGuard and fixedGuard).
    let count = text.matches("\"error\": \"refused with http 404\"").count();
    assert!(
        count >= 2,
        "the 404 guard must hold on both sides, found {count} occurrences"
    );
}

#[test]
fn no_face_holds_on_the_baseline_but_fails_on_the_fix_side_of_the_record() {
    // The five defect faces must be recorded as baseline=false / fixed=true;
    // the three guards as true on both. This is the shape a later rewrite
    // would most likely fudge (marking the baseline as "mostly fine").
    let text = artifact("webhook-redirect-two-tree-2026-11-19.json");
    for name in [
        "D1 the configured endpoint received ONE request and the event was not recorded delivered",
        "D2 the redirect destination received ZERO requests",
        "D3 the report holds the exit code and names the redirect mechanism",
        "D4 the attempt records the endpoint answer (3xx), not a 2xx",
        "D5 the cursor stays behind the undelivered event",
    ] {
        let marker = format!("\"name\": \"{name}\",\n      \"baseline\": false,\n      \"fixed\": true");
        assert!(
            text.contains(&marker),
            "the face must be recorded as baseline=false, fixed=true: {name}"
        );
    }
}
