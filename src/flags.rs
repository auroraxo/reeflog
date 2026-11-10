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
//!
//! Value kinds (measured 2026-11-08 on the published v0.7.1 tree):
//!
//!   reeflog tail --db p.db --cursor c.txt --limit abc  -> exit 0, the
//!     DEFAULT limit (1000) was used and every row it held was delivered;
//!   reeflog tail --db p.db --cursor c.txt --limit -1   -> exit 0, SQL's
//!     `LIMIT -1` means NO limit at all — the whole journal, delivered;
//!   reeflog verify --db p.db --pages abc               -> the DEFAULT page
//!     budget (40) was used and the scan reported itself as if asked for.
//!
//! Refusing an unknown FLAG while letting a known flag's VALUE fall back to
//! the default is the same defect half-fixed: the operator asked for 5 events
//! and the tool delivered every row it had, with an exit code that says the
//! run was fine. So the parser also classifies the value it is handed: a
//! numeric flag must carry a non-negative whole number, and anything else is
//! refused with the flag and the offending value named — before any journal
//! is opened, exactly like the flag refusals above.
//!
//! The next layer up (measured 2026-11-08 on the published v0.7.2 tree,
//! 1200 planted rows): "a non-negative whole number" is not one number
//! space. The CHECKER parses `u64`, the CONSUMERS parse `i64` or add the
//! value to a clock, so values that sat inside the checker's space and
//! outside the consumer's space fell straight back into the same silent
//! default — the exact defect this parser claims to close, one layer up:
//!
//!   reeflog tail --limit 18446744073709551615   -> exit 0, 1000 rows (the
//!     DEFAULT limit): the `i64` parse failed and the failure became the
//!     default;
//!   reeflog verify --since-slot 9223372036854775808
//!                                                -> exit 3 with
//!     `"since_slot": null` — the operator's window was DROPPED and the
//!     scan ran over a different window than the one asked for;
//!   reeflog start --seconds 18446744073709551615
//!                                                -> panic "overflow when
//!     adding duration to instant" (src/main.rs:534);
//!   reeflog start --repair-every 18446744073709551615
//!                                                -> the scheduler panics
//!     on the runtime worker (main.rs:657) while main awaits a deadline
//!     that never comes: the daemon HANGS instead of refusing.
//!
//! So the value is checked against the domain its CONSUMER uses:
//!
//! * every numeric flag must fit signed 64-bit — the narrowest consumer
//!   space (SQLite's LIMIT binds i64, slot arithmetic is i64);
//! * a cadence in seconds must additionally be schedulable on this clock:
//!   `Instant::now() + Duration` has a finite range and a value outside it
//!   is a panic or a hang, not a schedule;
//! * either way the refusal names the flag, the value and the reason,
//!   before any journal is opened.

/// Boolean flags: present or absent, never a value.
const SWITCHES: &[&str] = &[
    "--accounts",
    "--owners",
    "--account-data",
    "--no-logs",
    "--with-accounts",
];

/// Flags whose value must be a non-negative whole number. `0` stays legal
/// because several flags document it as "disable this pass"
/// (`--payload-limit 0`, `--repair-pages 0`); a negative value is refused
/// because `--limit -1` is not "the last row", it is "no limit at all".
const NUMERIC: &[&str] = &[
    "--limit",
    "--pages",
    "--seconds",
    "--since-slot",
    "--anchor-slot",
    "--anchor-tries",
    "--repair-every",
    "--repair-pages",
    "--repair-lookback",
    "--payload-limit",
    "--seal-every",
    "--reorg-every",
    "--reorg-limit",
    "--reorg-lookback",
    "--webhook-every",
    "--webhook-limit",
    "--owners-batch",
    "--account-data-batch",
];

fn is_numeric(flag: &str) -> bool {
    NUMERIC.contains(&flag)
}

/// Cadence flags: the value is a count of SECONDS that gets added to a clock
/// (`Instant::now() + Duration::from_secs(v)`), so it must not only parse —
/// it must be schedulable. Everything else is a count/slot/index, bounded by
/// the signed 64-bit space its consumer binds into.
const DURATION_FLAGS: &[&str] = &[
    "--seconds",
    "--repair-every",
    "--seal-every",
    "--reorg-every",
    "--webhook-every",
];

/// The largest count/slot value a consumer can hold: SQLite's LIMIT binds
/// i64, slots are i64, and the `parse::<i64>` calls in main.rs have this as
/// their natural ceiling. The checker used to accept up to `u64::MAX`, and
/// everything above this line silently became the consumer's default.
const MAX_COUNT: u64 = i64::MAX as u64;

/// A numeric flag's value: a non-negative whole number INSIDE the domain the
/// consumer uses, nothing else. `+5`, `5x`, `abc`, `-1` and the empty string
/// are refusals — the parser must never guess what the operator meant by
/// them. A number outside the consumer's space is the same refusal class:
/// accepting it only moves the guess to the consumer, where it becomes a
/// default (the operator asked for everything and got 1000 rows) or a panic.
fn check_numeric_value(flag: &str, value: &str) -> Result<(), String> {
    if !is_numeric(flag) {
        return Ok(());
    }
    let n = match value.parse::<u64>() {
        Ok(n) => n,
        Err(_) => {
            return Err(format!(
                "flag {flag} expects a non-negative whole number, got `{value}` — not a number (e.g. {flag} 10)"
            ));
        }
    };
    if n > MAX_COUNT {
        return Err(format!(
            "flag {flag} expects a non-negative whole number no greater than {MAX_COUNT}, got `{value}` — out of range for this flag (e.g. {flag} 10)"
        ));
    }
    if DURATION_FLAGS.contains(&flag)
        && std::time::Instant::now()
            .checked_add(std::time::Duration::from_secs(n))
            .is_none()
    {
        return Err(format!(
            "flag {flag} expects a delay this clock can schedule, got `{value}` — too large to add to the current time (e.g. {flag} 30)"
        ));
    }
    Ok(())
}

/// Flags whose value must be an absolute URL of a scheme the CONSUMER can
/// speak, with a host. Measured 2026-11-09 on the published v0.7.5 tree:
/// these two flags had NO domain in the parser at all, while the webhook
/// target URL (a config file, one layer further from the operator) already
/// required `http://` or `https://` at load.
///
/// The domains are the consumers', not an opinion:
///
/// * `--rpc` is POSTed to by `ureq`, which resolves only `http`/`https`
///   (ureq-proto's `SchemeExt::default_port` returns `None` for anything
///   else, and the resolver reports "unknown scheme") — `ftp://`, `ws://`
///   and a bare `not-a-url` all reached the transport and failed there,
///   AFTER the journal was opened, in ureq's words rather than reeflog's;
/// * `--ws` is handed to `tokio-tungstenite`, whose
///   `tungstenite::client::uri_mode` accepts only `ws`/`wss`
///   (`_ => Err(UnsupportedUrlScheme)`) — `https://` opened a journal, ran
///   its full duration and recorded a gap for a scheme that can never
///   connect, and `--ws ''` ran the daemon at all.
///
/// Accepted schemes are compared case-insensitively: MEASURED on v0.7.5,
/// `--ws WS://127.0.0.1:9/x` reached TCP connect, so the consumers accept an
/// uppercase scheme and a stricter checker would reject a working value.
const URL_FLAGS: &[(&str, &[&str])] = &[
    ("--rpc", &["http", "https"]),
    ("--ws", &["ws", "wss"]),
];

/// A URL flag's value: a URL the consumer can speak, with a host — nothing
/// else. A scheme the consumer cannot honour, a missing scheme, a missing
/// host and an empty value are all the same refusal class as a number
/// outside a numeric flag's domain: accepting it only moves the failure
/// into the consumer, where it arrives late, in someone else's words, and
/// (for `start`) behind a journal the run has already created.
fn check_url_value(flag: &str, value: &str) -> Result<(), String> {
    let schemes = match URL_FLAGS.iter().find(|(f, _)| *f == flag) {
        Some((_, schemes)) => *schemes,
        None => return Ok(()),
    };
    let example = match flag {
        "--rpc" => "--rpc https://api.mainnet-beta.solana.com",
        _ => "--ws wss://api.mainnet-beta.solana.com",
    };
    check_url_domain(&format!("flag {flag}"), value, schemes, example)
}

/// The URL domain rule, shared by every layer that hands a URL to a
/// consumer: an absolute URL, a scheme the CONSUMER speaks, a host, a port
/// the consumer can dial, and bytes the Request-URI can carry.
///
/// This is the ONE implementation, and it exists because two layers checked
/// the same consumer's URLs with two different rules. Measured 2026-11-09 on
/// the published v0.7.7 tree: the webhook CONFIG judged its target URL with
/// a PREFIX test (`url.starts_with("http://") || url.starts_with("https://")`)
/// — the FORM of a URL, not its MEANING — while the FLAG layer for the very
/// same consumer judged scheme and host properly. Two faces, both measured
/// against the real poster:
///
/// * `HTTP://host/hook` -> REFUSED by the config ("must be http:// or
///   https://"), although the consumer that posts it answers 200 to exactly
///   that URL: ureq lowercases the scheme before resolving it. The
///   operator is locked out of a working value by the checker, while the
///   same URL on the same consumer is accepted one layer down;
/// * `http://` -> ACCEPTED by the config, and then every delivery fails in
///   the transport with "http: empty string": the config passed, the queue
///   blocked, and the reason arrives in ureq's words, behind a delivery
///   pass that already started.
///
/// So the rule is the consumer's, and it is checked once:
/// `label` says who is complaining (the flag parser or the config loader),
/// `schemes` are the schemes that consumer speaks, `example` is a working
/// value in the same shape.
///
/// The byte-level rule is not re-implemented here: it is `http::Uri`
/// itself, the parser BOTH consumers link (Cargo.lock resolves one `http`
/// for ureq 3.5 and tungstenite 0.24). Measured 2026-11-09 against the
/// published v0.7.8 asset, one byte per delivery, with a receiver that
/// records what actually arrives:
///
/// | position   | refused by the parser                     |
/// |------------|-------------------------------------------|
/// | authority  | `" < > \ ^ \` { \| }`, space, controls, non-ASCII, `%` outside userinfo/zone |
/// | path       | space, controls, `<`, `>`, `` ` `` (plus `" { }` which RFC 3986 would refuse — the parser accepts them for parity with real clients) |
/// | query      | space, controls, `"`, `<`, `>`            |
/// | fragment   | NOTHING — it is never transmitted         |
///
/// and `http::Uri` refuses none of `" \ ^ { | }` in the path, none of
/// `` \ ` { | } `` in the query, and nothing at all after the first `#`
/// (the fragment is dropped before sending — measured: a space, a control
/// byte and a non-ASCII byte in the fragment all delivered). A checker that
/// refused those would refuse values the consumer honours.
///
/// Two consumer behaviours are measured and NOT visible to `http::Uri`:
///
/// * **a non-numeric port is silently dropped.** `http://127.0.0.1:abc/hook`
///   parses (`port()` returns `None`), and ureq then connects to the
///   scheme's default port — measured: the request went to nginx on port
///   80, not to the configured target. `:99999` and `:` behave the same.
///   So an explicit port must be 1..=65535, else the operator's port is
///   silently replaced by 80/443.
/// * **non-ASCII in the path or query parses but dies at the receiver** —
///   measured: `http://host/atüb` parses, is sent raw, and the receiver
///   answers `HTTP 400`. The URI grammar says percent-encode; the checker
///   names that fix instead of inventing a rule.
pub fn check_url_domain(
    label: &str,
    value: &str,
    schemes: &[&str],
    example: &str,
) -> Result<(), String> {
    if value.trim().is_empty() {
        return Err(format!(
            "{label} expects an absolute URL ({}) , got an empty value (e.g. {example})",
            schemes.join("/")
        ));
    }
    let Some((scheme, rest)) = value.split_once("://") else {
        return Err(format!(
            "{label} expects an absolute URL with scheme {} — got `{value}` (e.g. {example})",
            schemes.join(" or ")
        ));
    };
    // The scheme is compared case-insensitively because the CONSUMER is:
    // MEASURED, `ureq` answers 200 to `HTTP://…` and `HtTp://…`.
    if !schemes.iter().any(|s| s.eq_ignore_ascii_case(scheme)) {
        return Err(format!(
            "{label} expects a {} URL — got `{value}` (scheme `{scheme}` is not spoken by this consumer; e.g. {example})",
            schemes.join(" or ")
        ));
    }
    // The authority: everything up to the path/query/fragment. It must be
    // non-empty — `https://` has a scheme and no host, and the consumer
    // calls that "empty string", after the check let it through.
    let authority = rest.split(['/', '?', '#']).next().unwrap_or(rest);
    if authority.is_empty() {
        return Err(format!(
            "{label} expects a URL with a host — got `{value}` (e.g. {example})"
        ));
    }
    // The byte-level domain is the consumers' parser, not a re-implementation:
    // one `http` version in the lock, linked by both ureq and tungstenite.
    let uri: http::Uri = value.parse().map_err(|e| {
        format!(
            "{label} is not a URL the consumer's URI parser can read ({e}) — got `{value}` (e.g. {example})"
        )
    })?;
    // A port the parser accepted but cannot dial: MEASURED, `:abc`, `:` and
    // `:99999` all parse, and ureq silently connects to port 80 instead —
    // the request went to nginx, not to the configured target.
    if let Some(auth) = uri.authority() {
        let host_port = auth.as_str().rsplit('@').next().unwrap_or(auth.as_str());
        // An explicit `:` in the host part means the operator wrote a port.
        // For a bracketed IPv6 literal the port follows the `]`; a missing
        // `]` or a missing `:` after it means no port was written.
        let after_colon = if host_port.starts_with('[') {
            host_port
                .split_once(']')
                .and_then(|(_, p)| p.strip_prefix(':'))
        } else {
            host_port.rsplit_once(':').map(|(_, p)| p)
        };
        if let Some(port) = after_colon {
            match port.parse::<u16>() {
                Ok(0) => {
                    return Err(format!(
                        "{label} has port 0 — no service is ever reachable on it, and nothing listens there — got `{value}` (e.g. {example})"
                    ));
                }
                Ok(_) => {}
                Err(_) => {
                    return Err(format!(
                        "{label} has an invalid port `{port}` — the consumer would silently use the default port (80/443) instead of it — got `{value}` (e.g. {example})"
                    ));
                }
            }
        }
    }
    // Non-ASCII in the path or query: `http::Uri` accepts it, the wire
    // carries it raw, and the RECEIVER refuses it (measured: HTTP 400).
    // The fragment is exempt: it is never transmitted (measured).
    let before_fragment = value.split('#').next().unwrap_or(value);
    if let Some((idx, ch)) = before_fragment.char_indices().find(|(_, c)| !c.is_ascii()) {
        return Err(format!(
            "{label} contains a non-ASCII character at byte {idx} ({ch:?}) — the URI grammar carries the request-target as percent-encoded ASCII and the receiver refuses raw bytes (measured: HTTP 400); percent-encode it (e.g. `at%C3%BCb`) — got `{value}` (e.g. {example})"
        ));
    }
    Ok(())
}

/// The webhook target `id` domain rule — MEASURED 2026-11-09 against a real
/// consumer (a local receiver recording raw header bytes) and the published
/// v0.7.8 asset. A target id is not a label: it is consumed in three places,
/// and each of them imposes a domain the loader did not check:
///
/// 1. **It is an HTTP field value.** `delivery_id` builds
///    `X-Reeflog-Event-Id: <target>:<kind>:<seq>` from it, and ureq builds
///    that field with `http::HeaderValue::from_str` — the same function this
///    rule calls. Every C0 control byte except HTAB, and DEL, are refused by
///    it (`http: failed to parse header value`): MEASURED, 32 of the 33
///    control bytes were accepted by the old loader (exit 0) and then every
///    delivery failed, while HTAB delivered byte-identical. A CR in an id is
///    also a request-smuggling shape if a transport ever passes it through.
///
/// 2. **An HTTP receiver strips the leading whitespace of the value.**
///    Measured: with `"id": " t1"`, the journal stores ` t1` and the
///    receiver saw `t1:event:1` — the consumer's deduplication key and the
///    sender's record disagree, for exactly the field the README tells
///    consumers to dedup on. TRAILING whitespace is NOT stripped (the id is
///    followed by `:event:1`, so it is not outer whitespace of the field
///    value): measured, `"t1 "` arrived as `t1 :event:1`, key intact.
///
/// 3. **It is the name the operator types back** at `--target <id>` on
///    `webhook dlq list`/`replay`.
///
/// So: non-empty, no leading ASCII whitespace, no C0 control byte other
/// than HTAB, no DEL. Nothing else is refused — an internal space or tab,
/// a trailing space, a non-ASCII id (measured: it delivers; how the
/// receiver decodes its bytes is the receiver's business), even a duplicate
/// of some other target's name are inside the domain, and refusing them
/// would be the URL-prefix failure class: locking the operator out of a
/// value the consumer honours.
pub fn check_target_id_domain(label: &str, value: &str) -> Result<(), String> {
    if value.is_empty() {
        return Err(format!(
            "{label} must not be empty (e.g. \"primary\")"
        ));
    }
    if value.starts_with([' ', '\t']) {
        return Err(format!(
            "{label} has leading whitespace — got `{value}` (an HTTP receiver strips it, so the consumer's event-id would not match the journal; e.g. \"{}\")",
            value.trim_start()
        ));
    }
    // The consumer's own validator: `http::HeaderValue::from_str`, the
    // function ureq calls before every delivery. Calling it here is the
    // whole point — the domain is its domain, not a copy of it.
    if let Err(e) = http::HeaderValue::from_str(value) {
        // Name the offending byte: the validator only reports the failure.
        let (idx, ch) = value
            .char_indices()
            .find(|(_, c)| (c.is_ascii_control() && *c != '\t') || *c == '\u{7f}')
            .unwrap_or((0, ' '));
        let desc = match ch {
            '\r' => "carriage return (\\r)",
            '\n' => "newline (\\n)",
            '\0' => "NUL byte (\\0)",
            '\u{7f}' => "DEL byte (0x7f)",
            _ => "control character",
        };
        return Err(format!(
            "{label} contains a forbidden {desc} at byte {idx} — the consumer's header parser refuses the whole field value ({e}), so every delivery would fail with `http: failed to parse header value` (got `{value}`; e.g. \"primary\")"
        ));
    }
    Ok(())
}

/// The webhook config `secret` domain rule — MEASURED 2026-11-09. The secret
/// is an HMAC key: `signature_header` feeds its raw bytes to HMAC-SHA256 and
/// it is never sent on the wire (measured: a secret with a newline, a control
/// byte and a non-ASCII byte each delivered, and the receiver's own HMAC over
/// the received body verified against the configured secret every time). So
/// the only domain that exists is emptiness — an empty secret would sign with
/// the empty key, which is no authentication at all. Anything else is a key
/// the consumer can honour, and refusing it would lock the operator out of a
/// value that works (the same failure class the URL prefix test had).
pub fn check_secret_domain(label: &str, value: &str) -> Result<(), String> {
    if value.is_empty() {
        return Err(format!(
            "{label} must not be empty — an empty secret signs with the empty key, which authenticates nothing"
        ));
    }
    Ok(())
}

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
                Some(v) if !v.is_empty() => {
                    check_numeric_value(flag, &v)?;
                    check_url_value(flag, &v)?;
                    out.push(v)
                }
                Some(_) => return Err(format!("flag {flag} needs a value")),
                None => {
                    if let Some(v) = optional_value_consume(tokens, i) {
                        check_numeric_value(flag, &v)?;
                        check_url_value(flag, &v)?;
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
        check_numeric_value(flag, &value)?;
        check_url_value(flag, &value)?;
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
