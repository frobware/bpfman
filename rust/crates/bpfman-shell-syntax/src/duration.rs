//! Positive Go-compatible durations used by poll syntax.

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

/// Parse the positive subset of Go's `time.ParseDuration` grammar.
pub(crate) fn parse_positive_duration(raw: &str) -> Option<PositiveDuration> {
    let mut remaining = raw.strip_prefix('+').unwrap_or(raw);
    if remaining.is_empty() || remaining == "0" || remaining.starts_with('-') {
        return None;
    }
    let mut total = 0u64;
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
        if total > i64::MAX as u64 {
            return None;
        }
        remaining = tail;
    }
    NonZeroU64::new(total).map(PositiveDuration)
}

#[cfg(test)]
mod tests {
    use super::parse_positive_duration;

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
