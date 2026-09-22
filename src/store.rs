//! History, on disk.
//!
//! Usage is accumulated into fixed 5-minute buckets keyed by class. That's 288
//! rows a day per active class — small enough to keep forever, fine-grained
//! enough to draw an hourly graph. "Resets daily" is a query boundary rather
//! than a destructive reset, so yesterday stays readable.

use crate::classify::Class;
use rusqlite::{Connection, OptionalExtension, params};
use std::collections::HashMap;
use std::path::PathBuf;

pub const BUCKET_SECS: i64 = 300;

pub type Totals = HashMap<Class, (u64, u64)>;

pub struct Store {
    conn: Connection,
}

pub fn data_dir() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
    PathBuf::from(home).join("Library/Application Support/data-usage")
}

impl Store {
    pub fn open() -> rusqlite::Result<Self> {
        let dir = data_dir();
        let _ = std::fs::create_dir_all(&dir);
        let conn = Connection::open(dir.join("usage.db"))?;
        // WAL keeps the writer from blocking a concurrent reader, e.g. a menu
        // click firing a second copy of the binary while the sampler runs.
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS buckets (
                 bucket INTEGER NOT NULL,
                 class  TEXT    NOT NULL,
                 rx     INTEGER NOT NULL DEFAULT 0,
                 tx     INTEGER NOT NULL DEFAULT 0,
                 PRIMARY KEY (bucket, class)
             );
             CREATE TABLE IF NOT EXISTS iface_state (
                 iface   TEXT PRIMARY KEY,
                 rx      INTEGER NOT NULL,
                 tx      INTEGER NOT NULL,
                 updated INTEGER NOT NULL
             );
             CREATE TABLE IF NOT EXISTS overrides (
                 iface TEXT PRIMARY KEY,
                 class TEXT NOT NULL
             );",
        )?;
        Ok(Store { conn })
    }

    pub fn totals_between(&self, from: i64, to: i64) -> rusqlite::Result<Totals> {
        let mut stmt = self.conn.prepare(
            "SELECT class, SUM(rx), SUM(tx) FROM buckets
             WHERE bucket >= ?1 AND bucket < ?2 GROUP BY class",
        )?;
        let rows = stmt.query_map(params![from, to], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, i64>(2)?,
            ))
        })?;
        let mut out = Totals::new();
        for row in rows {
            let (k, rx, tx) = row?;
            if let Some(c) = Class::from_key(&k) {
                out.insert(c, (rx as u64, tx as u64));
            }
        }
        Ok(out)
    }

    /// Totals per class, grouped into `step`-second slots across [from, to).
    pub fn series(&self, from: i64, to: i64, step: i64) -> rusqlite::Result<Vec<(i64, Totals)>> {
        // div_ceil, not truncating division: with `from` at midnight 29 days
        // back, a plain divide yields 29 slots and silently drops today.
        let span = (to - from).max(0);
        let slots = ((span + step - 1) / step) as usize;
        let mut out: Vec<(i64, Totals)> = (0..slots)
            .map(|i| (from + i as i64 * step, Totals::new()))
            .collect();
        let mut stmt = self.conn.prepare(
            "SELECT bucket, class, rx, tx FROM buckets WHERE bucket >= ?1 AND bucket < ?2",
        )?;
        let rows = stmt.query_map(params![from, to], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, i64>(2)?,
                r.get::<_, i64>(3)?,
            ))
        })?;
        for row in rows {
            let (bucket, k, rx, tx) = row?;
            let Some(class) = Class::from_key(&k) else {
                continue;
            };
            let idx = ((bucket - from) / step) as usize;
            if let Some((_, totals)) = out.get_mut(idx) {
                let e = totals.entry(class).or_insert((0, 0));
                e.0 += rx as u64;
                e.1 += tx as u64;
            }
        }
        Ok(out)
    }

    /// Record a tick's traffic and the raw counters that produced it, atomically.
    ///
    /// These must land together. If the checkpoint lags behind the buckets, a
    /// restart re-reads an old checkpoint and counts the intervening bytes a
    /// second time; if it leads, those bytes are lost.
    pub fn commit_tick(
        &mut self,
        bucket: i64,
        added: &Totals,
        counters: &[(String, u64, u64)],
        now: i64,
    ) -> rusqlite::Result<()> {
        let tx = self.conn.transaction()?;
        for (class, (rx, txb)) in added {
            if *rx == 0 && *txb == 0 {
                continue;
            }
            tx.execute(
                "INSERT INTO buckets (bucket, class, rx, tx) VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT(bucket, class) DO UPDATE SET rx = rx + ?3, tx = tx + ?4",
                params![bucket, class.key(), *rx as i64, *txb as i64],
            )?;
        }
        for (iface, rx, txb) in counters {
            tx.execute(
                "INSERT INTO iface_state (iface, rx, tx, updated) VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT(iface) DO UPDATE SET rx = ?2, tx = ?3, updated = ?4",
                params![iface, *rx as i64, *txb as i64, now],
            )?;
        }
        tx.commit()
    }

    pub fn iface_state(&self, iface: &str) -> rusqlite::Result<Option<(u64, u64, i64)>> {
        self.conn
            .query_row(
                "SELECT rx, tx, updated FROM iface_state WHERE iface = ?1",
                params![iface],
                |r| {
                    Ok((
                        r.get::<_, i64>(0)? as u64,
                        r.get::<_, i64>(1)? as u64,
                        r.get::<_, i64>(2)?,
                    ))
                },
            )
            .optional()
    }

    pub fn overrides(&self) -> rusqlite::Result<HashMap<String, Class>> {
        let mut stmt = self.conn.prepare("SELECT iface, class FROM overrides")?;
        let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
        let mut out = HashMap::new();
        for row in rows {
            let (iface, k) = row?;
            if let Some(c) = Class::from_key(&k) {
                out.insert(iface, c);
            }
        }
        Ok(out)
    }

    pub fn set_override(&self, iface: &str, class: Option<Class>) -> rusqlite::Result<()> {
        match class {
            Some(c) => self.conn.execute(
                "INSERT INTO overrides (iface, class) VALUES (?1, ?2)
                 ON CONFLICT(iface) DO UPDATE SET class = ?2",
                params![iface, c.key()],
            )?,
            None => self
                .conn
                .execute("DELETE FROM overrides WHERE iface = ?1", params![iface])?,
        };
        Ok(())
    }

    /// Timestamp of the very first recorded bucket, for "tracking since".
    pub fn first_bucket(&self) -> rusqlite::Result<Option<i64>> {
        self.conn
            .query_row("SELECT MIN(bucket) FROM buckets", [], |r| {
                r.get::<_, Option<i64>>(0)
            })
    }
}

pub fn bucket_of(ts: i64) -> i64 {
    ts - ts.rem_euclid(BUCKET_SECS)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn buckets_align_to_the_interval() {
        assert_eq!(bucket_of(0), 0);
        assert_eq!(bucket_of(299), 0);
        assert_eq!(bucket_of(300), 300);
        assert_eq!(bucket_of(301), 300);
        assert_eq!(bucket_of(1_700_000_123), 1_700_000_100);
    }

    #[test]
    fn bucket_alignment_holds_before_the_epoch() {
        // rem_euclid rather than %, so negative timestamps floor rather than
        // rounding toward zero into the wrong bucket.
        assert_eq!(bucket_of(-1), -300);
        assert_eq!(bucket_of(-300), -300);
    }
}
