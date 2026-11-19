use std::{fmt, num::NonZeroU64};

/// A positive duration in nanoseconds with the Go duration display convention.
pub struct GoDuration(pub NonZeroU64);

impl fmt::Display for GoDuration {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let nanos = self.0.get();
        const SECOND: u64 = 1_000_000_000;
        let (nanos, unit, precision, suffix) = if nanos >= SECOND {
            let hours = nanos / (SECOND * 3600);
            let minutes = (nanos / (SECOND * 60)) % 60;
            if hours != 0 {
                write!(f, "{hours}h")?;
            }
            if hours != 0 || minutes != 0 {
                write!(f, "{minutes}m")?;
            }
            (nanos % (SECOND * 60), SECOND, 9, "s")
        } else if nanos >= 1_000_000 {
            (nanos, 1_000_000, 6, "ms")
        } else if nanos >= 1_000 {
            (nanos, 1_000, 3, "µs")
        } else {
            return write!(f, "{nanos}ns");
        };
        write!(f, "{}", nanos / unit)?;
        if nanos % unit != 0 {
            let fraction = format!("{:0precision$}", nanos % unit);
            write!(f, ".{}", fraction.trim_end_matches('0'))?;
        }
        f.write_str(suffix)
    }
}
