//! Go duration syntax and the lock command's nonpositive-means-unbounded policy.
//!
//! Fractional accumulation follows Go time.ParseDuration's precision rules.

use std::time::Duration;

const LIMIT: u64 = 1 << 63;

fn invalid() -> String {
    "expected a Go duration such as 30s, 1m500ms or 0".into()
}

pub(super) fn lock_timeout(raw: &str) -> Result<Duration, String> {
    let negative = raw.starts_with('-');
    let mut remaining = raw.strip_prefix(['+', '-']).unwrap_or(raw);
    if remaining == "0" {
        return Ok(Duration::ZERO);
    }
    if remaining.is_empty() {
        return Err(invalid());
    }
    let mut total = 0u64;
    while !remaining.is_empty() {
        let length = remaining.bytes().take_while(u8::is_ascii_digit).count();
        // Every consumed byte is an ASCII digit, hence a UTF-8 boundary.
        let (digits, rest) = remaining.split_at(length);
        let whole = if digits.is_empty() {
            0
        } else {
            digits.parse::<u64>().map_err(|_| invalid())?
        };
        if whole > LIMIT {
            return Err(invalid());
        }
        remaining = rest;
        let mut fraction = 0u64;
        let mut scale = 1.0f64;
        let mut fraction_length = 0;
        if let Some(rest) = remaining.strip_prefix('.') {
            fraction_length = rest.bytes().take_while(u8::is_ascii_digit).count();
            let (digits, rest) = rest.split_at(fraction_length);
            let mut exhausted = false;
            for digit in digits.bytes() {
                if exhausted {
                    continue;
                }
                if fraction > (LIMIT - 1) / 10 {
                    exhausted = true;
                    continue;
                }
                let next = fraction * 10 + u64::from(digit - b'0');
                if next > LIMIT {
                    exhausted = true;
                    continue;
                }
                fraction = next;
                scale *= 10.0;
            }
            remaining = rest;
        }
        if length == 0 && fraction_length == 0 {
            return Err(invalid());
        }
        let unit_length = remaining
            .bytes()
            .take_while(|byte| !byte.is_ascii_digit() && *byte != b'.')
            .count();
        // A delimiter is ASCII; all bytes of a Unicode unit stay together.
        let (unit, rest) = remaining.split_at(unit_length);
        let nanos = match unit {
            "ns" => 1,
            "us" | "µs" | "μs" => 1_000,
            "ms" => 1_000_000,
            "s" => 1_000_000_000,
            "m" => 60_000_000_000,
            "h" => 3_600_000_000_000,
            _ => return Err(invalid()),
        };
        let integer = whole.checked_mul(nanos).ok_or_else(invalid)?;
        // Go uses float64 here, then truncates each component to nanoseconds.
        let fractional = (fraction as f64 * (nanos as f64 / scale)) as u64;
        total = total
            .checked_add(integer)
            .and_then(|value| value.checked_add(fractional))
            .filter(|value| *value <= LIMIT)
            .ok_or_else(invalid)?;
        remaining = rest;
    }
    if negative {
        // Go lock.RunWithTimeout establishes a deadline only when timeout > 0.
        Ok(Duration::ZERO)
    } else if total <= i64::MAX as u64 {
        Ok(Duration::from_nanos(total))
    } else {
        Err(invalid())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn units_fractions_signs_and_boundaries_follow_go() -> Result<(), String> {
        for (input, nanos) in [
            ("+1µs", 1000),
            ("1μs", 1000),
            (".5s", 500_000_000),
            ("1m.5s", 60_500_000_000),
            ("1.5ns", 1),
            ("0.1ns", 0),
            ("9223372036854775807ns", i64::MAX as u64),
        ] {
            assert_eq!(lock_timeout(input)?, Duration::from_nanos(nanos), "{input}");
        }
        for input in ["0", "-0", "-1s", "-.5h", "-9223372036854775808ns"] {
            assert_eq!(lock_timeout(input)?, Duration::ZERO, "{input}");
        }
        for input in [
            "",
            "+",
            ".s",
            "1",
            " 1s",
            "1day",
            "1_0s",
            "9223372036854775808ns",
            "-9223372036854775809ns",
        ] {
            assert!(lock_timeout(input).is_err(), "{input}");
        }
        Ok(())
    }
}
