//! Strict, per-command flag parsing for the reeflog CLI.
//!
//! Measured 2026-11-06 on the pre-fix tree (v0.5.0): a flag the CLI did not
//! recognize was silently DROPPED and the command ran anyway.
//!
//!   reeflog verify --db reeflog.db --pagesx 400   -> exit 3, scanned with the
//!                                                     DEFAULT page budget
//!   reeflog status --dbx nope.db                  -> exit 0, printed a healthy
//!                                                     report for reeflog.db
//!   reeflog status --db reeflog.db --bogus-flag   -> exit 0, identical output
//!   reeflog init --db probe.db --program <pk> --nope
//!                                                  -> exit 0, journal created
//!
//! Every one of those is the same class of defect, and it is the most
//! expensive kind this product can have: the tool says it did something it
//! did not do. A typo in `--pages` does not fail — it produces a scan with a
//! different budget than the operator asked for, and the output looks
//! exactly like a successful run. The stranger has no way to tell.
//!
//! The rule this module enforces, before any journal is opened or created:
//!
//! * a flag the command does not know is a REFUSAL, with the likely intent
//!   named when the typo is close to a real flag;
//! * a flag that is missing its value is a REFUSAL (including a value that
//!   is itself a flag — `--db --program` must not become a path called
//!   `--program`);
//! * a flag given twice is a REFUSAL, not a silent pick of one of them;
//! * an argument that is not a flag is a REFUSAL;
//! * `--flag=value` is honoured instead of falling back to the default.
//!
//! The guards matter as much as the refusals: every documented invocation,
//! the bare `--anchor-slot` (optional value), the boolean switches and the
//! global `--help` / `--version` must keep working. A strict parser that
//! refuses a documented flag is worse than the defect it fixes.

/// Boolean flags: present or absent, never a value.
const SWITCHES: &[&str] = &[
    "--accounts",
    "--owners",
    "--account-data",
    "--no-logs",
    "--with-accounts",
];

/// Flags whose value may be omitted; the bare form is a documented meaning
/// of its own (`--anchor-slot` = anchor at the journal's own window).
const OPTIONAL_VALUE: &[&str] = &["--anchor-slot"];

/// How many leading positional words the command consumes, and which flags
/// it accepts. `None` means the command itself is unknown — the caller prints
/// the usage text, exactly as an unknown subcommand always has.
pub fn split(args: &[String]) -> Option<(usize, &'static [&'static str])> {
    let cmd = args.first()?.as_str();
    let sub = args.get(1).map(|s| s.as_str());
    let sub2 = args.get(2).map(|s| s.as_str());

    match cmd {
        "init" => Some((1, &["--db", "--program"])),
        "start" => Some((
            1,
            &[
                "--db",
                "--program",
                "--ws",
                "--rpc",
                "--seconds",
                "--seal-every",
                "--reorg-every",
                "--reorg-limit",
                "--reorg-lookback",
                "--repair-every",
                "--repair-pages",
                "--repair-lookback",
                "--payload-limit",
                "--webhook-config",
                "--webhook-every",
                "--webhook-limit",
            ],
        )),
        "seal" => Some((1, &["--db", "--rpc"])),
        "verify" | "repair" => Some((
            1,
            &[
                "--db",
                "--program",
                "--rpc",
                "--pages",
                "--since-slot",
                "--anchor-slot",
                "--anchor-tries",
            ],
        )),
        "hydrate" => Some((
            1,
            &[
                "--db",
                "--rpc",
                "--limit",
                "--accounts",
                "--owners",
                "--account-data",
                "--owners-batch",
                "--account-data-batch",
            ],
        )),
        "tail" => Some((
            1,
            &["--db", "--cursor", "--limit", "--no-logs", "--with-accounts"],
        )),
        "status" => Some((1, &["--db"])),
        "reorg" => match sub? {
            "check" => Some((2, &["--db", "--rpc", "--limit"])),
            "tail" => Some((2, &["--db", "--cursor", "--limit"])),
            _ => None,
        },
        "webhook" => match sub? {
            "push" => Some((2, &["--db", "--config", "--limit"])),
            "status" => Some((2, &["--db", "--config"])),
            "dlq" => match sub2? {
                "list" | "replay" => Some((3, &["--db", "--config", "--limit", "--target"])),
                _ => None,
            },
            _ => None,
        },
        _ => None,
    }
}

/// Validate the flag region of a command line and normalize it.
///
/// Returns the tokens rebuilt as `--flag value` pairs (so `--db=x` becomes
/// `--db x`), or the refusal the operator must see instead of a run that
/// silently did something else.
pub fn normalize(allowed: &'static [&'static str], tokens: &[String]) -> Result<Vec<String>, String> {
    let mut out: Vec<String> = Vec::new();
    let mut seen: Vec<&'static str> = Vec::new();
    let mut i = 0;

    while i < tokens.len() {
        let tok = tokens[i].as_str();
        if !tok.starts_with("--") {
            return Err(format!(
                "unexpected argument `{tok}` — this command takes flags only"
            ));
        }

        let (name, inline) = match tok.find('=') {
            Some(p) => (&tok[..p], Some(tok[p + 1..].to_string())),
            None => (tok, None),
        };

        let flag = match allowed.iter().find(|f| **f == name) {
            Some(f) => *f,
            None => return Err(unknown_flag(name, allowed)),
        };
        if seen.contains(&flag) {
            return Err(format!(
                "flag {flag} given more than once — pass it once"
            ));
        }
        seen.push(flag);

        if is_switch(flag) {
            if let Some(v) = inline {
                return Err(format!(
                    "{flag} is a switch and does not take a value (got `{v}`)"
                ));
            }
            out.push(flag.to_string());
            i += 1;
            continue;
        }

        // `--anchor-slot` takes an OPTIONAL value: `--anchor-slot 400` pins the
        // anchor at a slot, a bare `--anchor-slot` anchors at the journal's own
        // window — a documented meaning, so a strict parser must not demand a
        // value here. The next token is the value only when it is not a flag.
        if is_optional_value(flag) {
            out.push(flag.to_string());
            match inline {
                Some(v) if !v.is_empty() => out.push(v),
                Some(_) => return Err(format!("flag {flag} needs a value")),
                None => {
                    if let Some(v) = optional_value_consume(tokens, i) {
                        out.push(v);
                        i += 1;
                    }
                }
            }
            i += 1;
            continue;
        }

        let value = match inline {
            Some(v) if !v.is_empty() => v,
            Some(_) => return Err(format!("flag {flag} needs a value")),
            None => match tokens.get(i + 1) {
                // A value that looks like another flag is the `--db --program`
                // case: taking it would create a path named `--program`.
                Some(next) if !next.starts_with("--") => {
                    i += 1;
                    next.clone()
                }
                _ => return Err(format!("flag {flag} needs a value")),
            },
        };
        out.push(flag.to_string());
        out.push(value);
        i += 1;
    }

    Ok(out)
}

fn is_switch(flag: &str) -> bool {
    SWITCHES.contains(&flag)
}

fn is_optional_value(flag: &str) -> bool {
    OPTIONAL_VALUE.contains(&flag)
}

/// `--anchor-slot` takes an OPTIONAL value: `--anchor-slot 400` pins a slot
/// and a bare `--anchor-slot` anchors at the journal's own window. It is
/// pulled out of the flag region only when a non-flag token follows it.
pub fn optional_value_consume(tokens: &[String], i: usize) -> Option<String> {
    if i + 1 >= tokens.len() || tokens[i + 1].starts_with("--") {
        None
    } else {
        Some(tokens[i + 1].clone())
    }
}

fn unknown_flag(name: &str, allowed: &[&'static str]) -> String {
    match closest(name, allowed) {
        Some(s) => format!("unknown flag {name} (did you mean {s}?)"),
        None => format!("unknown flag {name}"),
    }
}

/// The nearest accepted flag to a typo, when one is close enough to be a
/// useful hint. `--pagesx` must point at `--pages`; `--bogus-flag` must not
/// point at anything, because a wrong guess is worse than none.
fn closest(name: &str, allowed: &[&'static str]) -> Option<&'static str> {
    let mut best: Option<(usize, &'static str)> = None;
    for cand in allowed {
        let d = distance(name, cand);
        if d > 3 {
            continue;
        }
        if best.map(|(bd, _)| d < bd).unwrap_or(true) {
            best = Some((d, cand));
        }
    }
    best.map(|(_, c)| c)
}

fn distance(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    let mut cur = vec![0usize; b.len() + 1];
    for (i, ca) in a.iter().enumerate() {
        cur[0] = i + 1;
        for (j, cb) in b.iter().enumerate() {
            let sub = prev[j] + usize::from(ca != cb);
            cur[j + 1] = sub.min(prev[j + 1] + 1).min(cur[j] + 1);
        }
        std::mem::swap(&mut prev, &mut cur);
    }
    prev[b.len()]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn an_unknown_flag_is_refused_and_names_the_intent() {
        let allowed = &["--db", "--pages"];
        let err = normalize(allowed, &t(&["--pagesx", "400"])).unwrap_err();
        assert!(err.contains("--pagesx"), "{err}");
        assert!(err.contains("--pages"), "{err}");
    }

    #[test]
    fn a_far_away_typo_gets_no_suggestion() {
        let err = normalize(&["--db"], &t(&["--bogus-flag"])).unwrap_err();
        assert!(err.contains("--bogus-flag"), "{err}");
        assert!(!err.contains("did you mean"), "{err}");
    }

    #[test]
    fn inline_form_is_normalized_to_a_pair() {
        let out = normalize(&["--db"], &t(&["--db=other.db"])).unwrap();
        assert_eq!(out, t(&["--db", "other.db"]));
    }

    #[test]
    fn a_flag_followed_by_a_flag_is_missing_its_value() {
        let err = normalize(&["--db", "--program"], &t(&["--db", "--program"])).unwrap_err();
        assert!(err.contains("--db"), "{err}");
    }

    #[test]
    fn duplicates_are_refused() {
        let err = normalize(&["--db"], &t(&["--db", "a.db", "--db", "b.db"])).unwrap_err();
        assert!(err.contains("--db"), "{err}");
    }

    #[test]
    fn a_stray_positional_is_refused() {
        let err = normalize(&["--db"], &t(&["--db", "x", "extra"])).unwrap_err();
        assert!(err.contains("extra"), "{err}");
    }

    #[test]
    fn a_switch_refuses_an_inline_value() {
        let err = normalize(&["--no-logs"], &t(&["--no-logs=true"])).unwrap_err();
        assert!(err.contains("--no-logs"), "{err}");
    }

    #[test]
    fn command_split_covers_the_documented_surface() {
        let a = split(&t(&["status"])).expect("status known");
        assert_eq!(a.1, &["--db"]);
        let a = split(&t(&["reorg", "check", "--db", "x"])).expect("reorg check known");
        assert_eq!(a.0, 2);
        let a = split(&t(&["webhook", "dlq", "replay"])).expect("dlq replay known");
        assert_eq!(a.0, 3);
        assert!(split(&t(&["frobnicate"])).is_none());
        assert!(split(&t(&["reorg", "bogus"])).is_none());
    }
}
