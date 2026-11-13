//! Webhook fan-out: deliver the mailbox to a consumer that is not on this
//! machine.
//!
//! Why this exists: until now the only consumer of a reeflog journal was a
//! process reading `reeflog tail` on the same host. That is a tool, not a
//! product — every outside service has to run a poller next to the daemon,
//! and the cursor file becomes the integration contract. A webhook makes the
//! journal a *mailbox* in the actual sense: the consumer gets the events
//! pushed to it, with a delivery cursor that survives a crash on either
//! side.
//!
//! The rules this module keeps, in order of importance:
//!
//! 1. **Never lie about delivery.** A delivery is recorded only when the
//!    endpoint answered 2xx. A refused endpoint, a timeout or a rate limit
//!    is recorded as a failed attempt with the status that caused it — not as
//!    a delivery. `reeflog webhook` exits 3 while a target is blocked, the
//!    same way `verify` and `hydrate` do, so a caller cannot mistake silence
//!    for health.
//!
//! 2. **At-least-once, in order.** Per target the deliveries follow the
//!    journal's `seq` and stop at the first failure. A consumer therefore
//!    sees events in the order the chain produced them, and it may see one
//!    twice — never out of order, never with a hole in front of it. The
//!    event id (`target + kind + seq`) is stable, so a consumer that needs
//!    exactly-once can dedup on it.
//!
//! 3. **The payload is authenticated, not just delivered.** Every request
//!    carries `X-Reeflog-Signature: sha256=<hmac-sha256(secret, body)>`. The
//!    receiver verifies the body it received against the shared secret, so
//!    an attacker who can reach the endpoint but not the secret cannot
//!    forge an event. The signature covers the exact bytes of the body; the
//!    body is canonical JSON (sorted keys, no whitespace), so the same
//!    event always produces the same signature.
//!
//! 4. **A retraction is delivered too.** The reorg channel has its own
//!    cursor and is delivered even when the event queue is blocked: a
//!    retraction is the one message whose lateness can cost money.
//!
//! Crash safety: the delivery state (per-target cursor) and the delivery
//! receipts live in the journal, next to the events. If the daemon dies
//! between "POST succeeded" and "cursor advanced", the receipt is already
//! there and the next run skips the re-send instead of delivering the same
//! event twice.

use crate::journal::{EventRow, Journal, ReorgRow};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::path::Path;

/// Header carrying the HMAC-SHA256 of the request body, hex encoded.
pub const SIGNATURE_HEADER: &str = "X-Reeflog-Signature";
/// Header carrying the journal `seq` of the delivered event.
pub const SEQ_HEADER: &str = "X-Reeflog-Seq";
/// Header carrying the stable delivery id (`target:kind:seq`).
pub const EVENT_ID_HEADER: &str = "X-Reeflog-Event-Id";

/// How many refused attempts on the same event move it to the dead-letter
/// queue when the config does not say. Five is deliberately more than one: a
/// target that refuses once because it is mid-deploy should not lose the
/// event, but a target that refuses five times in a row is not going to
/// accept it, and the events behind it must not wait for it forever.
pub const DEFAULT_DEAD_LETTER_AFTER: i64 = 5;

/// A webhook target: where to deliver, and the secret that authenticates the
/// delivery.
#[derive(Debug, Clone)]
pub struct Target {
    pub id: String,
    pub url: String,
    pub secret: String,
    /// Per-request timeout in milliseconds.
    pub timeout_ms: u64,
    /// After how many refused attempts the same event is moved to the
    /// dead-letter queue instead of holding the queue forever. 0 disables
    /// dead-lettering (the target blocks indefinitely).
    pub dead_letter_after: i64,
}

/// HMAC-SHA256, RFC 2104, over `msg` with `secret`.
///
/// Implemented directly on top of `sha2` so the crate stays small; the
/// vectors are pinned by unit tests (RFC 4231 case 1 and 2) and the live
/// harness checks the output against Python's `hmac` module — three
/// independent implementations, one of them not ours.
pub fn hmac_sha256(secret: &[u8], msg: &[u8]) -> [u8; 32] {
    const BLOCK: usize = 64;
    let mut key = [0u8; BLOCK];
    if secret.len() > BLOCK {
        let digest = Sha256::digest(secret);
        key[..32].copy_from_slice(&digest);
    } else {
        key[..secret.len()].copy_from_slice(secret);
    }
    let mut ipad = [0x36u8; BLOCK];
    let mut opad = [0x5cu8; BLOCK];
    for i in 0..BLOCK {
        ipad[i] ^= key[i];
        opad[i] ^= key[i];
    }
    let mut inner = Sha256::new();
    inner.update(ipad);
    inner.update(msg);
    let inner = inner.finalize();

    let mut outer = Sha256::new();
    outer.update(opad);
    outer.update(inner);
    outer.finalize().into()
}

pub fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        out.push(DIGITS[(b >> 4) as usize] as char);
        out.push(DIGITS[(b & 0x0f) as usize] as char);
    }
    out
}

/// The signature header value for a body.
pub fn signature_header(secret: &str, body: &str) -> String {
    format!("sha256={}", hex(&hmac_sha256(secret.as_bytes(), body.as_bytes())))
}

/// Verify a received body against a signature header. The receiver's half of
/// the contract — receivers reimplement this in their own language; it lives
/// here so the sender's signature is provably checkable from the other side
/// (the live harness checks it with Python's `hmac`).
#[allow(dead_code)] // receiver-side API; used by tests and documented for consumers
pub fn verify_signature(secret: &str, body: &str, header: &str) -> bool {
    let expected = signature_header(secret, body);
    // Constant-time-ish comparison: the values are not secrets themselves,
    // but a byte-by-byte early exit would leak the prefix of a forged tag.
    if header.len() != expected.len() {
        return false;
    }
    let mut diff = 0u8;
    for (a, b) in header.as_bytes().iter().zip(expected.as_bytes()) {
        diff |= a ^ b;
    }
    diff == 0
}

/// The largest `timeout_ms` this product will accept, in milliseconds (1 hour).
///
/// Measured 2026-11-08 on the published v0.7.4 tree: `"timeout_ms":
/// 18446744073709551615` was accepted and the delivery pass NEVER returned
/// against a receiver that accepted the connection and never answered (killed
/// at 25 s — the deadline was ~584 million years out). The key exists so "a
/// webhook whose endpoint hangs must not hang the delivery pass"; a value the
/// pass cannot honour inverts the guarantee. An HTTP attempt that cannot
/// finish within an hour is stuck, not slow, and the timeout exists to turn
/// "stuck" into a transient failure the queue reports — so the ceiling is
/// policy, stated here, documented in the README, and refused loudly instead
/// of accepted into a deadline that never comes.
pub const MAX_TIMEOUT_MS: u64 = 3_600_000;

/// The default `timeout_ms` when the key is absent or null.
pub const DEFAULT_TIMEOUT_MS: u64 = 5_000;

/// Helper: format a JSON value for a refusal message, quoting strings and
/// rendering numbers/booleans/null verbatim so the operator sees what they
/// actually wrote.
fn format_value(v: &Value) -> String {
    match v {
        Value::String(s) => format!("`{s}`"),
        // serde_json parses an integer beyond u64/i64 as f64 and its default
        // Display would echo the operator's `18446744073709551616` as
        // `1.8446744073709552e+19` — the refusal must show what was WRITTEN,
        // so an integral number is printed in full integer form.
        Value::Number(n) if n.as_f64().is_some_and(|f| f.fract() == 0.0) => {
            format!("{:.0}", n.as_f64().expect("integral number has an f64"))
        }
        _ => v.to_string(),
    }
}

/// Load the target list from a JSON configuration file.
///
/// The file is strict on purpose: a webhook target decides where secrets and
/// event data go, so an unknown key is a typo worth failing on, and a target
/// id is the key the delivery state is stored under — a duplicate id would
/// silently merge two consumers' cursors.
///
/// The VALUES are checked against the domain their consumers use, for the
/// same reason (measured 2026-11-08, published v0.7.4): `timeout_ms: 0` was
/// silently rewritten to 1 ms by the poster, a `timeout_ms` beyond any
/// reachable deadline hung the delivery pass forever, and a value that was a
/// number but not the right KIND of number was refused with "must be a
/// number" — without naming which value. A config file is one layer above
/// the flag parser, and it fails the same way the parser did: the checker
/// accepts what the consumer cannot honour, or the refusal does not say what
/// was wrong.
pub fn load_targets(path: &Path) -> Result<Vec<Target>, String> {
    let raw = std::fs::read_to_string(path).map_err(|e| format!("config read: {e}"))?;
    let v: Value = serde_json::from_str(&raw).map_err(|e| format!("config parse: {e}"))?;
    let obj = v
        .as_object()
        .ok_or_else(|| "config must be a JSON object".to_string())?;
    for k in obj.keys() {
        if k != "targets" {
            return Err(format!("unknown config key {k:?}"));
        }
    }
    let list = obj
        .get("targets")
        .and_then(|t| t.as_array())
        .ok_or_else(|| "config needs a \"targets\" array".to_string())?;
    let mut out: Vec<Target> = Vec::new();
    for (i, t) in list.iter().enumerate() {
        let o = t
            .as_object()
            .ok_or_else(|| format!("target #{i} must be an object"))?;
        for k in o.keys() {
            if !matches!(
                k.as_str(),
                "id" | "url" | "secret" | "timeout_ms" | "dead_letter_after"
            ) {
                return Err(format!("target #{i}: unknown key {k:?}"));
            }
        }
        let id = o
            .get("id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| format!("target #{i}: missing \"id\""))?;
        let url = o
            .get("url")
            .and_then(|v| v.as_str())
            .ok_or_else(|| format!("target #{i}: missing \"url\""))?;
        let secret = o
            .get("secret")
            .and_then(|v| v.as_str())
            .ok_or_else(|| format!("target #{i}: missing \"secret\""))?;
        let timeout_ms = match o.get("timeout_ms") {
            None | Some(Value::Null) => DEFAULT_TIMEOUT_MS,
            Some(v) => {
                let n = v.as_u64().ok_or_else(|| {
                    format!(
                        "target #{i}: timeout_ms must be a positive integer in milliseconds, got {} (e.g. 5000)",
                        format_value(v)
                    )
                })?;
                if n == 0 {
                    return Err(format!(
                        "target #{i}: timeout_ms must be greater than zero, got 0 — a timeout of 0 would fail every delivery immediately"
                    ));
                }
                if n > MAX_TIMEOUT_MS {
                    return Err(format!(
                        "target #{i}: timeout_ms must not exceed {MAX_TIMEOUT_MS} ms (1 hour), got {n} — out of domain"
                    ));
                }
                n
            }
        };
        let dead_letter_after = match o.get("dead_letter_after") {
            None | Some(Value::Null) => DEFAULT_DEAD_LETTER_AFTER,
            Some(v) => {
                let n = v.as_i64().ok_or_else(|| {
                    format!(
                        "target #{i}: dead_letter_after must be a non-negative integer, got {} (e.g. 5, or 0 to disable)",
                        format_value(v)
                    )
                })?;
                if n < 0 {
                    return Err(format!(
                        "target #{i}: dead_letter_after must not be negative, got {n}"
                    ));
                }
                n
            }
        };
        // The VALUE domain of the remaining config keys, shared with the
        // flag layer and judged by what the CONSUMER can carry
        // (measured 2026-11-09):
        //
        // * `id` becomes the HTTP field value of `X-Reeflog-Event-Id`, and a
        //   control character in it kills every delivery with `http: failed to
        //   parse header value`, while outer whitespace is stripped by the
        //   receiver — so the consumer's dedup key stops matching the
        //   journal's. Both were accepted here and configured a dead target.
        // * `secret` is an HMAC key, never sent on the wire: measured, a
        //   secret with a newline, a control byte or a non-ASCII byte
        //   delivers fine and verifies. Only emptiness is out of domain —
        //   an empty secret authenticates nothing.
        reeflog::flags::check_target_id_domain(&format!("target #{i} \"id\""), id)?;
        reeflog::flags::check_secret_domain(&format!("target #{i} \"secret\""), secret)?;
        // The URL domain rule, shared with the flag parser (measured 2026-11-09):
        // judges scheme case-insensitively (the consumer speaks `HTTP://`)
        // and requires a host (`http://` is not a URL the consumer can deliver to).
        reeflog::flags::check_url_domain(
            &format!("target #{i} url"),
            url,
            &["http", "https"],
            "http://127.0.0.1:8080/hook",
        )?;
        if out.iter().any(|t| t.id == id) {
            return Err(format!("duplicate target id {id:?}"));
        }
        // ONE SECRET IS ONE CONSUMER: two rows that share a secret while
        // pointing at different endpoints are one identity space, not two —
        // measured 2026-11-11 on the published v0.7.15 asset (4/4 faces;
        // see `check_secret_uniqueness`). Checked against every row already
        // accepted, so the offending PAIR is named wherever it sits in the
        // list. Same url + same secret = one consumer listed twice: legal.
        for prev in &out {
            if prev.secret == secret {
                reeflog::flags::check_secret_uniqueness(&prev.id, &prev.url, id, url)?;
            }
        }
        out.push(Target {
            id: id.to_string(),
            url: url.to_string(),
            secret: secret.to_string(),
            timeout_ms,
            dead_letter_after,
        });
    }
    if out.is_empty() {
        return Err("config lists no targets".to_string());
    }
    Ok(out)
}

/// The canonical body of an event delivery.
///
/// `serde_json` without the `preserve_order` feature serialises object keys
/// in sorted order, so the body — and therefore the signature — is
/// deterministic for a given event.
pub fn event_body(r: &EventRow) -> String {
    let logs = match &r.logs {
        Some(raw) => serde_json::from_str::<Value>(raw).unwrap_or(Value::String(raw.clone())),
        None => Value::Null,
    };
    // The account envelope travels with the event: a consumer outside this
    // machine gets what the transaction did to balances in the same signed
    // body, without an RPC call of its own. `null` = the journal does not
    // hold it yet (`hydrate --accounts`).
    let accounts = match &r.accounts {
        Some(raw) => serde_json::from_str::<Value>(raw).unwrap_or(Value::String(raw.clone())),
        None => Value::Null,
    };
    serde_json::to_string(&json!({
        "type": "event",
        "seq": r.seq,
        "signature": r.signature,
        "slot": r.slot,
        "err": r.err,
        "seal": r.seal_status,
        "payload_state": r.payload_state,
        "source": r.source,
        "first_seen": r.first_seen,
        "logs": logs,
        "accounts": accounts,
    }))
    .unwrap_or_default()
}

/// The canonical body of a retraction delivery.
pub fn reorg_body(r: &ReorgRow) -> String {
    serde_json::to_string(&json!({
        "type": "reorg",
        "reorg_id": r.id,
        "signature": r.signature,
        "slot": r.slot,
        "detected_at": r.detected_at,
        "status_source": r.status_source,
        "tx_source": r.tx_source,
        "detail": r.detail,
    }))
    .unwrap_or_default()
}

/// The stable delivery id. A consumer that wants exactly-once semantics
/// dedups on this.
pub fn delivery_id(target_id: &str, kind: &str, seq: i64) -> String {
    format!("{target_id}:{kind}:{seq}")
}

/// Something that can POST a body and report the HTTP status.
pub trait Poster {
    /// Ok(status) for any HTTP answer the server produced, Err(message) for a
    /// transport failure (DNS, connect, timeout, TLS).
    fn post(&mut self, url: &str, body: &str, headers: &[(&str, &str)]) -> Result<u16, String>;
}

/// A poster backed by `ureq`.
pub struct HttpPoster {
    pub timeout_ms: u64,
}

impl Poster for HttpPoster {
    fn post(&mut self, url: &str, body: &str, headers: &[(&str, &str)]) -> Result<u16, String> {
        // A webhook whose endpoint hangs must not hang the delivery pass: the
        // timeout turns "slow" into a transient failure the queue reports and
        // retries, instead of a stuck daemon.
        let cfg = ureq::Agent::config_builder()
            .timeout_global(Some(std::time::Duration::from_millis(self.timeout_ms.max(1))))
            .build();
        let agent = ureq::Agent::new_with_config(cfg);
        let mut req = agent.post(url);
        for (k, v) in headers {
            req = req.header(*k, *v);
        }
        let resp = match req.send(body) {
            Ok(r) => r,
            // ureq answers 4xx/5xx as an error carrying the status code.
            // A 429 is a rate limit and a 4xx/5xx is the server's verdict —
            // both must be visible to the caller, so they are reported as a
            // status rather than swallowed.
            Err(ureq::Error::StatusCode(c)) => return Ok(c),
            Err(e) => return Err(e.to_string()),
        };
        Ok(resp.status().as_u16())
    }
}

/// How a delivery attempt ended.
#[derive(Debug, Clone, PartialEq)]
pub enum Attempt {
    /// The endpoint accepted it (2xx).
    Delivered(u16),
    /// The endpoint refused it in a way that will not change on its own
    /// (4xx other than the two transient ones).
    Refused(u16),
    /// A temporary failure: 5xx, 408, 425, 429 or a transport error.
    Transient(String),
}

impl Attempt {
    #[allow(dead_code)] // part of the module's public vocabulary
    pub fn is_delivered(&self) -> bool {
        matches!(self, Attempt::Delivered(_))
    }
}

/// Classify an HTTP status.
pub fn classify_status(status: u16) -> Attempt {
    if (200..300).contains(&status) {
        Attempt::Delivered(status)
    } else if status == 408 || status == 425 || status == 429 {
        Attempt::Transient(format!("http {status}"))
    } else if (400..500).contains(&status) {
        Attempt::Refused(status)
    } else {
        Attempt::Transient(format!("http {status}"))
    }
}

/// The outcome of one target's delivery pass.
#[derive(Debug, Default, Clone)]
pub struct TargetReport {
    pub target: String,
    /// Events handed over successfully.
    pub delivered: i64,
    /// Retractions handed over successfully.
    pub reorg_delivered: i64,
    /// Attempts that failed (not counted as deliveries).
    pub failed: i64,
    /// The event seq the target is stuck at, if any.
    pub blocked_seq: Option<i64>,
    /// The reorg id the target is stuck at, if any.
    pub blocked_reorg_id: Option<i64>,
    /// Why the target is stuck.
    pub blocked_error: Option<String>,
    /// The event cursor after this pass.
    pub next_seq: i64,
    /// The retraction cursor after this pass.
    pub next_reorg_id: i64,
    /// Events that were refused outright and moved to the dead-letter queue.
    pub dead_lettered: i64,
}

impl TargetReport {
    pub fn blocked(&self) -> bool {
        self.blocked_seq.is_some() || self.blocked_reorg_id.is_some()
    }
}

/// Bind a target's delivery state to its endpoint, or refuse.
///
/// A state row recorded under a DIFFERENT url means the id was repointed:
/// inheriting the old cursors is the measured starvation (published
/// v0.7.14, probe `endpoint-probe-1.json`: the new consumer received 0,
/// `webhook push` exited 0, and `webhook status` printed the OLD endpoint's
/// `delivered: 3` next to the new url). So the pass refuses before reading
/// any cursor, before delivering, and without touching the journal — the
/// refusal names the target id and BOTH endpoints and states the repair.
///
/// A row with an EMPTY endpoint predates this rule (a journal written on
/// v0.7.14 or older): it carries history for the one url the operator had
/// configured, and is ADOPTED by the url it meets first — a pure
/// continuation of the old single-endpoint world: same url, same cursors,
/// no re-delivery. `true` is returned when a row was adopted.
pub fn bind_endpoint(j: &Journal, target: &Target) -> Result<bool, String> {
    match j.webhook_state_with_endpoint(&target.id)? {
        Some((recorded, _, _)) if recorded.is_empty() => {
            j.webhook_adopt_endpoint(&target.id, &target.url)
        }
        Some((recorded, _, _)) if recorded == target.url => Ok(false),
        Some((recorded, _, _)) => Err(format!(
            "target `{}` is bound to endpoint `{recorded}` in the journal, but this config points it at `{}` — delivery state and receipts are bound to the endpoint; give the new endpoint its own target id (measured v0.7.14: a repointed id starved the new consumer silently behind exit 0)",
            target.id, target.url
        )),
        None => Ok(false),
    }
}

/// Deliver events to one target, in `seq` order, stopping at the first
/// failure.
///
/// `limit` bounds how many events are attempted in one pass.
pub fn deliver_events(
    j: &Journal,
    target: &Target,
    poster: &mut dyn Poster,
    limit: i64,
) -> Result<TargetReport, String> {
    let mut rep = TargetReport {
        target: target.id.clone(),
        ..Default::default()
    };
    // The cursors and receipts below belong to ONE consumer — the url this
    // config carries. A repointed id is refused before a single byte moves;
    // see `bind_endpoint`.
    bind_endpoint(j, target)?;
    // This pass decides the target's standing verdict anew: the previous
    // pass's stop reason is cleared first and re-set by any block below, so
    // a standing failure is reported on every pass it still happens while a
    // pass that gets through leaves `webhook status` with no error behind
    // (PULS AG). Cleared AFTER the binding: a target that refuses to run
    // keeps the verdict it already has.
    j.webhook_clear_error(&target.id)?;
    let (next_seq, next_reorg_id) = j.webhook_state(&target.id)?.unwrap_or((0, 0));
    rep.next_seq = next_seq;
    rep.next_reorg_id = next_reorg_id;

    // `limit` is the operator's budget for THIS pass and is honoured as
    // given. Measured on the published v0.7.25 binary: `limit.max(1)`
    // rewrote `--limit 0` into one delivery per pass — bytes left the
    // machine and the cursor moved on a pass explicitly asked to deliver
    // nothing. `tail --limit 0` delivers zero rows (pinned legal by
    // numeric_flag_domain.rs), and `--payload-limit 0` / `--repair-pages 0`
    // are documented as "disable this pass": a zero the checker allows must
    // not become a third thing that is neither the request nor the default.
    let rows = if limit <= 0 {
        Vec::new()
    } else {
        j.events_after(rep.next_seq, limit)?
    };
    for r in &rows {
        // The payload debt rule, shared with `tail` (PULS AE): a row whose
        // payload the journal still OWES (`logs` null AND no recorded
        // outcome) is a signature, not an event. Delivering it would hand
        // the consumer a signed body with `logs: null`, advance this
        // target's cursor past the seq, and leave a receipt — when
        // `hydrate` later paid the debt, the finished event could never
        // reach this consumer again (measured pre-fix: the receiver got the
        // owed body, `next_seq` moved 0 -> 1, and the second pass delivered
        // nothing). Same class as the account filter: HOLD. The queue stops
        // at the row, the cursor stays behind it, the body is never sent,
        // and the pass exits 3 like any other blocked target. A recorded
        // outcome (`unavailable`/`invalid`) can never be filled, so it is
        // delivered with its marker — an answer, not a debt.
        if r.logs.is_none() && r.payload_state.is_none() {
            let reason = "the payload is still owed — run `hydrate`".to_string();
            j.webhook_set_error(&target.id, &target.url, Some(&reason))?;
            rep.blocked_seq = Some(r.seq);
            rep.blocked_error = Some(reason);
            break;
        }
        // A crash between "POST succeeded" and "cursor advanced" leaves a
        // receipt behind: skip the re-send instead of delivering twice.
        // The cursor holds the last seq that WAS handed over (it is the
        // argument of `events_after`, which is strictly greater): storing
        // `seq + 1` here would skip the very event that is still owed.
        if j.webhook_delivered(&target.id, &target.url, "event", r.seq)? {
            rep.next_seq = r.seq;
            j.webhook_set_state(&target.id, &target.url, rep.next_seq, rep.next_reorg_id)?;
            continue;
        }
        let body = event_body(r);
        let sig = signature_header(&target.secret, &body);
        let seq_s = r.seq.to_string();
        let eid = delivery_id(&target.id, "event", r.seq);
        let headers = [
            ("content-type", "application/json"),
            (SIGNATURE_HEADER, sig.as_str()),
            (SEQ_HEADER, seq_s.as_str()),
            (EVENT_ID_HEADER, eid.as_str()),
        ];
        let outcome = match poster.post(&target.url, &body, &headers) {
            Ok(status) => classify_status(status),
            Err(e) => Attempt::Transient(e),
        };
        match outcome {
            Attempt::Delivered(status) => {
                j.webhook_record_delivered(&target.id, &target.url, "event", r.seq, &body)?;
                j.webhook_record_attempt(&target.id, "event", r.seq, Some(status as i64), None)?;
                rep.delivered += 1;
                rep.next_seq = r.seq;
                j.webhook_set_state(&target.id, &target.url, rep.next_seq, rep.next_reorg_id)?;
            }
            Attempt::Refused(status) => {
                // A refusal is a verdict, not an outage: this target will
                // refuse this event again. But one refusal can be an accident
                // (mid-deploy, a config reload), so the attempt is counted
                // first and only a refusal that keeps repeating is treated as
                // the verdict it is.
                let reason = format!("refused with http {status}");
                j.webhook_record_attempt(
                    &target.id,
                    "event",
                    r.seq,
                    Some(status as i64),
                    Some(&reason),
                )?;
                rep.failed += 1;
                let refusals = j.webhook_refusal_count(&target.id, "event", r.seq)?;
                if target.dead_letter_after > 0 && refusals >= target.dead_letter_after {
                    // Quarantine: park the event with the exact bytes that
                    // were owed, and move the cursor past it. Without this
                    // one bad consumer stops its own mailbox forever and
                    // every event behind it is never delivered. The delivery
                    // is NOT counted as delivered — the row sits in the
                    // dead-letter queue until the target accepts it on a
                    // replay. There is no discard path: an event a consumer
                    // never got must not be able to disappear.
                    j.webhook_dead_letter(
                        &target.id,
                        "event",
                        r.seq,
                        &body,
                        &eid,
                        &reason,
                        Some(status as i64),
                        refusals,
                    )?;
                    rep.dead_lettered += 1;
                    rep.next_seq = r.seq;
                    j.webhook_set_state(&target.id, &target.url, rep.next_seq, rep.next_reorg_id)?;
                    // The queue keeps moving: the next event is attempted.
                    continue;
                }
                j.webhook_set_error(&target.id, &target.url, Some(&reason))?;
                rep.blocked_seq = Some(r.seq);
                rep.blocked_error = Some(reason);
                break;
            }
            other => {
                let (status, err) = match &other {
                    Attempt::Transient(e) => (None, Some(e.clone())),
                    Attempt::Delivered(_) => unreachable!(),
                    Attempt::Refused(_) => unreachable!(),
                };
                j.webhook_record_attempt(&target.id, "event", r.seq, status, err.as_deref())?;
                j.webhook_set_error(&target.id, &target.url, err.as_deref())?;
                rep.failed += 1;
                rep.blocked_seq = Some(r.seq);
                rep.blocked_error = err;
                break;
            }
        }
    }
    Ok(rep)
}

/// Deliver retractions to one target, using the retraction cursor.
///
/// Retractions are delivered even when the event queue is blocked: a
/// retraction that arrives late can cost money, and the consumer acts on
/// events it already received.
pub fn deliver_reorgs(
    j: &Journal,
    target: &Target,
    poster: &mut dyn Poster,
    limit: i64,
) -> Result<TargetReport, String> {
    let mut rep = TargetReport {
        target: target.id.clone(),
        ..Default::default()
    };
    // Same binding as `deliver_events`: refuse a repointed id before any
    // retraction follows the old endpoint's cursor.
    bind_endpoint(j, target)?;
    let (next_seq, next_reorg_id) = j
        .webhook_state(&target.id)?
        .unwrap_or((0, 0));
    rep.next_seq = next_seq;
    rep.next_reorg_id = next_reorg_id;

    // Same budget rule as `deliver_events`: a zero limit attempts no
    // retraction either (measured on the published v0.7.25 binary, where
    // `limit.max(1)` sent one signed retraction per `--limit 0` pass).
    let rows = if limit <= 0 {
        Vec::new()
    } else {
        j.reorgs_after(next_reorg_id, limit)?
    };
    for r in &rows {
        if j.webhook_delivered(&target.id, &target.url, "reorg", r.id)? {
            rep.next_reorg_id = r.id;
            j.webhook_set_state(&target.id, &target.url, rep.next_seq, rep.next_reorg_id)?;
            continue;
        }
        let body = reorg_body(r);
        let sig = signature_header(&target.secret, &body);
        let seq_s = r.id.to_string();
        let eid = delivery_id(&target.id, "reorg", r.id);
        let headers = [
            ("content-type", "application/json"),
            (SIGNATURE_HEADER, sig.as_str()),
            (SEQ_HEADER, seq_s.as_str()),
            (EVENT_ID_HEADER, eid.as_str()),
        ];
        let outcome = match poster.post(&target.url, &body, &headers) {
            Ok(status) => classify_status(status),
            Err(e) => Attempt::Transient(e),
        };
        match outcome {
            Attempt::Delivered(status) => {
                j.webhook_record_delivered(&target.id, &target.url, "reorg", r.id, &body)?;
                j.webhook_record_attempt(&target.id, "reorg", r.id, Some(status as i64), None)?;
                rep.reorg_delivered += 1;
                rep.next_reorg_id = r.id;
                j.webhook_set_state(&target.id, &target.url, rep.next_seq, rep.next_reorg_id)?;
            }
            other => {
                let (status, err) = match &other {
                    Attempt::Refused(s) => (Some(*s as i64), Some(format!("refused with http {s}"))),
                    Attempt::Transient(e) => (None, Some(e.clone())),
                    Attempt::Delivered(_) => unreachable!(),
                };
                j.webhook_record_attempt(&target.id, "reorg", r.id, status, err.as_deref())?;
                j.webhook_set_error(&target.id, &target.url, err.as_deref())?;
                rep.failed += 1;
                rep.blocked_reorg_id = Some(r.id);
                rep.blocked_error = err;
                break;
            }
        }
    }
    Ok(rep)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A poster that records what it was asked to send and answers from a
    /// script.
    struct ScriptedPoster {
        responses: Vec<Result<u16, String>>,
        seen: Vec<(String, String, Vec<(String, String)>)>,
    }

    impl ScriptedPoster {
        fn new(responses: Vec<Result<u16, String>>) -> Self {
            ScriptedPoster {
                responses,
                seen: Vec::new(),
            }
        }
    }

    impl Poster for ScriptedPoster {
        fn post(
            &mut self,
            _url: &str,
            body: &str,
            headers: &[(&str, &str)],
        ) -> Result<u16, String> {
            self.seen.push((
                body.to_string(),
                String::new(),
                headers
                    .iter()
                    .map(|(k, v)| (k.to_string(), v.to_string()))
                    .collect(),
            ));
            if self.responses.is_empty() {
                return Err("no scripted response".into());
            }
            self.responses.remove(0)
        }
    }

    fn tmp(tag: &str) -> Journal {
        use std::sync::atomic::{AtomicU64, Ordering};
        static N: AtomicU64 = AtomicU64::new(0);
        let n = N.fetch_add(1, Ordering::SeqCst);
        let p = std::env::temp_dir().join(format!("reeflog-webhook-{tag}-{n}.db"));
        let _ = std::fs::remove_file(&p);
        Journal::open(&p).expect("open journal")
    }

    fn target(id: &str) -> Target {
        Target {
            id: id.into(),
            url: "http://127.0.0.1:1/hook".into(),
            secret: "s3cret".into(),
            timeout_ms: 1000,
            dead_letter_after: DEFAULT_DEAD_LETTER_AFTER,
        }
    }

    #[test]
    fn hmac_matches_the_rfc_4231_vectors() {
        // RFC 4231 test case 1.
        let key = [0x0bu8; 20];
        let mac = hmac_sha256(&key, b"Hi There");
        assert_eq!(
            hex(&mac),
            "b0344c61d8db38535ca8afceaf0bf12b881dc200c9833da726e9376c2e32cff7"
        );
        // RFC 4231 test case 2.
        let mac = hmac_sha256(b"Jefe", b"what do ya want for nothing?");
        assert_eq!(
            hex(&mac),
            "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843"
        );
    }

    #[test]
    fn the_signature_is_checkable_by_the_receiver() {
        let body = r#"{"seq":1}"#;
        let sig = signature_header("s3cret", body);
        assert!(verify_signature("s3cret", body, &sig));
        assert!(!verify_signature("other", body, &sig));
        assert!(!verify_signature("s3cret", &format!("{body} "), &sig));
    }

    #[test]
    fn a_delivery_carries_the_signature_of_its_body() {
        let j = tmp("signed");
        j.append("sigA", 10, None, Some(r#"["hello"]"#), "ws").unwrap();
        let mut p = ScriptedPoster::new(vec![Ok(200)]);
        let rep = deliver_events(&j, &target("t1"), &mut p, 10).unwrap();
        assert_eq!(rep.delivered, 1);
        let (body, _, headers) = &p.seen[0];
        let sig = headers
            .iter()
            .find(|(k, _)| k == SIGNATURE_HEADER)
            .map(|(_, v)| v.clone())
            .expect("signature header");
        assert!(verify_signature("s3cret", body, &sig));
        assert!(!verify_signature("wrong", body, &sig));
    }

    #[test]
    fn a_refused_endpoint_is_not_a_delivery() {
        let j = tmp("refused");
        j.append("sigA", 10, None, Some("[]"), "ws").unwrap();
        let mut p = ScriptedPoster::new(vec![Ok(403)]);
        let rep = deliver_events(&j, &target("t1"), &mut p, 10).unwrap();
        assert_eq!(rep.delivered, 0);
        assert_eq!(rep.failed, 1);
        assert_eq!(rep.blocked_seq, Some(1));
        assert!(rep.blocked());
        assert!(!j.webhook_delivered("t1", "http://127.0.0.1:1/hook", "event", 1).unwrap());
        // The cursor did not advance: the event stays owed.
        let (next, _) = j.webhook_state("t1").unwrap().unwrap();
        assert_eq!(next, 0);
    }

    #[test]
    fn a_transient_failure_blocks_the_queue_in_order() {
        let j = tmp("transient");
        j.append("a", 10, None, Some("[]"), "ws").unwrap();
        j.append("b", 11, None, Some("[]"), "ws").unwrap();
        j.append("c", 12, None, Some("[]"), "ws").unwrap();
        // First delivery 200, second 503 (transient), the rest must not run.
        let mut p = ScriptedPoster::new(vec![Ok(200), Ok(503)]);
        let rep = deliver_events(&j, &target("t1"), &mut p, 10).unwrap();
        assert_eq!(rep.delivered, 1);
        assert_eq!(rep.failed, 1);
        assert_eq!(rep.blocked_seq, Some(2));
        assert_eq!(p.seen.len(), 2, "delivery must stop at the failure");
        // The next attempt starts where it stopped, not at the beginning.
        let mut p2 = ScriptedPoster::new(vec![Ok(200), Ok(200)]);
        let rep2 = deliver_events(&j, &target("t1"), &mut p2, 10).unwrap();
        assert_eq!(rep2.delivered, 2);
        assert_eq!(rep2.blocked_seq, None);
    }

    #[test]
    fn a_receipt_makes_a_redelivery_impossible_after_a_crash() {
        let j = tmp("crash");
        j.append("a", 10, None, Some("[]"), "ws").unwrap();
        // The endpoint answered, but the daemon died before advancing the
        // cursor: the receipt is there, the cursor is not.
        j.webhook_record_delivered("t1", "http://127.0.0.1:1/hook", "event", 1, "{}").unwrap();
        let mut p = ScriptedPoster::new(vec![Ok(200)]);
        let rep = deliver_events(&j, &target("t1"), &mut p, 10).unwrap();
        assert_eq!(rep.delivered, 0, "the event was already handed over");
        assert_eq!(p.seen.len(), 0, "no second POST");
        assert_eq!(rep.next_seq, 1, "the cursor caught up");
    }

    #[test]
    fn the_retraction_channel_has_its_own_cursor() {
        let j = tmp("reorgcursor");
        j.append("a", 10, None, Some("[]"), "ws").unwrap();
        j.mark_reverted("a", 10, "status", "tx", "gone").unwrap();
        let mut p = ScriptedPoster::new(vec![Ok(200)]);
        let rep = deliver_reorgs(&j, &target("t1"), &mut p, 10).unwrap();
        assert_eq!(rep.reorg_delivered, 1);
        let body = &p.seen[0].0;
        assert!(body.contains("\"type\":\"reorg\""), "body = {body}");
        // A second pass has nothing left to say.
        let mut p2 = ScriptedPoster::new(vec![]);
        let rep2 = deliver_reorgs(&j, &target("t1"), &mut p2, 10).unwrap();
        assert_eq!(rep2.reorg_delivered, 0);
    }

    #[test]
    fn a_retraction_is_delivered_even_when_the_event_queue_is_blocked() {
        let j = tmp("reorgblocked");
        j.append("a", 10, None, Some("[]"), "ws").unwrap();
        j.mark_reverted("a", 10, "status", "tx", "gone").unwrap();
        let mut p = ScriptedPoster::new(vec![Ok(500)]);
        let rep = deliver_events(&j, &target("t1"), &mut p, 10).unwrap();
        assert_eq!(rep.blocked_seq, Some(1));
        let mut p2 = ScriptedPoster::new(vec![Ok(200)]);
        let rep2 = deliver_reorgs(&j, &target("t1"), &mut p2, 10).unwrap();
        assert_eq!(rep2.reorg_delivered, 1, "a retraction must not wait behind a hole");
    }

    #[test]
    fn status_classification_separates_refusal_from_transient() {
        assert_eq!(classify_status(200), Attempt::Delivered(200));
        assert_eq!(classify_status(204), Attempt::Delivered(204));
        assert_eq!(classify_status(400), Attempt::Refused(400));
        assert_eq!(classify_status(404), Attempt::Refused(404));
        assert!(matches!(classify_status(429), Attempt::Transient(_)));
        assert!(matches!(classify_status(408), Attempt::Transient(_)));
        assert!(matches!(classify_status(500), Attempt::Transient(_)));
        assert!(matches!(classify_status(503), Attempt::Transient(_)));
    }

    #[test]
    fn a_config_with_a_typo_is_refused() {
        let p = std::env::temp_dir().join(format!("reeflog-webhook-cfg-{}.json", std::process::id()));
        std::fs::write(&p, r#"{"targets":[{"id":"a","url":"http://x/y","secret":"s","timeout":5}]}"#)
            .unwrap();
        assert!(load_targets(&p).is_err(), "unknown key must be refused");
        std::fs::write(
            &p,
            r#"{"targets":[{"id":"a","url":"http://x/y","secret":"s"},{"id":"a","url":"http://x/z","secret":"s"}]}"#,
        )
        .unwrap();
        assert!(load_targets(&p).is_err(), "duplicate id must be refused");
        std::fs::write(&p, r#"{"targets":[{"id":"a","url":"ftp://x/y","secret":"s"}]}"#).unwrap();
        assert!(load_targets(&p).is_err(), "non-http url must be refused");
        std::fs::write(
            &p,
            r#"{"targets":[{"id":"a","url":"http://x/y","secret":"s"}]}"#,
        )
        .unwrap();
        let t = load_targets(&p).unwrap();
        assert_eq!(t.len(), 1);
        assert_eq!(t[0].timeout_ms, 5_000, "the default timeout applies");
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn a_refusal_blocks_the_queue_until_it_repeats_enough() {
        let j = tmp("refusethreshold");
        j.append("a", 10, None, Some("[]"), "ws").unwrap();
        let mut t = target("t1");
        t.dead_letter_after = 3;
        // First refusal: the event stays owed, the queue is blocked.
        let mut p = ScriptedPoster::new(vec![Ok(403)]);
        let rep = deliver_events(&j, &t, &mut p, 10).unwrap();
        assert_eq!(rep.dead_lettered, 0);
        assert_eq!(rep.blocked_seq, Some(1));
        assert_eq!(rep.next_seq, 0, "the cursor must not move on the first refusal");
        // Second refusal: still blocked, still no dead letter.
        let mut p = ScriptedPoster::new(vec![Ok(403)]);
        let rep = deliver_events(&j, &t, &mut p, 10).unwrap();
        assert_eq!(rep.dead_lettered, 0);
        assert_eq!(rep.blocked_seq, Some(1));
        // Third refusal = the configured threshold: now it is a verdict.
        let mut p = ScriptedPoster::new(vec![Ok(403)]);
        let rep = deliver_events(&j, &t, &mut p, 10).unwrap();
        assert_eq!(rep.dead_lettered, 1, "the third refusal moves it to the dead-letter queue");
        assert_eq!(rep.blocked_seq, None, "the queue is not blocked any more");
        assert_eq!(rep.next_seq, 1, "the cursor moves past the quarantined event");
        assert_eq!(j.webhook_dead_letter_count("t1").unwrap(), 1);
        // And it was NOT counted as a delivery.
        assert!(!j.webhook_delivered("t1", "http://127.0.0.1:1/hook", "event", 1).unwrap());
    }

    #[test]
    fn a_dead_letter_keeps_the_exact_body_that_was_owed() {
        let j = tmp("dlqbody");
        j.append("a", 10, None, Some(r#"["log"]"#), "ws").unwrap();
        let mut t = target("t1");
        t.dead_letter_after = 1;
        let mut p = ScriptedPoster::new(vec![Ok(403)]);
        let rep = deliver_events(&j, &t, &mut p, 10).unwrap();
        assert_eq!(rep.dead_lettered, 1);
        let rows = j.webhook_dead_letters("t1", 10).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].seq, 1);
        assert_eq!(rows[0].kind, "event");
        assert_eq!(rows[0].status, Some(403));
        assert_eq!(rows[0].attempts, 1);
        // The body is the canonical event body, byte for byte.
        let ev = j.events_after(0, 10).unwrap();
        assert_eq!(rows[0].body, event_body(&ev[0]));
        // And the signature over that body is the one the target would have
        // received — a replay is the same event, not a new one.
        assert_eq!(
            signature_header(&t.secret, &rows[0].body),
            signature_header(&t.secret, &event_body(&ev[0]))
        );
    }

    #[test]
    fn a_transient_failure_never_becomes_a_dead_letter() {
        let j = tmp("dlqtransient");
        j.append("a", 10, None, Some("[]"), "ws").unwrap();
        let mut t = target("t1");
        t.dead_letter_after = 1;
        // 503 is transient: retried forever, never quarantined.
        let mut p = ScriptedPoster::new(vec![Ok(503), Ok(503), Ok(503)]);
        for _ in 0..3 {
            let rep = deliver_events(&j, &t, &mut p, 10).unwrap();
            assert_eq!(rep.dead_lettered, 0, "a 5xx is an outage, not a verdict");
            assert_eq!(rep.blocked_seq, Some(1));
        }
        assert_eq!(j.webhook_dead_letter_count("t1").unwrap(), 0);
        assert_eq!(j.webhook_refusal_count("t1", "event", 1).unwrap(), 0);
    }

    #[test]
    fn a_retraction_is_never_quarantined() {
        let j = tmp("dlqreorg");
        j.append("a", 10, None, Some("[]"), "ws").unwrap();
        j.mark_reverted("a", 10, "status", "tx", "gone").unwrap();
        let mut t = target("t1");
        t.dead_letter_after = 1;
        let mut p = ScriptedPoster::new(vec![Ok(403)]);
        let rep = deliver_reorgs(&j, &t, &mut p, 10).unwrap();
        assert_eq!(rep.reorg_delivered, 0);
        assert_eq!(rep.blocked_reorg_id, Some(1), "a late retraction must not be skipped");
        assert_eq!(j.webhook_dead_letter_count("t1").unwrap(), 0);
    }

    #[test]
    fn a_quarantined_event_lets_the_queue_keep_moving() {
        let j = tmp("dlqmoving");
        j.append("a", 10, None, Some("[]"), "ws").unwrap();
        j.append("b", 11, None, Some("[]"), "ws").unwrap();
        let mut t = target("t1");
        t.dead_letter_after = 1;
        // The target refuses the first event and accepts the second.
        let mut p = ScriptedPoster::new(vec![Ok(403), Ok(200)]);
        let rep = deliver_events(&j, &t, &mut p, 10).unwrap();
        assert_eq!(rep.dead_lettered, 1);
        assert_eq!(rep.delivered, 1, "the event behind the dead letter is delivered");
        assert_eq!(rep.next_seq, 2);
    }

    #[test]
    fn the_event_body_is_deterministic() {
        let j = tmp("body");
        j.append("sigA", 10, None, Some(r#"["b","a"]"#), "ws").unwrap();
        let rows = j.events_after(0, 10).unwrap();
        let a = event_body(&rows[0]);
        let b = event_body(&rows[0]);
        assert_eq!(a, b, "the same event must produce the same bytes");
        assert_eq!(
            signature_header("k", &a),
            signature_header("k", &b),
            "and therefore the same signature"
        );
    }
}
