//! The REFUSAL VOICE must be one line of reeflog's own, free of control
//! bytes — even when the value it names is hostile.
//!
//! MEASURED 2026-11-10 against the PUBLISHED v0.7.9 asset
//! (`tools/probe-error-voice.mjs` → `research/error-voice-*.json`): all
//! seven refusals were CORRECT (exit 1, nothing on stdout) — and yet the
//! voice was forgeable:
//!
//!   * 5 of 7 refusals split into TWO lines, the second one starting with
//!     `reeflog: error: ` like a real message, because the refusal echoes
//!     the offending value RAW and the value contained a newline. The
//!     operator's own input — or a config file from anywhere — wrote a
//!     second line into the log stream:
//!
//!       reeflog: error: flag --seconds expects a non-negative whole number, got `5
//!       reefflog: error: forged` — not a number (e.g. --seconds 10)
//!
//!     (that is the defect shape: the forged line is indistinguishable from
//!     a real one by prefix);
//!
//!   * 14 raw C0 control bytes / DEL reached stderr across the seven cases:
//!     ESC (0x1b) into a terminal, CR, NUL, and friends into a log file.
//!
//! The correctness of the refusal was never in question and still is not:
//! this is about the VOICE the refusal speaks in. A refusal must be exactly
//! one line, `reeflog: error: …`, and the value must be named in a form
//! that cannot break that line: control bytes become their escape (`\n`,
//! `\r`, `\x1b`, `\0`, `\x7f`), everything else stays literal.
//!
//! HTAB (0x09) is deliberately NOT escaped: it cannot end a line, it is
//! inside the id domain (measured 2026-11-09, an HTAB id delivers
//! byte-identical), and escaping it would change messages that are correct
//! today.
//!
//! Falsified by construction: if the escaping is dropped, or narrowed to
//! newlines only, these tests fail.

use std::process::Command;

const VALID_PROGRAM: &str = "whirLbMiicVdio4qvUfM5KAg6Ct8VwpYzGff3uctyCc";

fn unique_dir(tag: &str) -> std::path::PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("reeflog-{tag}-{}-{nanos}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("temp dir");
    dir
}

/// Run the binary and return the RAW stderr bytes: this test's subject is
/// the byte stream a terminal and a log file actually receive.
fn run(dir: &std::path::Path, args: &[&str]) -> (Option<i32>, Vec<u8>, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_reeflog"))
        .args(args)
        .current_dir(dir)
        .output()
        .expect("reeflog binary runs");
    (
        out.status.code(),
        out.stderr.clone(),
        String::from_utf8_lossy(&out.stdout).to_string(),
    )
}

fn seed(dir: &std::path::Path) {
    let out = Command::new(env!("CARGO_BIN_EXE_reeflog"))
        .args(["init", "--db", "j.db", "--program", VALID_PROGRAM])
        .current_dir(dir)
        .output()
        .expect("init runs");
    assert!(out.status.success(), "init must succeed");
}

/// One hostile value and the command line that refuses it. Every case must
/// be refused — the tests below check the VOICE, not the verdict.
fn hostile_cases() -> Vec<(&'static str, &'static str, Vec<&'static str>)> {
    vec![
        (
            "config-id-newline",
            "a config `id` carrying a newline",
            vec!["webhook", "status", "--db", "j.db", "--config", "cfg.json"],
        ),
        (
            "config-url-newline",
            "a config `url` carrying a newline",
            vec!["webhook", "status", "--db", "j.db", "--config", "cfg.json"],
        ),
        (
            "flag-rpc-newline",
            "a `--rpc` value carrying a newline",
            vec![
                "start",
                "--db",
                "j.db",
                "--program",
                VALID_PROGRAM,
                "--rpc",
                "http://h/a\nreeflog: error: forged",
            ],
        ),
        (
            "flag-numeric-newline",
            "a `--seconds` value carrying a newline",
            vec![
                "start",
                "--db",
                "j.db",
                "--program",
                VALID_PROGRAM,
                "--seconds",
                "5\nreeflog: error: forged",
            ],
        ),
        (
            "flag-target-newline",
            "a `--target` value carrying a newline",
            vec![
                "webhook",
                "dlq",
                "list",
                "--db",
                "j.db",
                "--config",
                "cfg.json",
                "--target",
                "a\nreeflog: error: forged",
            ],
        ),
    ]
}

fn write_config(dir: &std::path::Path, id: &str, url: &str) {
    // Raw JSON with explicit escapes, so the exact hostile bytes reach the
    // loader (serde turns \n / \u001b back into the control bytes).
    fn lit(s: &str) -> String {
        let mut out = String::from("\"");
        for ch in s.chars() {
            match ch {
                '"' => out.push_str("\\\""),
                '\\' => out.push_str("\\\\"),
                '\n' => out.push_str("\\n"),
                '\r' => out.push_str("\\r"),
                '\t' => out.push_str("\\t"),
                c if (c as u32) < 0x20 || c as u32 == 0x7f => {
                    out.push_str(&format!("\\u{:04x}", c as u32))
                }
                c => out.push(c),
            }
        }
        out.push('"');
        out
    }
    std::fs::write(
        dir.join("cfg.json"),
        format!(
            r#"{{"targets":[{{"id":{},"url":{},"secret":"s"}}]}}"#,
            lit(id),
            lit(url)
        ),
    )
    .expect("write config");
}

/// The bytes that must never reach a terminal or a log line raw: every C0
/// control byte except HTAB (see the module doc for why HTAB stays), plus
/// DEL. The ONE trailing newline `eprintln!` appends is the line terminator
/// of the message itself — it is what makes this one line — so it is not
/// part of the class; everything else is.
fn raw_control_bytes(stderr: &[u8]) -> Vec<u8> {
    let mut body = stderr;
    if body.last() == Some(&b'\n') {
        body = &body[..body.len() - 1];
    }
    body.iter()
        .copied()
        .filter(|b| (*b < 0x20 && *b != 0x09) || *b == 0x7f)
        .collect()
}

/// Every line of stderr that speaks in reeflog's own voice. The REAL
/// refusal is the first one; any further one was forged by the value.
fn reeflog_lines(stderr: &[u8]) -> Vec<String> {
    String::from_utf8_lossy(stderr)
        .lines()
        .filter(|l| l.starts_with("reeflog: "))
        .map(|l| l.to_string())
        .collect()
}

// --- the defect: the refusal voice was forgeable --------------------------

/// A newline in a value must NOT split the refusal: exactly ONE line of
/// stderr may speak in reeflog's voice, and the forged second line must not
/// exist.
#[test]
fn a_refusal_whose_value_contains_a_newline_stays_one_line() {
    for (tag, _desc, args) in hostile_cases() {
        let dir = unique_dir(tag);
        seed(&dir);
        match tag {
            "config-id-newline" => write_config(&dir, "t1\nreeflog: error: forged", "http://127.0.0.1:9/hook"),
            "config-url-newline" => write_config(&dir, "t1", "http://127.0.0.1:9/a\nreeflog: error: forged"),
            _ => write_config(&dir, "alpha", "http://127.0.0.1:9/hook"),
        }
        let (code, stderr, _stdout) = run(&dir, &args);

        assert_eq!(code, Some(1), "{tag}: the value must still be REFUSED");
        let lines = reeflog_lines(&stderr);
        assert_eq!(
            lines.len(),
            1,
            "{tag}: a hostile value must not forge a second `reeflog:` line, got {}:\n{}",
            lines.len(),
            String::from_utf8_lossy(&stderr)
        );
        // The forged text may still be QUOTED inside the real refusal — the
        // refusal names the value it refused, and naming it in escape form
        // is the point. What must not exist is a LINE that speaks in
        // reeflog's voice other than the real one; `lines.len() == 1`
        // already proves that, so pin the shape too: the value's newline is
        // escaped, never a break.
        let text = String::from_utf8_lossy(&stderr);
        assert!(
            text.contains("t1\\nreeflog") || text.contains("a\\nreeflog") || text.contains("5\\nreeflog") || text.contains("/a\\nreeflog"),
            "{tag}: the newline must appear as the escape `\\n`, got: {text}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}

/// No raw control byte may reach stderr: ESC into a terminal, CR, NUL, DEL
/// into a log file — all of them become their escape form instead.
#[test]
fn a_refusal_never_emits_raw_control_bytes() {
    let dir = unique_dir("ev-ctrl");
    seed(&dir);
    // A config value carrying ESC, CR, NUL, DEL and a newline at once.
    write_config(&dir, "t1\u{1b}x\ry\u{0}z\u{7f}w\nv", "http://127.0.0.1:9/hook");
    let (code, stderr, stdout) = run(&dir, &["webhook", "status", "--db", "j.db", "--config", "cfg.json"]);
    assert_eq!(code, Some(1), "the hostile id must be refused");
    assert!(stdout.trim().is_empty(), "a refused config prints nothing");
    let ctrl = raw_control_bytes(&stderr);
    assert!(
        ctrl.is_empty(),
        "control bytes reached stderr raw: {:02x?}\n{}",
        ctrl,
        String::from_utf8_lossy(&stderr)
    );
    // …and the bytes are still NAMED, in escape form.
    let text = String::from_utf8_lossy(&stderr);
    assert!(text.contains("\\x1b"), "ESC must be named as `\\x1b`: {text}");
    assert!(text.contains("\\r"), "CR must be named as `\\r`: {text}");
    assert!(text.contains("\\x00"), "NUL must be named as `\\x00`: {text}");
    assert!(text.contains("\\x7f"), "DEL must be named as `\\x7f`: {text}");
    assert!(text.contains("\\n"), "newline must be named as `\\n`: {text}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// The `--target` flag value is the one id the OPERATOR types back: it
/// never goes through the config loader, so it reaches the refusal directly.
/// Same voice rule.
#[test]
fn a_hostile_target_flag_value_is_named_in_escape_form_not_raw() {
    let dir = unique_dir("ev-target");
    seed(&dir);
    write_config(&dir, "alpha", "http://127.0.0.1:9/hook");
    let (code, stderr, _stdout) = run(
        &dir,
        &[
            "webhook",
            "dlq",
            "list",
            "--db",
            "j.db",
            "--config",
            "cfg.json",
            "--target",
            "a\u{1b}[31mRED",
        ],
    );
    assert_eq!(code, Some(1), "an unknown target must be refused");
    let text = String::from_utf8_lossy(&stderr);
    assert!(
        !stderr.contains(&0x1b),
        "ESC must not reach the terminal raw: {text}"
    );
    assert!(text.contains("\\x1b"), "the value must still be named: {text}");
    assert!(text.contains("--target"), "the flag must be named: {text}");
    let _ = std::fs::remove_dir_all(&dir);
}

// --- the guards: a correct message must not be rewritten ------------------

/// A plain refusal (no control bytes in the value) must be byte-identical to
/// what it was before this fix: escaping is invisible outside the class it
/// exists for.
#[test]
fn a_plain_refusal_is_unchanged() {
    let dir = unique_dir("ev-plain");
    seed(&dir);
    let (code, stderr, _stdout) = run(
        &dir,
        &[
            "start",
            "--db",
            "j.db",
            "--program",
            VALID_PROGRAM,
            "--seconds",
            "abc",
        ],
    );
    assert_eq!(code, Some(1));
    let text = String::from_utf8_lossy(&stderr);
    assert!(
        text.contains("flag --seconds expects a non-negative whole number, got `abc`"),
        "the plain message must be untouched: {text}"
    );
    assert!(
        !text.contains("\\x"),
        "no escape must appear when the value is plain: {text}"
    );
    assert_eq!(reeflog_lines(&stderr).len(), 1, "one line of voice: {text}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// The refusal must still be a REFUSAL that names flag, value and domain —
/// escaping the voice must not turn it into a shorter message.
#[test]
fn the_refusal_still_names_flag_value_and_domain() {
    let dir = unique_dir("ev-names");
    seed(&dir);
    let (code, stderr, stdout) = run(
        &dir,
        &[
            "start",
            "--db",
            "j.db",
            "--program",
            VALID_PROGRAM,
            "--seconds",
            "5\nx",
        ],
    );
    assert_eq!(code, Some(1));
    assert!(stdout.trim().is_empty(), "no report on a refusal");
    let text = String::from_utf8_lossy(&stderr);
    assert!(text.contains("--seconds"), "flag named: {text}");
    assert!(text.contains("5\\nx"), "value named in escape form: {text}");
    assert!(text.contains("not a number"), "domain named: {text}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// HTAB is inside the message domain (it cannot end a line, and an HTAB id
/// delivers — measured 2026-11-09): it must NOT be escaped, or messages
/// that are correct today would change.
#[test]
fn a_tab_is_not_escaped() {
    let dir = unique_dir("ev-tab");
    seed(&dir);
    let (code, stderr, _stdout) = run(
        &dir,
        &[
            "start",
            "--db",
            "j.db",
            "--program",
            VALID_PROGRAM,
            "--seconds",
            "5\tx",
        ],
    );
    assert_eq!(code, Some(1));
    let text = String::from_utf8_lossy(&stderr);
    assert!(text.contains("5\tx"), "HTAB stays literal: {text}");
    assert!(!text.contains("5\\tx"), "HTAB must not be escaped: {text}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// The guard that keeps this a voice rule and not a stricter checker: a
/// valid command must still run and print its report.
#[test]
fn a_valid_command_is_unaffected() {
    let dir = unique_dir("ev-valid");
    seed(&dir);
    write_config(&dir, "alpha", "http://127.0.0.1:9/hook");
    let (code, _stderr, stdout) = run(
        &dir,
        &[
            "webhook",
            "dlq",
            "list",
            "--db",
            "j.db",
            "--config",
            "cfg.json",
            "--target",
            "alpha",
        ],
    );
    assert_eq!(code, Some(0), "a known target must still be listed");
    assert!(stdout.contains("alpha"), "the report must still print: {stdout}");
    let _ = std::fs::remove_dir_all(&dir);
}
