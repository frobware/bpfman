// SPDX-License-Identifier: Apache-2.0
// Copyright Authors of bpfman

//! Commonly used database types and helpers.
//!
//! Intended for glob-importing in downstream modules:
//!
//! ```rust
//! use crate::db::prelude::*;
//! ```
//!
//! This module exposes domain-level types and column wrappers, but
//! **not** Diesel schema definitions like `bpf_programs::dsl::*`.
//! Schema elements are intentionally excluded to avoid polluting the
//! namespace with common column names like `id`, `name`, etc.
//! Instead, import schema elements explicitly where needed.

pub use super::{
    BpfLink, BpfMap, BpfProgram, BpfProgramMap, KernelU32, U8Blob, U16Blob, U32Blob, U64Blob,
    U128Blob, UnsignedIntBlobError,
};
