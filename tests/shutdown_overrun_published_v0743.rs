//! Pins the shutdown-overrun measurement (PULS BD, 2026-11-20).
//!
//! The finding: `reeflog start --seconds N` does not end in about N seconds
//! when the signature walks' pages arrive slowly. The daemon checked its
//! deadline only BETWEEN scheduler ticks, but the startup tail backfill, the
//! periodic repair pass and the final shutdown pass are each ONE synchronous
//! walk over `getSignaturesForAddress` pages, and each page pays its own
//! rate-limit backoff — so a walk could outlive the run that scheduled it.
//!
//! Measured live twice on the PUBLISHED v0.7.43 asset against mainnet-beta
//! (150 s requested -> 28:11 wall-clock; 120 s requested -> still running at
//! 14:34), then isolated from the network by
//! `tools/measure-shutdown-overrun.mjs`: one local sink, the delay applied
//! per page, so the mechanism and not the network is what is measured.
//!
//! This test pins the TWO-TREE measurement:
//!
//! * the BASELINE side is the PUBLISHED v0.7.43 linux-amd64 asset (archive
//!   digest `sha256:3e7bcc98…`, unpacked binary `816177e3…`) — the defect
//!   carrier, and it must STILL overrun the deadline;
//! * the FIXED side is this tree — it must stay inside its own shutdown
//!   budget (the run's seconds / 5, at least 2 s and at most 30 s), NOT
//!   inside the pages x delay ceiling the baseline lives under;
//! * the control holds on BOTH sides — a 0-delay sink honours `--seconds`:
//!   this is the false-positive guard, the ONE face expected to hold on the
//!   defect carrier too;
//! * no host path may leak into the artifact.

use std::path::PathBuf;

fn artifact(name: &str) -> serde_json::Value {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("research")
        .join(name);
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    serde_json::from_str(&text).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

const ARTIFACT: &str = "shutdown-overrun-published-v0743-2026-11-20.json";

/// The UNPACKED binary inside the PUBLISHED v0.7.43 linux-amd64 archive
/// (archive digest `sha256:3e7bcc98…`, verified against the Release API
/// before unpacking).
const PUBLISHED_V0743_SHA: &str =
    "816177e336be4f1a76f7ba996dc5d059a0c88e1460b735eeeb05be4ce10f63af";

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
    assert_eq!(
        v["baseline"]["binSha256"].as_str(),
        Some(PUBLISHED_V0743_SHA),
        "the baseline must be the PUBLISHED v0.7.43 binary, named by sha256"
    );
    assert_eq!(
        v["baseline"]["binVersion"].as_str(),
        Some("reeflog 0.7.43"),
        "the baseline version must be read from that binary"
    );
    // The fixed side must describe the release this tree is.
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
        "the fixed side must be the binary of THIS tree, by the version read from it"
    );
    assert_ne!(
        v["binSha256"].as_str(),
        v["baseline"]["binSha256"].as_str(),
        "the two sides must be different binaries"
    );
}

#[test]
fn the_fixed_side_honours_the_deadline_on_every_sink() {
    // The false-positive guard, on the FIXED side: a 0-delay sink must still
    // honour `--seconds`, so the fix is not "make every run slow". This is
    // the ONE face expected to hold on a fixed tree; the defect carrier
    // fails it (its walk is unbounded in PAGES, so even a 0-delay sink
    // costs it 50 s and an exit 3 at the transport failure) — which is the
    // measurement, not a reason to weaken this assertion.
    let v = artifact(ARTIFACT);
    let seconds = v["seconds"].as_i64().expect("seconds");
    let f = &v["runs"];
    for (name, run) in [
        ("control", &f["controlFast"]),
        ("slow", &f["slowSink"]),
        ("slow2", &f["slowSinkDoubleDelay"]),
    ] {
        let ms = run["ms"].as_i64().expect("wall-clock");
        let exit = run["exit"].as_i64().expect("exit");
        assert_eq!(exit, 0, "{name}: the fixed side exits 0 on every sink");
        assert!(
            ms < (seconds + 15) * 1000,
            "{name}: the fixed side honours --seconds: {ms}ms for {seconds}s"
        );
    }
}

#[test]
fn the_baseline_still_overruns_the_deadline() {
    let v = artifact(ARTIFACT);
    let b = &v["baselineRuns"];
    let control = b["controlFast"]["ms"].as_i64().expect("control");
    let slow = b["slowSink"]["ms"].as_i64().expect("slow");
    let delay = v["delayMsPerPage"].as_i64().expect("delay");
    let overshoot = slow - control;
    assert!(
        overshoot > delay,
        "the published binary must still overrun: overshoot {overshoot}ms vs {delay}ms/page"
    );
    assert!(
        b["slowSink"]["pagesAsked"].as_i64().unwrap_or(0) > 2,
        "the published binary must keep walking pages: {}",
        b["slowSink"]["pagesAsked"]
    );
    // Its walk is unbounded in PAGES: even a 0-delay sink costs the
    // baseline more than --seconds, and it ends at the transport failure
    // (exit 3) instead of at the deadline.
    let seconds = v["seconds"].as_i64().expect("seconds");
    assert!(
        control > (seconds + 15) * 1000,
        "the defect carrier must overrun even on a 0-delay sink: {control}ms for {seconds}s"
    );
}

#[test]
fn the_fixed_side_stays_inside_its_shutdown_budget() {
    let v = artifact(ARTIFACT);
    let f = &v["runs"];
    let control = f["controlFast"]["ms"].as_i64().expect("control");
    let slow = f["slowSink"]["ms"].as_i64().expect("slow");
    let delay = v["delayMsPerPage"].as_i64().expect("delay");
    let seconds = v["seconds"].as_i64().expect("seconds");
    let overshoot = slow - control;
    let budget = shutdown_budget_ms(seconds);
    assert!(
        overshoot <= budget + delay,
        "the fixed side must stay inside its shutdown budget: overshoot {overshoot}ms vs budget {budget}ms (+ one page {delay}ms)"
    );
    assert_eq!(
        f["slowSink"]["exit"].as_i64(),
        Some(0),
        "the fixed side still exits 0 — a slow endpoint is not a crash"
    );
}

#[test]
fn the_fixed_side_asks_far_fewer_pages_than_the_baseline() {
    // The mechanism, not just the wall-clock: the fixed walk STOPS at its
    // deadline, so it must not have walked the baseline's page count.
    let v = artifact(ARTIFACT);
    let fixed_pages = v["runs"]["slowSink"]["pagesAsked"].as_i64().expect("fixed pages");
    let base_pages = v["baselineRuns"]["slowSink"]["pagesAsked"]
        .as_i64()
        .expect("baseline pages");
    assert!(
        fixed_pages < base_pages,
        "the fixed walk must stop early: {fixed_pages} pages vs the baseline's {base_pages}"
    );
}

#[test]
fn the_artifact_carries_no_host_path() {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("research")
        .join(ARTIFACT);
    let text = std::fs::read_to_string(&path).expect("readable");
    for needle in ["/home/", "/tmp/", "C:\\\\", "/Users/", "aurora"] {
        assert!(
            !text.contains(needle),
            "the artifact must not carry the host path {needle:?}"
        );
    }
}
