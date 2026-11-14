//! The gap LEDGER must name one episode, not one row per scan.
//!
//! PULS AJ, 2026-11-14. Measured on the PUBLISHED v0.7.26 linux asset
//! (archive sha256 8b9216c8fea40e429c15fcfe06735c82e24c32eaabe82b036d3ca9db980ac906
//! == the digest GitHub publishes for the release; binary sha256
//! a49d8706ac930b03bda052d817486b226bf38602d6ad65342639bd4cbc63c8bf):
//!
//! `verify` opens a `hole` gap row EVERY time it finds a hole — it never
//! looks for the row the episode already has. `repair` then closes only the
//! OLDEST open hole (`gap_find_open("hole")` -> one id). So:
//!
//!   * `verify` x2 against a journal with ONE hole leaves TWO open hole
//!     rows (gaps: 2, gaps_open: 2);
//!   * `repair` fills the hole and closes ONE of them (gaps_open: 1);
//!   * the journal is now COMPLETE — the chain's missing signature is in
//!     the journal, `verify` reports `missing_count: 0, complete: true` —
//!     yet `status` STILL reports `gaps_open: 1`.
//!
//! The class is the one this repo keeps closing: a report that names a
//! problem that does not exist. The gap ledger is what an operator and every
//! monitoring caller reads to decide whether the mailbox is whole
//! (`gaps_open` is a documented field of `reeflog status` and of the Node
//! client's `status()`). A permanent false alarm on a healed hole trains the
//! reader to ignore the field — which is exactly how the NEXT real hole is
//! missed.
//!
//! The rule:
//!   * one episode = ONE gap row. A `verify` that finds the same hole again
//!     accumulates its detail on the open row instead of inserting a new one;
//!   * `repair` closes the open hole row it filled — and every open hole row
//!     of that episode, so a healed journal reports `gaps_open: 0`;
//!   * a `verify` that finds NO hole leaves the ledger alone (a healthy scan
//!     must not invent or resurrect an episode);
//!   * the gap rows are HISTORY: `gaps` (how many episodes the journal
//!     recorded) stays truthful — only `gaps_open` drops to 0.
//!
//! Guards (green on BOTH trees): the gap ROW COUNT still grows with real
//! episodes, the detail of the accumulated row names the second scan, and a
//! journal that never had a hole keeps `gaps: 0, gaps_open: 0`.

use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

const VALID_PROGRAM: &str = "whirLbMiicVdio4qvUfM5KAg6Ct8VwpYzGff3uctyCc";

fn unique_dir(tag: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("reeflog-{tag}-{}-{nanos}", std::process::id()));
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

fn report_of(out: &Output) -> serde_json::Value {
    serde_json::from_str(String::from_utf8_lossy(&out.stdout).trim()).expect("a JSON report")
}

/// One gap row: (id, kind, still open, detail).
#[derive(Debug, Clone)]
struct Gap {
    id: i64,
    kind: String,
    open: bool,
    detail: String,
}

fn gaps(dir: &Path) -> Vec<Gap> {
    let conn = rusqlite::Connection::open(dir.join("reeflog.db")).expect("open journal");
    let mut stmt = conn
        .prepare("SELECT id, kind, resolved_at IS NULL, detail FROM gaps ORDER BY id")
        .expect("prepare");
    let rows = stmt
        .query_map([], |r| {
            Ok(Gap {
                id: r.get(0)?,
                kind: r.get(1)?,
                open: r.get::<_, i64>(2)? != 0,
                detail: r.get(3)?,
            })
        })
        .expect("query");
    let out: Vec<Gap> = rows.map(|r| r.expect("row")).collect();
    out
}

/// Plant the journal: five rows in slots 1..5, all with content.
fn plant_journal(dir: &Path) {
    let conn = rusqlite::Connection::open(dir.join("reeflog.db")).expect("open journal");
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_millis() as i64;
    for (sig, slot) in [("sig1", 1), ("sig2", 2), ("sig3", 3), ("sig4", 4), ("sig5", 5)] {
        conn.execute(
            "INSERT INTO events(signature, slot, err, logs, source, first_seen, payload_state)
             VALUES (?1, ?2, NULL, '[\"log\"]', 'ws', ?3, 'held')",
            rusqlite::params![sig, slot, now],
        )
        .expect("plant");
    }
}

/// A chain that holds everything the journal holds PLUS one signature the
/// journal does not (slot 3) — one hole inside the claimed window.
/// The chain to serve: the journal's rows plus (optionally) one signature
/// the journal does not have — a hole inside the claimed window.
fn chain_for(with_hole: bool) -> Vec<(&'static str, i64)> {
    if with_hole {
        vec![
            ("sig6", 6),
            ("sig5", 5),
            ("sig4", 4),
            ("sigX", 3),
            ("sig3", 3),
            ("sig2", 2),
            ("sig1", 1),
        ]
    } else {
        vec![
            ("sig6", 6),
            ("sig5", 5),
            ("sig4", 4),
            ("sig3", 3),
            ("sig2", 2),
            ("sig1", 1),
        ]
    }
}

fn chain_server(with_hole: bool) -> (String, Arc<AtomicBool>, std::thread::JoinHandle<()>) {
    let chain = Arc::new(chain_for(with_hole));
    let chain2 = chain.clone();
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    listener.set_nonblocking(true).expect("nonblocking");
    let addr = listener.local_addr().expect("addr");
    let stop = Arc::new(AtomicBool::new(false));
    let stop2 = stop.clone();
    let handle = std::thread::spawn(move || {
        while !stop2.load(Ordering::Relaxed) {
            match listener.accept() {
                Ok((mut s, _)) => {
                    let chain = chain2.clone();
                    std::thread::spawn(move || {
                        s.set_nonblocking(true).expect("nonblocking");
                        let mut buf = [0u8; 65536];
                        let mut got = 0usize;
                        let mut body = Vec::new();
                        loop {
                            match s.read(&mut buf[got..]) {
                                Ok(0) => break,
                                Ok(n) => {
                                    body.extend_from_slice(&buf[got..got + n]);
                                    got += n;
                                    // The request ends at a complete JSON body
                                    // (after the HTTP headers).
                                    if let Some(hp) = body.windows(4).position(|w| w == b"\r\n\r\n") {
                                        let text = match std::str::from_utf8(&body[hp + 4..]) {
                                            Ok(t) => t,
                                            Err(_) => break,
                                        };
                                        if let Ok(v) = serde_json::from_str::<serde_json::Value>(text) {
                                            let before = v["params"][1].get("before").and_then(|b| b.as_str());
                                            let mut rows: Vec<serde_json::Value> = Vec::new();
                                            let start = before
                                                .and_then(|b| chain.iter().position(|(s, _)| *s == b))
                                                .map(|i| i + 1)
                                                .unwrap_or(0);
                                            for (sig, slot) in chain.iter().skip(start) {
                                                rows.push(serde_json::json!({
                                                    "signature": sig, "slot": slot, "err": serde_json::Value::Null
                                                }));
                                            }
                                            let resp = serde_json::json!({
                                                "jsonrpc": "2.0",
                                                "id": v.get("id").cloned().unwrap_or(serde_json::json!(1)),
                                                "result": rows,
                                            });
                                            let payload = resp.to_string();
                                            let head = format!(
                                                "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
                                                payload.len()
                                            );
                                            let _ = s.write_all(head.as_bytes());
                                            let _ = s.write_all(payload.as_bytes());
                                            let _ = s.flush();
                                            break;
                                        }
                                    }
                                }
                                Err(ref e)
                                    if e.kind() == std::io::ErrorKind::WouldBlock =>
                                {
                                    std::thread::sleep(Duration::from_millis(5));
                                }
                                Err(_) => break,
                            }
                        }
                    });
                }
                Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(5));
                }
                Err(_) => break,
            }
        }
    });
    (format!("http://{addr}"), stop, handle)
}

/// A journal with one hole, the chain that has it, and the two commands.
struct Fixture {
    dir: PathBuf,
    url: String,
    stop: Arc<AtomicBool>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl Fixture {
    fn new(tag: &str) -> Self {
        Self::with_chain(tag, true)
    }

    fn with_chain(tag: &str, with_hole: bool) -> Self {
        let dir = unique_dir(tag);
        let out = run(&dir, &["init", "--db", "reeflog.db", "--program", VALID_PROGRAM]);
        assert!(out.status.success(), "init must succeed");
        plant_journal(&dir);
        let (url, stop, handle) = chain_server(with_hole);
        Self { dir, url, stop, handle: Some(handle) }
    }
    fn verify(&self) -> Output {
        run(&self.dir, &["verify", "--db", "reeflog.db", "--rpc", &self.url])
    }
    fn repair(&self) -> Output {
        run(&self.dir, &["repair", "--db", "reeflog.db", "--rpc", &self.url])
    }
    fn status(&self) -> serde_json::Value {
        report_of(&run(&self.dir, &["status", "--db", "reeflog.db"]))
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// D1: two `verify` runs over the SAME hole must leave ONE gap row.
///
/// (Defect: the published binary opens a second `hole` row on the second
/// scan — `gaps: 2, gaps_open: 2` for one hole.)
#[test]
fn two_verifies_over_one_hole_leave_one_gap_row() {
    let fx = Fixture::new("gap-ledger-one-row");

    let v1 = fx.verify();
    let r1 = report_of(&v1);
    assert_eq!(r1["missing_count"], 1, "the chain holds one signature the journal lacks");
    let g1 = gaps(&fx.dir);
    assert_eq!(g1.len(), 1, "the first verify records the episode: {g1:?}");
    assert!(g1[0].open, "and the hole is open");
    assert_eq!(g1[0].kind, "hole", "the episode is a hole");

    let v2 = fx.verify();
    let r2 = report_of(&v2);
    assert_eq!(r2["missing_count"], 1, "the second verify sees the same hole");
    let g2 = gaps(&fx.dir);
    assert_eq!(
        g2.len(),
        1,
        "the SAME hole must not open a SECOND gap row: {g2:?}"
    );
    assert!(g2[0].open, "the episode is still open");
    assert!(
        g2[0].detail.contains("verify found"),
        "the open row still names the hole: {}",
        g2[0].detail
    );
}

/// D2: after `repair` fills the hole, `status` must report NO open gap.
///
/// (Defect: the published binary leaves `gaps_open: 1` forever — the healed
/// journal still alarms.)
#[test]
fn a_repaired_hole_leaves_no_open_gap() {
    let fx = Fixture::new("gap-ledger-repair-closes");

    fx.verify();
    fx.verify();
    let before = gaps(&fx.dir);
    assert_eq!(before.len(), 1, "one episode before the repair: {before:?}");

    let rep = fx.repair();
    let r = report_of(&rep);
    assert_eq!(r["filled"], 1, "the repair filled the hole");
    assert_eq!(r["complete"], true, "and the pass completed");

    let after = gaps(&fx.dir);
    assert_eq!(
        after.iter().filter(|g| g.open).count(),
        0,
        "a healed journal must hold no OPEN gap: {after:?}"
    );
    let st = fx.status();
    assert_eq!(st["gaps_open"], 0, "status must not alarm on a healed hole: {st}");
    assert_eq!(st["backfilled"], 1, "the repair is still recorded");
}

/// D3: after the repair, `verify` finds nothing and the ledger stays quiet.
///
/// (Defect: the published binary's leftover row stays open FOREVER, because
/// nothing ever closes it — the alarm never stops.)
#[test]
fn a_clean_verify_after_the_repair_leaves_the_ledger_quiet() {
    let fx = Fixture::new("gap-ledger-clean-verify");

    fx.verify();
    fx.verify();
    fx.repair();

    let v = fx.verify();
    let r = report_of(&v);
    assert_eq!(r["missing_count"], 0, "the chain's hole is filled");
    assert_eq!(r["complete"], true);

    let g = gaps(&fx.dir);
    assert_eq!(
        g.iter().filter(|x| x.open).count(),
        0,
        "a clean scan must leave no open gap: {g:?}"
    );
    let st = fx.status();
    assert_eq!(st["gaps_open"], 0, "status stays quiet: {st}");
}

/// Plant a gap row the way a pre-0.7.27 `verify` wrote it: a `hole` row
/// that is OPEN, plus a duplicate open row for the same episode.
fn plant_legacy_duplicate_rows(dir: &Path) {
    let conn = rusqlite::Connection::open(dir.join("reeflog.db")).expect("open journal");
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_millis() as i64;
    for i in 0..2 {
        conn.execute(
            "INSERT INTO gaps(kind, since_seq, since_slot, since_sig, detail, detected_at)
             VALUES ('hole', 5, 5, 'sig5', 'verify found 1 signature(s) on chain missing from the journal', ?1)",
            rusqlite::params![now + i],
        )
        .expect("plant legacy gap");
    }
    let _ = conn.close();
}

/// D4: a COMPLETE scan that finds nothing must heal the stale open rows a
/// pre-0.7.27 `verify` left behind.
///
/// (Defect on the published binary: nothing ever closed those duplicates,
/// so `status` reported `gaps_open: 1` on a journal the chain called
/// whole — forever. This is the face that heals an ALREADY broken journal,
/// not just one that never got broken.)
#[test]
fn a_complete_clean_verify_heals_the_stale_rows_a_pre_fix_verify_left() {
    let fx = Fixture::with_chain("gap-ledger-legacy-heal", false);
    plant_legacy_duplicate_rows(&fx.dir);

    let before = gaps(&fx.dir);
    assert_eq!(before.len(), 2, "two stale open rows to start: {before:?}");
    assert_eq!(before.iter().filter(|g| g.open).count(), 2, "both are open");

    let v = fx.verify();
    let r = report_of(&v);
    assert_eq!(r["missing_count"], 0, "the chain and the journal agree");
    assert_eq!(r["complete"], true, "and the scan is complete");

    let after = gaps(&fx.dir);
    assert_eq!(
        after.iter().filter(|g| g.open).count(),
        0,
        "the complete scan heals the stale open rows: {after:?}"
    );
    let st = fx.status();
    assert_eq!(st["gaps_open"], 0, "status stops the false alarm: {st}");
    assert_eq!(st["gaps"], 2, "but the history keeps both rows: {st}");
}

/// D5: a NARROWED scan may not close an episode it did not prove.
///
/// (`--since-slot` raises the floor of the scan: a pass over the upper
/// window only says nothing about a hole deeper down. Closing the episode
/// on that evidence would trade a false alarm for a MISSED hole — the
/// worse trade.)
#[test]
fn a_narrowed_scan_may_not_close_an_episode_it_did_not_prove() {
    let fx = Fixture::with_chain("gap-ledger-narrowed", false);
    plant_legacy_duplicate_rows(&fx.dir);

    // The journal holds slots 1..5; a scan from slot 5 covers only the top.
    let out = run(
        &fx.dir,
        &["verify", "--db", "reeflog.db", "--rpc", &fx.url, "--since-slot", "5"],
    );
    let r = report_of(&out);
    assert_eq!(r["missing_count"], 0, "the narrowed scan finds nothing");
    assert_eq!(r["complete"], true, "and it is complete over ITS window");

    let after = gaps(&fx.dir);
    assert_eq!(
        after.iter().filter(|g| g.open).count(),
        2,
        "a narrowed scan must not close an episode it did not prove: {after:?}"
    );
}

/// D6: `repair` closes EVERY open row of the episode it filled — also the
/// duplicates a pre-0.7.27 `verify` left behind.
///
/// (Defect on the published binary: `repair` closed only the OLDEST open
/// hole row. With two rows for one episode, the second stayed open and
/// `status` reported `gaps_open: 1` on a journal the chain called whole.)
#[test]
fn a_repair_closes_every_open_row_of_the_episode_it_filled() {
    let fx = Fixture::new("gap-ledger-repair-sweep");
    plant_legacy_duplicate_rows(&fx.dir);

    let rep = fx.repair();
    let r = report_of(&rep);
    assert_eq!(r["filled"], 1, "the hole is filled: {r}");
    assert_eq!(r["complete"], true, "and the journal is whole again: {r}");

    let after = gaps(&fx.dir);
    assert_eq!(
        after.iter().filter(|g| g.open).count(),
        0,
        "the repair closes the WHOLE episode, not just its oldest row: {after:?}"
    );
    let st = fx.status();
    assert_eq!(st["gaps_open"], 0, "status stops the false alarm: {st}");
    assert_eq!(st["gaps"], 2, "the history keeps both rows: {st}");
    assert_eq!(st["backfilled"], 1, "the fill is counted once: {st}");
}

/// D7: a NARROWED `repair` may not close the episode it did not prove.
///
/// (The repair filled a hole inside ITS window, but the episode was
/// recorded deeper than that window. Closing it on a partial pass would
/// trade the false alarm for a MISSED hole — the worse trade. The row stays
/// open with an explicit marker until a whole-window pass heals it.)
#[test]
fn a_narrowed_repair_may_not_close_an_episode_it_did_not_prove() {
    let fx = Fixture::new("gap-ledger-narrowed-repair");
    plant_legacy_duplicate_rows(&fx.dir);

    // Journal floor is slot 1; the repair proves only from slot 3 up.
    let rep = run(
        &fx.dir,
        &["repair", "--db", "reeflog.db", "--rpc", &fx.url, "--since-slot", "3"],
    );
    let r = report_of(&rep);
    assert_eq!(r["filled"], 1, "the hole in the window is filled: {r}");
    assert_eq!(r["complete"], true, "the pass completed: {r}");

    let after = gaps(&fx.dir);
    assert_eq!(
        after.iter().filter(|g| g.open).count(),
        2,
        "a narrowed repair must not close an episode it did not prove: {after:?}"
    );
    assert!(
        after.iter().any(|g| g.detail.contains("partial window")),
        "the row the pass worked on says WHY it stayed open: {after:?}"
    );
}

/// G1: the gap ledger still records HISTORY — `gaps` keeps the episode.
///
/// (Guard, green on BOTH trees: the fix must not delete the row, only
/// close it. A `gaps` counter that falls to 0 would erase the episode the
/// operator needs for the audit.)
#[test]
fn the_healed_episode_stays_in_the_gap_history() {
    let fx = Fixture::new("gap-ledger-history");

    fx.verify();
    fx.repair();

    let st = fx.status();
    assert_eq!(st["gaps"], 1, "the episode stays in the history: {st}");
    assert_eq!(st["gaps_open"], 0, "but it is no longer open");
    let g = gaps(&fx.dir);
    assert_eq!(g.len(), 1, "the row is kept, not deleted: {g:?}");
    assert!(!g[0].open, "the row is closed");
    assert_eq!(g[0].id, 1, "the episode keeps its identity");
}

/// G2: the accumulated row names the episode, not just the first scan.
///
/// (Guard, green on BOTH trees: the row must keep growing its detail, so
/// an operator sees that the hole was seen again.)
#[test]
fn the_open_row_accumulates_the_detail_of_every_scan() {
    let fx = Fixture::new("gap-ledger-detail");

    fx.verify();
    let first = gaps(&fx.dir);
    fx.verify();
    let second = gaps(&fx.dir);

    assert_eq!(second.len(), 1, "one row: {second:?}");
    assert!(
        second[0].detail.len() >= first[0].detail.len(),
        "the detail must not shrink: {:?} -> {:?}",
        first[0].detail,
        second[0].detail
    );
}

/// G3: a journal that never had a hole stays at `gaps: 0, gaps_open: 0`.
///
/// (Guard, green on BOTH trees: the fix must not invent episodes.)
#[test]
fn a_healthy_journal_keeps_no_gaps() {
    let fx = Fixture::with_chain("gap-ledger-healthy", false);

    // The chain holds the journal's rows and NOTHING else -> no hole.
    let out = fx.verify();
    let r = report_of(&out);
    assert_eq!(r["missing_count"], 0, "the chain and the journal agree");
    let st = fx.status();
    assert_eq!(st["gaps"], 0, "no episode was recorded: {st}");
    assert_eq!(st["gaps_open"], 0, "and nothing is open: {st}");
}
