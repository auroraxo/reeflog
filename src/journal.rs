//! SQLite-backed append-only journal for reeflog.
//!
//! One monotone `seq` per event, `signature` is the dedup key so a reconnect
//! that replays notifications never double-counts. Gaps (blind windows) get
//! their own table — a gap must be reported, never swallowed.

use rusqlite::{params, Connection, OptionalExtension};
use std::path::Path;

pub const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS events (
    seq         INTEGER PRIMARY KEY AUTOINCREMENT,
    signature   TEXT NOT NULL UNIQUE,
    slot        INTEGER NOT NULL,
    err         TEXT,
    logs        TEXT,
    source      TEXT NOT NULL DEFAULT 'ws',
    first_seen  INTEGER NOT NULL,
    seal_status TEXT,
    seal_seen   INTEGER
);
CREATE TABLE IF NOT EXISTS gaps (
    id             INTEGER PRIMARY KEY AUTOINCREMENT,
    kind           TEXT NOT NULL,
    since_seq      INTEGER,
    since_slot     INTEGER,
    since_sig      TEXT,
    detail         TEXT NOT NULL DEFAULT '',
    detected_at    INTEGER NOT NULL,
    resolved_at    INTEGER,
    until_slot     INTEGER,
    backfill_count INTEGER NOT NULL DEFAULT 0
);
CREATE TABLE IF NOT EXISTS meta (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
);
"#;

pub struct Journal {
    pub db: Connection,
}

#[derive(Debug, Clone)]
pub struct EventRow {
    pub seq: i64,
    pub signature: String,
    pub slot: i64,
    pub err: Option<String>,
    pub source: String,
    pub first_seen: i64,
    pub seal_status: Option<String>,
}

pub fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

impl Journal {
    pub fn open(path: &Path) -> Result<Journal, String> {
        let db = Connection::open(path).map_err(|e| e.to_string())?;
        let _ = db.query_row("PRAGMA journal_mode=WAL", [], |r| r.get::<_, String>(0));
        db.execute_batch("PRAGMA synchronous=NORMAL;").ok();
        db.execute_batch(SCHEMA).map_err(|e| e.to_string())?;
        Ok(Journal { db })
    }

    pub fn meta_get(&self, key: &str) -> Result<Option<String>, String> {
        self.db
            .query_row("SELECT value FROM meta WHERE key=?1", params![key], |r| r.get(0))
            .optional()
            .map_err(|e| e.to_string())
    }

    pub fn meta_set(&self, key: &str, value: &str) -> Result<(), String> {
        self.db
            .execute(
                "INSERT INTO meta(key,value) VALUES(?1,?2)
                 ON CONFLICT(key) DO UPDATE SET value=excluded.value",
                params![key, value],
            )
            .map_err(|e| e.to_string())?;
        Ok(())
    }

    /// Append an event. Returns (seq, inserted_new). A duplicate signature is
    /// a no-op returning the existing seq — dedup is the journal's job, not
    /// the consumer's.
    pub fn append(
        &self,
        signature: &str,
        slot: i64,
        err: Option<&str>,
        logs: Option<&str>,
        source: &str,
    ) -> Result<(i64, bool), String> {
        let existing: Option<i64> = self
            .db
            .query_row(
                "SELECT seq FROM events WHERE signature=?1",
                params![signature],
                |r| r.get(0),
            )
            .optional()
            .map_err(|e| e.to_string())?;
        if let Some(seq) = existing {
            return Ok((seq, false));
        }
        self.db
            .execute(
                "INSERT INTO events(signature,slot,err,logs,source,first_seen)
                 VALUES(?1,?2,?3,?4,?5,?6)",
                params![signature, slot, err, logs, source, now_ms()],
            )
            .map_err(|e| e.to_string())?;
        Ok((self.db.last_insert_rowid(), true))
    }

    pub fn max_seq(&self) -> Result<i64, String> {
        self.db
            .query_row("SELECT COALESCE(MAX(seq),0) FROM events", [], |r| r.get(0))
            .map_err(|e| e.to_string())
    }

    pub fn max_slot(&self) -> Result<Option<i64>, String> {
        self.db
            .query_row("SELECT MAX(slot) FROM events", [], |r| r.get(0))
            .map_err(|e| e.to_string())
    }

    pub fn last_signature(&self) -> Result<Option<String>, String> {
        self.db
            .query_row(
                "SELECT signature FROM events ORDER BY seq DESC LIMIT 1",
                [],
                |r| r.get(0),
            )
            .optional()
            .map_err(|e| e.to_string())
    }

    /// The NEWEST signature we hold — the boundary for a tail-gap fill.
    ///
    /// Ordered by SLOT, not by `seq`: `seq` is insertion order, and a
    /// backfill inserts *older* events after newer ones, so `seq` order is
    /// not chain order. Slot is the chain's own ordering.
    ///
    /// CAUTION (measured 2026-10-28): this signature may be a phantom the
    /// address index does not know (confirmed then forked out). Never pass it
    /// to `getSignaturesForAddress` as `before` — the endpoint answers -32020
    /// for the whole walk. See the anchor rule in `backfill.rs`.
    pub fn newest_signature(&self) -> Result<Option<String>, String> {
        self.db
            .query_row(
                "SELECT signature FROM events ORDER BY slot DESC, seq DESC LIMIT 1",
                [],
                |r| r.get(0),
            )
            .optional()
            .map_err(|e| e.to_string())
    }

    /// Newest held signatures as anchor CANDIDATES for a backwards walk,
    /// newest first. The walk must try them in order and fall back to a
    /// chain-tip start when every candidate is rejected (-32020): any single
    /// held signature can be a phantom, but it is very unlikely that all of
    /// them are.
    pub fn anchor_candidates(&self, limit: i64) -> Result<Vec<String>, String> {
        let mut stmt = self.db.prepare(
            "SELECT signature FROM events ORDER BY slot DESC, seq DESC LIMIT ?1",
        )
        .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map(params![limit], |r| r.get::<_, String>(0))
            .map_err(|e| e.to_string())?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row.map_err(|e| e.to_string())?);
        }
        Ok(out)
    }

    /// The OLDEST signature we hold, ordered by chain order (slot).
    pub fn oldest_signature(&self) -> Result<Option<String>, String> {        self.db
            .query_row(
                "SELECT signature FROM events ORDER BY slot ASC, seq ASC LIMIT 1",
                [],
                |r| r.get(0),
            )
            .optional()
            .map_err(|e| e.to_string())
    }

    /// The slot of a signature we hold, if we hold it.
    pub fn slot_of(&self, signature: &str) -> Result<Option<i64>, String> {
        self.db
            .query_row(
                "SELECT slot FROM events WHERE signature=?1",
                params![signature],
                |r| r.get(0),
            )
            .optional()
            .map_err(|e| e.to_string())
    }

    /// The OLDEST slot the journal claims to cover.
    pub fn oldest_slot(&self) -> Result<Option<i64>, String> {
        self.db
            .query_row("SELECT MIN(slot) FROM events", [], |r| r.get(0))
            .map_err(|e| e.to_string())
    }

    /// The NEWEST slot the journal claims to cover.
    pub fn newest_slot(&self) -> Result<Option<i64>, String> {
        self.db
            .query_row("SELECT MAX(slot) FROM events", [], |r| r.get(0))
            .map_err(|e| e.to_string())
    }

    /// Every signature we hold, for verification scans.
    pub fn all_signatures(&self) -> Result<std::collections::HashSet<String>, String> {
        let mut stmt = self
            .db
            .prepare("SELECT signature FROM events")
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([], |r| r.get::<_, String>(0))
            .map_err(|e| e.to_string())?;
        let mut out = std::collections::HashSet::new();
        for row in rows {
            out.insert(row.map_err(|e| e.to_string())?);
        }
        Ok(out)
    }

    pub fn gap_open(
        &self,
        kind: &str,
        since_seq: i64,
        since_slot: Option<i64>,
        since_sig: Option<&str>,
        detail: &str,
    ) -> Result<i64, String> {
        self.db
            .execute(
                "INSERT INTO gaps(kind,since_seq,since_slot,since_sig,detail,detected_at)
                 VALUES(?1,?2,?3,?4,?5,?6)",
                params![kind, since_seq, since_slot, since_sig, detail, now_ms()],
            )
            .map_err(|e| e.to_string())?;
        Ok(self.db.last_insert_rowid())
    }

    pub fn gap_close(
        &self,
        gap_id: i64,
        until_slot: Option<i64>,
        backfill_count: i64,
        detail_suffix: &str,
    ) -> Result<(), String> {
        self.db
            .execute(
                "UPDATE gaps SET resolved_at=?2, until_slot=?3, backfill_count=?4,
                   detail = detail || ?5
                 WHERE id=?1",
                params![gap_id, now_ms(), until_slot, backfill_count, detail_suffix],
            )
            .map_err(|e| e.to_string())?;
        Ok(())
    }

    /// A gap that never saw another event stays open — honest state.
    pub fn gap_update_detail(&self, gap_id: i64, suffix: &str) -> Result<(), String> {        self.db
            .execute(
                "UPDATE gaps SET detail = detail || ?2 WHERE id=?1",
                params![gap_id, suffix],
            )
            .map_err(|e| e.to_string())?;
        Ok(())
    }

    /// The oldest open gap of this kind, if any. Repeated repair passes that
    /// keep finding the SAME hole must accumulate detail on one row instead
    /// of inserting a new row every pass — a long run would otherwise flood
    /// `gaps` with thousands of duplicates of the same episode.
    pub fn gap_find_open(&self, kind: &str) -> Result<Option<i64>, String> {
        let mut stmt = self
            .db
            .prepare("SELECT id FROM gaps WHERE kind=?1 AND resolved_at IS NULL ORDER BY id LIMIT 1")
            .map_err(|e| e.to_string())?;
        let mut rows = stmt
            .query_map(params![kind], |r| r.get::<_, i64>(0))
            .map_err(|e| e.to_string())?;
        match rows.next() {
            Some(id) => Ok(Some(id.map_err(|e| e.to_string())?)),
            None => Ok(None),
        }
    }

    pub fn events_after(&self, after_seq: i64, limit: i64) -> Result<Vec<EventRow>, String> {
        let mut stmt = self
            .db
            .prepare(
                "SELECT seq, signature, slot, err, source, first_seen, seal_status
                 FROM events WHERE seq > ?1 ORDER BY seq LIMIT ?2",
            )
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map(params![after_seq, limit], |r| {
                Ok(EventRow {
                    seq: r.get(0)?,
                    signature: r.get(1)?,
                    slot: r.get(2)?,
                    err: r.get(3)?,
                    source: r.get(4)?,
                    first_seen: r.get(5)?,
                    seal_status: r.get(6)?,
                })
            })
            .map_err(|e| e.to_string())?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row.map_err(|e| e.to_string())?);
        }
        Ok(out)
    }

    /// Signatures still awaiting a seal decision (never sealed, or parked at
    /// 'confirmed' awaiting promotion to 'finalized').
    pub fn unsealed(&self, limit: i64) -> Result<Vec<String>, String> {
        let mut stmt = self
            .db
            .prepare(
                "SELECT signature FROM events
                 WHERE seal_status IS NULL OR seal_status = 'confirmed'
                 ORDER BY seq LIMIT ?1",
            )
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map(params![limit], |r| r.get::<_, String>(0))
            .map_err(|e| e.to_string())?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row.map_err(|e| e.to_string())?);
        }
        Ok(out)
    }

    pub fn seal(&self, signature: &str, status: &str, err: Option<&str>) -> Result<(), String> {
        self.db
            .execute(
                "UPDATE events SET seal_status=?2, err=COALESCE(?3, err), seal_seen=?4
                 WHERE signature=?1",
                params![signature, status, err, now_ms()],
            )
            .map_err(|e| e.to_string())?;
        Ok(())
    }

    pub fn counts(&self) -> Result<Counts, String> {
        let total: i64 = self
            .db
            .query_row("SELECT COUNT(*) FROM events", [], |r| r.get(0))
            .map_err(|e| e.to_string())?;
        let with_err: i64 = self
            .db
            .query_row(
                "SELECT COUNT(*) FROM events WHERE err IS NOT NULL AND err != 'null'",
                [],
                |r| r.get(0),
            )
            .map_err(|e| e.to_string())?;
        let finalized: i64 = self
            .db
            .query_row(
                "SELECT COUNT(*) FROM events WHERE seal_status='finalized'",
                [],
                |r| r.get(0),
            )
            .map_err(|e| e.to_string())?;
        let confirmed: i64 = self
            .db
            .query_row(
                "SELECT COUNT(*) FROM events WHERE seal_status='confirmed'",
                [],
                |r| r.get(0),
            )
            .map_err(|e| e.to_string())?;
        let missing: i64 = self
            .db
            .query_row(
                "SELECT COUNT(*) FROM events WHERE seal_status='missing'",
                [],
                |r| r.get(0),
            )
            .map_err(|e| e.to_string())?;
        let errored: i64 = self
            .db
            .query_row(
                "SELECT COUNT(*) FROM events WHERE seal_status='error'",
                [],
                |r| r.get(0),
            )
            .map_err(|e| e.to_string())?;
        let unsealed: i64 = self
            .db
            .query_row(
                "SELECT COUNT(*) FROM events WHERE seal_status IS NULL",
                [],
                |r| r.get(0),
            )
            .map_err(|e| e.to_string())?;
        let gaps: i64 = self
            .db
            .query_row("SELECT COUNT(*) FROM gaps", [], |r| r.get(0))
            .map_err(|e| e.to_string())?;
        let gaps_open: i64 = self
            .db
            .query_row(
                "SELECT COUNT(*) FROM gaps WHERE resolved_at IS NULL",
                [],
                |r| r.get(0),
            )
            .map_err(|e| e.to_string())?;
        let backfilled: i64 = self
            .db
            .query_row(
                "SELECT COALESCE(SUM(backfill_count),0) FROM gaps",
                [],
                |r| r.get(0),
            )
            .map_err(|e| e.to_string())?;
        let blind_ms: i64 = self
            .db
            .query_row(
                "SELECT COALESCE(SUM(
                    CASE WHEN resolved_at IS NOT NULL THEN resolved_at - detected_at ELSE 0 END
                 ),0) FROM gaps",
                [],
                |r| r.get(0),
            )
            .map_err(|e| e.to_string())?;
        Ok(Counts {
            total,
            with_err,
            finalized,
            confirmed,
            missing,
            errored,
            unsealed,
            gaps,
            gaps_open,
            backfilled,
            blind_ms,
        })
    }
}

#[derive(Debug)]
pub struct Counts {
    pub total: i64,
    pub with_err: i64,
    pub finalized: i64,
    pub confirmed: i64,
    pub missing: i64,
    pub errored: i64,
    pub unsealed: i64,
    pub gaps: i64,
    pub gaps_open: i64,
    pub backfilled: i64,
    pub blind_ms: i64,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(tag: &str) -> Journal {
        use std::sync::atomic::{AtomicU64, Ordering};
        static N: AtomicU64 = AtomicU64::new(0);
        let n = N.fetch_add(1, Ordering::SeqCst);
        let p = std::env::temp_dir().join(format!(
            "reeflog-test-{}-{}-{}.db",
            std::process::id(),
            tag,
            n
        ));
        let _ = std::fs::remove_file(&p);
        let _ = std::fs::remove_file(p.with_extension("db-wal"));
        let _ = std::fs::remove_file(p.with_extension("db-shm"));
        Journal::open(&p).expect("open journal")
    }

    #[test]
    fn append_is_idempotent_and_seq_monotone() {
        let j = tmp("append");
        let (s1, new1) = j.append("sigA", 100, None, Some("[]"), "ws").unwrap();
        let (s2, new2) = j.append("sigB", 101, None, Some("[]"), "ws").unwrap();
        let (s1b, new1b) = j.append("sigA", 100, None, Some("[]"), "ws").unwrap();
        assert!(new1 && new2 && !new1b, "dedup must swallow the replay");
        assert_eq!(s1, s1b, "replay returns the original seq");
        assert!(s1 < s2, "seq must be monotone");
        assert_eq!(j.max_seq().unwrap(), s2);
    }

    #[test]
    fn events_after_is_strictly_past_cursor() {
        let j = tmp("after");
        j.append("a", 10, None, None, "ws").unwrap();
        let (s2, _) = j.append("b", 11, None, None, "ws").unwrap();
        let rows = j.events_after(0, 100).unwrap();
        assert_eq!(rows.len(), 2);
        let rows = j.events_after(s2, 100).unwrap();
        assert!(rows.is_empty(), "cursor at tip yields nothing");
    }

    #[test]
    fn gap_roundtrip_reports_blind_window() {
        let j = tmp("gap");
        j.append("a", 10, None, None, "ws").unwrap();
        let g = j.gap_open("disconnect", 1, Some(10), Some("a"), "socket died").unwrap();
        let c = j.counts().unwrap();
        assert_eq!(c.gaps, 1);
        assert_eq!(c.gaps_open, 1);
        j.gap_close(g, Some(42), 5, "; backfilled 5").unwrap();
        let c = j.counts().unwrap();
        assert_eq!(c.gaps_open, 0);
        assert_eq!(c.backfilled, 5);
        assert!(c.blind_ms >= 0);
    }

    #[test]
    fn seal_lifecycle() {
        let j = tmp("seal");
        j.append("a", 10, None, None, "ws").unwrap();
        j.append("b", 11, None, None, "ws").unwrap();
        let u = j.unsealed(100).unwrap();
        assert_eq!(u.len(), 2);
        j.seal("a", "finalized", None).unwrap();
        let u = j.unsealed(100).unwrap();
        assert_eq!(u, vec!["b".to_string()], "finalized leaves the pending set");
        let c = j.counts().unwrap();
        assert_eq!(c.finalized, 1);
        assert_eq!(c.unsealed, 1);
    }

    #[test]
    fn restart_gap_uses_last_signature() {
        let j = tmp("restart");
        j.append("last", 500, None, None, "ws").unwrap();
        assert_eq!(j.max_slot().unwrap(), Some(500));
        assert_eq!(j.last_signature().unwrap(), Some("last".to_string()));
    }

    #[test]
    fn oldest_and_newest_signature_are_ordered() {
        let j = tmp("order");
        j.append("first", 10, None, None, "ws").unwrap();
        j.append("second", 20, None, None, "ws").unwrap();
        assert_eq!(j.oldest_signature().unwrap(), Some("first".to_string()));
        assert_eq!(j.last_signature().unwrap(), Some("second".to_string()));
        assert_eq!(j.newest_signature().unwrap(), Some("second".to_string()));
        let all = j.all_signatures().unwrap();
        assert_eq!(all.len(), 2);
        assert!(all.contains("first"));
        assert!(all.contains("second"));
    }

    #[test]
    fn newest_and_oldest_follow_chain_order_not_insertion_order() {
        // A backfill appends OLDER events after newer ones. seq order would
        // put the backfilled (older) event last; slot order must not.
        let j = tmp("chainorder");
        j.append("newer", 500, None, None, "ws").unwrap();
        j.append("older", 100, None, None, "ws").unwrap();
        assert_eq!(j.newest_signature().unwrap(), Some("newer".to_string()));
        assert_eq!(j.oldest_signature().unwrap(), Some("older".to_string()));
        assert_eq!(
            j.last_signature().unwrap(),
            Some("older".to_string()),
            "seq order is insertion order and does differ"
        );
    }
}
