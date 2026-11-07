//! The README must never document a line the strict flag parser refuses.
//!
//! Why this test exists (2026-11-07): reeflog 9dbd393 introduced a strict
//! per-command flag parser (shipped in v0.5.1). Its own guard —
//! `every_documented_flag_still_works` in tests/arg_parsing.rs — is a
//! HAND-WRITTEN list of invocations. That list drifts from the document: it
//! proves the parser accepts the cases somebody remembered to write down,
//! not the lines the README actually shows. src/flags.rs states the stakes
//! itself: "a strict parser that refuses a documented flag is worse than the
//! defect it fixes".
//!
//! So the claim is checked against the DOCUMENT, not against a list:
//!
//!   * every `reeflog ...` line in a fenced bash/sh block of README.md is
//!     extracted;
//!   * a line continuation (`... \`) joins with the next line;
//!   * an optional bracket group `[--flag value]` is checked in BOTH forms —
//!     with and without the group — because both are documented;
//!   * placeholders are substituted (`<pubkey>` -> a real address, `<slot>`
//!     -> a number), so no documented line is skipped for being a template;
//!   * the extracted flag region is run through the SAME `flags::normalize`
//!     the binary uses, so this test cannot pass while the binary refuses
//!     the line.
//!
//! Falsified by construction: removing `--limit` from the `reorg check`
//! allowlist turns README line 617 (`reeflog reorg check --db reeflog.db
//! --limit 200`) into a refusal, and this test goes red on it.

use std::fs;
use std::path::PathBuf;

const PROGRAM: &str = "whirLbMiicVdio4qvUfM5KAg6Ct8VwpYzGff3uctyCc";

fn readme() -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("README.md");
    fs::read_to_string(&path).expect("README.md is readable")
}

/// Every `reeflog ...` command line in a fenced bash/sh block, with the
/// line number it was found on.
fn documented_commands(readme: &str) -> Vec<(usize, String)> {
    let mut out = Vec::new();
    let mut in_fence = false;
    let mut fence_lang = String::new();
    let lines: Vec<&str> = readme.lines().collect();

    for (i, line) in lines.iter().enumerate() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("```") {
            if !in_fence {
                in_fence = true;
                fence_lang = trimmed.trim_start_matches('`').trim().to_string();
            } else {
                in_fence = false;
                fence_lang.clear();
            }
            continue;
        }
        if !in_fence {
            continue;
        }
        if !fence_lang.is_empty()
            && !["bash", "sh", "shell"].contains(&fence_lang.as_str())
        {
            continue;
        }
        let Some(idx) = line.find("reeflog ") else {
            continue;
        };
        let mut joined = line[idx..].trim().to_string();
        let mut j = i;
        loop {
            let trailing_backslash = joined.trim_end().ends_with('\\');
            let next_line = lines.get(j + 1).map(|n| n.trim());
            let bracket_continuation =
                next_line.map_or(false, |n| n.starts_with('['));
            if !trailing_backslash && !bracket_continuation {
                break;
            }
            let Some(next) = next_line else { break };
            if next.is_empty() {
                break;
            }
            let next = next.trim_end_matches('\\').trim();
            joined = format!("{} {}", joined.trim_end_matches('\\').trim(), next)
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ");
            j += 1;
            if !next.ends_with('\\') && !next.starts_with('[') {
                break;
            }
        }
        // Strip a trailing comment.
        if let Some(hash) = joined.find('#') {
            joined = joined[..hash].to_string();
        }
        let joined = joined.replace('\\', " ");
        let joined = joined.split_whitespace().collect::<Vec<_>>().join(" ");
        if joined.starts_with("reeflog ") {
            out.push((i + 1, joined));
        }
    }
    out
}

/// Expand optional bracket groups into every documented form: a group is
/// present or absent. Placeholders are substituted, never skipped.
fn expand(cmd: &str) -> Vec<String> {
    fn variants(cmd: &str) -> Option<Vec<String>> {
        let start = cmd.find('[')?;
        let end = cmd[start..].find(']')? + start;
        let inner = cmd[start + 1..end].trim();
        let without = format!("{} {}", &cmd[..start], &cmd[end + 1..])
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        let with = format!("{} {}", &cmd[..start], inner)
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        let mut out = variants(&without).unwrap_or_else(|| vec![without.clone()]);
        out.push(with);
        Some(out)
    }

    let mut forms = vec![cmd.to_string()];
    let mut guard = 0;
    while forms.iter().any(|c| c.contains('[')) && guard < 8 {
        guard += 1;
        let mut next = Vec::new();
        for f in &forms {
            match variants(f) {
                Some(v) => next.extend(v),
                None => next.push(f.clone()),
            }
        }
        forms = next;
    }    forms
        .into_iter()
        .map(|f| {
            f.replace("<pubkey>", PROGRAM)
                .replace("<slot>", "1")
                .replace('<', "x")
                .replace('>', "x")
        })
        .collect()
}

/// The command words of a line: everything before the first flag token,
/// with the leading `reeflog` dropped — `flags::split` expects the arguments
/// after the program name, exactly as `main.rs` passes them.
fn command_words(tokens: &[&str]) -> Vec<String> {
    tokens
        .iter()
        .skip(1)
        .take_while(|t| !t.starts_with("--"))
        .map(|t| t.to_string())
        .collect()
}

#[test]
fn every_documented_line_is_accepted_by_the_flag_parser() {
    let readme = readme();
    let documented = documented_commands(&readme);
    assert!(
        !documented.is_empty(),
        "no documented `reeflog` command was found — the extractor is broken, \
         and a broken extractor would make this test pass vacuously"
    );

    let mut checked = 0usize;
    for (line_no, cmd) in &documented {
        for form in expand(cmd) {
            let tokens: Vec<&str> = form.split_whitespace().collect();
            if tokens.first() != Some(&"reeflog") {
                continue;
            }
            let words = command_words(&tokens);
            let Some((positionals, allowed)) = reeflog::flags::split(&words) else {
                panic!("README line {line_no}: `{form}` names a command the CLI does not know");
            };
            let flag_region = &tokens[1 + positionals..];
            if flag_region.is_empty() {
                continue;
            }
            let owned: Vec<String> = flag_region.iter().map(|s| s.to_string()).collect();
            if let Err(e) = reeflog::flags::normalize(allowed, &owned) {
                panic!(
                    "README line {line_no} documents a line the CLI REFUSES:\n  {form}\n  -> {e}\n\
                     A strict parser that refuses a documented line is worse than the defect it fixes."
                );
            }
            checked += 1;
        }
    }
    assert!(
        checked >= 30,
        "only {checked} documented invocations were checked — the extractor \
         must not silently shrink its own coverage"
    );
}

#[test]
fn the_extraction_finds_the_documented_surface() {
    // A guard for the guard: if the README's shape changes, the extraction
    // must change WITH it — never shrink quietly into "nothing to check".
    let readme = readme();
    let documented = documented_commands(&readme);
    assert!(
        documented.len() >= 20,
        "expected the README to document many reeflog invocations, found {}",
        documented.len()
    );
    let joined = documented
        .iter()
        .map(|(_, c)| c.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    for expected in [
        "reeflog init",
        "reeflog start",
        "reeflog tail",
        "reeflog status",
        "reeflog verify",
        "reeflog reorg check",
        "reeflog reorg tail",
        "reeflog webhook push",
        "reeflog hydrate",
    ] {
        assert!(
            joined.contains(expected),
            "the documented surface no longer contains `{expected}` — the \
             extractor or the README changed; re-check the expectations"
        );
    }
}

#[test]
fn a_removed_flag_is_caught() {
    // The falsification of this test, as a unit test: an allowlist without
    // `--limit` must make the documented `reorg check --limit 200` line fail.
    let allowed: &[&str] = &["--db", "--rpc"];
    let tokens: Vec<String> = ["--db", "reeflog.db", "--limit", "200"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    let err = reeflog::flags::normalize(allowed, &tokens).unwrap_err();
    assert!(
        err.contains("--limit"),
        "the refusal must name the offending flag, got: {err}"
    );
}
