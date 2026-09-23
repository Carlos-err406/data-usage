//! Rate formatting for the menu bar.
//!
//! Byte formatting lives in the popover's JavaScript: that is the only thing
//! rendering totals now, and a second copy in Rust would only drift.

/// Rate for the menu bar, as short as it can be and still be read.
///
/// The menu bar is shared real estate, so this drops the "B/s" and the space:
/// `1.2M` rather than `1.2 MB/s`. Idle collapses to a bare `0`.
pub fn rate(bytes_per_sec: f64) -> String {
    let v = bytes_per_sec.max(0.0);
    let (unit, scale) = if v >= 1e9 {
        ("G", 1e9)
    } else if v >= 1e6 {
        ("M", 1e6)
    } else if v >= 1e3 {
        ("K", 1e3)
    } else {
        return "0".into();
    };
    let x = v / scale;
    if x < 10.0 {
        format!("{x:.1}{unit}")
    } else {
        format!("{x:.0}{unit}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rate_is_compact() {
        assert_eq!(rate(0.0), "0");
        assert_eq!(rate(500.0), "0");
        assert_eq!(rate(1_500.0), "1.5K");
        assert_eq!(rate(340_000.0), "340K");
        assert_eq!(rate(1_200_000.0), "1.2M");
        assert_eq!(rate(12_000_000.0), "12M");
        assert_eq!(rate(2_500_000_000.0), "2.5G");
    }

    #[test]
    fn rate_clamps_negative_input() {
        // Guards against a counter reset producing a negative delta.
        assert_eq!(rate(-5.0), "0");
    }
}
