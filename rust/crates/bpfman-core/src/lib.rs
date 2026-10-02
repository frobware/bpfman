//! Pure observation policy and SANS-I/O lifecycle decisions.
//!
//! Operation-specific machines will consume observations and return effects as
//! data. Adapters execute those effects outside this crate. Listing is an
//! ordinary pure function over complete store observations; lifecycle APIs
//! arrive with the first tracepoint vertical slice.

#![no_std]

extern crate alloc;

mod list;
mod setup;

pub use list::{ProgramFilter, list_programs};
pub use setup::plan_store_setup;

/// One setup decision over complete persistence observations; no I/O or paths.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StoreSetup {
    /// No store exists; create it at the supported schema version.
    Initialise,
    /// Use the observed store without changing its schema.
    UseExisting,
    /// Neither upgrade nor downgrade an existing incompatible store.
    RejectIncompatible {
        /// Observed schema version.
        found: i64,
        /// Schema version supported by the adapter.
        expected: i64,
    },
}
