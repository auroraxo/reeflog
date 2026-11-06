//! The false-positive guard for the strict flag parser: every invocation the
//! e2e harnesses in `tools/` actually pass must still be ACCEPTED.
//!
//! A strict parser that refuses a flag the project's own tooling passes is
//! worse than the defect it fixes — it breaks the measured e2e runs without
//! fixing anything a user would notice. So the arg arrays below are not
//! invented here: they are the shapes `tools/e2e-*.mjs` build, with the
//! variable parts replaced by placeholder values.
//!
//! The assertion is about FLAG HANDLING, not about the exit code: `verify`
//! on an empty journal legitimately exits 3 ("the scan proves nothing"), and
//! that must stay possible. What must NOT happen is a refusal of the flag
//! itself — the parser rejecting a flag the product documents and its own
//! tooling uses.
//!
//! The switch flags (`--no-logs`, `--with-accounts`, `--accounts`,
//! `--owners`, `--account-data`) are the ones a value-substituting harness
//! would most easily break, so they are listed explicitly.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const VALID_PROGRAM: &str = "whirLbMiicVdio4qvUfM5KAg6Ct8VwpYzGff3uctyCc";

fn unique_dir(tag: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("reeflog-tools-{tag}-{}-{nanos}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("temp dir");
    dir
}

fn run(dir: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_reeflog"))
        .args(args)
        .current_dir(dir)
        .output()
        .expect("reeflog binary runs")
}

fn stderr_of(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).to_lowercase()
}

/// The refusal the strict parser produces. A flag-handling refusal looks
/// exactly like this — none of these words may appear for a documented
/// invocation.
fn is_flag_refusal(err: &str) -> bool {
    err.contains("unknown flag")
        || err.contains("needs a value")
        || err.contains("given more than once")
        || err.contains("unexpected argument")
        || err.contains("does not take a value")
}

#[test]
fn every_flag_the_tooling_passes_is_accepted() {
    let dir = unique_dir("tool-flags");
    let out = run(
        &dir,
        &["init", "--db", "reeflog.db", "--program", VALID_PROGRAM],
    );
    assert!(out.status.success(), "init must succeed");

    // The invocation shapes `tools/e2e-*.mjs` build. Placeholder values stand
    // in for the JS variables (DB, PROGRAM, cursor paths, counts).
    let cases: Vec<Vec<&str>> = vec![
        // e2e-repair / e2e-accounts / e2e-owners / e2e-account-data
        vec!["init", "--db", "reeflog.db", "--program", VALID_PROGRAM],
        // `start` cases use --seconds 0 with unreachable endpoints so every
        // flag is parsed but no ingest starts (measured 11ms). A refusal
        // happens in flag parsing, before dispatch — which is the point.
        vec!["start", "--db", "reeflog.db", "--seconds", "0", "--ws", "ws://127.0.0.1:9", "--rpc", "http://127.0.0.1:9"],
        vec!["start", "--db", "reeflog.db", "--seconds", "0", "--ws", "ws://127.0.0.1:9", "--rpc", "http://127.0.0.1:9", "--program", VALID_PROGRAM],
        vec![
            "start", "--db", "reeflog.db", "--seconds", "0", "--ws", "ws://127.0.0.1:9", "--rpc",
            "http://127.0.0.1:9", "--program", VALID_PROGRAM, "--webhook-config", "webhooks.json",
            "--webhook-every", "3", "--webhook-limit", "200", "--seal-every", "5",
            "--reorg-every", "10", "--repair-every", "15", "--repair-lookback", "600",
            "--repair-pages", "10",
        ],
        // e2e-fork / e2e-retraction daemon flags
        vec![
            "start", "--db", "reeflog.db", "--seconds", "0", "--ws", "ws://127.0.0.1:9", "--rpc",
            "http://127.0.0.1:9", "--program", VALID_PROGRAM, "--seal-every", "5",
            "--reorg-every", "5", "--reorg-lookback", "1000", "--repair-every", "5",
            "--payload-limit", "100",
        ],
        vec![
            "start", "--db", "reeflog.db", "--seconds", "0", "--ws", "ws://127.0.0.1:9", "--rpc",
            "http://127.0.0.1:9", "--program", VALID_PROGRAM, "--seal-every", "5",
            "--reorg-every", "10", "--reorg-lookback", "2000", "--payload-limit", "20",
        ],
        // hydrate
        vec!["hydrate", "--db", "reeflog.db"],
        vec!["hydrate", "--db", "reeflog.db", "--limit", "10"],
        vec!["hydrate", "--db", "reeflog.db", "--limit", "5000"],
        vec!["hydrate", "--db", "reeflog.db", "--limit", "10", "--rpc", "http://127.0.0.1:9"],
        vec!["hydrate", "--db", "reeflog.db", "--limit", "10", "--rpc", "http://127.0.0.1:9", "--accounts"],
        vec!["hydrate", "--db", "reeflog.db", "--limit", "5000", "--accounts"],
        vec!["hydrate", "--db", "reeflog.db", "--limit", "5000", "--owners"],
        vec!["hydrate", "--db", "reeflog.db", "--limit", "5000", "--account-data"],
        // verify / repair, with and without the anchor
        vec!["verify", "--db", "reeflog.db", "--pages", "1"],
        vec!["verify", "--db", "reeflog.db", "--pages", "2"],
        vec!["verify", "--db", "reeflog.db", "--pages", "400"],
        vec!["verify", "--db", "reeflog.db", "--pages", "5", "--anchor-slot"],
        vec!["verify", "--db", "reeflog.db", "--pages", "5", "--anchor-slot", "400000000"],
        vec![
            "verify", "--db", "reeflog.db", "--pages", "5", "--anchor-slot", "999999999",
            "--anchor-tries", "2",
        ],
        vec!["repair", "--db", "reeflog.db", "--pages", "400"],
        vec!["repair", "--db", "reeflog.db", "--pages", "5", "--anchor-slot"],
        vec!["repair", "--db", "reeflog.db", "--pages", "50"],
        // tail
        vec!["tail", "--db", "reeflog.db", "--cursor", "c.txt"],
        vec!["tail", "--db", "reeflog.db", "--cursor", "c.txt", "--limit", "1000"],
        vec!["tail", "--db", "reeflog.db", "--cursor", "c.txt", "--limit", "10000", "--no-logs"],
        vec!["tail", "--db", "reeflog.db", "--cursor", "c.txt", "--limit", "400", "--with-accounts"],
        vec!["tail", "--db", "reeflog.db", "--cursor", "c.txt", "--limit", "5000", "--with-accounts"],
        // seal / status
        vec!["seal", "--db", "reeflog.db"],
        vec!["status", "--db", "reeflog.db"],
        // reorg
        vec!["reorg", "check", "--db", "reeflog.db"],
        vec!["reorg", "check", "--db", "reeflog.db", "--limit", "500"],
        vec!["reorg", "check", "--db", "reeflog.db", "--limit", "2000"],
        vec!["reorg", "check", "--db", "reeflog.db", "--limit", "500", "--rpc", "http://127.0.0.1:9"],
        vec!["reorg", "tail", "--db", "reeflog.db", "--cursor", "r.txt"],
        vec!["reorg", "tail", "--db", "reeflog.db", "--cursor", "r.txt", "--limit", "100"],
        // webhook
        vec!["webhook", "push", "--db", "reeflog.db", "--config", "webhooks.json"],
        vec!["webhook", "status", "--db", "reeflog.db", "--config", "webhooks.json"],
        vec!["webhook", "dlq", "list", "--db", "reeflog.db", "--config", "webhooks.json"],
        vec![
            "webhook", "dlq", "list", "--db", "reeflog.db", "--config", "webhooks.json",
            "--target", "t1",
        ],
        vec![
            "webhook", "dlq", "replay", "--db", "reeflog.db", "--config", "webhooks.json",
            "--limit", "100",
        ],
        vec![
            "webhook", "dlq", "replay", "--db", "reeflog.db", "--config", "webhooks.json",
            "--target", "t1", "--limit", "100",
        ],
    ];

    for args in cases {
        let out = run(&dir, &args);
        let err = stderr_of(&out);
        assert!(
            !is_flag_refusal(&err),
            "`reeflog {}` was refused for its FLAGS: {err}",
            args.join(" ")
        );
    }

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn the_switch_flags_keep_working_without_a_value() {
    // A harness that substitutes values must not turn a switch into a
    // value-flag. These are the flags a strict parser is most likely to
    // break, and each one is used by a measured e2e run.
    let dir = unique_dir("switches");
    let out = run(
        &dir,
        &["init", "--db", "reeflog.db", "--program", VALID_PROGRAM],
    );
    assert!(out.status.success(), "init must succeed");

    for args in [
        vec!["tail", "--db", "reeflog.db", "--cursor", "c.txt", "--no-logs"],
        vec!["tail", "--db", "reeflog.db", "--cursor", "c.txt", "--with-accounts"],
        vec!["hydrate", "--db", "reeflog.db", "--accounts"],
        vec!["hydrate", "--db", "reeflog.db", "--owners"],
        vec!["hydrate", "--db", "reeflog.db", "--account-data"],
    ] {
        let out = run(&dir, &args);
        let err = stderr_of(&out);
        assert!(
            !is_flag_refusal(&err),
            "`reeflog {}` was refused for its FLAGS: {err}",
            args.join(" ")
        );
    }

    let _ = std::fs::remove_dir_all(&dir);
}
