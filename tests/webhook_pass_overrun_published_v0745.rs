//! Pins the WEBHOOK PASS BUDGET on PUBLISHED bytes (v0.7.45, 2026-11-21).
//!
//! The defect (PULS BF): the webhook delivery pass was bounded per POST by
//! the target's `timeout_ms` but had NO budget of its own, so a run paid
//! `backlog x per-POST cost` past its own `--seconds`. The fix (v0.7.45)
//! grants the pass the time left until the run's deadline: it checks before
//! every row, stops with `budget_stopped` plus `events_owed`/`reorgs_owed`
//! (a time-slice, named — never an error) and reports `budget_stops` /
//! `rows_owed` in the summary.
//!
//! This file pins the measurement taken AFTER the release, with BOTH sides
//! being the bytes a stranger installs:
//!
//! * `research/webhook-pass-overrun-published-v0745-2026-11-21.json` —
//!   instrument `tools/measure-webhook-pass-overrun.mjs`, baseline = the
//!   PUBLISHED v0.7.44 asset (`ead75ad9…`, the defect carrier), fixed =
//!   the PUBLISHED v0.7.45 asset (`25d1990d…`, unpacked from the release
//!   archive whose digest matches the Release API).
//!
//! What this file defends:
//!
//! * the verdict — the carrier overruns, the published fix does not;
//! * the identities of BOTH sides by sha256 and version, read from the
//!   binaries themselves;
//! * the fixed side inside the honest tail (shutdown budget + one
//!   in-flight POST + exit slack) at BOTH queue sizes — the queue's size
//!   no longer decides what the run pays;
//! * the growth asymmetry: the carrier's overrun scales with the backlog,
//!   the fixed side's does not;
//! * the drain is untouched: `webhook push` still delivers every planted
//!   row and exits 0 on both sides;
//! * the budget does not starve delivery: the fixed daemon handed over rows
//!   before its slice ended and named what stayed owed;
//! * no artifact carries a host path.

use std::path::PathBuf;

fn artifact(name: &str) -> serde_json::Value {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("research")
        .join(name);
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    serde_json::from_str(&text).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

const ARTIFACT: &str = "webhook-pass-overrun-published-v0745-2026-11-21.json";

/// The PUBLISHED v0.7.44 linux-amd64 asset's unpacked binary — the defect
/// carrier (verified against the Release API before use).
const PUBLISHED_V0744_SHA: &str =
    "ead75ad96c442d4fd5ff0724e717840bad26f9c359084e2ec08acb65dcb5fea2";

/// The PUBLISHED v0.7.45 linux-amd64 asset's unpacked binary — the fix
/// (archive digest `sha256:24b71dc0…`, verified against the Release API
/// before unpacking).
const PUBLISHED_V0745_SHA: &str =
    "25d1990d0bf92b5805ec18b2f959bfd7d8128ebe0c1d82dc7a067c1a9b51958b";

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
fn the_artifact_carries_the_verdict_and_the_measurement_parameters() {
    let v = artifact(ARTIFACT);
    assert_eq!(
        v["verdict"].as_str(),
        Some("webhook-pass-bounded-by-the-pass-budget"),
        "the verdict the instrument computes on the published bytes"
    );
    assert_eq!(v["tool"].as_str(), Some("measure-webhook-pass-overrun.mjs"));
    assert_eq!(v["seconds"].as_i64(), Some(30));
    assert_eq!(v["delayMsPerPost"].as_i64(), Some(250));
    assert_eq!(v["limit"].as_i64(), Some(200));
    assert_eq!(v["limitSmall"].as_i64(), Some(50));
    assert_eq!(
        v["webhookEveryS"].as_i64(),
        Some(5),
        "the cadence must be due INSIDE the run, or the instrument measures nothing"
    );
    for face in v["faces"].as_array().expect("faces") {
        assert_eq!(
            face["ok"].as_bool(),
            Some(true),
            "every face must hold on the published bytes: {}",
            face["name"].as_str().unwrap_or("?")
        );
    }
}

#[test]
fn both_sides_are_published_assets_named_by_sha256_and_version() {
    let v = artifact(ARTIFACT);
    assert_eq!(
        v["baseline"]["binSha256"].as_str(),
        Some(PUBLISHED_V0744_SHA),
        "the baseline must be the PUBLISHED v0.7.44 binary"
    );
    assert_eq!(
        v["baseline"]["binVersion"].as_str(),
        Some("reeflog 0.7.44"),
        "the baseline version must be read from that binary"
    );
    assert_eq!(
        v["binSha256"].as_str(),
        Some(PUBLISHED_V0745_SHA),
        "the fixed side must be the PUBLISHED v0.7.45 binary"
    );
    assert_eq!(
        v["binVersion"].as_str(),
        Some("reeflog 0.7.45"),
        "the fixed version must be read from that binary"
    );
    assert_ne!(
        v["baseline"]["binSha256"].as_str(),
        v["binSha256"].as_str(),
        "the two sides must be different binaries"
    );
    // The tree this pin lives in must be the release the measurement names.
    assert_eq!(
        tree_version(),
        "0.7.45",
        "this pin belongs to the v0.7.45 tree"
    );
}

#[test]
fn the_published_fix_stays_inside_the_honest_tail_at_both_queue_sizes() {
    let v = artifact(ARTIFACT);
    let tail = v["honestTailMs"].as_i64().expect("honestTail");
    let over = overrun(&v);
    let over_small = overrun_small(&v);
    assert!(
        over <= tail,
        "the published fix must stay inside its honest tail: overrun {over}ms vs {tail}ms"
    );
    assert!(
        over_small <= tail,
        "the published fix must stay inside its honest tail at the SMALL queue too: {over_small}ms vs {tail}ms"
    );
}

#[test]
fn the_carrier_overruns_and_the_published_fix_does_not_scale_with_the_queue() {
    let v = artifact(ARTIFACT);
    let delay = v["delayMsPerPost"].as_i64().expect("delay");
    let carrier_over = v["baselineRuns"]["daemonSlow"]["ms"].as_i64().expect("carrier ms")
        - v["seconds"].as_i64().expect("seconds") * 1000;
    assert!(
        carrier_over > delay * 4,
        "the published carrier must overrun far past one POST's delay: {carrier_over}ms"
    );
    let limit = v["limit"].as_i64().expect("limit");
    let limit_small = v["limitSmall"].as_i64().expect("limitSmall");
    let expected_growth = (limit - limit_small) * delay;
    let carrier_growth = carrier_over
        - (v["baselineRuns"]["daemonSmall"]["ms"].as_i64().expect("carrier small ms")
            - v["seconds"].as_i64().expect("seconds") * 1000);
    let fixed_growth = overrun(&v) - overrun_small(&v);
    assert!(
        carrier_growth >= expected_growth * 2 / 5,
        "the carrier's overrun must scale with the backlog: grew {carrier_growth}ms, queue cost {expected_growth}ms"
    );
    let tail = v["honestTailMs"].as_i64().expect("honestTail");
    assert!(
        fixed_growth.abs() <= tail,
        "the published fix's overrun must NOT scale with the backlog: grew {fixed_growth}ms (tail {tail}ms)"
    );
}

#[test]
fn the_carrier_carries_no_slice_fields_and_the_fix_does() {
    let v = artifact(ARTIFACT);
    let carrier = &v["baselineRuns"]["daemonSlow"];
    assert_eq!(
        carrier["budgetStops"].as_str(),
        Some("missing"),
        "a pre-fix binary does not CARRY the slice fields — recorded as absent, never as 0"
    );
    assert_eq!(carrier["rowsOwed"].as_str(), Some("missing"));
    let fixed = &v["runs"]["daemonSlow"];
    assert!(
        fixed["budgetStops"].as_i64().expect("budgetStops") >= 1,
        "the slice must be named, not silent"
    );
    assert!(
        fixed["rowsOwed"].as_i64().expect("rowsOwed") >= 1,
        "the rows behind the slice must be named as owed"
    );
}

#[test]
fn the_drain_is_untouched_on_both_sides() {
    let v = artifact(ARTIFACT);
    let limit = v["limit"].as_i64().expect("limit");
    let limit_small = v["limitSmall"].as_i64().expect("limitSmall");
    for (side, label) in [(&v["runs"], "fixed"), (&v["baselineRuns"], "carrier")] {
        assert_eq!(
            side["pushSlow"]["exit"].as_i64(),
            Some(0),
            "{label}: push must still exit 0"
        );
        assert_eq!(
            side["pushSlow"]["deliveredBodies"].as_i64(),
            Some(limit),
            "{label}: the drain must still deliver EVERY planted row"
        );
        assert_eq!(
            side["pushSmall"]["deliveredBodies"].as_i64(),
            Some(limit_small),
            "{label}: the small drain too"
        );
    }
}

#[test]
fn a_slow_endpoint_is_not_a_crash_on_either_side() {
    let v = artifact(ARTIFACT);
    for (side, label) in [(&v["runs"], "fixed"), (&v["baselineRuns"], "carrier")] {
        for (run, name) in [
            ("daemonSlow", "slow"),
            ("daemonFast", "control"),
            ("daemonSmall", "small"),
        ] {
            assert_eq!(
                side[run]["exit"].as_i64(),
                Some(0),
                "{label}: the {name} daemon run must exit 0"
            );
            assert_eq!(
                side[run]["killed"].as_bool(),
                Some(false),
                "{label}: the {name} daemon run must not be killed"
            );
        }
    }
}

#[test]
fn the_control_keeps_the_daemon_near_its_seconds_on_both_sides() {
    let v = artifact(ARTIFACT);
    let seconds = v["seconds"].as_i64().expect("seconds");
    for (side, label) in [(&v["runs"], "fixed"), (&v["baselineRuns"], "carrier")] {
        let fast = side["daemonFast"]["ms"].as_i64().expect("daemonFast ms");
        assert!(
            fast < (seconds + 15) * 1000,
            "{label}: the 0-delay control must stay near its seconds: {fast}ms"
        );
    }
}

#[test]
fn no_artifact_carries_a_host_path() {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("research")
        .join(ARTIFACT);
    let text = std::fs::read_to_string(&path).expect("readable");
    for needle in ["/home/", "/tmp/", "C:\\\\", "/Users/", "aurora"] {
        assert!(
            !text.contains(needle),
            "{ARTIFACT} must not carry the host path {needle:?}"
        );
    }
}
