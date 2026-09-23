//! SwiftBar menu output.
//!
//! Format: the first block is the menu bar title, everything after a `---`
//! line is the dropdown. Parameters follow a `|` on each line.

use crate::classify::Class;
use crate::report;
use crate::sampler::{Rate, Sampler};
use crate::units;
use std::fmt::Write as _;
use std::path::Path;

/// Menlo rather than the system font: proportional digits make the rate jitter
/// as the number changes, which is very visible in a menu bar that updates
/// every second.
const BAR_FONT: &str = "font=Menlo-Regular size=12";

/// Percent-encode a path into a `file://` URL.
///
/// Needed twice over: the database lives under "Application Support", and
/// SwiftBar reads an unquoted parameter value only up to the next space.
fn file_url(path: &Path) -> String {
    let mut out = String::from("file://");
    for b in path.to_string_lossy().as_bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'/' | b'-' | b'_' | b'.' | b'~' => {
                out.push(*b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// Wrap text in an ANSI 256-colour escape.
///
/// Not the `color=` parameter, deliberately. SwiftBar attaches a click action
/// to any line carrying a colour — `configureAction` fires on
/// `params.hasAction || params.color != nil` — which gives these read-only
/// rows a highlight on hover and makes them look interactive. Colouring
/// through `ansi=true` leaves `params.color` nil, so the row stays inert.
fn ansi(code: u8, text: &str) -> String {
    format!("\u{1b}[38;5;{code}m{text}\u{1b}[0m")
}

/// Muted grey for labels and placeholders.
const DIM: u8 = 245;

/// Parameters that open the interactive page in a web view popover.
/// Height includes SwiftBar's own 28pt titlebar plus its 4pt top padding,
/// which sit above the web view and eat into whatever is asked for here.
const POPOVER: &str = "webview=true webvieww=560 webviewh=536";

/// Regenerate the interactive page and return its URL.
pub fn ensure_report(sampler: &mut Sampler) -> Option<String> {
    let link = sampler
        .current_link()
        .map(|l| format!("{} · {}", l.iface, l.detail));
    let page = crate::store::data_dir().join("report.html");
    report::write_if_changed(&sampler.store, &page, link.as_deref())
        .ok()
        .map(|_| file_url(&page))
}

/// The menu bar title — live throughput only.
///
/// Carrying the href here is what makes a left click open the popover instead
/// of the menu: `barItemClicked` runs the title line's action first and only
/// falls through to `showMenu()` if nothing fired. Right click still opens the
/// menu, which is where the actions live.
pub fn title(rate: Rate, href: Option<&str>) -> String {
    let mut out = format!(
        "↓{} ↑{} | {BAR_FONT}",
        units::rate(rate.rx),
        units::rate(rate.tx)
    );
    if let Some(url) = href {
        out.push_str(&format!(" href={url} {POPOVER}"));
    }
    out
}

/// The right-click menu.
///
/// Deliberately just the actions. Everything informational lives in the
/// popover now, and a menu that restates it is a second copy to keep in sync
/// and a worse way to read it. What cannot move is the actions: a web page in
/// SwiftBar's popover has no channel back to the plugin, so pinning a network
/// and revealing the database have to be menu items.
pub fn dropdown(sampler: &mut Sampler, exe: &str) -> String {
    let mut s = String::new();

    match sampler.current_link() {
        Some(link) => {
            // Context for the actions below: which network they apply to.
            let _ = writeln!(
                s,
                "{} | size=12 ansi=true",
                ansi(DIM, &format!("{} · {}", link.iface, link.detail))
            );
            // Without a fingerprint a pin would have to key on the interface,
            // which would tag every network reached over it.
            match &link.pin_key {
                Some(key) => {
                    let (label, target) = if link.class == Some(Class::Mobile) {
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
                None => {
                    let _ = writeln!(
                        s,
                        "{} | size=12 ansi=true",
                        ansi(DIM, "Identifying network…")
                    );
                }
            }
        }
        None => {
            let _ = writeln!(s, "{} | size=12 ansi=true", ansi(DIM, "Offline"));
        }
    }

    s.push_str("---\n");
    let _ = writeln!(
        s,
        "Reveal database | bash=/usr/bin/open param1=-R param2=\"{}/usage.db\" terminal=false size=12",
        crate::store::data_dir().display()
    );
    s
}

/// A first-run placeholder so the menu bar never looks broken.
pub fn boot_title() -> String {
    format!("↓ – ↑ – | {BAR_FONT}")
}
