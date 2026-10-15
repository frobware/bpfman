//! Host user-space probe intent, independent of kernel libraries.
use crate::Symbol;
use alloc::string::String;
use core::{fmt, num::NonZeroU32};

/// Validated host file target and probe location. Bare library-name lookup is
/// a separate capability; file paths must contain a directory separator.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UprobeTarget {
    target: String,
    function: Option<Symbol>,
    offset: u64,
    pid: Option<NonZeroU32>,
}

/// Invalid host probe intent.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct InvalidUprobe;

impl fmt::Display for InvalidUprobe {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(
            "expected a host file path, a symbol or nonzero offset, and representable offset/PID",
        )
    }
}

impl core::error::Error for InvalidUprobe {}

impl UprobeTarget {
    /// Validate file-target syntax, attachment location and persistence ranges.
    /// PID zero selects all processes, matching the public bpfman interface.
    pub fn new(
        target: &str,
        function: Option<Symbol>,
        offset: u64,
        pid: u32,
    ) -> Result<Self, InvalidUprobe> {
        if !target.contains('/')
            || target.ends_with('/')
            || target.contains('\0')
            || offset > i64::MAX as u64
            || pid > i32::MAX as u32
            || (function.is_none() && offset == 0)
        {
            return Err(InvalidUprobe);
        }

        Ok(Self {
            target: target.into(),
            function,
            offset,
            pid: NonZeroU32::new(pid),
        })
    }

    /// File target spelling retained as operator intent.
    pub fn target(&self) -> &str {
        &self.target
    }

    /// Optional ELF symbol. Without a symbol, offset is absolute in the file.
    pub fn function(&self) -> Option<&str> {
        self.function.as_ref().map(Symbol::as_str)
    }

    /// Symbol-relative or absolute file offset.
    pub fn offset(&self) -> u64 {
        self.offset
    }

    /// Optional positive process filter; absence means all processes.
    pub fn pid(&self) -> Option<NonZeroU32> {
        self.pid
    }

    /// Combine a validated target with entry/return intent derived from a program.
    pub fn into_link(self, retprobe: bool) -> UprobeLink {
        UprobeLink {
            target: self,
            retprobe,
        }
    }
}

/// Stored user-space attachment intent, including entry/return selection.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UprobeLink {
    target: UprobeTarget,
    retprobe: bool,
}

impl UprobeLink {
    /// Validate persisted target fields and entry/return intent.
    pub fn new(
        target: &str,
        function: Option<Symbol>,
        offset: u64,
        pid: u32,
        retprobe: bool,
    ) -> Result<Self, InvalidUprobe> {
        Ok(UprobeTarget::new(target, function, offset, pid)?.into_link(retprobe))
    }

    /// File target spelling retained as operator intent.
    pub fn target(&self) -> &str {
        self.target.target()
    }

    /// Optional ELF symbol.
    pub fn function(&self) -> Option<&str> {
        self.target.function()
    }

    /// Symbol-relative or absolute file offset.
    pub fn offset(&self) -> u64 {
        self.target.offset()
    }

    /// Optional positive process filter; absence means all processes.
    pub fn pid(&self) -> Option<NonZeroU32> {
        self.target.pid()
    }

    /// Return rather than entry probe.
    pub fn retprobe(&self) -> bool {
        self.retprobe
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn admission_rejects_ambiguous_targets_and_unrepresentable_values() -> Result<(), InvalidUprobe>
    {
        for path in ["", "libc", "/tmp/", "/tmp/a\0"] {
            assert!(
                UprobeLink::new(
                    path,
                    Some("entry".try_into().map_err(|_| InvalidUprobe)?),
                    0,
                    0,
                    false
                )
                .is_err()
            );
        }
        assert!(UprobeLink::new("/tmp/a", None, 0, 0, false).is_err());
        assert!(UprobeLink::new("/tmp/a", None, u64::MAX, 0, false).is_err());
        assert!(UprobeLink::new("/tmp/a", None, 1, u32::MAX, false).is_err());
        let target = UprobeLink::new("./a", None, 42, 7, true)?;
        assert_eq!(target.offset(), 42);
        assert_eq!(target.pid().map(NonZeroU32::get), Some(7));
        assert!(target.retprobe());
        Ok(())
    }
}
