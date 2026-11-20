//! Pins the FIXED side of the shutdown-overrun measurement on this tree.
//!
//! The finding (PULS BD, 2026-11-20): `reeflog start --seconds N` did not
//! end in about N seconds when the signature walks' pages arrived slowly.
//! The published v0.7.43 asset walked 41 pages and took 91 s for a 5 s run
//! against a sink that answers a page per second.
//!
//! This test pins the FIXED tree's own measurement: the run honours its
//! `--seconds` on every sink, the run stays inside the shutdown budget that
//! scales with the run, and the artifact names the binary it measured by
//! sha256 and by the version read from it. The published-side pin lives in
//! `tests/shutdown_overrun_published_v0743.rs`.
//!
//! What this test defends:
//!
//! * the run honours `--seconds` against a slow sink — the defect's own
//!   face, measured on THIS tree;
//! * the overshoot stays inside the shutdown budget (seconds / 5, at least
//!   2 s and at most 30 s), not merely inside the pages x delay ceiling —
//!   the ceiling is what the DEFEAT lived under, so it is not proof;
//! * the fixed side asks far fewer pages than the baseline: a bounded run
//!   stops asking, it does not walk the whole budget and then stop;
//! * the daemon still exits 0 on every sink — a slow endpoint is not a
//!   crash;
//! * the artifact carries no host path.

use std::path::PathBuf;

fn artifact(name: &str) -> serde_json::Value {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("research")
        .join(name);
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    serde_json::from_str(&text).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

const ARTIFACT: &str = "shutdown-overrun-fixed-vs-published-v0743-2026-11-20.json";

/// The shutdown budget the fix grants the passes that run after the run's
/// own deadline has passed: a fifth of the requested seconds, at least 2 s
/// and at most 30 s.
fn shutdown_budget_ms(seconds: i64) -> i64 {
    (seconds / 5).clamp(2, 30) * 1000
}

#[test]
fn the_artifact_names_the_verdict_and_both_sides() {
    let v = artifact(ARTIFACT);
    assert_eq!(
        v["verdict"].as_str(),
        Some("shutdown-deadline-held-by-the-walk"),
        "the verdict the instrument computes"
    );
    // The baseline must be the PUBLISHED v0.7.43 binary, named by sha256:
    // the published asset is the defect carrier and it cannot be re-tagged.
    assert_eq!(
        v["baseline"]["binSha256"].as_str(),
        Some("816177e336be4f1a76f7ba996dc5d059a0c88e1460b735eeeb05be4ce10f63af"),
        "the baseline must be the PUBLISHED v0.7.43 binary, named by sha256"
    );
    assert_eq!(
        v["baseline"]["binVersion"].as_str(),
        Some("reeflog 0.7.43"),
        "the baseline version must be read from that binary"
    );
    // The fixed side must describe the release THIS tree is.
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
        v["binVersion"].as_str(),
        Some(format!("reeflog {version}").as_str()),
        "the artifact must name the binary of THIS tree, by the version read from it"
    );
    let sha = v["binSha256"].as_str().expect("binSha256");
    assert_eq!(sha.len(), 64, "the binary must be named by a full sha256");
}

#[test]
fn the_run_honours_its_seconds_against_a_slow_sink() {
    let v = artifact(ARTIFACT);
    let control = v["runs"]["controlFast"]["ms"].as_i64().expect("control");
    let slow = v["runs"]["slowSink"]["ms"].as_i64().expect("slow");
    assert_eq!(v["runs"]["slowSink"]["exit"].as_i64(), Some(0));
    let budget = shutdown_budget_ms(v["seconds"].as_i64().expect("seconds"));
    // One page of the slow sink's delay is what the FIRST page of the
    // startup walk legitimately costs; the shutdown budget is what bounds
    // the rest. A loose bound here would pin nothing, a tight one would
    // pin the run's noise.
    let delay = v["delayMsPerPage"].as_i64().expect("delayMsPerPage");
    assert!(
        slow - control <= budget + delay,
        "a slow sink must not cost the run more than its budget plus one page: \
         overshoot {}ms vs budget {budget}ms + {delay}ms/page",
        slow - control
    );
}

#[test]
fn the_run_stays_inside_the_shutdown_budget() {
    // The pages x delay ceiling is NOT proof: the DEFEAT lived inside it.
    // The bound that counts is the shutdown budget, which scales with the
    // run.
    let v = artifact(ARTIFACT);
    let seconds = v["seconds"].as_i64().expect("seconds");
    let budget = shutdown_budget_ms(seconds);
    let control = v["runs"]["controlFast"]["ms"].as_i64().expect("control");
    let slow = v["runs"]["slowSink"]["ms"].as_i64().expect("slow");
    assert!(
        slow - control <= budget,
        "the run must stay inside its shutdown budget: overshoot {}ms vs budget {budget}ms",
        slow - control
    );
}

#[test]
fn the_fixed_side_asks_far_fewer_pages_than_the_baseline() {
    let v = artifact(ARTIFACT);
    let fixed = v["runs"]["slowSink"]["pagesAsked"].as_i64().expect("fixed pages");
    let baseline = v["baselineRuns"]["slowSink"]["pagesAsked"]
        .as_i64()
        .expect("baseline pages");
    assert!(
        fixed < baseline,
        "a bounded run stops asking: fixed {fixed} pages vs baseline {baseline} pages"
    );
}

#[test]
fn the_daemon_exits_zero_on_every_sink() {
    let v = artifact(ARTIFACT);
    for (name, run) in [("control", &v["runs"]["controlFast"]), ("slow", &v["runs"]["slowSink"])] {
        assert_eq!(
            run["exit"].as_i64(),
            Some(0),
            "{name}: a slow endpoint is not a crash — the run must still exit 0"
        );
    }
    assert_eq!(v["runs"]["slowSinkDoubleDelay"]["exit"].as_i64(), Some(0));
}

#[test]
fn the_artifact_carries_no_host_path() {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("research")
        .join(ARTIFACT);
    let text = std::fs::read_to_string(&path).expect("readable");
    for needle in ["/home/", "/tmp/", "C:\\\\", "/Users/"] {
        assert!(
            !text.contains(needle),
            "the artifact must not carry the host path {needle:?}"
        );
    }
}
