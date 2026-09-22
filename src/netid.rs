//! Identifying *which network* an interface is attached to.
//!
//! Pinning a classification to an interface name is too coarse on a laptop:
//! home Wi-Fi and a phone hotspot are both `en0`, so a pin meant for one
//! silently relabels the other.
//!
//! The obvious per-network identifier is the SSID, but reading it triggers a
//! Location Services prompt on macOS 14+. The default gateway's MAC address is
//! just as stable per network, distinguishes a hotspot from home Wi-Fi, and
//! needs no permission at all.
//!
//! Both lookups shell out. They run at most once every `TTL` seconds rather
//! than per tick, and the routing/ARP tables have no stable public C API worth
//! the size of the binding.

use std::collections::HashMap;
use std::process::Command;
use std::time::{Duration, Instant};

const TTL: Duration = Duration::from_secs(30);
/// A gateway that hasn't answered ARP yet is a transient state, not an answer.
/// Re-ask soon instead of committing to "unknown" for a full TTL.
const RETRY: Duration = Duration::from_secs(5);

/// Default gateway reachable over `iface`, ignoring any VPN default route.
fn gateway_ip(iface: &str) -> Option<String> {
    let out = Command::new("/sbin/route")
        .args(["-n", "get", "-ifscope", iface, "default"])
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&out.stdout);
    text.lines()
        .find_map(|l| l.trim().strip_prefix("gateway:"))
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

/// Hardware address the gateway answers on, from the ARP cache.
fn gateway_mac(ip: &str) -> Option<String> {
    let out = Command::new("/usr/sbin/arp")
        .args(["-n", ip])
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&out.stdout);
    let raw = text.split(" at ").nth(1)?.split_whitespace().next()?;
    if raw.contains("incomplete") || !raw.contains(':') {
        return None;
    }
    // `arp` prints octets unpadded ("f0:9:d:..."); normalise so the same
    // network always produces the same key.
    let mut parts = Vec::new();
    for octet in raw.split(':') {
        let v = u8::from_str_radix(octet, 16).ok()?;
        parts.push(format!("{v:02x}"));
    }
    (parts.len() == 6).then(|| parts.join(":"))
}

/// Stable key for the network on `iface`, or None until one can be determined.
///
/// Deliberately returns nothing rather than falling back to the gateway *IP*.
/// The ARP entry can take a few seconds to appear on a fresh connection, and a
/// key that starts as the IP and later becomes the MAC describes one network
/// two ways — so a pin made in the first seconds would be orphaned.
pub fn fingerprint(iface: &str) -> Option<String> {
    let ip = gateway_ip(iface)?;
    gateway_mac(&ip).map(|mac| format!("gw:{mac}"))
}

/// Short form for the menu, e.g. "42:6d:b8".
pub fn short(key: &str) -> String {
    match key.strip_prefix("gw:") {
        Some(mac) => mac.split(':').skip(3).collect::<Vec<_>>().join(":"),
        None => key.to_string(),
    }
}

/// Caches fingerprints so the lookups stay off the per-second path.
#[derive(Default)]
pub struct Cache {
    entries: HashMap<String, (Option<String>, Instant)>,
}

impl Cache {
    pub fn get(&mut self, iface: &str) -> Option<String> {
        if let Some((value, at)) = self.entries.get(iface) {
            let ttl = if value.is_some() { TTL } else { RETRY };
            if at.elapsed() < ttl {
                return value.clone();
            }
        }
        let value = fingerprint(iface);
        self.entries
            .insert(iface.to_string(), (value.clone(), Instant::now()));
        value
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_form_keeps_the_distinguishing_octets() {
        assert_eq!(short("gw:f0:09:0d:42:6d:b8"), "42:6d:b8");
    }

    #[test]
    fn short_form_passes_through_unknown_shapes() {
        assert_eq!(short("en0"), "en0");
    }
}
