//! Interface classification via Network.framework.
//!
//! This is the part that answers "is this mobile data?". macOS sets a path's
//! `expensive` flag when it runs over an iPhone Personal Hotspot — over Wi-Fi
//! *and* over USB — which is the same signal the OS uses to hold back iCloud
//! sync and App Store downloads. Reading it needs no entitlement and no
//! permission prompt, unlike asking for the Wi-Fi SSID.
//!
//! `expensive` describes a *path*, not a counter, so we keep the current
//! per-interface verdict in a shared map and let the sampler attribute each
//! tick's bytes to whatever the link was at that moment.

use block2::RcBlock;
use dispatch2::DispatchQueue;
use objc2::runtime::{AnyObject, Bool};
use std::cell::RefCell;
use std::collections::HashMap;
use std::ffi::{CStr, c_char, c_void};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

// nw_* handles are Objective-C objects under the hood. Typing them as
// AnyObject rather than c_void is what lets them cross a block boundary:
// block2 requires every block argument to implement EncodeArgument, and
// *mut c_void does not.
type NwObject = *mut AnyObject;

#[link(name = "Network", kind = "framework")]
unsafe extern "C" {
    fn nw_path_monitor_create() -> NwObject;
    fn nw_path_monitor_create_with_type(interface_type: u32) -> NwObject;
    fn nw_path_monitor_set_queue(monitor: NwObject, queue: *mut c_void);
    fn nw_path_monitor_set_update_handler(monitor: NwObject, handler: *mut c_void);
    fn nw_path_monitor_start(monitor: NwObject);
    fn nw_path_get_status(path: NwObject) -> u32;
    fn nw_path_is_expensive(path: NwObject) -> bool;
    fn nw_path_is_constrained(path: NwObject) -> bool;
    fn nw_path_enumerate_interfaces(path: NwObject, enumerator: *mut c_void);
    fn nw_interface_get_name(interface: NwObject) -> *const c_char;
    fn nw_interface_get_type(interface: NwObject) -> u32;
}

const NW_PATH_STATUS_SATISFIED: u32 = 1;

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum LinkType {
    #[default]
    Other,
    Wifi,
    Cellular,
    Wired,
    Loopback,
}

impl LinkType {
    fn from_raw(v: u32) -> Self {
        match v {
            1 => LinkType::Wifi,
            2 => LinkType::Cellular,
            3 => LinkType::Wired,
            4 => LinkType::Loopback,
            _ => LinkType::Other,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            LinkType::Wifi => "Wi-Fi",
            LinkType::Cellular => "Cellular",
            LinkType::Wired => "Ethernet",
            LinkType::Loopback => "Loopback",
            LinkType::Other => "Other",
        }
    }
}

/// What Network.framework currently believes about one interface.
#[derive(Clone, Copy, Debug, Default)]
pub struct LinkInfo {
    pub itype: LinkType,
    /// Set for Personal Hotspot (Wi-Fi or USB) and real cellular modems.
    pub expensive: bool,
    /// Set under Low Data Mode.
    pub constrained: bool,
}

#[derive(Default)]
struct State {
    links: HashMap<String, LinkInfo>,
    /// Interface carrying the default route — where traffic actually goes.
    primary: Option<String>,
    updates: u64,
}

static STATE: OnceLock<Mutex<State>> = OnceLock::new();

fn state() -> &'static Mutex<State> {
    STATE.get_or_init(|| Mutex::new(State::default()))
}

/// Collect (name, type) for every interface backing a path.
fn interfaces_of(path: NwObject) -> Vec<(String, LinkType)> {
    let found = RefCell::new(Vec::new());
    // Returns objc2's Bool, not Rust's: objc2 leaves `Encode` unimplemented for
    // `bool` on purpose, and Bool has the same 1-byte ABI as the C `_Bool` that
    // nw_path_enumerate_interfaces expects back.
    let collect = RcBlock::new(|iface: NwObject| -> Bool {
        if !iface.is_null() {
            let ptr = unsafe { nw_interface_get_name(iface) };
            if !ptr.is_null() {
                let name = unsafe { CStr::from_ptr(ptr) }
                    .to_string_lossy()
                    .into_owned();
                let itype = LinkType::from_raw(unsafe { nw_interface_get_type(iface) });
                found.borrow_mut().push((name, itype));
            }
        }
        Bool::new(true) // keep enumerating
    });
    // Synchronous callback, so lending the block a borrowed closure is fine.
    unsafe { nw_path_enumerate_interfaces(path, RcBlock::as_ptr(&collect) as *mut c_void) };
    drop(collect); // releases the borrow of `found`
    found.into_inner()
}

/// Spawn the path monitors. Call once; they live for the process.
pub fn start() {
    let queue = DispatchQueue::new("dev.datausage.nwpath", None);

    // One monitor per interface type. A typed monitor's path is restricted to
    // that kind of link, so its `expensive` flag is unambiguously about the
    // interfaces it enumerates — which a monitor on the default path is not.
    for raw_type in [
        1u32, /* wifi */
        2,    /* cellular */
        3,    /* wired */
    ] {
        let monitor = unsafe { nw_path_monitor_create_with_type(raw_type) };
        let handler = RcBlock::new(move |path: NwObject| {
            if path.is_null() {
                return;
            }
            let expensive = unsafe { nw_path_is_expensive(path) };
            let constrained = unsafe { nw_path_is_constrained(path) };
            let ifaces = interfaces_of(path);
            let mut st = state().lock().unwrap();
            for (name, itype) in ifaces {
                st.links.insert(
                    name,
                    LinkInfo {
                        itype,
                        expensive,
                        constrained,
                    },
                );
            }
            st.updates += 1;
        });
        unsafe {
            nw_path_monitor_set_update_handler(monitor, RcBlock::as_ptr(&handler) as *mut c_void);
            nw_path_monitor_set_queue(monitor, (&*queue as *const DispatchQueue) as *mut c_void);
            nw_path_monitor_start(monitor);
        }
        // The monitor and its handler must outlive this scope; they run until exit.
        std::mem::forget(handler);
    }

    // A plain monitor reports the default route, which tells us which link is
    // actually carrying traffic right now.
    let monitor = unsafe { nw_path_monitor_create() };
    let handler = RcBlock::new(move |path: NwObject| {
        if path.is_null() {
            return;
        }
        let satisfied = unsafe { nw_path_get_status(path) } == NW_PATH_STATUS_SATISFIED;
        let ifaces = interfaces_of(path);
        let mut st = state().lock().unwrap();
        // First non-loopback interface of a satisfied path is the primary one.
        st.primary = if satisfied {
            ifaces
                .iter()
                .find(|(_, t)| *t != LinkType::Loopback)
                .map(|(n, _)| n.clone())
        } else {
            None
        };
        st.updates += 1;
    });
    unsafe {
        nw_path_monitor_set_update_handler(monitor, RcBlock::as_ptr(&handler) as *mut c_void);
        nw_path_monitor_set_queue(monitor, (&*queue as *const DispatchQueue) as *mut c_void);
        nw_path_monitor_start(monitor);
    }
    std::mem::forget(handler);
    std::mem::forget(queue);
}

/// Block until the monitors have reported at least once, or the timeout lapses.
pub fn wait_ready(timeout: Duration) {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if state().lock().unwrap().updates > 0 {
            return;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// Current verdict for every interface Network.framework knows about.
pub fn links() -> HashMap<String, LinkInfo> {
    state().lock().unwrap().links.clone()
}

/// Interface currently carrying the default route, if any.
pub fn primary() -> Option<String> {
    state().lock().unwrap().primary.clone()
}
