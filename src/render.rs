//! SwiftBar menu output.
//!
//! Format: the first block is the menu bar title, everything after a `---`
//! line is the dropdown. Parameters follow a `|` on each line.

use crate::chart;
use crate::classify::Class;
use crate::sampler::{Rate, Sampler};
use crate::store::Totals;
use crate::units;
use chrono::{Duration as ChronoDuration, Local, NaiveDate, TimeZone};
use std::fmt::Write as _;

/// Menlo rather than the system font: proportional digits make the rate jitter
/// as the number changes, which is very visible in a menu bar that updates
/// every second.
const BAR_FONT: &str = "font=Menlo-Regular size=12";

fn b64(data: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for c in data.chunks(3) {
        let b = [c[0], *c.get(1).unwrap_or(&0), *c.get(2).unwrap_or(&0)];
        let n = ((b[0] as u32) << 16) | ((b[1] as u32) << 8) | b[2] as u32;
        out.push(T[(n >> 18) as usize & 63] as char);
        out.push(T[(n >> 12) as usize & 63] as char);
        out.push(if c.len() > 1 {
            T[(n >> 6) as usize & 63] as char
        } else {
            '='
        });
        out.push(if c.len() > 2 {
            T[n as usize & 63] as char
        } else {
            '='
        });
    }
    out
}

fn total_of(t: &Totals) -> u64 {
    t.values().map(|(rx, tx)| rx + tx).sum()
}

fn local_midnight(days_ago: i64) -> i64 {
    let day = Local::now().date_naive() - ChronoDuration::days(days_ago);
    midnight_of(day)
}

fn midnight_of(day: NaiveDate) -> i64 {
    Local
        .from_local_datetime(&day.and_hms_opt(0, 0, 0).unwrap())
        .earliest()
        .map(|d| d.timestamp())
        .unwrap_or(0)
}

/// Row colour, matching the bars in the charts below.
fn class_color(c: Class) -> &'static str {
    match c {
        Class::Mobile => "#FF9500",
        Class::Wifi => "#0A84FF",
        Class::Wired => "#34C759",
    }
}

/// The menu bar title — live throughput only.
pub fn title(rate: Rate) -> String {
    format!(
        "↓{} ↑{} | {BAR_FONT}",
        units::rate(rate.rx),
        units::rate(rate.tx)
    )
}

/// The whole dropdown.
pub fn dropdown(sampler: &mut Sampler, exe: &str) -> String {
    let now = Sampler::now();
    let link = sampler.current_link();
    let store = &sampler.store;
    let today_start = local_midnight(0);
    let mut s = String::new();

    let today = store
        .totals_between(today_start, now + 1)
        .unwrap_or_default();
    let _ = writeln!(s, "{} | size=13", Local::now().format("Today · %A %-d %B"));

    // Header row. Without it the first number is unlabelled, and there is
    // nothing to say which of the three columns is which.
    //
    // Padded with non-breaking spaces and set at the same size as the rows:
    // leading ordinary spaces are stripped before the line is drawn, which
    // slides the whole header a column to the left, and a smaller font would
    // put the columns on a different monospace grid even if they survived.
    let _ = writeln!(
        s,
        "{}{:>9}{:>9}{:>9} | font=Menlo-Regular size=12 color=#8E8E93",
        "\u{00A0}".repeat(7),
        "total",
        "↓ down",
        "↑ up"
    );

    // No sfimage on these rows: SwiftBar indents the text past the icon, which
    // knocks the icon-less Total row out of alignment with the ones above it.
    let mut day_rx = 0u64;
    let mut day_tx = 0u64;
    for class in Class::ALL {
        let (rx, tx) = today.get(&class).copied().unwrap_or((0, 0));
        day_rx += rx;
        day_tx += tx;
        if rx == 0 && tx == 0 && class == Class::Wired {
            continue; // don't clutter the menu with a link that isn't in use
        }
        let _ = writeln!(
            s,
            "{:<7}{:>9}{:>9}{:>9} | font=Menlo-Regular size=12 color={}",
            class.label(),
            units::bytes(rx + tx),
            units::bytes(rx),
            units::bytes(tx),
            class_color(class)
        );
    }
    let _ = writeln!(
        s,
        "{:<7}{:>9}{:>9}{:>9} | font=Menlo-Regular size=12",
        "Total",
        units::bytes(day_rx + day_tx),
        units::bytes(day_rx),
        units::bytes(day_tx)
    );

    // --- today, by hour ---
    s.push_str("---\n");
    let _ = writeln!(s, "Last 24 hours | size=12");
    let day_series = store.series(now - 24 * 3600, now, 3600).unwrap_or_default();
    push_chart(&mut s, &day_series, 250, 44);

    // --- last 30 days ---
    s.push_str("---\n");
    let _ = writeln!(s, "Last 30 days | size=12");
    let month_start = local_midnight(29);
    let month_series = store.series(month_start, now, 86400).unwrap_or_default();
    push_chart(&mut s, &month_series, 250, 44);

    let month_total: u64 = month_series.iter().map(|(_, t)| total_of(t)).sum();
    let yesterday = store
        .totals_between(local_midnight(1), today_start)
        .unwrap_or_default();
    let _ = writeln!(
        s,
        "Yesterday {:>12} | font=Menlo-Regular size=12",
        units::bytes(total_of(&yesterday))
    );
    let _ = writeln!(
        s,
        "30-day total {:>9} | font=Menlo-Regular size=12",
        units::bytes(month_total)
    );

    // --- current link + manual override ---
    s.push_str("---\n");
    if let Some(link) = link {
        let _ = writeln!(s, "Now on {} · {} | size=12", link.iface, link.detail);
        match &link.pin_key {
            Some(key) => {
                let is_mobile = link.class == Some(Class::Mobile);
                let (label, target) = if is_mobile {
                    ("Count this network as Wi-Fi", "wifi")
                } else {
                    ("Count this network as Mobile", "mobile")
                };
                let _ = writeln!(
                    s,
                    "{label} | bash=\"{exe}\" param1=set-class param2={key} param3={target} terminal=false refresh=true size=12"
                );
                let _ = writeln!(
                    s,
                    "Back to automatic | bash=\"{exe}\" param1=set-class param2={key} param3=auto terminal=false refresh=true size=12"
                );
            }
            // Offering a pin here would key it on an identifier that is about
            // to change, silently orphaning it.
            None => {
                let _ = writeln!(s, "Identifying network… | size=12 color=#8E8E93");
            }
        }
    } else {
        let _ = writeln!(s, "Offline | size=12");
    }

    s.push_str("---\n");
    if let Ok(Some(first)) = store.first_bucket() {
        let since = Local.timestamp_opt(first, 0).earliest();
        if let Some(d) = since {
            let _ = writeln!(s, "Tracking since {} | size=11", d.format("%-d %b %Y"));
        }
    }
    let _ = writeln!(
        s,
        "Reveal database | bash=/usr/bin/open param1=-R param2=\"{}/usage.db\" terminal=false size=11",
        crate::store::data_dir().display()
    );
    s
}

fn push_chart(s: &mut String, series: &[(i64, Totals)], w: u32, h: u32) {
    if series.iter().all(|(_, t)| total_of(t) == 0) {
        let _ = writeln!(s, "  no traffic recorded yet | size=11 color=#8E8E93");
        return;
    }
    // `width`/`height` are undocumented in SwiftBar's README but honoured in
    // MenuLineParameters.resizedImageIfRequested. Both are required: omit
    // either and the image collapses to a few pixels.
    let png = b64(&chart::bars(series, w, h, &chart::PALETTE));
    let _ = writeln!(s, " | image={png} width={w} height={h}");
}

/// A first-run placeholder so the menu never looks broken.
pub fn boot_title() -> String {
    format!("↓– ↑– | {BAR_FONT}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_matches_known_vectors() {
        // Hand-rolled encoder: a fault here would silently corrupt every chart
        // image handed to SwiftBar, which renders as a blank menu item rather
        // than an error.
        assert_eq!(b64(b""), "");
        assert_eq!(b64(b"f"), "Zg==");
        assert_eq!(b64(b"fo"), "Zm8=");
        assert_eq!(b64(b"foo"), "Zm9v");
        assert_eq!(b64(b"foob"), "Zm9vYg==");
        assert_eq!(b64(b"fooba"), "Zm9vYmE=");
        assert_eq!(b64(b"foobar"), "Zm9vYmFy");
    }

    #[test]
    fn base64_covers_the_high_bytes() {
        // Exercises the +/ end of the alphabet, which PNG data hits constantly.
        assert_eq!(b64(&[0xff, 0xef, 0xfe]), "/+/+");
        assert_eq!(b64(&[0x00, 0x00, 0x00]), "AAAA");
    }

    #[test]
    fn base64_length_is_always_a_multiple_of_four() {
        for len in 0..32 {
            let data = vec![0xa5u8; len];
            assert_eq!(b64(&data).len() % 4, 0, "length {len}");
        }
    }
}
