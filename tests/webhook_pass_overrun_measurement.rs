//! Pins BOTH sides of the webhook pass-overrun measurement (PULS BF).
//!
//! The finding (2026-11-21, `tools/measure-webhook-pass-overrun.mjs`): the
//! webhook delivery pass was bounded per POST by the target's `timeout_ms`
//! but had NO budget of its own, so a run paid `backlog x per-POST cost`
//! past its own `--seconds`. Measured against the PUBLISHED v0.7.44 asset
//! (the defect carrier): a 30 s run with a 200-row backlog against a
//! 250 ms/POST sink ended 29 s late; the overrun grew 24 s when the backlog
//! grew 4x. The fix grants the pass the time left until the run's deadline
//! (`budget_stops`/`rows_owed` in the summary, a slice, not a failure).
//!
//! Two artifacts, both committed, both naming every binary by sha256 and
//! version:
//!
//! * `webhook-pass-overrun-falsify-2026-11-21.json` — the DEFECT, measured
//!   while this tree was still unfixed, against the published carrier;
//! * `webhook-pass-overrun-fixed-vs-published-v0744-2026-11-21.json` — the
//!   FIX, same instrument, same parameters, fixed tree vs the same carrier.
//!
//! What this file defends:
//!
//! * the two verdicts — the carrier overruns, the fixed tree does not;
//! * the defect's own face on the carrier (overrun far past the delay of
//!   ONE POST) and the slice fields MISSING there (a pre-fix binary does
//!   not carry them; recording that absence is part of the measurement);
//! * the fixed side inside the honest tail (shutdown budget + one
//!   in-flight POST + exit slack) at BOTH queue sizes — the queue's size
//!   no longer decides what the run pays;
//! * the growth asymmetry: the carrier's overrun scales with the backlog,
//!   the fixed side's does not;
//! * the drain is untouched: `webhook push` still delivers every planted
//!   row and exits 0 on both sides — the budget is the SCHEDULER's
//!   contract, never the drain's;
//! * the budget does not starve delivery: the fixed daemon handed over
//!   rows before its slice ended and named what stayed owed;
//! * exit voices: a slow endpoint is not a crash on either side.

use std::path::PathBuf;

fn artifact(name: &str) -> serde_json::Value {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("research")
        .join(name);
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    serde_json::from_str(&text).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

const ARTIFACT_CARRIER: &str = "webhook-pass-overrun-falsify-2026-11-21.json";
const ARTIFACT_FIXED: &str = "webhook-pass-overrun-fixed-vs-published-v0744-2026-11-21.json";

/// The PUBLISHED v0.7.44 linux-amd64 asset's unpacked binary — the defect
/// carrier for this defect (verified against the Release API before use).
const PUBLISHED_V0744_SHA: &str =
    "ead75ad96c442d4fd5ff0724e717840bad26f9c359084e2ec08acb65dcb5fea2";

fn tree_version() -> String {
    let cargo =
        std::fs::read_to_string(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml"))
            .expect("Cargo.toml");
    cargo
        .lines()
        .find_map(|l| l.strip_prefix("version = \""))
        .and_then(|l| l.split('"').next())
        .expect("the crate version")
        .to_string()
}

fn overrun(side: &serde_json::Value) -> i64 {
    side["runs"]["daemonSlow"]["ms"].as_i64().expect("daemonSlow ms")
        - side["seconds"].as_i64().expect("seconds") * 1000
}

fn overrun_small(side: &serde_json::Value) -> i64 {
    side["runs"]["daemonSmall"]["ms"].as_i64()
        .expect("daemonSmall ms")
        - side["seconds"].as_i64().expect("seconds") * 1000
}

#[test]
fn the_two_artifacts_carry_the_two_verdicts() {
    let carrier = artifact(ARTIFACT_CARRIER);
    let fixed = artifact(ARTIFACT_FIXED);
    assert_eq!(
        carrier["verdict"].as_str(),
        Some("webhook-pass-overruns-the-run"),
        "the carrier's own verdict: the pass outlives the run"
    );
    assert_eq!(
        fixed["verdict"].as_str(),
        Some("webhook-pass-bounded-by-the-pass-budget"),
        "the fixed tree's verdict: the pass stays inside its budget"
    );
    // Same instrument, same parameters: the comparison is honest.
    for (name, v) in [(ARTIFACT_CARRIER, &carrier), (ARTIFACT_FIXED, &fixed)] {
        assert_eq!(v["tool"].as_str(), Some("measure-webhook-pass-overrun.mjs"), "{name}");
        assert_eq!(v["seconds"].as_i64(), Some(30), "{name}");
        assert_eq!(v["delayMsPerPost"].as_i64(), Some(250), "{name}");
        assert_eq!(v["limit"].as_i64(), Some(200), "{name}");
        assert_eq!(v["limitSmall"].as_i64(), Some(50), "{name}");
        assert_eq!(v["webhookEveryS"].as_i64(), Some(5), "{name} cadence must be due INSIDE the run");
    }
}

#[test]
fn both_artifacts_name_the_published_carrier_by_sha256_and_version() {
    for name in [ARTIFACT_CARRIER, ARTIFACT_FIXED] {
        let v = artifact(name);
        let b = &v["baseline"];
        assert_eq!(
            b["binSha256"].as_str(),
            Some(PUBLISHED_V0744_SHA),
            "{name}: the baseline must be the PUBLISHED v0.7.44 binary"
        );
        assert_eq!(
            b["binVersion"].as_str(),
            Some("reeflog 0.7.44"),
            "{name}: the baseline version must be read from that binary"
        );
    }
}

#[test]
fn the_fixed_side_describes_this_tree_and_not_the_carrier() {
    let v = artifact(ARTIFACT_FIXED);
    assert_eq!(
        v["binVersion"].as_str(),
        Some(format!("reeflog {}", tree_version()).as_str()),
        "the artifact must name the binary of THIS tree, by the version read from it"
    );
    let sha = v["binSha256"].as_str().expect("binSha256");
    assert_eq!(sha.len(), 64, "the fixed binary must be named by a full sha256");
    assert_ne!(
        sha,
        PUBLISHED_V0744_SHA,
        "the fixed side must be a DIFFERENT binary from the published carrier"
    );
}

#[test]
fn the_carrier_shows_the_defect_and_no_slice_fields() {
    let v = artifact(ARTIFACT_CARRIER);
    let delay = v["delayMsPerPost"].as_i64().expect("delay");
    let over = overrun(&v);
    assert!(
        over > delay * 4,
        "the carrier's overrun must be far more than one POST's delay: {over}ms"
    );
    // A pre-fix binary does not CARRY the slice fields. The instrument
    // records that absence verbatim — never as 0, which would read as
    // "nothing owed".
    let slow = &v["runs"]["daemonSlow"];
    assert_eq!(slow["budgetStops"].as_str(), Some("missing"));
    assert_eq!(slow["rowsOwed"].as_str(), Some("missing"));
    assert_eq!(
        slow["delivered"].as_i64(),
        Some(200),
        "the carrier pays the whole queue — in wall time it cannot afford"
    );
}

#[test]
fn the_fixed_side_stays_inside_the_honest_tail_at_both_queue_sizes() {
    let v = artifact(ARTIFACT_FIXED);
    let tail = v["honestTailMs"].as_i64().expect("honestTail");
    let over = overrun(&v);
    let over_small = overrun_small(&v);
    assert!(
        over <= tail,
        "the fixed run must stay inside its honest tail: overrun {over}ms vs {tail}ms"
    );
    assert!(
        over_small <= tail,
        "the fixed run must stay inside its honest tail at the SMALL queue too: {over_small}ms vs {tail}ms"
    );
}

#[test]
fn the_growth_scales_with_the_backlog_on_the_carrier_and_not_on_the_fixed_side() {
    let carrier = artifact(ARTIFACT_CARRIER);
    let fixed = artifact(ARTIFACT_FIXED);
    let limit = carrier["limit"].as_i64().expect("limit");
    let limit_small = carrier["limitSmall"].as_i64().expect("limitSmall");
    let delay = carrier["delayMsPerPost"].as_i64().expect("delay");
    let expected_growth = (limit - limit_small) * delay;
    let carrier_growth = overrun(&carrier) - overrun_small(&carrier);
    let fixed_growth = overrun(&fixed) - overrun_small(&fixed);
    assert!(
        carrier_growth >= expected_growth * 2 / 5,
        "the carrier's overrun must scale with the backlog: grew {carrier_growth}ms, queue cost {expected_growth}ms"
    );
    let tail = fixed["honestTailMs"].as_i64().expect("honestTail");
    assert!(
        fixed_growth.abs() <= tail,
        "the fixed side's overrun must NOT scale with the backlog: grew {fixed_growth}ms (tail {tail}ms)"
    );
}

#[test]
fn the_drain_is_untouched_on_both_sides() {
    for name in [ARTIFACT_CARRIER, ARTIFACT_FIXED] {
        let v = artifact(name);
        let limit = v["limit"].as_i64().expect("limit");
        let limit_small = v["limitSmall"].as_i64().expect("limitSmall");
        let slow = &v["runs"]["pushSlow"];
        let small = &v["runs"]["pushSmall"];
        assert_eq!(slow["exit"].as_i64(), Some(0), "{name}: push must still exit 0");
        assert_eq!(
            slow["deliveredBodies"].as_i64(),
            Some(limit),
            "{name}: the drain must still deliver EVERY planted row"
        );
        assert_eq!(
            small["deliveredBodies"].as_i64(),
            Some(limit_small),
            "{name}: the small drain too"
        );
    }
}

#[test]
fn the_budget_does_not_starve_delivery_on_the_fixed_side() {
    let v = artifact(ARTIFACT_FIXED);
    let slow = &v["runs"]["daemonSlow"];
    let delivered = slow["delivered"].as_i64().expect("delivered");
    let stops = slow["budgetStops"].as_i64().expect("budgetStops");
    let owed = slow["rowsOwed"].as_i64().expect("rowsOwed");
    assert!(
        delivered >= 1,
        "a budget that delivers NOTHING is a defect of its own"
    );
    assert!(stops >= 1, "the slice must be named, not silent");
    assert!(owed >= 1, "the rows behind the slice must be named as owed");
    assert!(
        delivered + owed >= 200,
        "slice plus owed must account for the page: {delivered} + {owed}"
    );
}

#[test]
fn a_slow_endpoint_is_not_a_crash_on_either_side() {
    for name in [ARTIFACT_CARRIER, ARTIFACT_FIXED] {
        let v = artifact(name);
        for (run, label) in [
            ("daemonSlow", "slow"),
            ("daemonFast", "control"),
            ("daemonSmall", "small"),
        ] {
            assert_eq!(
                v["runs"][run]["exit"].as_i64(),
                Some(0),
                "{name}: the {label} daemon run must exit 0"
            );
            assert_eq!(
                v["runs"][run]["killed"].as_bool(),
                Some(false),
                "{name}: the {label} daemon run must not be killed"
            );
        }
    }
}

#[test]
fn the_control_keeps_the_daemon_near_its_seconds_on_both_sides() {
    for name in [ARTIFACT_CARRIER, ARTIFACT_FIXED] {
        let v = artifact(name);
        let seconds = v["seconds"].as_i64().expect("seconds");
        let fast = v["runs"]["daemonFast"]["ms"].as_i64().expect("daemonFast ms");
        assert!(
            fast < (seconds + 15) * 1000,
            "{name}: the 0-delay control must stay near its seconds: {fast}ms"
        );
    }
}
