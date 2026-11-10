//! The cursor FILE: one module that knows what a cursor file may hold and
//! what it proves.
//!
//! Why this module exists (measured 2026-11-10 on the published v0.7.12
//! binary, `research/cursor-provenance-2026-11-10.json`): a cursor file was a
//! bare integer with no record of WHICH journal or WHICH channel wrote it.
//! reeflog itself writes two different kinds of position into files of the
//! same shape — `tail` stores an event `seq`, `reorg tail` stores a reorg
//! `id` — and each reader accepted any number from any source:
//!
//!   * an event cursor (`20`) handed to `reorg tail` → `0 reorg(s) after
//!     cursor 20`, exit 0, while the journal held 3 undelivered reorgs: the
//!     retraction channel was silently lost;
//!   * a reorg cursor (`3`) handed to `tail` → 17 already-delivered events
//!     replayed, exit 0: duplicate delivery behind a healthy exit code;
//!   * a cursor copied to a DIFFERENT journal → `0 events after cursor 20`,
//!     exit 0: a fresh mailbox read as fully delivered.
//!
//! The promise this file carries is *"resumes exactly where it stopped"* — a
//! position without provenance cannot keep that promise, because it does not
//! say where.
//!
//! The rule, in two halves:
//!
//!   * a cursor reeflog WROTE carries its provenance (channel + journal id)
//!     and is honoured only by the same channel of the same journal. Every
//!     other use is a refusal that names both sides and the repair, delivers
//!     nothing, and leaves the file untouched;
//!   * a BARE hand-written non-negative whole number stays legal. It is the
//!     documented rewind/resume (`0` from the beginning, `15` to resume, an
//!     ahead-of-tip value to wait), and a reader that refuses the operator's
//!     own deliberate numbers is worse than the defect it fixes. A bare value
//!     simply makes no promise about where it came from, so there is nothing
//!     to contradict.
//!
//! The strict content rules from v0.7.4 (garbage / empty / negative / outside
//! i64 is a refusal, never a silent restart at 0) live here too, so the read
//! path has exactly one voice.

use serde_json::Value;

/// The channel a cursor belongs to. `tail` and `reorg tail` are separate
/// channels: an event seq and a reorg id are different number lines.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Channel {
    Events,
    Reorgs,
}

impl Channel {
    pub fn as_str(self) -> &'static str {
        match self {
            Channel::Events => "events",
            Channel::Reorgs => "reorgs",
        }
    }
}

/// What one cursor file says.
pub struct Cursor {
    /// The position to read from.
    pub value: i64,
    /// The journal that wrote it — `None` for a hand-written bare number.
    pub journal_id: Option<String>,
    /// The channel that wrote it — `None` for a hand-written bare number.
    pub channel: Option<Channel>,
    /// Whether the file is reeflog's own structured form.
    pub structured: bool,
}

/// The structured form, as ONE line of JSON. The fields are short because
/// this file is written once per delivered batch on a hot path.
pub fn render(channel: Channel, journal_id: &str, value: i64) -> String {
    format!(
        "{{\"v\":1,\"channel\":\"{}\",\"journal\":\"{}\",\"value\":{}}}\n",
        channel.as_str(),
        journal_id,
        value
    )
}

fn refuse(content: &str, path_display: &str, why: String) -> String {
    // The content is quoted with `escape_for_message` applied by the caller's
    // `die`; here it is trimmed so the refusal shows what the file holds.
    let shown = content.trim();
    format!(
        "cursor {path_display} holds `{shown}` — {why}; refusing to guess a position (a bare whole number from 0 up is a deliberate rewind; delete the file to start from the beginning)"
    )
}

/// Read a cursor file and classify it. `Ok(None)` means the file is not
/// there: the documented first run.
///
/// Every failure mode returns the refusal TEXT (the caller `die`s with it) —
/// never a position. Collapsing a failure into 0 is the defect v0.7.4 closed,
/// and a new reader must not reopen it.
pub fn parse(raw: &str, path_display: &str) -> Result<Cursor, String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(refuse(
            raw,
            path_display,
            "an empty file is what a crash between truncate and write leaves behind".into(),
        ));
    }
    if trimmed.starts_with('{') {
        let value: Value = serde_json::from_str(trimmed)
            .map_err(|_| refuse(raw, path_display, "a `{`-cursor must be one JSON object".into()))?;
        let version = value.get("v").and_then(Value::as_i64);
        if version != Some(1) {
            return Err(refuse(
                raw,
                path_display,
                format!("cursor format `v` is {:?}, this build writes v1", version),
            ));
        }
        let channel = match value.get("channel").and_then(Value::as_str) {
            Some("events") => Channel::Events,
            Some("reorgs") => Channel::Reorgs,
            other => {
                return Err(refuse(
                    raw,
                    path_display,
                    format!("channel is {:?}, expected \"events\" or \"reorgs\"", other),
                ))
            }
        };
        let journal_id = match value.get("journal").and_then(Value::as_str) {
            Some(j) if !j.is_empty() => j.to_string(),
            other => {
                return Err(refuse(
                    raw,
                    path_display,
                    format!("journal is {:?}, expected the id of the journal that wrote it", other),
                ))
            }
        };
        let number = match value.get("value").and_then(Value::as_i64) {
            Some(n) if n >= 0 => n,
            other => {
                return Err(refuse(
                    raw,
                    path_display,
                    format!("value is {:?}, expected a non-negative whole number inside i64", other),
                ))
            }
        };
        return Ok(Cursor {
            value: number,
            journal_id: Some(journal_id),
            channel: Some(channel),
            structured: true,
        });
    }
    match trimmed.parse::<i64>() {
        Ok(n) if n >= 0 => Ok(Cursor {
            value: n,
            journal_id: None,
            channel: None,
            structured: false,
        }),
        Ok(n) => Err(refuse(
            raw,
            path_display,
            format!("({n}) — a cursor is a position, never negative"),
        )),
        Err(_) => Err(refuse(
            raw,
            path_display,
            "not a non-negative whole number inside i64".into(),
        )),
    }
}

/// The provenance verdict for a structured cursor read against a journal.
/// `Ok(true)` means the file may be honoured; `Ok(false)` means it is a bare
/// hand-written number (no claim to check). `Err` carries the refusal text.
pub fn honour(
    cursor: &Cursor,
    want: Channel,
    journal_id: &str,
    path_display: &str,
) -> Result<bool, String> {
    if !cursor.structured {
        return Ok(false);
    }
    let wrote_channel = cursor.channel.expect("a structured cursor carries a channel");
    let wrote_journal = cursor
        .journal_id
        .as_deref()
        .expect("a structured cursor carries a journal");
    if wrote_channel != want || wrote_journal != journal_id {
        let mut why = Vec::new();
        if wrote_channel != want {
            why.push(format!(
                "it was written by the `{}` channel and this command reads `{}`",
                wrote_channel.as_str(),
                want.as_str()
            ));
        }
        if wrote_journal != journal_id {
            why.push(format!(
                "it names journal {wrote_journal} and this journal is {journal_id}"
            ));
        }
        return Err(format!(
            "cursor {} does not belong here — {} — refusing to read a position from another channel or journal (use the cursor this command wrote, or write a bare whole number to resume deliberately)",
            path_display,
            why.join(" and ")
        ));
    }
    Ok(true)
}
