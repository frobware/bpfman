// SPDX-License-Identifier: Apache-2.0
// Copyright Authors of bpfman

//! Top-level database module.
//!
//! This module re-exports types and helpers used throughout the
//! crate, flattening the module structure so consumers can simply
//! use crate::db::KernelU32;
//!
//! The `types` module and its internal submodules are kept private to
//! encourage consistent usage and avoid exposing internal structure.
//!
//! If a type appears in `crate::db`, it's part of the crate's
//! intended public interface.

pub mod bpf_link;
pub mod bpf_map;
pub mod bpf_program;
pub mod bpf_program_map;
pub mod prelude;
pub mod schema;
mod types;

pub use bpf_link::*;
pub use bpf_map::*;
pub use bpf_program::*;
pub use bpf_program_map::*;
pub use schema::{bpf_links, bpf_maps, bpf_program_maps, bpf_programs};

// Re-export database column types and wrappers for public use. These
// types are defined in the private `types` module and exposed here to
// flatten the db module API.
//
// See also: `crate::db::prelude` for glob imports.
pub use types::{KernelU32, U8Blob, U16Blob, U32Blob, U64Blob, U128Blob, UnsignedIntBlobError};
