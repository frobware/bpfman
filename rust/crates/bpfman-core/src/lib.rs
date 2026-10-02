//! Pure observation policy and SANS-I/O lifecycle decisions.
//!
//! Operation-specific machines will consume observations and return effects as
//! data. Adapters execute those effects outside this crate. Listing is an
//! ordinary pure function over complete store observations; lifecycle APIs
//! arrive with the first tracepoint vertical slice.

#![no_std]

extern crate alloc;

mod list;
mod store;

pub use list::{ProgramFilter, list_programs};
pub use store::plan_store_open;

/// A complete store observation and the evidence that produced it.
///
/// `T` is opaque to policy. The interpreter can supply an opened store; policy
/// only moves it into the decision, never inspects, duplicates, or drops it.
/// Failed observations must be handled before constructing this value.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StoreObservation<T> {
    /// No store was found.
    Missing,
    /// An existing store and its observed schema version.
    Existing {
        /// Observed schema version.
        version: i64,
        /// Evidence retained by the interpreter, such as the opened store.
        evidence: T,
    },
}

/// A store-opening decision retaining all evidence needed to execute it.
///
/// Selecting an existing store without retaining that store is not representable:
///
/// ```compile_fail
/// use bpfman_core::StoreOpenPlan;
/// let decision: StoreOpenPlan<()> = StoreOpenPlan::UseExisting;
/// ```
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StoreOpenPlan<T> {
    /// No store exists; create it at the supported schema version.
    Create,
    /// Use the observed store without changing its schema.
    UseExisting(T),
    /// Neither upgrade nor downgrade an existing incompatible store.
    RejectIncompatible {
        /// Observed schema version.
        found: i64,
        /// Schema version supported by the adapter.
        expected: i64,
        /// Rejected evidence, returned so cleanup remains the interpreter's job.
        evidence: T,
    },
}
