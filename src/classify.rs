//! Deciding which interfaces to count, and which bucket their bytes land in.

use crate::nwpath::{LinkInfo, LinkType};
use std::fmt;

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, PartialOrd, Ord)]
pub enum Class {
    Mobile,
    Wifi,
    Wired,
}

impl Class {
    pub fn key(self) -> &'static str {
        match self {
            Class::Mobile => "mobile",
            Class::Wifi => "wifi",
            Class::Wired => "wired",
        }
    }

    pub fn from_key(s: &str) -> Option<Self> {
        match s {
            "mobile" => Some(Class::Mobile),
            "wifi" => Some(Class::Wifi),
            "wired" => Some(Class::Wired),
            _ => None,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Class::Mobile => "Mobile",
            Class::Wifi => "Wi-Fi",
            Class::Wired => "Ethernet",
        }
    }

    pub const ALL: [Class; 3] = [Class::Mobile, Class::Wifi, Class::Wired];
}

impl fmt::Display for Class {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.label())
    }
}

/// Which bucket an interface's bytes belong to, or None if it shouldn't count.
///
/// Only links Network.framework recognises as physical are counted. That
/// deliberately drops `utun*` (VPN), `awdl0`/`llw0` (AirDrop), `bridge*`,
/// `ap1` and `lo0`. VPN exclusion matters most: tunnel traffic is also counted
/// on the physical interface carrying it, so counting both would double every
/// byte sent over the VPN.
pub fn classify(info: Option<&LinkInfo>, override_class: Option<Class>) -> Option<Class> {
    if let Some(c) = override_class {
        return Some(c);
    }
    let info = info?;

    // `expensive` is raised by Apple's own hotspot signalling, so it catches an
    // iPhone Personal Hotspot but *not* an Android one, which presents as an
    // ordinary access point. `constrained` is macOS Low Data Mode, which is
    // settable per Wi-Fi network by hand and so works for any phone.
    let metered = info.expensive || info.constrained;

    match info.itype {
        LinkType::Cellular => Some(Class::Mobile),
        LinkType::Wifi | LinkType::Wired if metered => Some(Class::Mobile),
        LinkType::Wifi => Some(Class::Wifi),
        LinkType::Wired => Some(Class::Wired),
        LinkType::Loopback | LinkType::Other => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nwpath::LinkType;

    fn link(itype: LinkType, expensive: bool, constrained: bool) -> LinkInfo {
        LinkInfo {
            itype,
            expensive,
            constrained,
        }
    }

    #[test]
    fn plain_wifi_is_not_mobile() {
        let l = link(LinkType::Wifi, false, false);
        assert_eq!(classify(Some(&l), None), Some(Class::Wifi));
    }

    #[test]
    fn expensive_wifi_is_mobile() {
        // An iPhone Personal Hotspot over Wi-Fi.
        let l = link(LinkType::Wifi, true, false);
        assert_eq!(classify(Some(&l), None), Some(Class::Mobile));
    }

    #[test]
    fn constrained_wifi_is_mobile() {
        // Low Data Mode — the only native signal that catches an Android
        // hotspot, which never raises `expensive`.
        let l = link(LinkType::Wifi, false, true);
        assert_eq!(classify(Some(&l), None), Some(Class::Mobile));
    }

    #[test]
    fn expensive_ethernet_is_mobile() {
        // USB tethering presents as a wired link.
        let l = link(LinkType::Wired, true, false);
        assert_eq!(classify(Some(&l), None), Some(Class::Mobile));
    }

    #[test]
    fn cellular_is_always_mobile() {
        let l = link(LinkType::Cellular, false, false);
        assert_eq!(classify(Some(&l), None), Some(Class::Mobile));
    }

    #[test]
    fn virtual_links_are_not_counted() {
        // `Other` covers utun/VPN, whose bytes are already counted on the
        // physical interface carrying them.
        assert_eq!(
            classify(Some(&link(LinkType::Other, false, false)), None),
            None
        );
        assert_eq!(
            classify(Some(&link(LinkType::Loopback, false, false)), None),
            None
        );
        assert_eq!(classify(None, None), None);
    }

    #[test]
    fn a_pin_overrides_the_automatic_verdict() {
        let l = link(LinkType::Wifi, false, false);
        assert_eq!(classify(Some(&l), Some(Class::Mobile)), Some(Class::Mobile));
        // And applies even to a link we would otherwise skip entirely.
        assert_eq!(classify(None, Some(Class::Mobile)), Some(Class::Mobile));
    }

    #[test]
    fn class_keys_round_trip() {
        for c in Class::ALL {
            assert_eq!(Class::from_key(c.key()), Some(c));
        }
        assert_eq!(Class::from_key("nonsense"), None);
    }
}
