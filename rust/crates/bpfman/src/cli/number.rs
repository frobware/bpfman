//! Kong's base-zero numeric options and decimal managed identities.

use std::num::{NonZeroU32, NonZeroU64};

pub(crate) fn boolean(raw: &str) -> Result<bool, String> {
    match raw.to_ascii_lowercase().as_str() {
        "true" | "1" | "yes" => Ok(true),
        "false" | "0" | "no" => Ok(false),
        _ => Err("expected a Go boolean value".into()),
    }
}

fn decimal(raw: &str) -> Result<&str, String> {
    let raw = raw.trim();
    if raw.is_empty() || !raw.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err("expected an unsigned decimal resource ID".into());
    }
    Ok(raw)
}

pub(super) fn program_id(raw: &str) -> Result<NonZeroU32, String> {
    NonZeroU32::new(owner(raw)?).ok_or_else(|| "program ID must be nonzero".into())
}

pub(super) fn owner(raw: &str) -> Result<u32, String> {
    decimal(raw)?
        .parse()
        .map_err(|_| "expected a decimal program ID".into())
}

pub(super) fn link_id(raw: &str) -> Result<NonZeroU64, String> {
    NonZeroU64::new(link_operand(raw)?).ok_or_else(|| "link ID must be nonzero".into())
}

pub(super) fn link_operand(raw: &str) -> Result<u64, String> {
    decimal(raw)?
        .parse()
        .map_err(|_| "expected a decimal link ID".into())
}

pub(super) fn offset(raw: &str) -> Result<u64, String> {
    uint(raw).and_then(|value| {
        if value <= i64::MAX as u64 {
            Ok(value)
        } else {
            Err("offset exceeds i64::MAX".into())
        }
    })
}

pub(super) fn signed32(raw: &str) -> Result<u32, String> {
    let (negative, digits) = raw
        .strip_prefix('-')
        .map_or((false, raw), |digits| (true, digits));
    let value = uint(if negative {
        digits
    } else {
        digits.strip_prefix('+').unwrap_or(digits)
    })?;
    u32::try_from(value)
        .ok()
        .filter(|value| *value <= i32::MAX as u32 && (!negative || *value == 0))
        .ok_or_else(|| "expected a nonnegative signed 32-bit value".into())
}

pub(super) fn ifindex(raw: &str) -> Result<u32, String> {
    u32::try_from(uint(raw)?).map_err(|_| "interface index exceeds u32::MAX".into())
}

pub(super) fn namespace(raw: &str) -> Result<NonZeroU64, String> {
    NonZeroU64::new(uint(raw)?).ok_or_else(|| "namespace ID must be nonzero".into())
}

pub(super) fn interface(raw: &str) -> Result<NonZeroU32, String> {
    NonZeroU32::new(ifindex(raw)?).ok_or_else(|| "interface index must be nonzero".into())
}

pub(super) fn uint(raw: &str) -> Result<u64, String> {
    let invalid =
        || "expected an unsigned integer (decimal, octal, hexadecimal or binary)".to_owned();
    let (base, digits, prefixed) =
        if let Some(digits) = raw.strip_prefix("0x").or_else(|| raw.strip_prefix("0X")) {
            (16, digits, true)
        } else if let Some(digits) = raw.strip_prefix("0o").or_else(|| raw.strip_prefix("0O")) {
            (8, digits, true)
        } else if let Some(digits) = raw.strip_prefix("0b").or_else(|| raw.strip_prefix("0B")) {
            (2, digits, true)
        } else if raw.len() > 1 && raw.starts_with('0') {
            (8, &raw[1..], true)
        } else {
            (10, raw, false)
        };
    let digits = if prefixed {
        digits.strip_prefix('_').unwrap_or(digits)
    } else {
        digits
    };
    if digits.is_empty()
        || digits.starts_with('_')
        || digits.ends_with('_')
        || digits.contains("__")
    {
        return Err(invalid());
    }
    // from_str_radix accepts a plus sign; Go's unsigned mapper does not.
    if !digits
        .chars()
        .all(|digit| digit == '_' || digit.is_digit(base))
    {
        return Err(invalid());
    }
    u64::from_str_radix(&digits.replace('_', ""), base).map_err(|_| invalid())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn radix_prefixes_separators_signs_and_widths_match_kong() -> Result<(), String> {
        for (raw, expected) in [
            ("010", 8),
            ("0o10", 8),
            ("0x10", 16),
            ("0b10", 2),
            ("0x_ff", 255),
            ("1_000", 1000),
            ("0_7", 7),
            ("0", 0),
        ] {
            assert_eq!(uint(raw)?, expected);
        }
        for raw in [
            "",
            "+1",
            "-1",
            " 1",
            "08",
            "0x",
            "0x__f",
            "1__0",
            "1_",
            "18446744073709551616",
        ] {
            assert!(uint(raw).is_err(), "{raw}");
        }
        assert_eq!(signed32("+010")?, 8);
        assert_eq!(signed32("-0x0")?, 0);
        assert!(signed32("-1").is_err());
        assert!(signed32("-+0").is_err());
        assert!(signed32("0x80000000").is_err());
        assert_eq!(program_id(" 010 ")?.get(), 10);
        assert!(program_id("0x10").is_err());
        assert!(owner("+1").is_err());
        assert!(link_operand("+1").is_err());
        assert!(boolean("tRuE")?);
        assert!(boolean("t").is_err());
        assert!(boolean("YES")?);
        assert!(!boolean("No")?);
        assert!(boolean("TRUE")?);
        assert!(!boolean("0")?);
        assert_eq!(owner(" 0 ")?, 0);
        Ok(())
    }
}
