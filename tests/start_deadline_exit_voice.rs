//! Pins the START-DEADLINE + EXIT-VOICE measurement (PULS BE, 2026-11-20).
//!
//! Two measurements, both on PUBLISHED bytes, both live against
//! mainnet-beta:
//!
//! 1. `research/start-deadline-exit-published-v0744-2026-11-20.json`
//!    (instrument `tools/measure-start-deadline-exit.mjs`): does
//!    `reeflog start --seconds N` end near N, and does the exit code tell
//!    the truth? The PUBLISHED v0.7.43 asset (the defect carrier) never
//!    returned inside a 240 s ceiling for a 120 s run; the PUBLISHED v0.7.44
//!    asset ended at ~123 s with exit 0, and a quiet program ended at ~62 s
//!    with exit 0.
//!
//! 2. `research/start-exit-voice-debt-published-v0744-2026-11-20.json`:
//!    when the endpoint is contended and the run ends still owing
//!    payloads, the daemon exits 3 and names the repair — the README's
//!    "reported and held" voice, documented since v0.7.31. The live
//!    user-zero harness demanded exit 0; that expectation only ever passed
//!    on v0.7.43 because that binary OVERRAN its deadline (223 s for a
//!    120 s run) and so had time to pay the debt.
//!
//! The pins: the verdicts, the measured numbers, the binary identities by
//! sha256 and version, the two sides being different binaries, and no host
//! path in any artifact.

use std::path::PathBuf;

fn artifact(name: &str) -> serde_json::Value {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("research")
        .join(name);
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    serde_json::from_str(&text).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

const DEADLINE_ARTIFACT: &str = "start-deadline-exit-published-v0744-2026-11-20.json";
const VOICE_ARTIFACT: &str = "start-exit-voice-debt-published-v0744-2026-11-20.json";

/// The UNPACKED binary inside the PUBLISHED v0.7.44 linux-amd64 archive
/// (archive digest `sha256:aa8b4a1c…`, verified against the Release API
/// before unpacking).
const PUBLISHED_V0744_SHA: &str =
    "ead75ad96c442d4fd5ff0724e717840bad26f9c359084e2ec08acb65dcb5fea2";
/// The PUBLISHED v0.7.43 linux-amd64 asset's unpacked binary — the defect
/// carrier.
const PUBLISHED_V0743_SHA: &str =
    "816177e336be4f1a76f7ba996dc5d059a0c88e1460b735eeeb05be4ce10f63af";

fn tree_version() -> String {
    let cargo = std::fs::read_to_string(
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml"),
    )
    .expect("Cargo.toml");
    cargo
        .lines()
        .find_map(|l| l.strip_prefix("version = \""))
        .and_then(|l| l.split('"').next())
        .expect("the crate version")
        .to_string()
}

#[test]
fn the_deadline_artifact_names_the_verdict_and_both_sides() {
    let v = artifact(DEADLINE_ARTIFACT);
    assert_eq!(
        v["verdict"].as_str(),
        Some("start-deadline-and-exit-voice-hold"),
        "the verdict the instrument computes"
    );
    let fixed = &v["runs"]["fixed"];
    let baseline = &v["runs"]["baseline"];
    assert_eq!(
        fixed["binSha256"].as_str(),
        Some(PUBLISHED_V0744_SHA),
        "the fixed side must be the PUBLISHED v0.7.44 binary, named by sha256"
    );
    assert_eq!(
        fixed["binVersion"].as_str(),
        Some("reeflog 0.7.44"),
        "the fixed side's version is read from that binary"
    );
    assert_eq!(
        baseline["binSha256"].as_str(),
        Some(PUBLISHED_V0743_SHA),
        "the baseline must be the PUBLISHED v0.7.43 binary, named by sha256"
    );
    assert_eq!(
        baseline["binVersion"].as_str(),
        Some("reeflog 0.7.43"),
        "the baseline version must be read from that binary"
    );
    assert_ne!(
        fixed["binSha256"].as_str(),
        baseline["binSha256"].as_str(),
        "the two sides must be different binaries"
    );
    // The tree this pin lives in must be the release the measurement names.
    assert_eq!(
        tree_version(),
        "0.7.44",
        "this pin belongs to the v0.7.44 tree"
    );
}

#[test]
fn the_fixed_side_ends_near_its_deadline_and_the_baseline_does_not() {
    let v = artifact(DEADLINE_ARTIFACT);
    let seconds = v["seconds"].as_i64().expect("seconds");
    let budget = v["shutdownBudgetMs"].as_i64().expect("shutdown budget");
    let fixed = &v["runs"]["fixed"];
    let baseline = &v["runs"]["baseline"];

    let fixed_wall = fixed["wallMs"].as_i64().expect("fixed wall");
    assert!(
        fixed_wall <= (seconds * 1000 + budget + 15000),
        "the fixed side must end near its deadline: {fixed_wall}ms for {seconds}s"
    );
    assert_eq!(
        fixed["returnedOnItsOwn"].as_bool(),
        Some(true),
        "the fixed side returns on its own"
    );

    // The defect carrier: it either takes far longer than the caller asked
    // for, or it never returns at all inside the instrument's ceiling.
    let baseline_wall = baseline["wallMs"].as_i64().expect("baseline wall");
    let baseline_killed = baseline["killed"].as_bool().unwrap_or(false);
    assert!(
        baseline_killed || baseline_wall > (seconds * 1000 + budget),
        "the published v0.7.43 asset must overrun: {baseline_wall}ms for {seconds}s (killed: {baseline_killed})"
    );
}

#[test]
fn the_quiet_program_is_the_false_positive_guard() {
    // The ONE face that must hold on the fixed side too: a quiet program
    // ends near its deadline and exits 0. The fix must not turn every run
    // into exit 3.
    let v = artifact(DEADLINE_ARTIFACT);
    let quiet = &v["runs"]["fixedQuiet"];
    let seconds = v["quietSeconds"].as_i64().expect("quiet seconds");
    assert_eq!(
        quiet["exitCode"].as_i64(),
        Some(0),
        "a quiet program must exit 0"
    );
    assert_eq!(
        quiet["payloadPending"].as_i64().unwrap_or(0),
        0,
        "a quiet program owes nothing"
    );
    let wall = quiet["wallMs"].as_i64().expect("quiet wall");
    assert!(
        wall <= (seconds * 1000 + 15000 + 30000),
        "a quiet program must end near its deadline: {wall}ms for {seconds}s"
    );
}

#[test]
fn the_exit_voice_artifact_names_the_debt_and_the_repair() {
    let v = artifact(VOICE_ARTIFACT);
    assert_eq!(
        v["verdict"].as_str(),
        Some("exit-3-is-the-documented-voice-for-an-owed-journal"),
        "the verdict of the debt run"
    );
    let run = &v["runs"]["fixedBusyWithDebt"];
    assert_eq!(
        run["binSha256"].as_str(),
        Some(PUBLISHED_V0744_SHA),
        "the debt run must be the PUBLISHED v0.7.44 binary"
    );
    assert_eq!(
        run["binVersion"].as_str(),
        Some("reeflog 0.7.44"),
        "its version is read from that binary"
    );
    assert_eq!(
        run["exitCode"].as_i64(),
        Some(3),
        "an owed journal must not exit 0"
    );
    assert_eq!(
        run["statusExit"].as_i64(),
        Some(3),
        "the health report must agree with the daemon"
    );
    let pending = run["payloadPending"].as_i64().expect("payload pending");
    assert!(pending > 0, "the run must actually owe payloads: {pending}");
    assert_eq!(
        run["namesDebt"].as_bool(),
        Some(true),
        "the debt must be named on stderr"
    );
    assert_eq!(
        run["namesRepair"].as_bool(),
        Some(true),
        "the repair must be named on stderr"
    );
    let line = run["verdictLine"].as_str().expect("the verdict line");
    assert!(
        line.contains("payload hydrate INCOMPLETE") && line.contains("reeflog hydrate"),
        "the line must name the debt and the repair: {line}"
    );
    // The run ENDED inside its own deadline plus the shutdown budget.
    let wall = run["wallSeconds"].as_i64().expect("wall");
    let requested = run["requestedSeconds"].as_i64().expect("requested");
    assert!(
        wall <= requested + 30,
        "the run must end inside its deadline plus the shutdown budget: {wall}s for {requested}s"
    );
}

#[test]
fn no_artifact_carries_a_host_path() {
    for name in [DEADLINE_ARTIFACT, VOICE_ARTIFACT] {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("research")
            .join(name);
        let text = std::fs::read_to_string(&path).expect("readable");
        for needle in ["/home/", "/tmp/", "C:\\\\", "/Users/", "aurora"] {
            assert!(
                !text.contains(needle),
                "{} must not carry the host path {needle:?}",
                name
            );
        }
    }
}
