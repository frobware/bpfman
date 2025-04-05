// SPDX-License-Identifier: Apache-2.0
// Copyright Authors of bpfman

//! Internal submodule for database-specific wrapper types.
//!
//! This module contains helper types for integrating non-standard
//! Rust types into Diesel models, such as:
//!
//! - [`KernelU32`]: for storing unsigned `u32` values in SQLite `BIGINT` columns
//! - [`U64Blob`], [`U128Blob`]: for storing larger unsigned integers in `BLOB` columns
//!
//! These types are re-exported at the `db` module level, and this
//! module itself is kept private. Callers should prefer:
//!
//! ```rust
//! use crate::db::KernelU32;
//! ```
//!
//! This allows us to flatten the external API while keeping
//! implementation details (directory layout, submodule structure)
//! private.

mod k32;
pub use self::k32::KernelU32;
