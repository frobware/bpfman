//! Pure domain vocabulary shared by lifecycle policy and I/O adapters.
//!
//! Wire formats and kernel-library types belong at the application boundary.

#![no_std]

extern crate alloc;

mod program_type;
mod summary;

pub use program_type::{ParseProgramTypeError, ProgramType};
pub use summary::StoredProgramSummary;
