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
//! module itself is kept private. Callers should prefer: `use
//! crate::db::KernelU32` rather than referring to deeply nested
//! paths. This allows us to flatten the external API while keeping
//! implementation details (directory layout, submodule structure)
//! private.

mod ku32;
mod uintblob;

pub use self::{
    ku32::KernelU32,
    uintblob::{U8Blob, U16Blob, U32Blob, U64Blob, U128Blob, UnsignedIntBlobError},
};
