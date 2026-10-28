//! reeflog — durable local mailbox for Solana events.
//!
//! v0 scope (see README "Next qualification"):
//! one program over WebSocket → append-only journal with monotone seq →
//! CLI cursor resume → gaps are reported, never swallowed.

mod backfill;
mod journal;

use backfill::{backfill_tail, repair_holes, verify_recent, HttpFetcher, Report};
use futures_util::{SinkExt, StreamExt};
use journal::{EventRow, Journal};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use tokio_tungstenite::tungstenite::Message;

const DEFAULT_WS: &str = "wss://api.mainnet-beta.solana.com";
const DEFAULT_RPC: &str = "https://api.mainnet-beta.solana.com";
/// getSignaturesForAddress accepts at most 1000 signatures per page. Using
/// the maximum halves the round trips a scan needs, which matters on a
/// public endpoint that rate-limits mid-walk.
const PAGE_SIZE: usize = 1000;
const RECONNECT_BACKOFF_MS: u64 = 1500;

fn die(msg: impl std::fmt::Display) -> ! {
    eprintln!("reeflog: error: {msg}");
    std::process::exit(1);
}

fn arg(args: &[String], key: &str) -> Option<String> {
    args.iter()
        .position(|a| a == key)
        .and_then(|i| args.get(i + 1))
        .cloned()
}

fn require(args: &[String], key: &str) -> String {
    arg(args, key).unwrap_or_else(|| die(format!("missing {key} <value>")))
}

// ---------------------------------------------------------------------------
// HTTP JSON-RPC (blocking; called while the WS connection is down or closed)
// ---------------------------------------------------------------------------

fn rpc(url: &str, method: &str, params: Value) -> Result<Value, String> {
    let body = json!({"jsonrpc":"2.0","id":1,"method":method,"params":params}).to_string();
    let mut resp = ureq::post(url)
        .header("content-type", "application/json")
        .send(body)
        .map_err(|e| format!("{method}: {e}"))?;
    let v: Value = resp
        .body_mut()
        .read_to_string()
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .ok_or_else(|| format!("{method}: bad rpc response"))?;
    if let Some(err) = v.get("error") {
        if !err.is_null() {
            return Err(format!("{method}: rpc error {err}"));
        }
    }
    Ok(v.get("result").cloned().unwrap_or(Value::Null))
}

/// Backfill signatures the journal missed since its oldest held signature.
/// Returns how many *new* events were appended (dedup is the journal's job).
fn backfill_gap(j: &Journal, fetcher: &mut HttpFetcher, program: &str) -> Result<Report, String> {
    backfill_tail(j, fetcher, program, 40, PAGE_SIZE)
}

/// Detect deep holes: scan chain history over the journal's claimed window
/// (optionally raised to `since_slot`) and report signatures it is missing.
fn verify_gap(
    j: &Journal,
    fetcher: &mut HttpFetcher,
    program: &str,
    pages: usize,
    since_slot: Option<i64>,
) -> Result<Report, String> {
    verify_recent(j, fetcher, program, pages, PAGE_SIZE, since_slot)
}

/// One bounded repair pass. Records what it did as a gap (opened, then
/// closed with the number of rows it filled) so `status` keeps reporting the
/// episode instead of the rewrite happening invisibly. An incomplete pass
/// leaves the gap open with an explicit marker.
fn repair_pass(j: &Journal, rpc_url: &str, program: &str, pages: usize, since_slot: Option<i64>) -> Value {
    let mut fetcher = HttpFetcher {
        url: rpc_url.to_string(),
    };
    let r = match repair_holes(j, &mut fetcher, program, pages, PAGE_SIZE, since_slot) {
        Ok(r) => r,
        Err(e) => {
            return json!({"complete": false, "error": e, "found_missing": 0, "filled": 0});
        }
    };
    if !r.missing.is_empty() {
        // Repeated passes finding the SAME hole must accumulate on one row:
        // a 30s cadence would otherwise insert thousands of duplicates.
        let g = match j.gap_find_open("hole") {
            Ok(Some(id)) => id,
            _ => j
                .gap_open(
                    "hole",
                    j.max_seq().unwrap_or(0),
                    j.max_slot().unwrap_or(None),
                    j.newest_signature().unwrap_or(None).as_deref(),
                    &format!(
                        "repair pass found {} signature(s) missing from the journal (scanned {} signatures over {} pages, truncated={}, error={})",
                        r.missing.len(),
                        r.found,
                        r.pages,
                        r.truncated,
                        r.error.clone().unwrap_or_else(|| "none".into())
                    ),
                )
                .unwrap_or(-1),
        };
        if g >= 0 {
            if r.complete() && r.filled == r.missing.len() as i64 {
                let _ = j.gap_close(
                    g,
                    j.max_slot().unwrap_or(None),
                    r.filled,
                    &format!("; repaired {} row(s), journal re-check clean", r.filled),
                );
            } else {
                let _ = j.gap_update_detail(g, "; REPAIR INCOMPLETE — GAP STILL OPEN");
            }
        }
    }    json!({
        "complete": r.complete(),
        "error": r.error,
        "truncated": r.truncated,
        "pages": r.pages,
        "scanned_from_slot": r.scanned_from_slot,
        "since_slot": since_slot,
        "found_missing": r.missing.len(),
        "filled": r.filled,
        "missing_sample": r.missing.iter().take(5).cloned().collect::<Vec<_>>(),
    })
}

fn bump(v: &mut Value, key: &str) {
    if let Some(n) = v.get_mut(key).and_then(|x| x.as_i64()) {
        v[key] = Value::from(n + 1);
    }
}

/// Promote pending events using getSignatureStatuses.
/// finalized → 'finalized'; confirmed → 'confirmed' (stays pending for the
/// next seal); err → 'error'; not found even with history → 'missing'.
fn seal(j: &Journal, rpc_url: &str) -> Result<Value, String> {
    let pending = j.unsealed(150)?;
    let mut promoted = json!({"finalized":0,"confirmed":0,"error":0,"missing":0});
    if pending.is_empty() {
        return Ok(promoted);
    }
    let result = rpc(
        rpc_url,
        "getSignatureStatuses",
        json!([pending.clone(), {"searchTransactionHistory": true}]),
    )?;
    let values = result
        .get("value")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    for (sig, st) in pending.iter().zip(values.iter()) {
        if st.is_null() {
            j.seal(sig, "missing", None)?;
            bump(&mut promoted, "missing");
            continue;
        }
        let err = st.get("err");
        let err_str = match err {
            None | Some(Value::Null) => None,
            Some(e) => Some(e.to_string()),
        };
        let conf = st
            .get("confirmationStatus")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let status = if err_str.is_some() {
            "error"
        } else if conf == "finalized" {
            "finalized"
        } else if conf == "confirmed" {
            "confirmed"
        } else {
            continue; // 'processed' is not a seal — leave pending
        };
        j.seal(sig, status, err_str.as_deref())?;
        bump(&mut promoted, status);
    }
    Ok(promoted)
}

// ---------------------------------------------------------------------------
// WebSocket ingest
// ---------------------------------------------------------------------------

struct Stats {
    ws_new: i64,
    ws_dupes: i64,
    backfill_new: i64,
    disconnects: i64,
    reconnects: i64,
    repair_filled: i64,
    repair_passes: i64,
}

impl Stats {
    fn new() -> Self {
        Stats {
            ws_new: 0,
            ws_dupes: 0,
            backfill_new: 0,
            disconnects: 0,
            reconnects: 0,
            repair_filled: 0,
            repair_passes: 0,
        }
    }
}

async fn run_start(args: &[String]) {
    let db = PathBuf::from(arg(args, "--db").unwrap_or_else(|| "reeflog.db".into()));
    let j = Journal::open(&db).unwrap_or_else(|e| die(e));
    let program = match j.meta_get("program") {
        Ok(Some(p)) => {
            if let Some(given) = arg(args, "--program") {
                if given != p {
                    die(format!("journal watches {p}, refusing --program {given}"));
                }
            }
            p
        }
        Ok(None) => {
            let p = require(args, "--program");
            j.meta_set("program", &p).unwrap_or_else(|e| die(e));
            p
        }
        Err(e) => die(e),
    };
    let ws_url = arg(args, "--ws").unwrap_or_else(|| DEFAULT_WS.into());
    let rpc_url = arg(args, "--rpc").unwrap_or_else(|| DEFAULT_RPC.into());
    let seconds: u64 = arg(args, "--seconds")
        .and_then(|s| s.parse().ok())
        .unwrap_or(60);
    // Periodic hole repair. The tail backfill only reaches forward from the
    // newest held slot; events the WS dropped while we were connected sit
    // BEHIND that frontier and are unreachable that way. Measured in the kill
    // test: even the naive-live control (never killed) lost 158 of 11,275
    // deliverable events, so a connected socket alone is not a guarantee.
    // The repair pass polices a recent rolling window and fills what the
    // socket dropped.
    let repair_every_s: u64 = arg(args, "--repair-every").and_then(|s| s.parse().ok()).unwrap_or(30);
    let repair_pages: usize = arg(args, "--repair-pages").and_then(|s| s.parse().ok()).unwrap_or(40);
    let repair_lookback: i64 = arg(args, "--repair-lookback").and_then(|s| s.parse().ok()).unwrap_or(1800);

    let mut stats = Stats::new();
    let deadline = Instant::now() + Duration::from_secs(seconds);

    // Gap 1: anything since this journal's last event (previous run or crash).
    let mut fetcher = HttpFetcher {
        url: rpc_url.clone(),
    };
    let mut open_gap = if j.max_seq().unwrap_or(0) > 0 {
        let g = j
            .gap_open(
                "restart",
                j.max_seq().unwrap_or(0),
                j.max_slot().unwrap_or(None),
                j.last_signature().unwrap_or(None).as_deref(),
                "journal had events before this start",
            )
            .unwrap_or_else(|e| die(e));
        let r = backfill_gap(&j, &mut fetcher, &program);
        let r = match r {
            Ok(r) => r,
            Err(e) => {
                j.gap_update_detail(g, &format!("; tail backfill failed: {e}")).ok();
                Report {
                    truncated: true,
                    error: Some(e),
                    ..Report::default()
                }
            }
        };
        stats.backfill_new += r.inserted;
        // A truncated tail fill means the gap is NOT closed. Say so.
        let note = format!(
            "; restart tail-backfill found={} inserted={} dupes={} pages={} truncated={} error={}",
            r.found,
            r.inserted,
            r.duplicates,
            r.pages,
            r.truncated,
            r.error.clone().unwrap_or_else(|| "none".into())
        );
        if r.truncated || r.error.is_some() {
            j.gap_update_detail(g, &format!("{note}; GAP STILL OPEN")).ok();
            Some(g)
        } else {
            j.gap_close(g, j.max_slot().unwrap_or(None), r.inserted, &note)
                .unwrap_or_else(|e| die(e));
            None
        }
    } else {
        None
    };
    let _ = &mut open_gap;

    // The periodic hole-repair scheduler. `repair_every_s` must be well under
    // `repair_lookback` in chain-progress terms, or a hole can age out of the
    // rolling window between two passes and never be filled: at ~10 slots/s
    // a 3600-slot window gives a hole ~6 minutes of life and passes run every
    // 30s, so each hole gets ~12 chances.
    let mut next_repair = Instant::now() + Duration::from_secs(repair_every_s.max(1));
    // Runs one bounded repair pass over the recent rolling window.
    // Kept as a closure-free block so the journal is never borrowed twice.
    macro_rules! repair_tick {
        () => {{
            next_repair = Instant::now() + Duration::from_secs(repair_every_s.max(1));
            let since = j.max_slot().unwrap_or(None).map(|s| s - repair_lookback);
            let summary = repair_pass(&j, &rpc_url, &program, repair_pages, since);
            stats.repair_passes += 1;
            stats.repair_filled += summary["filled"].as_i64().unwrap_or(0);
            if summary["found_missing"].as_i64().unwrap_or(0) > 0 || summary["complete"].as_bool() != Some(true) {
                eprintln!("reeflog: repair pass {summary}");
            }
        }};
    }

    let mut first_connect = true;
    loop {
        if Instant::now() >= deadline {
            break;
        }
        if !first_connect {
            stats.reconnects += 1;
        }
        first_connect = false;

        let connect = tokio::time::timeout(
            Duration::from_secs(15),
            tokio_tungstenite::connect_async(&ws_url),
        )
        .await;
        let (mut ws, _) = match connect {
            Ok(Ok(pair)) => pair,
            Ok(Err(e)) => {
                stats.disconnects += 1;
                open_gap = Some(
                    j.gap_open(
                        "connect_fail",
                        j.max_seq().unwrap_or(0),
                        j.max_slot().unwrap_or(None),
                        j.last_signature().unwrap_or(None).as_deref(),
                        &format!("ws connect failed: {e}"),
                    )
                    .unwrap_or_else(|e| die(e)),
                );
                tokio::time::sleep(Duration::from_millis(RECONNECT_BACKOFF_MS)).await;
                continue;
            }
            Err(_) => {
                stats.disconnects += 1;
                open_gap = Some(
                    j.gap_open(
                        "connect_fail",
                        j.max_seq().unwrap_or(0),
                        j.max_slot().unwrap_or(None),
                        j.last_signature().unwrap_or(None).as_deref(),
                        "ws connect timed out",
                    )
                    .unwrap_or_else(|e| die(e)),
                );
                tokio::time::sleep(Duration::from_millis(RECONNECT_BACKOFF_MS)).await;
                continue;
            }
        };

        let sub = json!({
            "jsonrpc": "2.0", "id": 1, "method": "logsSubscribe",
            "params": [{"mentions": [program]}, {"commitment": "confirmed"}]
        });
        if let Err(e) = ws.send(Message::Text(sub.to_string())).await {
            stats.disconnects += 1;
            let g = j
                .gap_open(
                    "subscribe_fail",
                    j.max_seq().unwrap_or(0),
                    j.max_slot().unwrap_or(None),
                    j.last_signature().unwrap_or(None).as_deref(),
                    &format!("subscribe failed: {e}"),
                )
                .unwrap_or_else(|e| die(e));
            open_gap = Some(g);
            tokio::time::sleep(Duration::from_millis(RECONNECT_BACKOFF_MS)).await;
            continue;
        }

        // If a gap is still open from a previous disconnect, close it now that
        // the stream is back: catch up first, then resume live.
        if let Some(g) = open_gap.take() {
            let r = match backfill_gap(&j, &mut fetcher, &program) {
                Ok(r) => r,
                Err(e) => {
                    j.gap_update_detail(g, &format!("; tail backfill failed: {e}")).ok();
                    Report {
                        truncated: true,
                        error: Some(e),
                        ..Report::default()
                    }
                }
            };
            stats.backfill_new += r.inserted;
            let note = format!(
                "; reconnect tail-backfill found={} inserted={} dupes={} pages={} truncated={} error={}",
                r.found,
                r.inserted,
                r.duplicates,
                r.pages,
                r.truncated,
                r.error.clone().unwrap_or_else(|| "none".into())
            );
            if r.truncated || r.error.is_some() {
                j.gap_update_detail(g, &format!("{note}; GAP STILL OPEN")).ok();
                open_gap = Some(g);
            } else {
                j.gap_close(g, j.max_slot().unwrap_or(None), r.inserted, &note)
                    .unwrap_or_else(|e| die(e));
            }
        }

        let mut broke = false;
        let mut last_frame = Instant::now();
        while Instant::now() < deadline {
            // Poll the socket in short slices so a repair pass can run even
            // when the stream is quiet: a hole dropped by the socket looks
            // exactly like a healthy connection with nothing to say.
            let frame = tokio::time::timeout(Duration::from_secs(5), ws.next()).await;
            if Instant::now() >= next_repair {
                repair_tick!();
            }
            if Instant::now() >= deadline {
                break;
            }
            let msg = match frame {
                Ok(Some(Ok(m))) => {
                    last_frame = Instant::now();
                    m
                }
                Ok(Some(Err(e))) => {
                    stats.disconnects += 1;
                    open_gap = Some(
                        j.gap_open(
                            "socket",
                            j.max_seq().unwrap_or(0),
                            j.max_slot().unwrap_or(None),
                            j.last_signature().unwrap_or(None).as_deref(),
                            &format!("socket error: {e}"),
                        )
                        .unwrap_or_else(|e| die(e)),
                    );
                    broke = true;
                    break;
                }
                Ok(None) => {
                    stats.disconnects += 1;
                    open_gap = Some(
                        j.gap_open(
                            "closed",
                            j.max_seq().unwrap_or(0),
                            j.max_slot().unwrap_or(None),
                            j.last_signature().unwrap_or(None).as_deref(),
                            "server closed the stream",
                        )
                        .unwrap_or_else(|e| die(e)),
                    );
                    broke = true;
                    break;
                }
                Err(_) => {
                    // 600s without a frame: we call it, we report it.
                    if last_frame.elapsed().as_secs() >= 600 {
                        stats.disconnects += 1;
                        open_gap = Some(
                            j.gap_open(
                                "idle",
                                j.max_seq().unwrap_or(0),
                                j.max_slot().unwrap_or(None),
                                j.last_signature().unwrap_or(None).as_deref(),
                                "no ws frame for 600s",
                            )
                            .unwrap_or_else(|e| die(e)),
                        );
                        broke = true;
                        break;
                    }
                    continue; // quiet socket: just a poll timeout
                }
            };
            if Instant::now() >= deadline {
                break;
            }
            let Message::Text(text) = msg else { continue };
            let Ok(v) = serde_json::from_str::<Value>(&text) else { continue };
            // Only log notifications; subscription acks and errors are skipped.
            if v.get("method").and_then(|m| m.as_str()) != Some("logsNotification") {
                continue;
            }
            let value = v
                .pointer("/params/result/value")
                .cloned()
                .unwrap_or(Value::Null);
            let sig = value
                .get("signature")
                .and_then(|s| s.as_str())
                .unwrap_or("");
            if sig.is_empty() {
                continue;
            }
            let slot = v
                .pointer("/params/result/context/slot")
                .and_then(|s| s.as_i64())
                .unwrap_or(0);
            let err = match value.get("err") {
                None | Some(Value::Null) => None,
                Some(e) => Some(e.to_string()),
            };
            let logs = value
                .get("logs")
                .map(|l| l.to_string())
                .or_else(|| Some("[]".into()));
            match j.append(sig, slot, err.as_deref(), logs.as_deref(), "ws") {
                Ok((_, true)) => stats.ws_new += 1,
                Ok((_, false)) => stats.ws_dupes += 1,
                Err(e) => {
                    // A journal write failure is a protocol error, not a shrug.
                    eprintln!("reeflog: journal write failed: {e}");
                    std::process::exit(2);
                }
            }
        }
        if broke {
            tokio::time::sleep(Duration::from_millis(RECONNECT_BACKOFF_MS)).await;
            continue;
        }
        if Instant::now() >= deadline {
            break;
        }
    }

    // Final seal pass so the summary reflects the durable state.
    let promoted = seal(&j, &rpc_url).unwrap_or_else(|e| {
        eprintln!("reeflog: seal failed at shutdown: {e}");
        json!({"error": e})
    });
    if let Some(g) = open_gap {
        j.gap_update_detail(g, "; run ended before the stream came back").ok();
    }
    // Final repair pass before the summary: the last window of the run may
    // still hold holes the socket dropped. Without it the journal would end
    // the run with known holes and no record of them.
    let final_repair = if repair_every_s > 0 {
        let since = j.max_slot().unwrap_or(None).map(|s| s - repair_lookback);
        let s = repair_pass(&j, &rpc_url, &program, repair_pages, since);
        stats.repair_passes += 1;
        stats.repair_filled += s["filled"].as_i64().unwrap_or(0);
        s
    } else {
        json!({"skipped": true})
    };

    let c = j.counts().unwrap_or_else(|e| die(e));
    let summary = json!({
        "program": program,
        "seconds": seconds,
        "ws_new": stats.ws_new,
        "ws_dupes": stats.ws_dupes,
        "backfill_new": stats.backfill_new,
        "disconnects": stats.disconnects,
        "reconnects": stats.reconnects,
        "seal_pass": promoted,
        "repair_passes": stats.repair_passes,
        "repair_filled": stats.repair_filled,
        "final_repair": final_repair,
        "journal": {
            "events": c.total, "with_err": c.with_err,
            "finalized": c.finalized, "confirmed": c.confirmed,
            "error": c.errored, "missing": c.missing, "unsealed": c.unsealed,
            "gaps": c.gaps, "gaps_open": c.gaps_open,
            "backfilled": c.backfilled, "blind_window_ms": c.blind_ms,
        }
    });
    println!("{}", serde_json::to_string_pretty(&summary).unwrap());
}

fn run_tail(args: &[String]) {
    let db = PathBuf::from(arg(args, "--db").unwrap_or_else(|| "reeflog.db".into()));
    let j = Journal::open(&db).unwrap_or_else(|e| die(e));
    let cursor_path = PathBuf::from(require(args, "--cursor"));
    let limit: i64 = arg(args, "--limit")
        .and_then(|s| s.parse().ok())
        .unwrap_or(1000);
    let after: i64 = read_cursor(&cursor_path);
    let rows: Vec<EventRow> = j.events_after(after, limit).unwrap_or_else(|e| die(e));
    for r in &rows {
        println!(
            "{}",
            serde_json::json!({
                "seq": r.seq, "signature": r.signature, "slot": r.slot,
                "err": r.err, "source": r.source, "seal": r.seal_status
            })
        );
    }
    if let Some(last) = rows.last() {
        write_cursor(&cursor_path, last.seq);
    }
    eprintln!(
        "reeflog: {} events after cursor {after} → cursor now {}",
        rows.len(),
        read_cursor(&cursor_path)
    );
}

fn read_cursor(path: &Path) -> i64 {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|s| s.trim().parse().ok())
        .unwrap_or(0)
}

fn write_cursor(path: &Path, seq: i64) {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            let _ = std::fs::create_dir_all(parent);
        }
    }
    std::fs::write(path, format!("{seq}\n")).unwrap_or_else(|e| die(format!("cursor write: {e}")));
}

fn run_verify(args: &[String]) {
    let db = PathBuf::from(arg(args, "--db").unwrap_or_else(|| "reeflog.db".into()));
    let j = Journal::open(&db).unwrap_or_else(|e| die(e));
    let program = j
        .meta_get("program")
        .ok()
        .flatten()
        .unwrap_or_else(|| require(args, "--program"));
    let rpc_url = arg(args, "--rpc").unwrap_or_else(|| DEFAULT_RPC.into());
    let pages: usize = arg(args, "--pages")
        .and_then(|s| s.parse().ok())
        .unwrap_or(40);
    let since_slot: Option<i64> = arg(args, "--since-slot").and_then(|s| s.parse().ok());
    let mut fetcher = HttpFetcher { url: rpc_url };
    let r = verify_gap(&j, &mut fetcher, &program, pages, since_slot).unwrap_or_else(|e| die(e));
    // A hole is a hole: record it so status keeps reporting it.
    if !r.missing.is_empty() {
        let _ = j.gap_open(
            "hole",
            j.max_seq().unwrap_or(0),
            j.max_slot().unwrap_or(None),
            j.newest_signature().unwrap_or(None).as_deref(),
            &format!(
                "verify found {} signature(s) on chain missing from the journal (scanned {} signatures over {} pages, truncated={})",
                r.missing.len(), r.found, r.pages, r.truncated
            ),
        );
    }
    // An incomplete scan proves NOTHING. It used to be reported as
    // `missing_count: 0` — the exact lie the kill test caught. Say so loudly
    // and exit non-zero so a caller cannot mistake silence for health.
    let complete = r.complete();
    println!(
        "{}",
        serde_json::to_string_pretty(&json!({
            "program": program,
            "complete": complete,
            "scanned_signatures": r.found,
            "pages": r.pages,
            "truncated": r.truncated,
            "error": r.error,
            "scanned_from_slot": r.scanned_from_slot,
            "since_slot": since_slot,
            "missing_count": r.missing.len(),
            "missing_sample": r.missing.iter().take(5).cloned().collect::<Vec<_>>(),
            "journal_events": j.counts().unwrap_or_else(|e| die(e)).total,
        }))
        .unwrap()
    );
    if !complete {
        eprintln!(
            "reeflog: verify INCOMPLETE (error={:?}, truncated={}, pages={}) — the scan proves nothing about the journal's completeness",
            r.error, r.truncated, r.pages
        );
        std::process::exit(3);
    }
}

/// Fill holes the journal has: scan the claimed window and write back every
/// signature the journal is missing. Bounded, idempotent, and honest about
/// what it could not cover.
fn run_repair(args: &[String]) {
    let db = PathBuf::from(arg(args, "--db").unwrap_or_else(|| "reeflog.db".into()));
    let j = Journal::open(&db).unwrap_or_else(|e| die(e));
    let program = j
        .meta_get("program")
        .ok()
        .flatten()
        .unwrap_or_else(|| require(args, "--program"));
    let rpc_url = arg(args, "--rpc").unwrap_or_else(|| DEFAULT_RPC.into());
    let pages: usize = arg(args, "--pages")
        .and_then(|s| s.parse().ok())
        .unwrap_or(40);
    let since_slot: Option<i64> = arg(args, "--since-slot").and_then(|s| s.parse().ok());
    let summary = repair_pass(&j, &rpc_url, &program, pages, since_slot);
    println!("{}", serde_json::to_string_pretty(&summary).unwrap());
    if summary["complete"].as_bool() != Some(true) {
        eprintln!("reeflog: repair pass INCOMPLETE — the journal may still hold holes");
        std::process::exit(3);
    }
}

fn run_seal(args: &[String]) {
    let db = PathBuf::from(arg(args, "--db").unwrap_or_else(|| "reeflog.db".into()));
    let j = Journal::open(&db).unwrap_or_else(|e| die(e));
    let rpc_url = arg(args, "--rpc").unwrap_or_else(|| DEFAULT_RPC.into());
    let promoted = seal(&j, &rpc_url).unwrap_or_else(|e| die(e));
    let c = j.counts().unwrap_or_else(|e| die(e));
    println!("{}", serde_json::to_string_pretty(&json!({
        "seal_pass": promoted,
        "events": c.total, "finalized": c.finalized, "confirmed": c.confirmed,
        "error": c.errored, "missing": c.missing, "unsealed": c.unsealed,
    })).unwrap());
}

fn run_status(args: &[String]) {
    let db = PathBuf::from(arg(args, "--db").unwrap_or_else(|| "reeflog.db".into()));
    let j = Journal::open(&db).unwrap_or_else(|e| die(e));
    let c = j.counts().unwrap_or_else(|e| die(e));
    println!(
        "{}",
        serde_json::to_string_pretty(&json!({
            "program": j.meta_get("program").unwrap_or(None),
            "events": c.total, "with_err": c.with_err,
            "finalized": c.finalized, "confirmed": c.confirmed,
            "error": c.errored, "missing": c.missing, "unsealed": c.unsealed,
            "gaps": c.gaps, "gaps_open": c.gaps_open,
            "backfilled": c.backfilled, "blind_window_ms": c.blind_ms,
            "max_seq": j.max_seq().unwrap_or(0),
            "max_slot": j.max_slot().unwrap_or(None),
        }))
        .unwrap()
    );
}

fn usage() -> ! {
    eprintln!(
        "reeflog v0 — durable local mailbox for Solana events\n\n\
         USAGE:\n\
         \x20 reeflog init  --db <file> --program <pubkey>\n\
         \x20 reeflog start --db <file> [--seconds N] [--ws URL] [--rpc URL] [--program <pubkey>]\n\
         \x20 reeflog verify --db <file> [--pages N] [--since-slot N] [--rpc URL]\n\
         \x20 reeflog repair --db <file> [--pages N] [--since-slot N] [--rpc URL]\n\
         \x20 reeflog seal  --db <file> [--rpc URL]\n\
         \x20 reeflog tail  --db <file> --cursor <file> [--limit N]\n\
         \x20 reeflog status --db <file>"
    );
    std::process::exit(64);
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let cmd = args.first().map(|s| s.as_str()).unwrap_or("");
    match cmd {
        "init" => {
            let db = PathBuf::from(arg(&args, "--db").unwrap_or_else(|| "reeflog.db".into()));
            let j = Journal::open(&db).unwrap_or_else(|e| die(e));
            let program = require(&args, "--program");
            j.meta_set("program", &program).unwrap_or_else(|e| die(e));
            println!("initialized {} for program {program}", db.display());
        }
        "start" => {
            let rt = tokio::runtime::Runtime::new().unwrap_or_else(|e| die(e));
            rt.block_on(run_start(&args));
        }
        "seal" => run_seal(&args),
        "verify" => run_verify(&args),
        "repair" => run_repair(&args),
        "tail" => run_tail(&args),
        "status" => run_status(&args),
        _ => usage(),
    }
}
