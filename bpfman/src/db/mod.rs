// SPDX-License-Identifier: Apache-2.0
// Copyright Authors of bpfman

//! Top-level database module.
//!
//! This module re-exports types and helpers used throughout the
//! crate, flattening the module structure so consumers can simply
//! use:
//!
//! ```rust
//! use crate::db::KernelU32;
//! ```
//!
//! ...rather than referring to deeply nested paths like:
//!
//! ```rust
//! use crate::db::types::k32::KernelU32;
//! ```
//!
//! The `types` module and its internal submodules are kept private to
//! encourage consistent usage and avoid exposing internal structure.
//!
//! If a type appears in `crate::db`, it's part of the crate's
//! intended public interface.

mod types;

pub use types::KernelU32;
