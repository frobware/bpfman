//! Go-compatible duration parsing shared by poll syntax and literal checks.

use std::num::NonZeroU64;
use std::time::Duration;

/// A positive duration representable by Go's signed nanosecond `time.Duration`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PositiveDuration(NonZeroU64);

impl PositiveDuration {
    /// Duration measured in nanoseconds.
    #[must_use]
    pub fn as_nanos(self) -> u64 {
        self.0.get()
    }

    /// Convert to Rust's standard duration.
    #[must_use]
    pub fn as_std(self) -> Duration {
        Duration::from_nanos(self.as_nanos())
    }
}

/// Parse Go's signed `time.ParseDuration` grammar into nanoseconds.
#[must_use]
pub fn parse_go_duration(raw: &str) -> Option<i64> {
    let (negative, mut remaining) = if let Some(rest) = raw.strip_prefix('-') {
        (true, rest)
    } else {
        (false, raw.strip_prefix('+').unwrap_or(raw))
    };
    if remaining == "0" {
        return Some(0);
    }
    if remaining.is_empty() {
        return None;
    }
    let mut total = 0u64;
    let limit = if negative {
        1u64 << 63
    } else {
        i64::MAX as u64
    };
    while !remaining.is_empty() {
        let whole_len = remaining.bytes().take_while(u8::is_ascii_digit).count();
        let (digits, rest) = remaining.split_at(whole_len);
        let whole = if digits.is_empty() {
            0
        } else {
            digits.parse::<u64>().ok()?
        };
        if whole > 1 << 63 {
            return None;
        }
        remaining = rest;
        let mut fraction = 0u64;
        let mut scale = 1.0f64;
        let mut fraction_len = 0;
        if let Some(rest) = remaining.strip_prefix('.') {
            fraction_len = rest.bytes().take_while(u8::is_ascii_digit).count();
            let (digits, tail) = rest.split_at(fraction_len);
            let mut exhausted = false;
            for digit in digits.bytes() {
                if exhausted {
                    continue;
                }
                if fraction > ((1u64 << 63) - 1) / 10 {
                    exhausted = true;
                    continue;
                }
                let next = fraction * 10 + u64::from(digit - b'0');
                if next > 1 << 63 {
                    exhausted = true;
                    continue;
                }
                fraction = next;
                scale *= 10.0;
            }
            remaining = tail;
        }
        if whole_len == 0 && fraction_len == 0 {
            return None;
        }
        let unit_len = remaining
            .bytes()
            .take_while(|byte| !byte.is_ascii_digit() && *byte != b'.')
            .count();
        let (unit, tail) = remaining.split_at(unit_len);
        let nanos = match unit {
            "ns" => 1,
            "us" | "µs" | "μs" => 1_000,
            "ms" => 1_000_000,
            "s" => 1_000_000_000,
            "m" => 60_000_000_000,
            "h" => 3_600_000_000_000,
            _ => return None,
        };
        // Match Go's float64 truncation of each fractional component.
        let fractional = (fraction as f64 * (nanos as f64 / scale)) as u64;
        total = total
            .checked_add(whole.checked_mul(nanos)?)?
            .checked_add(fractional)?;
        if total > limit {
            return None;
        }
        remaining = tail;
    }
    if negative {
        if total == 1u64 << 63 {
            Some(i64::MIN)
        } else {
            Some(-(total as i64))
        }
    } else {
        Some(total as i64)
    }
}

/// Parse the positive subset of Go's `time.ParseDuration` grammar.
pub(crate) fn parse_positive_duration(raw: &str) -> Option<PositiveDuration> {
    let nanos = parse_go_duration(raw)?;
    NonZeroU64::new(u64::try_from(nanos).ok()?).map(PositiveDuration)
}

#[cfg(test)]
mod tests {
    use super::{parse_go_duration, parse_positive_duration};

    #[test]
    fn signed_go_durations_include_zero_negative_and_minimum() {
        for (input, nanos) in [
            ("0", 0),
            ("+0", 0),
            ("-0", 0),
            ("0s", 0),
            ("-0.1ns", 0),
            ("-1.5s", -1_500_000_000),
            ("-9223372036854775808ns", i64::MIN),
        ] {
            assert_eq!(parse_go_duration(input), Some(nanos), "{input}");
        }
        for input in ["", "+", "-", "1", "1day", "--1s", "9223372036854775808ns"] {
            assert_eq!(parse_go_duration(input), None, "{input}");
        }
    }

    #[test]
    fn positive_go_durations_cover_units_fractions_and_boundaries() {
        for (input, nanos) in [
            ("1ns", 1),
            ("1.5ns", 1),
            (".5s", 500_000_000),
            ("1m.5s", 60_500_000_000),
            ("1µs", 1_000),
            ("1μs", 1_000),
            ("1us", 1_000),
            ("9223372036854775807ns", i64::MAX as u64),
        ] {
            assert_eq!(
                parse_positive_duration(input).map(|d| d.as_nanos()),
                Some(nanos),
                "{input}"
            );
        }
        for input in [
            "",
            "0",
            "0.1ns",
            "-1s",
            "1",
            "1day",
            "1_0s",
            "9223372036854775808ns",
        ] {
            assert!(parse_positive_duration(input).is_none(), "{input}");
        }
    }
}
