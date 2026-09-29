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

/// One period of a chart: its start, its end, and what was used in it.
pub type Slot = (i64, i64, Totals);

/// One tick for the live chart: when it ended, the seconds it covered, its
/// bytes, and the class that carried most of them.
pub type LiveTick = (i64, i64, u64, u64, Option<Class>);

/// How much live history is kept. The chart shows less; the margin covers a
/// slow refresh without leaving its left edge blank.
pub const LIVE_KEEP: i64 = 600;

/// An app's current rates: name, class, down and up in bytes per second.
pub type AppNow = (String, Class, f64, f64);

/// Per-app traffic waiting to be written, keyed by (hour, app, class).
pub type AppUsage = HashMap<(i64, String, Class), (u64, u64)>;

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
             );
             -- Per-app traffic, from nettop. Hourly rather than 5-minute
             -- buckets: rows here multiply by the number of apps, and nothing
             -- reads them finer than a day.
             -- Each tick's traffic for the last few minutes, for the live
             -- chart: finer than the buckets, and trimmed as it goes. `span` is
             -- the seconds the tick's bytes were measured over.
             CREATE TABLE IF NOT EXISTS live (
                 ts    INTEGER PRIMARY KEY,
                 span  INTEGER NOT NULL,
                 rx    INTEGER NOT NULL,
                 tx    INTEGER NOT NULL,
                 class TEXT
             );
             CREATE TABLE IF NOT EXISTS app_usage (
                 hour  INTEGER NOT NULL,
                 app   TEXT    NOT NULL,
                 class TEXT    NOT NULL,
                 rx    INTEGER NOT NULL DEFAULT 0,
                 tx    INTEGER NOT NULL DEFAULT 0,
                 PRIMARY KEY (hour, app, class)
             );
             -- Each app's icon, as a small PNG, once found.
             CREATE TABLE IF NOT EXISTS app_icons (
                 app  TEXT PRIMARY KEY,
                 png  BLOB NOT NULL
             );
             -- Which apps are busy right now: their rates over nettop's latest
             -- sample, in bytes per second, rewritten every couple of seconds.
             CREATE TABLE IF NOT EXISTS app_now (
                 app   TEXT PRIMARY KEY,
                 class TEXT NOT NULL,
                 rx    REAL NOT NULL,
                 tx    REAL NOT NULL,
                 ts    INTEGER NOT NULL
             );",
        )?;
        Ok(Store { conn })
    }

    /// Totals per class, grouped into `step`-second slots across [from, to).
    pub fn series(&self, from: i64, to: i64, step: i64) -> rusqlite::Result<Vec<Slot>> {
        // div_ceil, not truncating division: a span that is not a whole
        // number of steps would otherwise silently drop its last slot.
        let span = (to - from).max(0);
        let slots = (span + step - 1) / step;
        let bounds: Vec<i64> = (0..=slots).map(|i| (from + i * step).min(to)).collect();
        self.series_at(&bounds)
    }

    /// Totals per class for each slot `[bounds[i], bounds[i + 1])`.
    ///
    /// Explicit boundaries rather than a fixed step, for slots that are not
    /// all the same length — calendar months, or days across a DST change.
    pub fn series_at(&self, bounds: &[i64]) -> rusqlite::Result<Vec<Slot>> {
        let mut out: Vec<Slot> = bounds
            .windows(2)
            .map(|w| (w[0], w[1], Totals::new()))
            .collect();
        let (Some(&from), Some(&to)) = (bounds.first(), bounds.last()) else {
            return Ok(out);
        };
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
            // The slot whose start is the last one at or before the bucket.
            let idx = bounds.partition_point(|&b| b <= bucket).wrapping_sub(1);
            if let Some((_, _, totals)) = out.get_mut(idx) {
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
        span: Option<i64>,
    ) -> rusqlite::Result<()> {
        let tx = self.conn.transaction()?;
        // Recorded even when nothing moved: a quiet tick is a zero on the live
        // chart, not a gap in it.
        if let Some(span) = span {
            let (rx, txb) = added
                .values()
                .fold((0u64, 0u64), |a, b| (a.0 + b.0, a.1 + b.1));
            let class = added
                .iter()
                .filter(|(_, (r, t))| r + t > 0)
                .max_by_key(|(_, (r, t))| r + t)
                .map(|(c, _)| c.key());
            // Two refreshes in the same second share a row.
            tx.execute(
                "INSERT INTO live (ts, span, rx, tx, class) VALUES (?1, ?2, ?3, ?4, ?5)
                 ON CONFLICT(ts) DO UPDATE SET rx = rx + ?3, tx = tx + ?4,
                     span = MAX(span, ?2), class = COALESCE(?5, class)",
                params![now, span, rx as i64, txb as i64, class],
            )?;
            tx.execute("DELETE FROM live WHERE ts < ?1", params![now - LIVE_KEEP])?;
        }
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

    /// Live ticks that ended at or after `from`, oldest first.
    pub fn live_since(&self, from: i64) -> rusqlite::Result<Vec<LiveTick>> {
        let mut stmt = self
            .conn
            .prepare("SELECT ts, span, rx, tx, class FROM live WHERE ts >= ?1 ORDER BY ts")?;
        let rows = stmt.query_map(params![from], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, i64>(2)? as u64,
                r.get::<_, i64>(3)? as u64,
                r.get::<_, Option<String>>(4)?
                    .and_then(|k| Class::from_key(&k)),
            ))
        })?;
        rows.collect()
    }

    /// Add per-app traffic to its hourly buckets, in one transaction.
    pub fn add_app_usage(&mut self, usage: &AppUsage) -> rusqlite::Result<()> {
        let tx = self.conn.transaction()?;
        for ((hour, app, class), (rx, txb)) in usage {
            tx.execute(
                "INSERT INTO app_usage (hour, app, class, rx, tx) VALUES (?1, ?2, ?3, ?4, ?5)
                 ON CONFLICT(hour, app, class) DO UPDATE SET rx = rx + ?4, tx = tx + ?5",
                params![hour, app, class.key(), *rx as i64, *txb as i64],
            )?;
        }
        tx.commit()
    }

    /// Replace the apps-active-now snapshot.
    pub fn set_app_now(&mut self, rows: &[AppNow], ts: i64) -> rusqlite::Result<()> {
        let tx = self.conn.transaction()?;
        tx.execute("DELETE FROM app_now", [])?;
        for (app, class, rx, txr) in rows {
            tx.execute(
                "INSERT INTO app_now (app, class, rx, tx, ts) VALUES (?1, ?2, ?3, ?4, ?5)",
                params![app, class.key(), rx, txr, ts],
            )?;
        }
        tx.commit()
    }

    /// The apps-active-now snapshot, if it was written at or after `since`.
    /// An older one is from a helper that has stopped, and says nothing.
    pub fn app_now(&self, since: i64) -> rusqlite::Result<Vec<AppNow>> {
        let mut stmt = self
            .conn
            .prepare("SELECT app, class, rx, tx FROM app_now WHERE ts >= ?1")?;
        let rows = stmt.query_map(params![since], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, f64>(2)?,
                r.get::<_, f64>(3)?,
            ))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (app, k, rx, tx) = row?;
            if let Some(c) = Class::from_key(&k) {
                out.push((app, c, rx, tx));
            }
        }
        Ok(out)
    }

    pub fn has_icon(&self, app: &str) -> bool {
        self.conn
            .query_row(
                "SELECT 1 FROM app_icons WHERE app = ?1",
                params![app],
                |_| Ok(()),
            )
            .optional()
            .ok()
            .flatten()
            .is_some()
    }

    pub fn set_icon(&self, app: &str, png: &[u8]) -> rusqlite::Result<()> {
        self.conn.execute(
            "INSERT INTO app_icons (app, png) VALUES (?1, ?2)
             ON CONFLICT(app) DO UPDATE SET png = ?2",
            params![app, png],
        )?;
        Ok(())
    }

    /// Icons for whichever of these apps have one.
    pub fn icons(&self, apps: &[&str]) -> rusqlite::Result<Vec<(String, Vec<u8>)>> {
        let mut stmt = self
            .conn
            .prepare("SELECT png FROM app_icons WHERE app = ?1")?;
        let mut out = Vec::new();
        for app in apps {
            if let Some(png) = stmt
                .query_row(params![app], |r| r.get::<_, Vec<u8>>(0))
                .optional()?
            {
                out.push((app.to_string(), png));
            }
        }
        Ok(out)
    }

    /// The hour per-app tracking began, if it has.
    pub fn first_app_hour(&self) -> rusqlite::Result<Option<i64>> {
        self.conn
            .query_row("SELECT MIN(hour) FROM app_usage", [], |r| {
                r.get::<_, Option<i64>>(0)
            })
    }

    /// Each of these apps' totals per class for each slot
    /// `[bounds[i], bounds[i + 1])`, as `series_at` gives them for everything.
    pub fn app_series_at(
        &self,
        bounds: &[i64],
        apps: &[&str],
    ) -> rusqlite::Result<HashMap<String, Vec<Totals>>> {
        let slots = bounds.len().saturating_sub(1);
        let mut out: HashMap<String, Vec<Totals>> = apps
            .iter()
            .map(|a| (a.to_string(), vec![Totals::new(); slots]))
            .collect();
        let (Some(&from), Some(&to)) = (bounds.first(), bounds.last()) else {
            return Ok(out);
        };
        let mut stmt = self.conn.prepare(
            "SELECT hour, class, rx, tx FROM app_usage WHERE app = ?1 AND hour >= ?2 AND hour < ?3",
        )?;
        for app in apps {
            let rows = stmt.query_map(params![app, from, to], |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, i64>(2)?,
                    r.get::<_, i64>(3)?,
                ))
            })?;
            let series = out.get_mut(*app).expect("seeded above");
            for row in rows {
                let (hour, k, rx, tx) = row?;
                let Some(class) = Class::from_key(&k) else {
                    continue;
                };
                let idx = bounds.partition_point(|&b| b <= hour).wrapping_sub(1);
                if let Some(totals) = series.get_mut(idx) {
                    let e = totals.entry(class).or_insert((0, 0));
                    e.0 += rx as u64;
                    e.1 += tx as u64;
                }
            }
        }
        Ok(out)
    }

    /// Each app's totals per class over [from, to).
    pub fn apps_between(&self, from: i64, to: i64) -> rusqlite::Result<Vec<(String, Totals)>> {
        let mut stmt = self.conn.prepare(
            "SELECT app, class, SUM(rx), SUM(tx) FROM app_usage
             WHERE hour >= ?1 AND hour < ?2 GROUP BY app, class",
        )?;
        let rows = stmt.query_map(params![from, to], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, i64>(2)?,
                r.get::<_, i64>(3)?,
            ))
        })?;
        let mut by_app: HashMap<String, Totals> = HashMap::new();
        for row in rows {
            let (app, k, rx, tx) = row?;
            if let Some(c) = Class::from_key(&k) {
                by_app
                    .entry(app)
                    .or_default()
                    .insert(c, (rx as u64, tx as u64));
            }
        }
        Ok(by_app.into_iter().collect())
    }

    /// When the accounting instance last checkpointed, i.e. whether the
    /// plugin is still running.
    pub fn last_tick(&self) -> rusqlite::Result<Option<i64>> {
        self.conn
            .query_row("SELECT MAX(updated) FROM iface_state", [], |r| {
                r.get::<_, Option<i64>>(0)
            })
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

pub fn hour_of(ts: i64) -> i64 {
    ts - ts.rem_euclid(3600)
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
