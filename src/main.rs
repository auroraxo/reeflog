//! reeflog — durable local mailbox for Solana events.
//!
//! v0 scope (see README "Next qualification"):
//! one program over WebSocket → append-only journal with monotone seq →
//! CLI cursor resume → gaps are reported, never swallowed.

mod backfill;
mod journal;
mod payload;
mod reorg;

use backfill::{backfill_tail, repair_holes, verify_recent, HttpFetcher, Report};
use futures_util::{SinkExt, StreamExt};
use journal::{EventRow, Journal};
use payload::hydrate_payloads;
use reorg::{check_sealed, check_sealed_window};
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
// Pipeline helpers
// ---------------------------------------------------------------------------

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
///
/// A null from the status endpoint is NOT enough to write 'missing'. Measured
/// 2026-10-28 against api.mainnet-beta.solana.com: the endpoint answers null
/// for signatures it cannot answer for — and a transport hiccup looks exactly
/// like a fork-out. So a null is confirmed with a second, independent source
/// (`getTransaction`) before the row is declared missing; if that second
/// source cannot answer either, the row stays pending and the pass is
/// incomplete. 'missing' means "the chain does not know this", never "we
/// could not find out".
fn seal<F: backfill::Fetcher>(j: &Journal, f: &mut F) -> Result<Value, String> {
    let pending = j.unsealed(150)?;
    let mut promoted = json!({"finalized":0,"confirmed":0,"error":0,"missing":0,"undecided":0});
    if pending.is_empty() {
        return Ok(promoted);
    }
    let result = f.call(
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
            // Second source: only a refusal from BOTH proves the chain does
            // not know the transaction.
            let tx = f.call(
                "getTransaction",
                json!([sig, {"encoding": "json", "maxSupportedTransactionVersion": 1, "commitment": "finalized"}]),
            );
            match tx {
                // The chain serves it: the status endpoint simply could not
                // answer. Leave the row pending — it is not missing.
                Ok(v) if !v.is_null() => {
                    bump(&mut promoted, "undecided");
                    continue;
                }
                // The chain does not know it: a genuine fork-out.
                Ok(_) => {
                    j.seal(sig, "missing", None)?;
                    bump(&mut promoted, "missing");
                }
                // The second source failed too: no verdict at all. A rate
                // limit must never be recorded as a fact about the chain.
                Err(_) => {
                    bump(&mut promoted, "undecided");
                }
            }
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

/// Batched re-check of recently sealed events against the two independent
/// sources, from inside the running daemon.
///
/// Why it lives here: a retraction that only exists when an operator remembers
/// to run `reeflog reorg check` is not a product — the consumer who acted on a
/// fill at 14:02 finds out at 18:00, if ever. The check is the same one the
/// CLI runs (`check_sealed`), with the same two-source rule, so it cannot
/// invent a revert: on a healthy chain the whole pass costs one batched
/// `getSignatureStatuses` call, because rows the status source serves need no
/// second opinion.
///
/// It is bounded on purpose: `--reorg-limit` rows per pass, newest first, and
/// only rows inside `--reorg-lookback` slots behind the tip — the rows a fork
/// can still unmake, and therefore the rows a consumer could still have acted
/// on. Rate limits are not verdicts here either — an incomplete pass is
/// reported and counted, never folded into "no reorgs".
fn reorg_tick(j: &Journal, rpc_url: &str, limit: i64, lookback_slots: i64) -> Value {
    let min_slot = j.max_slot().unwrap_or(None).map(|s| s - lookback_slots).unwrap_or(0);
    let mut fetcher = HttpFetcher::new(rpc_url.to_string());
    match check_sealed_window(j, &mut fetcher, limit, min_slot) {
        Ok(r) => json!({
            "complete": r.complete(),
            "min_slot": min_slot,
            "considered": r.considered,
            "intact": r.intact,
            "reverted": r.reverted,
            "invalid": r.invalid,
            "incomplete": r.incomplete,
            "more": r.more,
            "error": r.error,
            "reverted_sample": r.reverted_sample,
        }),
        Err(e) => json!({"complete": false, "error": e}),
    }
}

/// Seal a bounded batch of pending events. Exposed for the periodic seal tick
/// inside `start`: without it the daemon would deliver events it has never
/// promised as final, and the reorg check would have no sealed rows to look at.
///
/// The batch is drained, not just taken once: a busy program produces events
/// faster than one batch of 150 rows covers, and a seal tick that only ever
/// sealed the first 150 pending rows would fall behind the stream forever.
/// The drain is bounded so a single tick cannot run unbounded against a
/// rate-limited endpoint.
fn seal_tick(j: &Journal, rpc_url: &str) -> Value {
    let mut fetcher = HttpFetcher::new(rpc_url.to_string());
    let mut total = json!({"finalized":0,"confirmed":0,"error":0,"missing":0,"undecided":0});
    let mut batches = 0;
    loop {
        let before = j.unsealed(150).map(|v| v.len()).unwrap_or(0);
        if before == 0 || batches >= 20 {
            break;
        }
        match seal(j, &mut fetcher) {
            Ok(v) => {
                for k in ["finalized", "confirmed", "error", "missing", "undecided"] {
                    let n = total[k].as_i64().unwrap_or(0) + v[k].as_i64().unwrap_or(0);
                    total[k] = Value::from(n);
                }
            }
            Err(e) => {
                total["error"] = Value::String(e.to_string());
                break;
            }
        }
        batches += 1;
        // Nothing changed: the batch was refused or already sealed. Stop
        // instead of spinning on the same rows.
        let after = j.unsealed(150).map(|v| v.len()).unwrap_or(0);
        if after >= before {
            break;
        }
    }
    total
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
    seal_passes: i64,
    seal_promoted: i64,
    reorg_passes: i64,
    reorg_considered: i64,
    reorg_reverted: i64,
    reorg_incomplete: i64,
    reorg_truncated: i64,
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
            seal_passes: 0,
            seal_promoted: 0,
            reorg_passes: 0,
            reorg_considered: 0,
            reorg_reverted: 0,
            reorg_incomplete: 0,
            reorg_truncated: 0,
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
    // Rows whose content is fetched at shutdown. The socket costs nothing
    // here (its rows already carry payload); this budget is only for rows
    // the signature walk recovered. `0` disables the pass.
    let payload_limit: i64 = arg(args, "--payload-limit")
        .and_then(|s| s.parse().ok())
        .unwrap_or(500);

    // The seal + reorg scheduler. These two passes are what turn a stream of
    // events into a *promise* a consumer can act on: `seal` promotes confirmed
    // to finalized, and the reorg check withdraws that promise again if the
    // chain forgets the transaction. Both ran only as manual CLI commands
    // until now, which meant the mailbox could deliver an event and never
    // tell the consumer that it was taken back.
    //
    // Cadence: the reorg check follows the repair pass (`--reorg-every`,
    // default 60 s) because it needs sealed rows, and a row only becomes
    // sealed when the seal pass has seen it. The reorg window
    // (`--reorg-lookback`, default 300 slots) bounds the exposure: a fork can
    // only unmake an event near the tip, and the rows behind it are the
    // consumer's problem only in the sense that they are already final.
    let seal_every_s: u64 = arg(args, "--seal-every").and_then(|s| s.parse().ok()).unwrap_or(15);
    let reorg_every_s: u64 = arg(args, "--reorg-every").and_then(|s| s.parse().ok()).unwrap_or(60);
    let reorg_limit: i64 = arg(args, "--reorg-limit").and_then(|s| s.parse().ok()).unwrap_or(200);
    let reorg_lookback: i64 = arg(args, "--reorg-lookback").and_then(|s| s.parse().ok()).unwrap_or(300);

    let mut stats = Stats::new();
    let deadline = Instant::now() + Duration::from_secs(seconds);

    // Gap 1: anything since this journal's last event (previous run or crash).
    let mut fetcher = HttpFetcher::new(rpc_url.clone());
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

    // Content recovery. The gap machinery above recovers *signatures*; the
    // rows it fills carry no logs, so without this pass the rows a crash
    // made missing are exactly the rows a consumer cannot read. Killed runs
    // never reach the shutdown pass — the kill test SIGKILLs the daemon — so
    // this pass is what makes the product survive its own failure mode.
    //
    // It runs in the BACKGROUND, beside the ingest loop, not before it: a
    // restart after a kill is precisely the moment when delaying ingestion
    // would widen the blind window. The socket is opened first; the rows get
    // their content while the stream is already running.
    // The payload worker runs for the WHOLE life of the run, not once. The
    // periodic repair pass above keeps recovering rows while we run; a worker
    // that only fired at startup would leave every one of those rows without
    // content, and a killed run never reaches the shutdown pass. Measured
    // 2026-10-28: a 120 s kill-test run recovered 267 rows during execution
    // and the startup-only worker hydrated none of them.
    //
    // It runs in the BACKGROUND, beside the ingest loop: hydration must not
    // delay ingestion, which is precisely the moment that matters after a
    // kill. Each pass drains a bounded batch so a huge backlog cannot starve
    // the process; the loop exits as soon as the run is over.
    let (stop_tx, stop_rx) = std::sync::mpsc::channel::<()>();
    let payload_handle = if payload_limit > 0 {
        let db = db.clone();
        let rpc = rpc_url.clone();
        let rx = stop_rx;
        Some(tokio::task::spawn_blocking(move || {
            // A separate connection: the ingest loop owns the journal handle
            // for the whole run.
            let j2 = match Journal::open(&db) {
                Ok(x) => x,
                Err(e) => {
                    eprintln!("reeflog: payload worker could not open the journal: {e}");
                    return None;
                }
            };
            let mut f = HttpFetcher::new(rpc);
            let mut last: Option<payload::PayloadReport> = None;
            loop {
                // Non-blocking check: the run is over, stop hydrating.
                if rx.try_recv().is_ok() {
                    break;
                }
                match hydrate_payloads(&j2, &mut f, payload_limit) {
                    Ok(r) => {
                        let drained = r.pending_left == 0;
                        last = Some(r);
                        // Nothing left to do: idle until the run ends rather
                        // than spinning on an empty queue.
                        if drained {
                            std::thread::sleep(Duration::from_millis(250));
                            continue;
                        }
                    }
                    Err(e) => {
                        // A journal error is not a reason to stop trying; the
                        // next pass will report it again if it persists.
                        eprintln!("reeflog: payload worker: {e}");
                    }
                }
                std::thread::sleep(Duration::from_millis(250));
            }
            last
        }))
    } else {
        None
    };

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

    // The seal + reorg scheduler. Sealing first, then checking: a reorg check
    // over unsealed rows would prove nothing, because an unsealed row was
    // never promised as final.
    let mut next_seal = Instant::now() + Duration::from_secs(seal_every_s.max(1));
    let mut next_reorg = Instant::now() + Duration::from_secs(reorg_every_s.max(1));
    macro_rules! seal_tick_now {
        () => {{
            next_seal = Instant::now() + Duration::from_secs(seal_every_s.max(1));
            let promoted = seal_tick(&j, &rpc_url);
            stats.seal_passes += 1;
            for k in ["finalized", "confirmed", "error", "missing"] {
                stats.seal_promoted += promoted[k].as_i64().unwrap_or(0);
            }
        }};
    }
    macro_rules! reorg_tick_now {
        () => {{
            next_reorg = Instant::now() + Duration::from_secs(reorg_every_s.max(1));
            let report = reorg_tick(&j, &rpc_url, reorg_limit, reorg_lookback);
            stats.reorg_passes += 1;
            stats.reorg_considered += report["considered"].as_i64().unwrap_or(0);
            stats.reorg_reverted += report["reverted"].as_i64().unwrap_or(0);
            stats.reorg_incomplete += report["incomplete"].as_i64().unwrap_or(0);
            if report["complete"].as_bool() != Some(true) {
                stats.reorg_truncated += 1;
                eprintln!(
                    "reeflog: reorg check INCOMPLETE {report} — a transport failure is not a verdict"
                );
            } else if report["reverted"].as_i64().unwrap_or(0) > 0 {
                eprintln!(
                    "reeflog: {} event(s) REVERTED — the chain no longer knows them; consumers read them from `reeflog reorg tail`",
                    report["reverted"].as_i64().unwrap_or(0)
                );
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
            if Instant::now() >= next_seal {
                seal_tick_now!();
            }
            if Instant::now() >= next_reorg {
                reorg_tick_now!();
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
    let mut seal_fetcher = HttpFetcher::new(rpc_url.clone());
    let promoted = seal(&j, &mut seal_fetcher).unwrap_or_else(|e| {
        eprintln!("reeflog: seal failed at shutdown: {e}");
        json!({"error": e})
    });
    // Final reorg check. Without it the daemon could end its run with a
    // retraction it had already seen but never recorded — the consumer would
    // act on a fill the chain no longer knows.
    let final_reorg = if reorg_every_s > 0 {
        let r = reorg_tick(&j, &rpc_url, reorg_limit, reorg_lookback);
        stats.reorg_passes += 1;
        stats.reorg_considered += r["considered"].as_i64().unwrap_or(0);
        stats.reorg_reverted += r["reverted"].as_i64().unwrap_or(0);
        stats.reorg_incomplete += r["incomplete"].as_i64().unwrap_or(0);
        if r["reverted"].as_i64().unwrap_or(0) > 0 {
            eprintln!(
                "reeflog: {} event(s) REVERTED at shutdown — read them from `reeflog reorg tail`",
                r["reverted"].as_i64().unwrap_or(0)
            );
        }
        r
    } else {
        json!({"skipped": true})
    };
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

    // Content, not just signatures: rows recovered above arrived from the
    // signature walk and carry no logs. Measured 2026-10-28 — a 60 s run
    // recovered 13 rows and stored all 13 without payload. Without this pass
    // the recovered rows are the ones a consumer cannot read.
    //
    // The background worker (started above, beside the ingest loop) gets
    // first refusal on the rows it was already fetching; this pass covers
    // whatever is still missing — including rows the worker never reached.
    let payload_stop = stop_tx;
    let hydrate = if let Some(h) = payload_handle {
        // Tell the worker to stop, then join it: its last pass still counts.
        let _ = payload_stop.send(());
        match h.await {
            Ok(Some(r)) => {
                if r.hydrated > 0 || r.unavailable > 0 || r.invalid > 0 {
                    eprintln!(
                        "reeflog: payload worker filled {} row(s), {} chain-unknown, {} invalid",
                        r.hydrated, r.unavailable, r.invalid
                    );
                }
                json!({
                    "worker": {
                        "hydrated": r.hydrated, "unavailable": r.unavailable,
                        "invalid": r.invalid, "failed": r.failed,
                        "pending_left": r.pending_left, "error": r.error,
                    }
                })
            }
            Ok(None) => json!({"worker": {"error": "payload worker did not finish"}}),
            Err(e) => json!({"worker": {"error": e.to_string()}}),
        }
    } else {
        json!({"worker": {"skipped": true}})
    };
    let mut fetcher_payload = HttpFetcher::new(rpc_url.clone());
    let (r, hydrate_err) = match hydrate_payloads(&j, &mut fetcher_payload, payload_limit) {
        Ok(r) => (r, None),
        Err(e) => {
            eprintln!("reeflog: payload hydrate failed: {e}");
            (Default::default(), Some(e))
        }
    };
    let hydrate = json!({
        "worker": hydrate["worker"].clone(),
        "complete": r.complete() && hydrate_err.is_none(),
        "considered": r.considered,
        "hydrated": r.hydrated,
        "unavailable": r.unavailable,
        "invalid": r.invalid,
        "failed": r.failed,
        "pending_left": r.pending_left,
        "error": hydrate_err.or(r.error),
    });

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
        "seal_passes": stats.seal_passes,
        "seal_promoted": stats.seal_promoted,
        "reorg_passes": stats.reorg_passes,
        "reorg_considered": stats.reorg_considered,
        "reorg_reverted": stats.reorg_reverted,
        "reorg_incomplete": stats.reorg_incomplete,
        "reorg_incomplete_passes": stats.reorg_truncated,
        "final_reorg": final_reorg,
        "repair_passes": stats.repair_passes,
        "repair_filled": stats.repair_filled,
        "final_repair": final_repair,
        "hydrate": hydrate,
        "journal": {
            "events": c.total, "with_err": c.with_err,
            "finalized": c.finalized, "confirmed": c.confirmed,
            "error": c.errored, "missing": c.missing, "unsealed": c.unsealed,
            "reverted": c.reverted,
            "gaps": c.gaps, "gaps_open": c.gaps_open,
            "backfilled": c.backfilled, "blind_window_ms": c.blind_ms,
            "payload_pending": c.payload_pending,
            "payload_unavailable": c.payload_unavailable,
            "payload_invalid": c.payload_invalid,
        }
    });
    println!("{}", serde_json::to_string_pretty(&summary).unwrap());
    if hydrate["complete"].as_bool() != Some(true) {
        eprintln!(
            "reeflog: payload hydrate INCOMPLETE ({} row(s) still pending) — run `reeflog hydrate`",
            hydrate["pending_left"].as_i64().unwrap_or(0)
        );
        std::process::exit(3);
    }
}

fn run_tail(args: &[String]) {
    let db = PathBuf::from(arg(args, "--db").unwrap_or_else(|| "reeflog.db".into()));
    let j = Journal::open(&db).unwrap_or_else(|e| die(e));
    let cursor_path = PathBuf::from(require(args, "--cursor"));
    let limit: i64 = arg(args, "--limit")
        .and_then(|s| s.parse().ok())
        .unwrap_or(1000);
    // The payload is the point of the mailbox: a row without it is a
    // signature, not an event. `--no-logs` keeps the old compact shape for
    // callers that only need identities (the kill-test harness does).
    let with_logs = !args.iter().any(|a| a == "--no-logs");
    let after: i64 = read_cursor(&cursor_path);
    let rows: Vec<EventRow> = j.events_after(after, limit).unwrap_or_else(|e| die(e));
    for r in &rows {
        let mut v = json!({
            "seq": r.seq, "signature": r.signature, "slot": r.slot,
            "err": r.err, "source": r.source, "seal": r.seal_status,
            "first_seen": r.first_seen,
        });
        if with_logs {
            // `logs` is the event's content: the log lines as a JSON array.
            // `null` means the journal holds the signature but not what
            // happened — `payload_state` says whether that is still pending
            // or the chain never knew the transaction.
            v["logs"] = match &r.logs {
                Some(raw) => serde_json::from_str::<Value>(raw).unwrap_or(Value::String(raw.clone())),
                None => Value::Null,
            };
            v["payload_state"] = match r.payload_state.as_deref() {
                Some(s) => Value::String(s.into()),
                None => Value::Null,
            };
        }
        println!("{}", serde_json::to_string(&v).unwrap());
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
    let mut fetcher = HttpFetcher::new(rpc_url);
    let promoted = seal(&j, &mut fetcher).unwrap_or_else(|e| die(e));
    let c = j.counts().unwrap_or_else(|e| die(e));
    println!("{}", serde_json::to_string_pretty(&json!({
        "seal_pass": promoted,
        "events": c.total, "finalized": c.finalized, "confirmed": c.confirmed,
        "error": c.errored, "missing": c.missing, "unsealed": c.unsealed,
        "reverted": c.reverted,
    })).unwrap());
}

/// Re-check sealed events against two independent sources and record the
/// ones the chain no longer knows.
///
/// The two-source rule is the whole point: `getSignatureStatuses` alone can
/// answer null while `getTransaction` still serves the transaction, and a
/// rate limit looks like silence. A revert is recorded only when both
/// sources refuse; anything else is reported as what it is, and an unfinished
/// pass exits 3 like `verify` and `hydrate` do.
fn run_reorg_check(args: &[String]) {
    let db = PathBuf::from(arg(args, "--db").unwrap_or_else(|| "reeflog.db".into()));
    let j = Journal::open(&db).unwrap_or_else(|e| die(e));
    let rpc_url = arg(args, "--rpc").unwrap_or_else(|| DEFAULT_RPC.into());
    let limit: i64 = arg(args, "--limit")
        .and_then(|s| s.parse().ok())
        .unwrap_or(200);
    let mut fetcher = HttpFetcher::new(rpc_url);
    let r = check_sealed(&j, &mut fetcher, limit).unwrap_or_else(|e| die(e));
    let c = j.counts().unwrap_or_else(|e| die(e));
    println!(
        "{}",
        serde_json::to_string_pretty(&json!({
            "complete": r.complete(),
            "considered": r.considered,
            "intact": r.intact,
            "reverted": r.reverted,
            "invalid": r.invalid,
            "incomplete": r.incomplete,
            "more": r.more,
            "error": r.error,
            "reverted_sample": r.reverted_sample,
            "journal": {
                "events": c.total,
                "reverted": c.reverted,
                "reorg_records": j.reorg_count().unwrap_or(0),
            },
        }))
        .unwrap()
    );
    if !r.complete() {
        eprintln!(
            "reeflog: reorg check INCOMPLETE (error={:?}, undecided={}) — a transport failure is not a verdict",
            r.error, r.incomplete
        );
        std::process::exit(3);
    }
}

/// Deliver recorded reorgs to a consumer.
///
/// Reorgs live in their own channel with their own cursor, because a
/// consumer that already read an event cannot learn about its retraction from
/// the event stream: the event keeps its seq and is delivered exactly once.
/// `tail` reports the event's current seal, and `reorg tail` reports the
/// retraction itself.
fn run_reorg_tail(args: &[String]) {
    let db = PathBuf::from(arg(args, "--db").unwrap_or_else(|| "reeflog.db".into()));
    let j = Journal::open(&db).unwrap_or_else(|e| die(e));
    let cursor_path = PathBuf::from(require(args, "--cursor"));
    let limit: i64 = arg(args, "--limit")
        .and_then(|s| s.parse().ok())
        .unwrap_or(1000);
    let after: i64 = read_cursor(&cursor_path);
    let rows = j.reorgs_after(after, limit).unwrap_or_else(|e| die(e));
    for r in &rows {
        println!(
            "{}",
            serde_json::to_string(&json!({
                "reorg_id": r.id,
                "signature": r.signature,
                "slot": r.slot,
                "detected_at": r.detected_at,
                "status_source": r.status_source,
                "tx_source": r.tx_source,
                "detail": r.detail,
            }))
            .unwrap()
        );
    }
    if let Some(last) = rows.last() {
        write_cursor(&cursor_path, last.id);
    }
    eprintln!(
        "reeflog: {} reorg(s) after cursor {after} → cursor now {}",
        rows.len(),
        read_cursor(&cursor_path)
    );
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
            "reverted": c.reverted,
            "gaps": c.gaps, "gaps_open": c.gaps_open,
            "backfilled": c.backfilled, "blind_window_ms": c.blind_ms,
            // How much of the journal actually knows WHAT happened. A row
            // without content is a signature, not an event.
            "payload_pending": c.payload_pending,
            "payload_unavailable": c.payload_unavailable,
            "payload_invalid": c.payload_invalid,
            "max_seq": j.max_seq().unwrap_or(0),
            "max_slot": j.max_slot().unwrap_or(None),
            "min_slot": j.oldest_slot().unwrap_or(None),
            // The exact signatures that bound the window verify scans, so an
            // operator can pin which two chain positions the claim spans.
            "oldest_signature": j.oldest_signature().unwrap_or(None),
            "newest_signature": j.newest_signature().unwrap_or(None),
        }))
        .unwrap()
    );
}

/// Fill in the content of rows the journal holds a signature for but does not
/// know what happened to — the rows the gap machinery recovered.
///
/// Measured 2026-10-28 (live, 60 s run on mainnet): the socket delivered
/// 2,082 rows with payload; `repair` recovered 13 rows, and all 13 were
/// stored without payload. The recovered rows are exactly the rows a
/// consumer could not read, so a mailbox that stops there has not delivered
/// what it promised.
///
/// The pass is honest about three states: `held` (content fetched), 
/// `unavailable` (the chain does not know the transaction — forked out or
/// pruned), and pending (still unknown). An incomplete pass exits 3, like
/// `verify` and `repair`: a rate limit must never look like a clean pass.
fn run_hydrate(args: &[String]) {
    let db = PathBuf::from(arg(args, "--db").unwrap_or_else(|| "reeflog.db".into()));
    let j = Journal::open(&db).unwrap_or_else(|e| die(e));
    let rpc_url = arg(args, "--rpc").unwrap_or_else(|| DEFAULT_RPC.into());
    let limit: i64 = arg(args, "--limit")
        .and_then(|s| s.parse().ok())
        .unwrap_or(500);
    let mut fetcher = HttpFetcher::new(rpc_url);
    let r = hydrate_payloads(&j, &mut fetcher, limit).unwrap_or_else(|e| die(e));
    let c = j.counts().unwrap_or_else(|e| die(e));
    println!(
        "{}",
        serde_json::to_string_pretty(&json!({
            "complete": r.complete(),
            "considered": r.considered,
            "hydrated": r.hydrated,
            "unavailable": r.unavailable,
            "invalid": r.invalid,
            "failed": r.failed,
            "pending_left": r.pending_left,
            "error": r.error,
            "hydrated_sample": r.hydrated_sample,
            "unavailable_sample": r.unavailable_sample,
            "invalid_sample": r.invalid_sample,
            "journal": {
                "events": c.total,
                "payload_pending": c.payload_pending,
                "payload_unavailable": c.payload_unavailable,
                "payload_invalid": c.payload_invalid,
            },
        }))
        .unwrap()
    );
    if !r.complete() {
        eprintln!(
            "reeflog: hydrate INCOMPLETE (error={:?}, failed={}, pending_left={}) — some rows still have no content",
            r.error, r.failed, r.pending_left
        );
        std::process::exit(3);
    }
}

fn usage() -> ! {
    eprintln!(
        "reeflog v0 — durable local mailbox for Solana events\n\n\
         USAGE:\n\
         \x20 reeflog init  --db <file> --program <pubkey>\n\
         \x20 reeflog start --db <file> [--seconds N] [--ws URL] [--rpc URL] [--program <pubkey>]\n\
         \x20              [--seal-every N] [--reorg-every N] [--reorg-limit N] [--reorg-lookback N]\n\
         \x20 reeflog verify --db <file> [--pages N] [--since-slot N] [--rpc URL]\n\
         \x20 reeflog repair --db <file> [--pages N] [--since-slot N] [--rpc URL]\n\
         \x20 reeflog hydrate --db <file> [--limit N] [--rpc URL]\n\
         \x20 reeflog seal  --db <file> [--rpc URL]\n\
         \x20 reeflog tail  --db <file> --cursor <file> [--limit N] [--no-logs]\n\
         \x20 reeflog reorg check --db <file> [--limit N] [--rpc URL]\n\
         \x20 reeflog reorg tail  --db <file> --cursor <file> [--limit N]\n\
         \x20 reeflog status --db <file>"
    );
    std::process::exit(64);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::journal::Journal;
    use std::collections::HashMap;

    #[derive(Default)]
    struct FakeChain {
        status: HashMap<String, Option<String>>,
        tx: HashMap<String, Option<Value>>,
    }

    impl backfill::Fetcher for FakeChain {
        fn call(&mut self, method: &str, params: Value) -> Result<Value, String> {
            match method {
                "getSignatureStatuses" => {
                    let sigs: Vec<String> = params[0]
                        .as_array()
                        .unwrap()
                        .iter()
                        .map(|v| v.as_str().unwrap().to_string())
                        .collect();
                    let out: Vec<Value> = sigs
                        .iter()
                        .map(|s| match self.status.get(s) {
                            Some(Some(st)) => json!({"confirmationStatus": st}),
                            _ => Value::Null,
                        })
                        .collect();
                    Ok(json!({"context":{"slot":1},"value":out}))
                }
                "getTransaction" => {
                    let sig = params[0].as_str().unwrap();
                    match self.tx.get(sig) {
                        Some(Some(v)) => Ok(v.clone()),
                        Some(None) => Ok(Value::Null),
                        None => Err("connection refused".into()),
                    }
                }
                other => panic!("unexpected method {other}"),
            }
        }
    }

    fn tmp(tag: &str) -> Journal {
        use std::sync::atomic::{AtomicU64, Ordering};
        static N: AtomicU64 = AtomicU64::new(0);
        let n = N.fetch_add(1, Ordering::SeqCst);
        let p = std::env::temp_dir().join(format!("reeflog-seal-{tag}-{n}.db"));
        let _ = std::fs::remove_file(&p);
        Journal::open(&p).expect("open journal")
    }

    #[test]
    fn seal_requires_two_sources_before_declaring_missing() {
        let j = tmp("seal2src");
        j.append("sigFinal", 10, None, None, "ws").unwrap();
        j.append("sigTrap", 11, None, None, "ws").unwrap();
        j.append("sigGone", 12, None, None, "ws").unwrap();

        let mut chain = FakeChain::default();
        chain.status.insert("sigFinal".into(), Some("finalized".into()));
        // sigTrap: status source says null, but getTransaction still has it.
        chain.status.insert("sigTrap".into(), None);
        chain.tx.insert("sigTrap".into(), Some(json!({"slot": 11})));
        // sigGone: both sources refuse.
        chain.status.insert("sigGone".into(), None);
        chain.tx.insert("sigGone".into(), None);

        let r = seal(&j, &mut chain).unwrap();
        assert_eq!(r["finalized"], 1);
        assert_eq!(r["missing"], 1, "only the double refusal is missing");
        assert_eq!(r["undecided"], 1, "the trap row stayed pending");

        let c = j.counts().unwrap();
        assert_eq!(c.finalized, 1);
        assert_eq!(c.missing, 1);
        assert_eq!(c.unsealed, 1, "sigTrap is still unsealed for the next pass");
    }
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
        "hydrate" => run_hydrate(&args),
        "tail" => run_tail(&args),
        "status" => run_status(&args),
        "reorg" => {
            let sub = args.get(1).map(|s| s.as_str()).unwrap_or("");
            match sub {
                "check" => run_reorg_check(&args),
                "tail" => run_reorg_tail(&args),
                _ => usage(),
            }
        }
        _ => usage(),
    }
}
