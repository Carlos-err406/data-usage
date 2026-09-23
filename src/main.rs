//! Live macOS network data usage for the menu bar, split by mobile vs Wi-Fi.
//!
//! Runs as a SwiftBar streamable plugin: it prints a menu, then a `~~~`
//! separator, then the next menu, forever.

mod chart;
mod classify;
mod ifstat;
mod lock;
mod netid;
mod nwpath;
mod render;
mod report;
mod sampler;
mod store;
mod units;

use classify::Class;
use std::io::Write;
use std::time::Duration;

/// How often the menu bar rate updates.
const TICK: Duration = Duration::from_secs(1);
/// The dropdown is far more expensive to build than the title (SQL + two PNGs),
/// and nothing in it changes second to second.
const DROPDOWN_EVERY: u32 = 10;

fn main() {
    match std::env::args().nth(1).as_deref() {
        Some("set-class") => cmd_set_class(),
        Some("once") => cmd_once(),
        Some("status") => cmd_status(),
        Some("stream") | None => cmd_stream(),
        Some(other) => {
            eprintln!("unknown command: {other}");
            eprintln!(
                "usage: data-usage [stream|once|status|set-class <iface> <mobile|wifi|wired|auto>]"
            );
            std::process::exit(2);
        }
    }
}

fn exe_path() -> String {
    std::env::current_exe()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| "data-usage".into())
}

fn cmd_stream() {
    nwpath::start();
    nwpath::wait_ready(Duration::from_secs(3));

    let store = match store::Store::open() {
        Ok(s) => s,
        Err(e) => {
            println!("⚠︎ data usage");
            println!("---");
            println!("Cannot open database: {e}");
            return;
        }
    };
    let mut sampler = sampler::Sampler::new(store);
    let exe = exe_path();

    // The first tick only establishes a baseline, so show it as such rather
    // than reporting a rate of zero.
    println!("{}", render::boot_title());
    let _ = std::io::stdout().flush();
    sampler.tick();

    let mut dropdown = String::new();
    let mut n = 0u32;
    loop {
        std::thread::sleep(TICK);
        sampler.tick();

        if n.is_multiple_of(DROPDOWN_EVERY) {
            let href = render::ensure_report(&mut sampler);
            dropdown = render::dropdown(&mut sampler, &exe, href.as_deref());
        }
        n = n.wrapping_add(1);

        let mut out = std::io::stdout().lock();
        let _ = writeln!(out, "{}", render::title(sampler.rate(), None));
        let _ = writeln!(out, "---");
        let _ = out.write_all(dropdown.as_bytes());
        // Tells SwiftBar this menu is complete and the next one follows.
        let _ = writeln!(out, "~~~");
        let _ = out.flush();
    }
}

/// Render one menu and exit. This is what the SwiftBar plugin runs.
///
/// A streaming plugin would update the menu bar more smoothly, but SwiftBar
/// resets the menu item on every `~~~` block, which dismisses the dropdown
/// while you are reading it. A refresh plugin is deferred while the menu is
/// open, so it stays put.
fn cmd_once() {
    nwpath::start();
    nwpath::wait_ready(Duration::from_secs(3));
    let store = store::Store::open().expect("open database");
    let mut sampler = sampler::Sampler::new(store);
    // One read: the delta is measured against the checkpoint the previous run
    // left behind, so there is nothing to wait around for.
    sampler.tick();
    // The page has to exist before the title line can point at it.
    let href = render::ensure_report(&mut sampler);
    println!("{}", render::title(sampler.rate(), href.as_deref()));
    println!("---");
    print!(
        "{}",
        render::dropdown(&mut sampler, &exe_path(), href.as_deref())
    );
}

/// Report what this instance sees, including whether it is the one accounting.
fn cmd_status() {
    nwpath::start();
    nwpath::wait_ready(Duration::from_secs(3));
    let store = store::Store::open().expect("open database");
    let mut sampler = sampler::Sampler::new(store);
    println!(
        "accounting: {}",
        if sampler.is_accounting() {
            "yes"
        } else {
            "no (another instance holds the lock)"
        }
    );
    println!(
        "database:   {}",
        store::data_dir().join("usage.db").display()
    );
    match sampler.current_link() {
        Some(l) => println!(
            "link:       {} · {} → {}",
            l.iface,
            l.detail,
            l.class.map(|c| c.label()).unwrap_or("not counted")
        ),
        None => println!("link:       offline"),
    }
    for (name, info) in nwpath::links() {
        println!(
            "  {name:<8} {:<9} expensive={} constrained={}",
            info.itype.label(),
            info.expensive,
            info.constrained
        );
    }
}

/// Pin an interface to a class, or clear the pin with `auto`.
fn cmd_set_class() {
    let args: Vec<String> = std::env::args().skip(2).collect();
    let (Some(iface), Some(class)) = (args.first(), args.get(1)) else {
        eprintln!("usage: data-usage set-class <iface> <mobile|wifi|wired|auto>");
        std::process::exit(2);
    };
    let class = match class.as_str() {
        "auto" => None,
        other => match Class::from_key(other) {
            Some(c) => Some(c),
            None => {
                eprintln!("unknown class: {other}");
                std::process::exit(2);
            }
        },
    };
    let store = store::Store::open().expect("open database");
    store.set_override(iface, class).expect("save override");
}
