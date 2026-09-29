//! Which apps the traffic belongs to.
//!
//! The interface counters the sampler reads say how much, never who. `nettop`
//! knows who, and needs no privileges to say it — but only while it runs. A
//! one-shot `nettop` sums the sockets open at that instant, so a download that
//! starts and finishes between two plugin refreshes is never seen. A long-lived
//! `nettop` in delta mode does see it: it reports each process's bytes per
//! interval, closed sockets included.
//!
//! The plugin itself lives for milliseconds, so this runs as its own process,
//! `data-usage apps`, which each plugin run starts if it is not already going.
//! It exits on its own once the plugin stops running, or once the binary has
//! been replaced by a new install, and the next plugin run starts it afresh.

use crate::classify::{Class, classify};
use crate::netid;
use crate::nwpath::{self, LinkType};
use crate::store::{AppNow, AppUsage, Store, hour_of};
use std::collections::{HashMap, HashSet, VecDeque};
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::os::fd::{FromRawFd, OwnedFd};
use std::os::unix::process::CommandExt;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant, SystemTime};

const LOCK: &str = "apps.lock";

/// nettop's sample interval, in seconds. Short intervals cost nothing in
/// accuracy — a delta is a delta — so this only trades CPU against how soon
/// the popover catches up.
const INTERVAL: &str = "10";

/// How often buffered usage is written out and the exit conditions checked.
const FLUSH: Duration = Duration::from_secs(15);

/// How often the apps-active-now snapshot is rewritten.
const NOW_EVERY: Duration = Duration::from_secs(2);

/// Each nettop reports once per INTERVAL, so a window a little longer than
/// that holds exactly the latest sample from each.
const NOW_WINDOW: Duration = Duration::from_secs(11);

/// Without a plugin run for this long, the plugin is off (SwiftBar quit, the
/// plugin removed) and so should this be.
const STALE_AFTER: i64 = 120;

/// One nettop per kind of link. nettop can filter by interface type but not
/// by interface, and the type is enough: the link of that type that is up
/// right now decides the class, the same way the sampler decides it.
const TYPES: [(LinkType, &str); 2] = [(LinkType::Wifi, "wifi"), (LinkType::Wired, "wired")];

/// Start the helper unless one is already running.
///
/// Probing the lock is what makes this cheap enough to do on every plugin run.
/// The probe is released before spawning; two runs racing through here just
/// start two helpers, and the one that loses the lock exits.
pub fn ensure_running(exe: &str) {
    if crate::lock::acquire_named(LOCK).is_none() {
        return;
    }
    let _ = Command::new(exe)
        .arg("apps")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        // Its own process group, so it outlives the plugin run that started
        // it rather than being killed along with it.
        .process_group(0)
        .spawn();
}

enum Msg {
    Delta {
        ltype: LinkType,
        app: String,
        /// The app bundle it belongs to, for its icon.
        bundle: Option<String>,
        rx: u64,
        tx: u64,
    },
    /// A nettop exited. The helper exits with it and the next plugin run
    /// starts a fresh one.
    Ended,
}

/// The helper's main loop.
pub fn run() {
    let Some(_lock) = crate::lock::acquire_named(LOCK) else {
        return;
    };
    nwpath::start();
    nwpath::wait_ready(Duration::from_secs(3));
    let Ok(mut store) = Store::open() else {
        return;
    };
    let stamp = exe_stamp();

    let (send, recv) = mpsc::channel();
    let mut children = Vec::new();
    for (ltype, flag) in TYPES {
        let Ok((child, out)) = spawn_nettop(flag) else {
            break;
        };
        children.push(child);
        let send = send.clone();
        std::thread::spawn(move || read_samples(out, ltype, send));
    }
    drop(send);

    let mut net_ids = netid::Cache::default();
    let mut overrides = store.overrides().unwrap_or_default();
    let mut pending = AppUsage::new();
    let mut last_flush = Instant::now();
    // The latest sample's deltas, for which apps are busy right now.
    let mut recent: VecDeque<(Instant, String, Class, u64, u64)> = VecDeque::new();
    let mut last_now = Instant::now();
    // Apps whose icon has been looked for since this helper started.
    let mut iconed: HashSet<String> = HashSet::new();
    loop {
        match recv.recv_timeout(Duration::from_secs(1)) {
            Ok(Msg::Delta {
                ltype,
                app,
                bundle,
                rx,
                tx,
            }) => {
                if let Some(bundle) = bundle
                    && iconed.insert(app.clone())
                    && !store.has_icon(&app)
                    && let Some(png) = icon_png(&bundle)
                {
                    let _ = store.set_icon(&app, &png);
                }
                if let Some(class) = class_of(ltype, &mut net_ids, &overrides) {
                    let hour = hour_of(crate::sampler::Sampler::now());
                    let e = pending.entry((hour, app.clone(), class)).or_insert((0, 0));
                    e.0 += rx;
                    e.1 += tx;
                    recent.push_back((Instant::now(), app, class, rx, tx));
                }
            }
            Ok(Msg::Ended) | Err(mpsc::RecvTimeoutError::Disconnected) => break,
            Err(mpsc::RecvTimeoutError::Timeout) => {}
        }
        if last_now.elapsed() >= NOW_EVERY {
            last_now = Instant::now();
            while recent.front().is_some_and(|r| r.0.elapsed() > NOW_WINDOW) {
                recent.pop_front();
            }
            let _ = store.set_app_now(&now_rates(&recent), crate::sampler::Sampler::now());
        }
        if last_flush.elapsed() < FLUSH {
            continue;
        }
        last_flush = Instant::now();
        if store.add_app_usage(&pending).is_ok() {
            pending.clear();
        }
        overrides = store.overrides().unwrap_or_default();
        let now = crate::sampler::Sampler::now();
        let stale = store
            .last_tick()
            .ok()
            .flatten()
            .is_none_or(|t| now - t > STALE_AFTER);
        if stale || exe_stamp() != stamp {
            break;
        }
    }
    let _ = store.add_app_usage(&pending);
    // Nothing is being watched any more, so nothing is active as far as we know.
    let _ = store.set_app_now(&[], crate::sampler::Sampler::now());
    for mut c in children {
        let _ = c.kill();
        let _ = c.wait();
    }
}

/// Per-app rates over the latest sample, in bytes per second, each with the
/// class that carried most of it.
fn now_rates(recent: &VecDeque<(Instant, String, Class, u64, u64)>) -> Vec<AppNow> {
    let secs: f64 = INTERVAL.parse().unwrap_or(10.0);
    let mut by_app: HashMap<&str, HashMap<Class, (u64, u64)>> = HashMap::new();
    for (_, app, class, rx, tx) in recent {
        let e = by_app
            .entry(app)
            .or_default()
            .entry(*class)
            .or_insert((0, 0));
        e.0 += rx;
        e.1 += tx;
    }
    by_app
        .into_iter()
        .filter_map(|(app, classes)| {
            let (rx, tx) = classes.values().fold((0, 0), |a, b| (a.0 + b.0, a.1 + b.1));
            let class = classes.iter().max_by_key(|(_, (r, t))| r + t)?.0;
            Some((app.to_string(), *class, rx as f64 / secs, tx as f64 / secs))
        })
        .collect()
}

/// The class of whichever link of this type is up, pins included.
fn class_of(
    ltype: LinkType,
    net_ids: &mut netid::Cache,
    overrides: &HashMap<String, Class>,
) -> Option<Class> {
    let links = nwpath::links();
    // With two links of one type, the default route is the likelier carrier.
    let mut names: Vec<_> = links
        .iter()
        .filter(|(_, l)| l.itype == ltype)
        .map(|(n, _)| n.clone())
        .collect();
    names.sort();
    let primary = nwpath::primary();
    let name = primary
        .filter(|p| names.contains(p))
        .or_else(|| names.into_iter().next())?;
    let pin = net_ids
        .get(&name)
        .and_then(|k| overrides.get(&k).copied())
        .or_else(|| overrides.get(&name).copied());
    classify(links.get(&name), pin)
}

/// Start `nettop` for one interface type, writing to a pseudo-terminal.
///
/// Through a pipe nettop's output is block-buffered, and arrives in lumps
/// minutes apart. A terminal gets it line by line as each sample is taken.
fn spawn_nettop(itype: &str) -> std::io::Result<(Child, File)> {
    let (mut master, mut slave) = (0, 0);
    // Wide enough that nothing is cut to fit.
    let mut size = libc::winsize {
        ws_row: 50,
        ws_col: 1000,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    let rc = unsafe {
        libc::openpty(
            &mut master,
            &mut slave,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            &mut size,
        )
    };
    if rc != 0 {
        return Err(std::io::Error::last_os_error());
    }
    // openpty's descriptors survive exec. Left that way, each nettop would
    // inherit its own pty's master, and the other's, and hold them open —
    // defeating the hang-up below that is meant to stop it.
    for fd in [master, slave] {
        unsafe { libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC) };
    }
    let (master, slave) = unsafe { (File::from_raw_fd(master), OwnedFd::from_raw_fd(slave)) };
    // The Command, and with it this process's copy of the slave end, is
    // dropped once spawned. That matters: with no copy left here, the master
    // reads end-of-file when nettop exits instead of blocking for ever.
    let mut cmd = Command::new("/usr/bin/nettop");
    cmd.args(["-P", "-d", "-x", "-n", "-L", "0", "-s", INTERVAL])
        .args(["-t", itype, "-J", "bytes_in,bytes_out"])
        // The terminal on stdin too. On a terminal nettop polls stdin for
        // keystrokes, and /dev/null answers every poll at once with
        // end-of-file, which spins it at a full core.
        .stdin(Stdio::from(slave.try_clone()?))
        .stdout(Stdio::from(slave))
        .stderr(Stdio::null());
    // Its own session, with the pty as its controlling terminal. Then however
    // this process goes — killed outright included — the master closes and
    // the kernel hangs nettop up. Without it, nettop outlives us: it carries on
    // sampling into a terminal nobody reads.
    unsafe {
        cmd.pre_exec(|| {
            if libc::setsid() == -1 || libc::ioctl(1, libc::TIOCSCTTY.into(), 0) == -1 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let child = cmd.spawn()?;
    Ok((child, master))
}

/// Forward each process's per-interval bytes from one nettop.
fn read_samples(out: File, ltype: LinkType, send: mpsc::Sender<Msg>) {
    let mut names = Names::default();
    let mut samples = 0;
    for line in BufReader::new(out).lines() {
        let Ok(line) = line else { break };
        // A terminal ends lines with CRLF.
        let line = line.trim_end_matches('\r');
        // Each sample opens with the column header.
        if line.starts_with(',') {
            samples += 1;
            continue;
        }
        // The first sample is each process's total since it started, not a
        // delta; counting it would claim traffic from before we were looking.
        if samples < 2 {
            continue;
        }
        let Some((comm, pid, rx, tx)) = parse_line(line) else {
            continue;
        };
        if rx == 0 && tx == 0 {
            continue;
        }
        let (app, bundle) = names.get(comm, pid);
        let msg = Msg::Delta {
            ltype,
            app,
            bundle,
            rx,
            tx,
        };
        if send.send(msg).is_err() {
            return;
        }
    }
    let _ = send.send(Msg::Ended);
}

/// Split a `-P -J bytes_in,bytes_out` row, e.g. `Google Chrome H.4989,120,7,`.
///
/// Parsed from the right: the numbers are always last, while the process
/// name is free text.
fn parse_line(line: &str) -> Option<(&str, i32, u64, u64)> {
    let mut parts = line.strip_suffix(',').unwrap_or(line).rsplitn(3, ',');
    let tx = parts.next()?.parse().ok()?;
    let rx = parts.next()?.parse().ok()?;
    let (comm, pid) = parts.next()?.rsplit_once('.')?;
    Some((comm, pid.parse().ok()?, rx, tx))
}

/// A display name, and the app bundle behind it if there is one.
type Ident = (String, Option<String>);

/// Display names by pid, since resolving one takes a syscall or two, and
/// bundle names by bundle, since reading one takes a subprocess.
#[derive(Default)]
struct Names {
    by_pid: HashMap<i32, (String, Ident)>,
    by_bundle: HashMap<String, String>,
    /// What each process name last turned out to be, for processes gone
    /// before they could be looked up. Short-lived workers are the usual
    /// case: an app's download helper, started and finished within a sample,
    /// shares its name with siblings that lived long enough to be seen.
    by_comm: HashMap<String, Ident>,
}

impl Names {
    fn get(&mut self, comm: &str, pid: i32) -> Ident {
        // Keyed on the pid but checked against the name too: pids get reused.
        if let Some((c, ident)) = self.by_pid.get(&pid)
            && c == comm
        {
            return ident.clone();
        }
        let ident = self.identify(comm, pid);
        self.by_pid.insert(pid, (comm.to_string(), ident.clone()));
        ident
    }

    /// Who a process is, as a person would name it.
    ///
    /// Inside an app bundle, the app. Outside one, the tool — and when a tool
    /// was started from an app, as `claude` is from a terminal, both: macOS
    /// tracks which app is "responsible" for each process, and it is the
    /// terminal. A process run by the system, like a daemon, has no
    /// responsible app and keeps its own name.
    fn identify(&mut self, comm: &str, pid: i32) -> Ident {
        // Gone already, like most short-lived tools: nettop's name is all
        // there is. It is cut to 15 characters, but it is something.
        let Some(path) = pid_path(pid) else {
            return self
                .by_comm
                .get(comm)
                .cloned()
                .unwrap_or_else(|| (comm.to_string(), None));
        };
        let ident = self.identify_path(&path, pid, comm);
        self.by_comm.insert(comm.to_string(), ident.clone());
        ident
    }

    fn identify_path(&mut self, path: &str, pid: i32, comm: &str) -> Ident {
        let path = path.to_string();
        if let Some(bundle) = outer_bundle(&path) {
            return (self.bundle_name(&bundle), Some(bundle));
        }
        let tool = tool_name(&path).unwrap_or_else(|| comm.to_string());
        if let Some(owner) = responsible_for(pid).filter(|&r| r != pid)
            && let Some(bundle) = pid_path(owner).and_then(|p| outer_bundle(&p))
        {
            return (
                format!("{} › {tool}", self.bundle_name(&bundle)),
                Some(bundle),
            );
        }
        (tool, None)
    }

    fn bundle_name(&mut self, bundle: &str) -> String {
        if let Some(n) = self.by_bundle.get(bundle) {
            return n.clone();
        }
        // The name the app shows itself, when it sets one; otherwise its name
        // in Finder. Not CFBundleName, which is often a short internal name.
        let name = plist_value(bundle, "CFBundleDisplayName")
            .map(|n| visible(&n))
            .filter(|n| !n.is_empty())
            .unwrap_or_else(|| folder_name(bundle));
        self.by_bundle.insert(bundle.to_string(), name.clone());
        name
    }
}

/// A name without the invisible direction marks some apps put in theirs —
/// WhatsApp's starts with a left-to-right mark — which would otherwise make
/// two spellings of one name.
fn visible(name: &str) -> String {
    name.chars()
        .filter(|c| !matches!(c, '\u{200e}' | '\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}' | '\u{feff}'))
        .collect::<String>()
        .trim()
        .to_string()
}

/// The app bundle's name as Finder shows it: its folder, less `.app`.
fn folder_name(bundle: &str) -> String {
    let base = bundle.rsplit('/').next().unwrap_or(bundle);
    base.strip_suffix(".app").unwrap_or(base).to_string()
}

/// One string from a bundle's Info.plist. `plutil` reads both the XML and
/// the binary form, which a hand-rolled parser would have to as well.
fn plist_value(bundle: &str, key: &str) -> Option<String> {
    let out = Command::new("/usr/bin/plutil")
        .args(["-extract", key, "raw", "-o", "-"])
        .arg(format!("{bundle}/Contents/Info.plist"))
        .stderr(Stdio::null())
        .output()
        .ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// The app's icon as a 32-pixel PNG — enough for a 16-point icon on a Retina
/// screen — or None if it has no `.icns` to read.
///
/// `sips` rather than Quick Look, whose `qlmanage` hangs when run without a
/// window server session, as this helper is.
fn icon_png(bundle: &str) -> Option<Vec<u8>> {
    let name = plist_value(bundle, "CFBundleIconFile")
        .or_else(|| plist_value(bundle, "CFBundleIconName"))?;
    let file = if name.ends_with(".icns") {
        name
    } else {
        format!("{name}.icns")
    };
    let src = format!("{bundle}/Contents/Resources/{file}");
    let out = crate::store::data_dir().join("icon.tmp.png");
    let ok = Command::new("/usr/bin/sips")
        .args(["-s", "format", "png", "-Z", "32"])
        .arg(&src)
        .arg("--out")
        .arg(&out)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success());
    let png = ok.then(|| std::fs::read(&out).ok()).flatten();
    let _ = std::fs::remove_file(&out);
    png
}

/// The app macOS holds responsible for a process — the terminal a command
/// runs in, say. A private but long-standing libSystem call, looked up at run
/// time so that its absence costs only this feature.
fn responsible_for(pid: i32) -> Option<i32> {
    type Lookup = unsafe extern "C" fn(libc::pid_t) -> libc::pid_t;
    static LOOKUP: std::sync::OnceLock<Option<Lookup>> = std::sync::OnceLock::new();
    let lookup = LOOKUP.get_or_init(|| {
        let sym = unsafe {
            libc::dlsym(
                libc::RTLD_DEFAULT,
                c"responsibility_get_pid_responsible_for_pid".as_ptr(),
            )
        };
        (!sym.is_null()).then(|| unsafe { std::mem::transmute::<*mut libc::c_void, Lookup>(sym) })
    });
    let r = unsafe { (*lookup)?(pid) };
    (r > 0).then_some(r)
}

fn pid_path(pid: i32) -> Option<String> {
    let mut buf = vec![0u8; libc::PROC_PIDPATHINFO_MAXSIZE as usize];
    let n = unsafe { libc::proc_pidpath(pid, buf.as_mut_ptr().cast(), buf.len() as u32) };
    if n <= 0 {
        return None;
    }
    buf.truncate(n as usize);
    String::from_utf8(buf).ok()
}

/// The outermost app bundle an executable lives in, as a path.
///
/// Outermost, because Chrome runs its network traffic through
/// "Google Chrome Helper.app" nested deep inside "Google Chrome.app", and it
/// is Chrome that used the data.
fn outer_bundle(path: &str) -> Option<String> {
    let i = path
        .find(".app/")
        .or_else(|| path.ends_with(".app").then(|| path.len() - 4))?;
    Some(path[..i + 4].to_string())
}

/// An executable's name outside any bundle — unless that is only a version
/// number, as with tools that install each release as `…/versions/1.2.3`, in
/// which case the nearest directory with a real name says what it is.
fn tool_name(path: &str) -> Option<String> {
    let parts: Vec<&str> = path.split('/').filter(|p| !p.is_empty()).collect();
    const GENERIC: [&str; 5] = ["versions", "bin", "libexec", "current", "latest"];
    parts
        .iter()
        .rev()
        .find(|p| p.chars().any(|c| c.is_alphabetic()) && !GENERIC.contains(p))
        .map(|p| p.to_string())
}

/// Identifies the installed binary, to notice it being replaced.
fn exe_stamp() -> Option<SystemTime> {
    std::env::current_exe()
        .and_then(std::fs::metadata)
        .and_then(|m| m.modified())
        .ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rows_parse_from_the_right() {
        assert_eq!(
            parse_line("Google Chrome H.49893,3409146,102665,"),
            Some(("Google Chrome H", 49893, 3409146, 102665))
        );
        // A name with dots of its own keeps them; only the last one is the pid.
        assert_eq!(
            parse_line("2.1.284.34615,128894,1102404,"),
            Some(("2.1.284", 34615, 128894, 1102404))
        );
        // And with a comma of its own.
        assert_eq!(parse_line("a,b.7,1,2,"), Some(("a,b", 7, 1, 2)));
    }

    #[test]
    fn headers_and_blanks_are_not_rows() {
        assert_eq!(parse_line(",bytes_in,bytes_out,"), None);
        assert_eq!(parse_line("launchd.1,,,"), None);
        assert_eq!(parse_line(""), None);
    }

    #[test]
    fn helpers_belong_to_their_outermost_app() {
        assert_eq!(
            outer_bundle(
                "/Applications/Google Chrome.app/Contents/Frameworks/Google Chrome Framework.framework/Versions/140/Helpers/Google Chrome Helper.app/Contents/MacOS/Google Chrome Helper"
            )
            .as_deref(),
            Some("/Applications/Google Chrome.app")
        );
        assert_eq!(
            outer_bundle("/Applications/Slack.app/Contents/MacOS/Slack").as_deref(),
            Some("/Applications/Slack.app")
        );
        assert_eq!(
            folder_name("/Applications/Google Chrome.app"),
            "Google Chrome"
        );
    }

    #[test]
    fn plain_executables_have_no_bundle_and_keep_their_own_name() {
        assert_eq!(outer_bundle("/usr/bin/curl"), None);
        assert_eq!(tool_name("/usr/bin/curl").as_deref(), Some("curl"));
        assert_eq!(
            tool_name("/usr/libexec/syspolicyd").as_deref(),
            Some("syspolicyd")
        );
    }

    #[test]
    fn direction_marks_are_not_part_of_a_name() {
        assert_eq!(visible("\u{200e}WhatsApp"), "WhatsApp");
        assert_eq!(visible(" Mail "), "Mail");
    }

    #[test]
    fn a_version_number_is_not_a_name() {
        assert_eq!(
            tool_name("/Users/me/.local/share/claude/versions/2.1.284").as_deref(),
            Some("claude")
        );
    }

    #[test]
    fn a_process_answers_for_itself_or_its_app() {
        // This test's own process: run from a terminal or by cargo, either an
        // app or nothing, but never an error.
        let me = std::process::id() as i32;
        if let Some(r) = responsible_for(me) {
            assert!(r > 0);
        }
    }
}
