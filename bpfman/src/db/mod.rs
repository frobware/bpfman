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

mod types;

pub use types::{KernelU32, U8Blob, U16Blob, U32Blob, U64Blob, U128Blob, UnsignedIntBlobError};
