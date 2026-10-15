//! Kernel probe targets, independent of ELF section and kernel-library types.
use alloc::string::String;
use core::{fmt, str::FromStr};

/// A kernel function symbol accepted by the probe attachment interface.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct KernelFunction(String);

/// A malformed kernel function name.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct InvalidKernelFunction;

impl fmt::Display for InvalidKernelFunction {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("expected a kernel function name matching [a-zA-Z_][a-zA-Z0-9_.]*")
    }
}

impl core::error::Error for InvalidKernelFunction {}

impl FromStr for KernelFunction {
    type Err = InvalidKernelFunction;

    fn from_str(raw: &str) -> Result<Self, Self::Err> {
        if raw.is_empty()
            || !raw.bytes().enumerate().all(|(i, b)| {
                b.is_ascii_alphabetic() || b == b'_' || (i > 0 && (b.is_ascii_digit() || b == b'.'))
            })
        {
            return Err(InvalidKernelFunction);
        }
        Ok(Self(raw.into()))
    }
}

impl KernelFunction {
    /// Function spelling, without normalization or truncation.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Stored probe intent. Return-probe selection is derived from the loaded program.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct KprobeLink {
    /// Target kernel function.
    pub function: KernelFunction,
    /// Byte offset from the function entry.
    pub offset: u64,
    /// Attach at function return rather than entry.
    pub retprobe: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn function_names_match_the_probe_interface() {
        for raw in [
            "", "1foo", ".foo", "foo/bar", "a b", "a\0", "mod:fn", "foo\n",
        ] {
            assert!(raw.parse::<KernelFunction>().is_err(), "{raw:?}");
        }
        for raw in ["_foo", "foo", "foo.isra.42", "bpfman_test_kfunc_0"] {
            assert_eq!(
                raw.parse::<KernelFunction>()
                    .as_ref()
                    .map(KernelFunction::as_str),
                Ok(raw)
            );
        }
    }
}
