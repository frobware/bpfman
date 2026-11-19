//! SANS-I/O compilation of shell syntax into verified block IR.

mod lower;

pub use lower::{LowerError, lower, lower_unchecked};
