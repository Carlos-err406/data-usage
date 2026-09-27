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
    if v < 1e3 {
        return "0".into();
    }
    const UNITS: [(&str, f64); 4] = [("K", 1e3), ("M", 1e6), ("G", 1e9), ("T", 1e12)];
    // Start at the largest unit the value reaches, but choose the precision on
    // the *rounded* figure. Deciding before rounding is what printed 999.6K as
    // "1000K" and 9.96K as "10.0K" — five characters, which moves the menu bar
    // item and everything anchored to it.
    let mut i = UNITS.iter().rposition(|(_, s)| v >= *s).unwrap_or(0);
    loop {
        let (unit, scale) = UNITS[i];
        let x = v / scale;
        let tenths = (x * 10.0).round() / 10.0;
        if tenths < 10.0 {
            return format!("{tenths:.1}{unit}");
        }
        let whole = x.round();
        if whole < 1000.0 || i + 1 == UNITS.len() {
            return format!("{whole:.0}{unit}");
        }
        i += 1;
    }
}

/// [`rate`], right-aligned in a fixed four-character field.
///
/// The menu bar item is variable-length, and SwiftBar anchors the popover to
/// it, so any change in the title's width slides the open popover sideways.
/// In a monospaced font a constant character count is a constant width.
/// Right-aligned so the digits stay put as the value changes.
pub fn padded_rate(bytes_per_sec: f64) -> String {
    format!("{:>4}", rate(bytes_per_sec))
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
    fn rate_rolls_over_after_rounding_not_before() {
        // 999.6K is below the M cutoff but rounds to 1000 — it must read as the
        // next unit up, not as a five-character "1000K".
        assert_eq!(rate(999_600.0), "1.0M");
        assert_eq!(rate(999_600_000.0), "1.0G");
        // Same trap one digit down: 9.96 rounds to 10.0, which needs no decimal.
        assert_eq!(rate(9_960.0), "10K");
        assert_eq!(rate(9_960_000.0), "10M");
    }

    #[test]
    fn rate_never_exceeds_four_characters() {
        // The menu bar title reserves exactly four per rate so the item never
        // changes width; a fifth character would shift the popover again.
        let mut v = 1.0;
        while v < 1e11 {
            for f in [1.0, 0.9995, 0.99996, 1.00004, 9.96, 99.96, 999.6] {
                let s = rate(v * f);
                assert!(s.chars().count() <= 4, "rate({}) = {s:?}", v * f);
            }
            v *= 10.0;
        }
    }

    #[test]
    fn padded_rate_is_always_four_characters() {
        for v in [0.0, 1_500.0, 12_000.0, 340_000.0, 999_600.0, 2.5e9, 5e12] {
            assert_eq!(padded_rate(v).chars().count(), 4, "padded_rate({v})");
        }
        assert_eq!(padded_rate(0.0), "   0");
        assert_eq!(padded_rate(12_000.0), " 12K");
    }

    #[test]
    fn rate_clamps_negative_input() {
        // Guards against a counter reset producing a negative delta.
        assert_eq!(rate(-5.0), "0");
    }
}
