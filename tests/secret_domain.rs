//! ONE SECRET MUST NOT BE TWO CONSUMERS (2026-11-11).
//!
//! Measured 2026-11-11 on the PUBLISHED v0.7.15 release asset (sha256
//! 8f941253… == the Release-API digest), with two VERIFYING live receivers
//! (`tools/measure-secret-domain.mjs`):
//!
//!   config: two targets `ta` and `tb`, DIFFERENT urls, ONE shared
//!   non-empty secret. The secret is inside the documented domain — the
//!   README says every byte except emptiness is allowed, because "the
//!   secret is an HMAC key and never goes on the wire".
//!
//!   F1  the (secret, body) HMAC is target-agnostic: the pair receiver A
//!       accepted verifies 3/3 — and the very same pair is what receiver B
//!       computes, because B holds the same secret.
//!   F2  the delivery-id header (`X-Reeflog-Event-Id`) names the target but
//!       is OUTSIDE the signature: A's accepted bytes replayed at B under a
//!       forged `tb:` id answer **200** — B's OWN HMAC rule says yes — and B
//!       records the forged id as its own.
//!   F3  cross-target FORWARD: a body B received replays at A and A's own
//!       rule accepts it (200). One secret = one identity space in BOTH
//!       directions.
//!   F4  the seq/event-id headers lie independently of the signature: A's
//!       body with forged headers answers 200 at B.
//!
//!   4 of 4 faces violated, verdict `SIGNATURE-TARGET-AGNOSTIC` — while the
//!   GUARD (two targets, DISTINCT secrets) is 4/4 green: the foreign pairs
//!   are refused by the receivers' own rules with 401
//!   (`tools/measure-secret-domain-guard.mjs`).
//!
//! What that means for the product: the signature attests "someone who
//! knows this secret signed THIS BODY" — never "signed this FOR THIS
//! CONSUMER". With one secret on two endpoints, a consumer of A can forge a
//! delivery into B's stream (and the other way round), and neither side can
//! tell: the forged body is byte-identical to a legitimate one.
//!
//! The rule this ladder enforces:
//!
//! * two targets that share a secret while pointing at DIFFERENT urls must
//!   be REFUSED (exit 1) before a byte moves — one secret is one consumer;
//! * the refusal must name the two target ids and the two urls, and must
//!   NOT echo the secret itself (a refusal line that carries the key is a
//!   leak into logs, terminals and CI output);
//! * the refusal must not be forgeable from operator input: a target id
//!   carrying a newline must not be able to write a second, real-looking
//!   line into the refusal;
//! * the same secret on two targets with the SAME url stays legal — that is
//!   one consumer listed twice, and nothing about it is forgeable.
//!
//! The guards matter as much: a config with ONE target must keep working,
//! two targets with distinct secrets must keep working, and a shared secret
//! on one url must keep working. A checker that refuses a working config is
//! worse than the defect it fixes.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const VALID_PROGRAM: &str = "whirLbMiicVdio4qvUfM5KAg6Ct8VwpYzGff3uctyCc";

fn unique_dir(tag: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("reeflog-secret-domain-{tag}-{nanos}"));
    std::fs::create_dir_all(&dir).expect("create dir");
    dir
}

fn seed(dir: &Path) -> PathBuf {
    let db = dir.join("reeflog.db");
    let out = Command::new(env!("CARGO_BIN_EXE_reeflog"))
        .args(["init", "--db"])
        .arg(&db)
        .args(["--program", VALID_PROGRAM])
        .output()
        .expect("run reeflog init");
    assert!(
        out.status.success(),
        "init failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    db
}

/// `webhook status` on the given config: the cheapest path that loads the
/// config and applies every domain rule.
fn status(dir: &Path, cfg: &Path, db: &Path) -> Output {
    Command::new(env!("CARGO_BIN_EXE_reeflog"))
        .args(["webhook", "status", "--db"])
        .arg(db)
        .arg("--config")
        .arg(cfg)
        .output()
        .expect("run reeflog webhook status")
}

fn write_cfg(dir: &Path, name: &str, body: &str) -> PathBuf {
    let cfg = dir.join(name);
    std::fs::write(&cfg, body).expect("write config");
    cfg
}

// --- the defect -------------------------------------------------------------

#[test]
fn two_targets_with_one_secret_and_different_urls_are_refused() {
    let dir = unique_dir("shared-secret");
    let db = seed(&dir);
    let cfg = write_cfg(
        &dir,
        "wh.json",
        r#"{"targets":[
             {"id":"ta","url":"http://127.0.0.1:9101/hook","secret":"whsec_ALPHA_77x"},
             {"id":"tb","url":"http://127.0.0.1:9102/hook","secret":"whsec_ALPHA_77x"}
           ]}"#,
    );
    let out = status(&dir, &cfg, &db);
    assert!(
        !out.status.success(),
        "one secret on two endpoints must be refused — the signature cannot \
         tell the two consumers apart"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("secret"),
        "the refusal must say what is wrong: {stderr}"
    );
    // The refusal must name BOTH consumers, or the operator cannot know
    // which pair to fix.
    assert!(
        stderr.contains("ta") && stderr.contains("tb"),
        "the refusal must name both target ids: {stderr}"
    );
    // And it must NOT carry the secret: a refusal line that echoes the key
    // leaks it into logs, terminals and CI output.
    assert!(
        !stderr.contains("whsec_ALPHA_77x"),
        "the refusal must not echo the secret itself: {stderr}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn the_refusal_names_both_endpoints() {
    let dir = unique_dir("shared-secret-urls");
    let db = seed(&dir);
    let cfg = write_cfg(
        &dir,
        "wh.json",
        r#"{"targets":[
             {"id":"ta","url":"http://127.0.0.1:9101/hook","secret":"k"},
             {"id":"tb","url":"http://127.0.0.1:9102/hook","secret":"k"}
           ]}"#,
    );
    let out = status(&dir, &cfg, &db);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("9101") && stderr.contains("9102"),
        "the refusal must name both endpoints: {stderr}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn the_refusal_is_one_line_the_operator_can_act_on() {
    let dir = unique_dir("shared-secret-voice");
    let db = seed(&dir);
    let cfg = write_cfg(
        &dir,
        "wh.json",
        r#"{"targets":[
             {"id":"ta","url":"http://127.0.0.1:9101/hook","secret":"k"},
             {"id":"tb","url":"http://127.0.0.1:9102/hook","secret":"k"}
           ]}"#,
    );
    let out = status(&dir, &cfg, &db);
    let stderr = String::from_utf8_lossy(&out.stderr);
    let lines: Vec<&str> = stderr.lines().filter(|l| !l.trim().is_empty()).collect();
    assert_eq!(
        lines.len(),
        1,
        "the refusal must be ONE line, not a report: {stderr}"
    );
    assert!(
        stderr.contains("reeflog: error:"),
        "the refusal must carry reeflog's own prefix: {stderr}"
    );
    // It must say how to repair it — the family's rule: a refusal that does
    // not name a repair leaves the operator guessing.
    assert!(
        stderr.contains("fix:"),
        "the refusal must name a repair: {stderr}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_target_id_carrying_a_newline_cannot_forge_a_second_refusal_line() {
    let dir = unique_dir("shared-secret-forge");
    let db = seed(&dir);
    // The id carries a newline and the prefix of a real reeflog error line.
    // If the refusal echoed the id raw, the operator would read two lines —
    // the second one looking like a separate, real verdict.
    let cfg = write_cfg(
        &dir,
        "wh.json",
        "{\"targets\":[\
         {\"id\":\"ta\\nreeflog: error: forged\",\"url\":\"http://127.0.0.1:9101/hook\",\"secret\":\"k\"},\
         {\"id\":\"tb\",\"url\":\"http://127.0.0.1:9102/hook\",\"secret\":\"k\"}]}",
    );
    let out = status(&dir, &cfg, &db);
    let stderr = String::from_utf8_lossy(&out.stderr);
    let error_lines = stderr
        .lines()
        .filter(|l| l.starts_with("reeflog: error:"))
        .count();
    assert_eq!(
        error_lines, 1,
        "a newline in a target id must not forge a second error line: {stderr}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

// --- the guards -------------------------------------------------------------

#[test]
fn one_target_with_a_secret_still_works() {
    let dir = unique_dir("guard-single");
    let db = seed(&dir);
    let cfg = write_cfg(
        &dir,
        "wh.json",
        r#"{"targets":[{"id":"ta","url":"http://127.0.0.1:9101/hook","secret":"k"}]}"#,
    );
    let out = status(&dir, &cfg, &db);
    assert!(
        out.status.success(),
        "a single target must keep working: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn two_targets_with_distinct_secrets_still_work() {
    let dir = unique_dir("guard-distinct");
    let db = seed(&dir);
    let cfg = write_cfg(
        &dir,
        "wh.json",
        r#"{"targets":[
             {"id":"ta","url":"http://127.0.0.1:9101/hook","secret":"ka"},
             {"id":"tb","url":"http://127.0.0.1:9102/hook","secret":"kb"}
           ]}"#,
    );
    let out = status(&dir, &cfg, &db);
    assert!(
        out.status.success(),
        "two consumers with their own secrets must keep working: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn one_secret_on_two_targets_with_the_same_url_stays_legal() {
    let dir = unique_dir("guard-same-url");
    let db = seed(&dir);
    // One consumer, listed twice: nothing about it is forgeable, because
    // both rows point at the same endpoint.
    let cfg = write_cfg(
        &dir,
        "wh.json",
        r#"{"targets":[
             {"id":"ta","url":"http://127.0.0.1:9101/hook","secret":"k"},
             {"id":"tb","url":"http://127.0.0.1:9101/hook","secret":"k"}
           ]}"#,
    );
    let out = status(&dir, &cfg, &db);
    assert!(
        out.status.success(),
        "one consumer listed twice must keep working: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn three_targets_where_only_two_share_a_secret_are_refused() {
    let dir = unique_dir("guard-three");
    let db = seed(&dir);
    let cfg = write_cfg(
        &dir,
        "wh.json",
        r#"{"targets":[
             {"id":"ta","url":"http://127.0.0.1:9101/hook","secret":"k"},
             {"id":"tb","url":"http://127.0.0.1:9102/hook","secret":"k"},
             {"id":"tc","url":"http://127.0.0.1:9103/hook","secret":"other"}
           ]}"#,
    );
    let out = status(&dir, &cfg, &db);
    assert!(
        !out.status.success(),
        "the offending PAIR must be refused, even in a longer list"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("ta") && stderr.contains("tb"),
        "the refusal must name the offending pair: {stderr}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}
