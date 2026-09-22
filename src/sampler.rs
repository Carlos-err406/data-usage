//! The accounting loop: turn raw lifetime counters into per-class deltas.

use crate::classify::{Class, classify};
use crate::ifstat;
use crate::netid;
use crate::nwpath;
use crate::store::{Store, Totals, bucket_of};
use std::collections::HashMap;
use std::time::{SystemTime, UNIX_EPOCH};

/// Traffic that happened while we weren't sampling is only counted if the gap
/// was short. Past this we silently resync — otherwise starting the app after a
/// week would dump a week of bytes into the current minute and call it today.
///
/// An hour is generous on purpose: SwiftBar defers a plugin refresh while its
/// menu is open, and the machine sleeps, so ordinary gaps are minutes long and
/// the traffic in them is real. A gap longer than this is the app having been
/// off, where the counters have usually been reset by a reboot anyway.
const MAX_CATCHUP_GAP: i64 = 3600;

#[derive(Default, Clone, Copy)]
pub struct Rate {
    pub rx: f64,
    pub tx: f64,
}

pub struct Sampler {
    pub store: Store,
    last: HashMap<String, (u64, u64)>,
    window: Vec<(i64, u64, u64)>,
    /// Seconds covered by the last on-disk checkpoint, and the bytes seen over
    /// it. This is how a short-lived refresh run derives a rate: it has no
    /// history of its own to difference against.
    checkpoint: Option<(f64, u64, u64)>,
    net_ids: netid::Cache,
    /// None when another instance already holds the accounting lock, in which
    /// case this sampler reads and displays but never writes.
    lock: Option<crate::lock::AccountingLock>,
}

/// Interface currently carrying traffic, for display.
pub struct CurrentLink {
    pub iface: String,
    pub class: Option<Class>,
    pub detail: String,
    /// What a manual pin keys on. None while the network is still being
    /// identified — pinning before then would write a key that changes.
    pub pin_key: Option<String>,
}

impl Sampler {
    pub fn new(store: Store) -> Self {
        Sampler {
            store,
            last: HashMap::new(),
            window: Vec::new(),
            checkpoint: None,
            net_ids: netid::Cache::default(),
            lock: crate::lock::acquire(),
        }
    }

    /// Whether this instance is the one writing history.
    pub fn is_accounting(&self) -> bool {
        self.lock.is_some()
    }

    pub fn now() -> i64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0)
    }

    /// Read counters, attribute the delta, persist. Returns this tick's bytes.
    pub fn tick(&mut self) -> Totals {
        let now = Self::now();
        let links = nwpath::links();
        let overrides = self.store.overrides().unwrap_or_default();
        let counters = match ifstat::read() {
            Ok(c) => c,
            Err(_) => return Totals::new(),
        };

        // Only physical links get a fingerprint lookup — doing it for every
        // interface would spawn dozens of subprocesses per refresh.
        let mut net_keys: HashMap<String, String> = HashMap::new();
        for name in links.keys() {
            if let Some(k) = self.net_ids.get(name) {
                net_keys.insert(name.clone(), k);
            }
        }

        let accounting = self.is_accounting();
        let mut added = Totals::new();
        let mut seen: Vec<(String, u64, u64)> = Vec::new();
        // Oldest checkpoint we resumed from this tick, for the rate fallback.
        let mut resumed_from: Option<i64> = None;
        for c in &counters {
            let pin = net_keys
                .get(&c.name)
                .and_then(|k| overrides.get(k))
                // Pins written before overrides were per-network are keyed on
                // the interface; keep honouring them.
                .or_else(|| overrides.get(&c.name))
                .copied();
            let Some(class) = classify(links.get(&c.name), pin) else {
                continue;
            };

            let prev = match self.last.get(&c.name) {
                Some(p) => Some(*p),
                // Only the accounting instance resumes from a stored checkpoint;
                // a read-only one must not claim traffic it isn't recording.
                None if accounting => match self.store.iface_state(&c.name) {
                    // Only trust a checkpoint from the recent past. Note this
                    // deliberately accepts a zero-second gap: two refreshes can
                    // land in the same wall-clock second, and those bytes are
                    // still real. Only the rate needs a non-zero span.
                    Ok(Some((rx, tx, at))) if now - at <= MAX_CATCHUP_GAP => {
                        resumed_from = Some(resumed_from.map_or(at, |p: i64| p.min(at)));
                        Some((rx, tx))
                    }
                    _ => None,
                },
                None => None,
            };

            if let Some((prx, ptx)) = prev {
                // A counter going backwards means the interface was torn down
                // and rebuilt, so the current value *is* the delta.
                let drx = if c.rx >= prx { c.rx - prx } else { c.rx };
                let dtx = if c.tx >= ptx { c.tx - ptx } else { c.tx };
                if drx > 0 || dtx > 0 {
                    let e = added.entry(class).or_insert((0, 0));
                    e.0 += drx;
                    e.1 += dtx;
                }
            }
            self.last.insert(c.name.clone(), (c.rx, c.tx));
            seen.push((c.name.clone(), c.rx, c.tx));
        }

        if accounting {
            let _ = self.store.commit_tick(bucket_of(now), &added, &seen, now);
        }

        let (rx, tx) = added
            .values()
            .fold((0u64, 0u64), |a, b| (a.0 + b.0, a.1 + b.1));
        if let Some(at) = resumed_from {
            // Floor the span at one second: a same-second gap would otherwise
            // divide by zero, and a clock stepping backwards would go negative.
            self.checkpoint = Some(((now - at).max(1) as f64, rx, tx));
        }
        self.window.push((now, rx, tx));
        self.window.retain(|(t, _, _)| now - t < 3);
        added
    }

    /// Throughput over the last few seconds, in bytes/sec.
    pub fn rate(&self) -> Rate {
        if self.window.len() < 2 {
            // A refresh run lives for milliseconds and never builds a window,
            // so it measures against the checkpoint the previous run left.
            return match self.checkpoint {
                Some((span, rx, tx)) if span > 0.0 => Rate {
                    rx: rx as f64 / span,
                    tx: tx as f64 / span,
                },
                _ => Rate::default(),
            };
        }
        let span = (self.window.last().unwrap().0 - self.window[0].0).max(1) as f64;
        // The first entry is the baseline for the span, so its bytes fall outside it.
        let rx: u64 = self.window[1..].iter().map(|w| w.1).sum();
        let tx: u64 = self.window[1..].iter().map(|w| w.2).sum();
        Rate {
            rx: rx as f64 / span,
            tx: tx as f64 / span,
        }
    }

    /// Which physical link is carrying traffic right now.
    pub fn current_link(&mut self) -> Option<CurrentLink> {
        let links = nwpath::links();
        let overrides = self.store.overrides().unwrap_or_default();
        // The default route may be a VPN tunnel, which isn't a link we count;
        // prefer whichever physical interface is actually classified.
        let iface = nwpath::primary()
            .filter(|n| links.contains_key(n))
            .or_else(|| {
                let mut names: Vec<_> = links.keys().cloned().collect();
                names.sort();
                names
                    .into_iter()
                    .find(|n| classify(links.get(n), overrides.get(n).copied()).is_some())
            })?;
        let info = links.get(&iface)?;
        let net_key = self.net_ids.get(&iface);
        let pin_key = net_key.clone();
        let pin = net_key
            .as_ref()
            .and_then(|k| overrides.get(k))
            .or_else(|| overrides.get(&iface))
            .copied();
        let class = classify(Some(info), pin);
        let mut detail = info.itype.label().to_string();
        if let Some(k) = &net_key {
            detail.push_str(&format!(" · net {}", netid::short(k)));
        }
        if info.expensive {
            detail.push_str(" · metered");
        }
        if info.constrained {
            detail.push_str(" · low data mode");
        }
        if pin.is_some() {
            detail.push_str(" · manual");
        }
        Some(CurrentLink {
            iface,
            class,
            detail,
            pin_key,
        })
    }
}
