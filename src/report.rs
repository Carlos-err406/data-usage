//! The interactive chart, as a self-contained HTML page.
//!
//! A PNG in a menu item cannot report which bar the pointer is over — AppKit
//! hands the plugin no per-pixel hover — so real per-bar tooltips need a real
//! web view. SwiftBar opens one anchored under the menu bar item for any line
//! carrying `href=<url> webview=true`.
//!
//! The page is written to disk and loaded over `file://`. Everything is inlined:
//! the popover has no network and WKWebView is given no read access beyond the
//! page itself, so an external stylesheet or script would silently not load.

use crate::classify::Class;
use crate::store::{Store, Totals};
use chrono::{Duration as ChronoDuration, Local, TimeZone};
use std::io;
use std::path::Path;

const TEMPLATE: &str = include_str!("report.html");

/// Local midnight, `days_ago` days back.
fn midnight(days_ago: i64) -> i64 {
    let day = Local::now().date_naive() - ChronoDuration::days(days_ago);
    Local
        .from_local_datetime(&day.and_hms_opt(0, 0, 0).unwrap())
        .earliest()
        .map(|d| d.timestamp())
        .unwrap_or(0)
}

/// Start of the current local hour.
fn hour_start(ts: i64) -> i64 {
    let dt = Local.timestamp_opt(ts, 0).earliest();
    match dt {
        Some(d) => d.timestamp() - (d.timestamp().rem_euclid(3600)),
        None => ts - ts.rem_euclid(3600),
    }
}

fn json_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out
}

/// One series as a JSON array of `{label, title, mobile:[rx,tx], ...}`.
fn series_json(series: &[(i64, Totals)], fmt: &str) -> String {
    let mut items = Vec::with_capacity(series.len());
    for (start, totals) in series {
        let label = Local
            .timestamp_opt(*start, 0)
            .earliest()
            .map(|d| d.format(fmt).to_string())
            .unwrap_or_default();
        let title = Local
            .timestamp_opt(*start, 0)
            .earliest()
            .map(|d| d.format("%a %-d %b, %-I:%M %p").to_string())
            .unwrap_or_default();
        let mut parts = Vec::new();
        for class in Class::ALL {
            let (rx, tx) = totals.get(&class).copied().unwrap_or((0, 0));
            parts.push(format!("\"{}\":[{},{}]", class.key(), rx, tx));
        }
        items.push(format!(
            "{{\"label\":\"{}\",\"title\":\"{}\",{}}}",
            json_escape(&label),
            json_escape(&title),
            parts.join(",")
        ));
    }
    format!("[{}]", items.join(","))
}

/// Today's totals, for the header.
fn today_json(store: &Store) -> String {
    let now = crate::sampler::Sampler::now();
    let start = midnight(0);
    let totals = store.totals_between(start, now + 1).unwrap_or_default();
    let mut rows = Vec::new();
    let (mut trx, mut ttx) = (0u64, 0u64);
    for class in Class::ALL {
        let (rx, tx) = totals.get(&class).copied().unwrap_or((0, 0));
        trx += rx;
        ttx += tx;
        rows.push(format!(
            "{{\"key\":\"{}\",\"name\":\"{}\",\"rx\":{},\"tx\":{}}}",
            class.key(),
            class.label(),
            rx,
            tx
        ));
    }
    format!(
        "{{\"date\":\"{}\",\"rows\":[{}],\"total\":[{},{}]}}",
        json_escape(&Local::now().format("%A %-d %B").to_string()),
        rows.join(","),
        trx,
        ttx
    )
}

/// Build the page for the current contents of the database.
pub fn html(store: &Store, link: Option<&str>) -> String {
    let now = crate::sampler::Sampler::now();

    // Aligned to real hour and midnight boundaries, so a bar's label means what
    // it says. A rolling window would put "14:00" on a bar covering 14:37–15:37.
    let hours_from = hour_start(now) - 23 * 3600;
    let hours = store
        .series(hours_from, hour_start(now) + 3600, 3600)
        .unwrap_or_default();
    let days_from = midnight(29);
    let days = store
        .series(days_from, midnight(0) + 86400, 86400)
        .unwrap_or_default();

    // Saying when tracking began is the honest caption for a 30-day chart with
    // two days in it — otherwise 28 empty columns read as 28 idle days.
    let days_label = match store.first_bucket() {
        Ok(Some(first)) if first > days_from => Local
            .timestamp_opt(first, 0)
            .earliest()
            .map(|d| format!("Since {}", d.format("%-d %b")))
            .unwrap_or_else(|| "Last 30 days".into()),
        _ => "Last 30 days".into(),
    };

    let link_json = match link {
        Some(t) => format!("\"{}\"", json_escape(t)),
        None => "null".to_string(),
    };

    TEMPLATE
        .replace("\"__HOURS__\"", &series_json(&hours, "%-I%P"))
        .replace("\"__DAYS__\"", &series_json(&days, "%-d"))
        .replace("\"__TODAY__\"", &today_json(store))
        .replace("__DAYSLABEL__", &days_label)
        .replace("\"__LINK__\"", &link_json)
}

/// Write the page, but only when it differs — the menu re-renders every couple
/// of seconds and there is no reason to touch the disk each time.
pub fn write_if_changed(store: &Store, path: &Path, link: Option<&str>) -> io::Result<()> {
    let next = html(store, link);
    if let Ok(current) = std::fs::read_to_string(path)
        && current == next
    {
        return Ok(());
    }
    std::fs::write(path, next)
}
