//! Human-readable byte and rate formatting.

/// Formats with enough precision to stay readable but a stable-ish width, so
/// the menu bar doesn't jitter as digits come and go.
pub fn bytes(n: u64) -> String {
    const UNITS: [(&str, f64); 5] = [
        ("TB", 1e12),
        ("GB", 1e9),
        ("MB", 1e6),
        ("KB", 1e3),
        ("B", 1.0),
    ];
    let v = n as f64;
    for (unit, scale) in UNITS {
        if v >= scale {
            let x = v / scale;
            let digits = if unit == "B" {
                0
            } else if x < 10.0 {
                2
            } else if x < 100.0 {
                1
            } else {
                0
            };
            return format!("{x:.digits$} {unit}");
        }
    }
    "0 B".into()
}

/// Compact rate for the menu bar: no space, fewer digits.
pub fn rate(bytes_per_sec: f64) -> String {
    let v = bytes_per_sec.max(0.0);
    let (unit, scale) = if v >= 1e9 {
        ("GB/s", 1e9)
    } else if v >= 1e6 {
        ("MB/s", 1e6)
    } else if v >= 1e3 {
        ("KB/s", 1e3)
    } else {
        return "0 KB/s".into();
    };
    let x = v / scale;
    let digits = if x < 10.0 { 1 } else { 0 };
    format!("{x:.digits$} {unit}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scales_to_the_largest_fitting_unit() {
        assert_eq!(bytes(0), "0 B");
        assert_eq!(bytes(999), "999 B");
        assert_eq!(bytes(1_000), "1.00 KB");
        assert_eq!(bytes(1_500_000), "1.50 MB");
        assert_eq!(bytes(2_410_000_000), "2.41 GB");
    }

    #[test]
    fn sheds_decimals_as_the_number_widens() {
        // Keeps the rendered width roughly constant so the menu doesn't jitter.
        assert_eq!(bytes(9_900_000), "9.90 MB");
        assert_eq!(bytes(99_000_000), "99.0 MB");
        assert_eq!(bytes(990_000_000), "990 MB");
    }

    #[test]
    fn rate_floors_below_a_kilobyte() {
        assert_eq!(rate(0.0), "0 KB/s");
        assert_eq!(rate(500.0), "0 KB/s");
        assert_eq!(rate(1_500.0), "1.5 KB/s");
        assert_eq!(rate(12_000_000.0), "12 MB/s");
    }

    #[test]
    fn rate_clamps_negative_input() {
        // Guards against a counter reset producing a negative delta.
        assert_eq!(rate(-5.0), "0 KB/s");
    }
}
