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
use crate::store::{Slot, Store, Totals};
use chrono::{
    DateTime, Datelike, Duration as ChronoDuration, Local, Months, NaiveDate, TimeZone, Timelike,
};
use std::io;
use std::path::Path;

const TEMPLATE: &str = include_str!("report.html");

/// Local midnight, `days_ago` days back.
fn midnight(days_ago: i64) -> i64 {
    local_midnight(Local::now().date_naive() - ChronoDuration::days(days_ago))
}

fn local_midnight(day: NaiveDate) -> i64 {
    Local
        .from_local_datetime(&day.and_hms_opt(0, 0, 0).unwrap())
        .earliest()
        .map(|d| d.timestamp())
        .unwrap_or(0)
}

/// How finely the all-time chart is sliced.
#[derive(Clone, Copy, PartialEq, Debug)]
enum Grain {
    Day,
    Week,
    Month,
}

/// Up to this many days of history, the all-time chart is daily; up to this
/// many weeks, weekly; past that, monthly. Each switch lands the chart on a
/// few dozen points or more, never on a handful.
const DAILY_UP_TO: i64 = 90;
const WEEKLY_UP_TO: i64 = 104;
/// A first day's history drawn daily would be one point stretched across the
/// width; a week of columns, most still untracked, reads as a start.
const MIN_DAYS: i64 = 7;

/// Slot boundaries covering everything from `first` to `today`, as dates.
///
/// The last boundary is the end of the current period, so today, this week
/// or this month is always the final slot.
fn all_time_bounds(first: NaiveDate, today: NaiveDate) -> (Grain, Vec<NaiveDate>) {
    let days = (today - first).num_days() + 1;
    let (grain, start) = if days <= DAILY_UP_TO {
        let start = first.min(today - ChronoDuration::days(MIN_DAYS - 1));
        (Grain::Day, start)
    } else if days <= WEEKLY_UP_TO * 7 {
        // Weeks run Monday to Sunday.
        let monday = first - ChronoDuration::days(first.weekday().num_days_from_monday() as i64);
        (Grain::Week, monday)
    } else {
        (Grain::Month, first.with_day(1).unwrap_or(first))
    };
    let next = |d: NaiveDate| match grain {
        Grain::Day => d + ChronoDuration::days(1),
        Grain::Week => d + ChronoDuration::days(7),
        Grain::Month => d + Months::new(1),
    };
    let mut bounds = vec![start];
    while *bounds.last().unwrap() <= today {
        bounds.push(next(*bounds.last().unwrap()));
    }
    (grain, bounds)
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

/// One series as a JSON array of `{label, title, tracked, mobile:[rx,tx], ...}`.
///
/// `tracked` is false for periods that ended before the first recorded
/// bucket. The chart needs it to tell "no data" from "no traffic": a line
/// drawn at zero across days before tracking began would claim nothing was
/// used, when really nothing was being measured.
fn series_json(
    series: &[Slot],
    label: impl Fn(DateTime<Local>) -> String,
    title_fmt: &str,
    first: Option<i64>,
) -> String {
    let mut items = Vec::with_capacity(series.len());
    for (start, end, totals) in series {
        let tracked = first.is_some_and(|f| *end > f);
        let label = Local
            .timestamp_opt(*start, 0)
            .earliest()
            .map(&label)
            .unwrap_or_default();
        let title = Local
            .timestamp_opt(*start, 0)
            .earliest()
            .map(|d| d.format(title_fmt).to_string())
            .unwrap_or_default();
        let mut parts = Vec::new();
        for class in Class::ALL {
            let (rx, tx) = totals.get(&class).copied().unwrap_or((0, 0));
            parts.push(format!("\"{}\":[{},{}]", class.key(), rx, tx));
        }
        items.push(format!(
            "{{\"label\":\"{}\",\"title\":\"{}\",\"tracked\":{},{}}}",
            json_escape(&label),
            json_escape(&title),
            tracked,
            parts.join(",")
        ));
    }
    format!("[{}]", items.join(","))
}

/// Apps named in the apps card; the rest are summed into one "Other" row.
const APP_ROWS: usize = 5;

/// `{"mobile":[rx,tx],"wifi":[…],"wired":[…]}` for a set of totals.
fn classes_json(totals: &Totals) -> String {
    let parts: Vec<String> = Class::ALL
        .iter()
        .map(|c| {
            let (rx, tx) = totals.get(c).copied().unwrap_or((0, 0));
            format!("\"{}\":[{rx},{tx}]", c.key())
        })
        .collect();
    parts.join(",")
}

/// The apps card for one timeframe: its top apps, each with its usage per
/// period so the chart can draw it on hover, and everything else summed —
/// and listed, for the full list behind the summed row.
/// Adds every app it names to `named`, for their icons.
fn range_apps_json(store: &Store, slots: &[Slot], named: &mut Vec<String>) -> String {
    let mut bounds: Vec<i64> = slots.iter().map(|s| s.0).collect();
    if let Some(last) = slots.last() {
        bounds.push(last.1);
    }
    let (Some(&from), Some(&to)) = (bounds.first(), bounds.last()) else {
        return "{\"list\":[],\"other\":null,\"more\":[]}".into();
    };
    let mut apps = store.apps_between(from, to).unwrap_or_default();
    let sum = |t: &Totals| t.values().map(|(rx, tx)| rx + tx).sum::<u64>();
    apps.sort_by(|a, b| sum(&b.1).cmp(&sum(&a.1)).then_with(|| a.0.cmp(&b.0)));
    let rest = apps.split_off(APP_ROWS.min(apps.len()));
    let top: Vec<&str> = apps.iter().map(|(n, _)| n.as_str()).collect();
    let series = store.app_series_at(&bounds, &top).unwrap_or_default();

    let list: Vec<String> = apps
        .iter()
        .map(|(name, totals)| {
            // Per period, just each class's total: enough to draw a line.
            let per_slot = series.get(name).map(Vec::as_slice).unwrap_or(&[]);
            let lines: Vec<String> = Class::ALL
                .iter()
                .map(|c| {
                    let v: Vec<String> = per_slot
                        .iter()
                        .map(|t| t.get(c).map_or(0, |(rx, tx)| rx + tx).to_string())
                        .collect();
                    format!("\"{}\":[{}]", c.key(), v.join(","))
                })
                .collect();
            if !named.contains(name) {
                named.push(name.clone());
            }
            format!(
                "{{\"name\":\"{}\",{},\"s\":{{{}}}}}",
                json_escape(name),
                classes_json(totals),
                lines.join(",")
            )
        })
        .collect();

    // Every app past the top, by name, for the full list. No per-period
    // series: that list covers the chart, so nothing there draws on it.
    let more: Vec<String> = rest
        .iter()
        .map(|(name, totals)| {
            if !named.contains(name) {
                named.push(name.clone());
            }
            format!(
                "{{\"name\":\"{}\",{}}}",
                json_escape(name),
                classes_json(totals)
            )
        })
        .collect();
    let other = if rest.is_empty() {
        "null".to_string()
    } else {
        let mut totals = Totals::new();
        for (_, t) in &rest {
            for (c, (rx, tx)) in t {
                let e = totals.entry(*c).or_insert((0, 0));
                e.0 += rx;
                e.1 += tx;
            }
        }
        format!("{{\"n\":{},{}}}", rest.len(), classes_json(&totals))
    };
    // The first period per-app tracking covers, so an app's line on the chart
    // starts there rather than claiming zeros from before it was watched.
    let a0 = store
        .first_app_hour()
        .ok()
        .flatten()
        .and_then(|h| slots.iter().position(|s| s.1 > h))
        .map_or("null".into(), |i| i.to_string());
    format!(
        "{{\"from\":{from},\"a0\":{a0},\"list\":[{}],\"other\":{other},\"more\":[{}]}}",
        list.join(","),
        more.join(",")
    )
}

/// Icons for the named apps, as data URIs: the page may load nothing but
/// itself, so an image has to travel inside it.
fn icons_json(store: &Store, named: &[String]) -> String {
    let names: Vec<&str> = named.iter().map(String::as_str).collect();
    let items: Vec<String> = store
        .icons(&names)
        .unwrap_or_default()
        .into_iter()
        .map(|(app, png)| {
            format!(
                "\"{}\":\"data:image/png;base64,{}\"",
                json_escape(&app),
                base64(&png)
            )
        })
        .collect();
    format!("{{{}}}", items.join(","))
}

/// Which apps are busy right now, from the per-app helper.
fn app_now_json(store: &Store, now: i64) -> String {
    // The helper rewrites this every couple of seconds; anything older is
    // from one that has stopped.
    let items: Vec<String> = store
        .app_now(now - 30)
        .unwrap_or_default()
        .into_iter()
        .map(|(app, c, rx, tx)| {
            format!(
                "\"{}\":[{:.0},{:.0},\"{}\"]",
                json_escape(&app),
                rx,
                tx,
                c.key()
            )
        })
        .collect();
    format!("{{{}}}", items.join(","))
}

fn base64(bytes: &[u8]) -> String {
    const ABC: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let n = chunk.iter().fold(0u32, |a, &b| a << 8 | b as u32) << (8 * (3 - chunk.len()));
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(ABC[(n >> (18 - 6 * i) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

/// Seconds of live history the chart shows.
const LIVE_SECS: i64 = 180;

/// The live chart's ticks, plus the current link's class for colouring the
/// quiet ones that carried nothing.
fn live_json(store: &Store, now: i64, class: Option<Class>) -> String {
    let key = |c: Option<Class>| match c {
        Some(c) => format!("\"{}\"", c.key()),
        None => "null".into(),
    };
    // A tick is kept if any of its span falls inside the window, so the left
    // edge starts filled rather than blank until the next tick.
    let pts: Vec<String> = store
        .live_since(now - LIVE_SECS)
        .unwrap_or_default()
        .into_iter()
        .map(|(ts, span, rx, tx, c)| format!("[{ts},{span},{rx},{tx},{}]", key(c)))
        .collect();
    format!(
        "{{\"now\":{now},\"secs\":{LIVE_SECS},\"k\":{},\"pts\":[{}]}}",
        key(class),
        pts.join(",")
    )
}

/// One timeframe, as the page's tabs expect it. The tabs drive the main card,
/// the history chart and the apps card together.
struct Range {
    key: &'static str,
    tab: &'static str,
    /// For the main card: "Last 7 days".
    title: &'static str,
    /// For the history chart, which the main card has already named the
    /// timeframe for: how it is sliced, "Every 3 hours".
    chart: &'static str,
    /// The dates it covers, beside the main card's title.
    span: String,
    /// Label every this-many periods on the axis.
    every: usize,
    data: String,
    apps: String,
}

impl Range {
    fn json(&self) -> String {
        format!(
            "{{\"key\":\"{}\",\"tab\":\"{}\",\"title\":\"{}\",\"chart\":\"{}\",\"span\":\"{}\",\"every\":{},\"data\":{},\"apps\":{}}}",
            self.key,
            self.tab,
            json_escape(self.title),
            json_escape(self.chart),
            json_escape(&self.span),
            self.every,
            self.data,
            self.apps
        )
    }
}

/// A timestamp in local time, formatted.
fn local_fmt(ts: i64, f: &str) -> String {
    Local
        .timestamp_opt(ts, 0)
        .earliest()
        .map(|d| d.format(f).to_string())
        .unwrap_or_default()
}

/// A strftime label, for series that label every period.
fn fmt_label(f: &'static str) -> impl Fn(DateTime<Local>) -> String {
    move |d| d.format(f).to_string()
}

/// Build the page for the current contents of the database.
pub fn html(store: &Store, link: Option<&str>, class: Option<Class>) -> String {
    let now = crate::sampler::Sampler::now();
    let first = store.first_bucket().ok().flatten();

    // Aligned to real hour and midnight boundaries, so a period's label means
    // what it says. A rolling window would put "14:00" on a period covering
    // 14:37–15:37.
    let hour = hour_start(now);
    // Today from midnight, through the hour now under way.
    let today_hours = store
        .series(midnight(0), hour + 3600, 3600)
        .unwrap_or_default();
    let hours = store
        .series(hour - 23 * 3600, hour + 3600, 3600)
        .unwrap_or_default();

    // A week in three-hour blocks: hourly would be 168 points, too fine to
    // hover, and daily would be seven, too coarse to show a day's shape. The
    // blocks start at midnight, so a label goes on each day's first.
    let local_hour = Local
        .timestamp_opt(now, 0)
        .earliest()
        .map_or(0, |d| d.hour() as i64);
    let block = hour - (local_hour % 3) * 3600;
    let week = store
        .series(block - 55 * 10800, block + 10800, 10800)
        .unwrap_or_default();
    let day_starts = |d: DateTime<Local>| {
        if d.hour() == 0 {
            d.format("%a").to_string()
        } else {
            String::new()
        }
    };

    // Calendar days, so a day across a DST change is its real length.
    let today = Local::now().date_naive();
    let month_bounds: Vec<i64> = (0..=30)
        .map(|i| local_midnight(today - ChronoDuration::days(29 - i)))
        .collect();
    let month = store.series_at(&month_bounds).unwrap_or_default();

    // Everything since tracking began, sliced more coarsely as history grows
    // so the chart keeps a readable number of points.
    let first_day = first
        .and_then(|f| Local.timestamp_opt(f, 0).earliest())
        .map(|d| d.date_naive())
        .unwrap_or(today);
    let (grain, dates) = all_time_bounds(first_day, today);
    let bounds: Vec<i64> = dates.into_iter().map(local_midnight).collect();
    let all = store.series_at(&bounds).unwrap_or_default();
    let (all_chart, label_fmt, title_fmt) = match grain {
        Grain::Day => ("Daily", "%-d %b", "%A %-d %B"),
        Grain::Week => ("Weekly", "%-d %b", "Week of %-d %B %Y"),
        Grain::Month => ("Monthly", "%b %y", "%B %Y"),
    };

    let link_json = match link {
        Some(t) => format!("\"{}\"", json_escape(t)),
        None => "null".to_string(),
    };

    // A day's tooltip names the day; a time on a daily bucket means nothing.
    let hour_title = "%a %-d %b, %-I:%M %p";
    let start = |slots: &[Slot]| slots.first().map_or(now, |s| s.0);
    let dates = |slots: &[Slot]| {
        format!(
            "{} – {}",
            local_fmt(start(slots), "%-d %b"),
            local_fmt(now, "%-d %b")
        )
    };
    let mut named = Vec::new();
    let ranges = [
        Range {
            key: "today",
            tab: "Today",
            title: "Today",
            chart: "Hourly",
            span: local_fmt(now, "%A %-d %B"),
            every: 3,
            data: series_json(&today_hours, fmt_label("%-I%P"), hour_title, first),
            apps: range_apps_json(store, &today_hours, &mut named),
        },
        Range {
            key: "24h",
            tab: "24H",
            title: "Last 24 hours",
            chart: "Hourly",
            span: format!("Since {}", local_fmt(start(&hours), "%a %-I %p")),
            every: 3,
            data: series_json(&hours, fmt_label("%-I%P"), hour_title, first),
            apps: range_apps_json(store, &hours, &mut named),
        },
        Range {
            key: "7d",
            tab: "7D",
            title: "Last 7 days",
            chart: "Every 3 hours",
            span: dates(&week),
            every: 1,
            data: series_json(&week, day_starts, hour_title, first),
            apps: range_apps_json(store, &week, &mut named),
        },
        Range {
            key: "30d",
            tab: "30D",
            title: "Last 30 days",
            chart: "Daily",
            span: dates(&month),
            every: 5,
            data: series_json(&month, fmt_label("%-d %b"), "%A %-d %B", first),
            apps: range_apps_json(store, &month, &mut named),
        },
        Range {
            key: "all",
            tab: "All",
            title: "All time",
            chart: all_chart,
            span: format!("Since {}", local_fmt(first.unwrap_or(now), "%-d %b %Y")),
            // Every sixth or so, however many periods history has grown to.
            every: all.len().div_ceil(6).max(1),
            data: series_json(&all, fmt_label(label_fmt), title_fmt, first),
            apps: range_apps_json(store, &all, &mut named),
        },
    ];
    let ranges: Vec<String> = ranges.iter().map(Range::json).collect();

    TEMPLATE
        .replace("\"__RANGES__\"", &format!("[{}]", ranges.join(",")))
        .replace("\"__ICONS__\"", &icons_json(store, &named))
        .replace("\"__APPNOW__\"", &app_now_json(store, now))
        .replace(
            "\"__APPSFROM__\"",
            &store
                .first_app_hour()
                .ok()
                .flatten()
                .map_or("null".into(), |h| h.to_string()),
        )
        .replace("\"__LIVE__\"", &live_json(store, now, class))
        .replace("\"__LINK__\"", &link_json)
}

/// Write the page, but only when it differs — the menu re-renders every couple
/// of seconds and there is no reason to touch the disk each time.
pub fn write_if_changed(
    store: &Store,
    path: &Path,
    link: Option<&str>,
    class: Option<Class>,
) -> io::Result<()> {
    let next = html(store, link, class);
    if let Ok(current) = std::fs::read_to_string(path)
        && current == next
    {
        return Ok(());
    }
    // Written aside and renamed into place. An open popover re-reads this file
    // every couple of seconds, and must never catch it half-written.
    let tmp = path.with_extension("html.tmp");
    std::fs::write(&tmp, next)?;
    std::fs::rename(&tmp, path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_matches_the_standard_alphabet_and_padding() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64(b"foobar"), "Zm9vYmFy");
        assert_eq!(base64(&[0xff, 0xfe]), "//4=");
    }

    fn d(y: i32, m: u32, day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, day).unwrap()
    }

    #[test]
    fn a_new_install_gets_a_week_of_days() {
        let (grain, b) = all_time_bounds(d(2026, 9, 27), d(2026, 9, 28));
        assert_eq!(grain, Grain::Day);
        assert_eq!(b.first(), Some(&d(2026, 9, 22)));
        // Seven slots: eight boundaries, the last one tomorrow.
        assert_eq!(b.len(), 8);
        assert_eq!(b.last(), Some(&d(2026, 9, 29)));
    }

    #[test]
    fn daily_history_starts_on_the_first_day() {
        let (grain, b) = all_time_bounds(d(2026, 7, 1), d(2026, 9, 28));
        assert_eq!(grain, Grain::Day);
        assert_eq!(b.first(), Some(&d(2026, 7, 1)));
        assert_eq!(b.len() as i64, 90 + 1);
    }

    #[test]
    fn past_ninety_days_it_goes_weekly_from_a_monday() {
        let (grain, b) = all_time_bounds(d(2026, 6, 10), d(2026, 9, 28));
        assert_eq!(grain, Grain::Week);
        // 10 June 2026 is a Wednesday.
        assert_eq!(b.first(), Some(&d(2026, 6, 8)));
        assert!(b.iter().all(|x| x.weekday() == chrono::Weekday::Mon));
        // The current week is the last slot.
        assert!(b[b.len() - 2] <= d(2026, 9, 28) && d(2026, 9, 28) < b[b.len() - 1]);
    }

    #[test]
    fn past_two_years_it_goes_monthly() {
        let (grain, b) = all_time_bounds(d(2024, 3, 17), d(2026, 9, 28));
        assert_eq!(grain, Grain::Month);
        assert_eq!(b.first(), Some(&d(2024, 3, 1)));
        assert_eq!(b.last(), Some(&d(2026, 10, 1)));
        assert!(b.iter().all(|x| x.day() == 1));
    }
}
