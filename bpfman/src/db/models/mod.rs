// SPDX-License-Identifier: Apache-2.0
// Copyright Authors of bpfman

//! Diesel model definitions for bpfman.
//!
//! This module defines domain-level types that correspond to rows in
//! the database. These types implement Diesel traits like
//! `Queryable`, `Insertable`, and `Identifiable`.
//!
//! Each entity (program, map, link, etc.) has its own submodule for
//! clarity and separation of concerns.

// Diesel Derive Macros Explanation:
//
// - AsChangeset: Lets you update existing rows by setting columns to
//   struct fields, e.g. `diesel::update(table).set(...)`.
//
// - Associations: Allows Diesel to handle table relationships (for
//   example, `belongs_to`, `has_many`) based on the struct.
//
// - Identifiable: Associates the struct with a primary key, aiding
//   lookups by ID or establishing relations.
//
// - Insertable: Allows inserting new rows into a table using
//   `insert_into(...)` with `.values(...)`.
//
// - Queryable: Permits loading rows from a table into this struct,
//   typically using `table.load::<Struct>(...)`.
//
// - QueryableByName: Needed only if you use raw SQL queries via
//   `sql_query("...").load::<Struct>(...)`. It matches fields in
//   custom SQL statements to this struct.
//
// - Selectable: Adds typed projections so that
//   `table.select(Struct::as_select())` does a compile-time check
//   verifying columns match the struct.

mod bpf_link;
mod bpf_map;
mod bpf_program;
mod bpf_program_map;

pub use bpf_link::*;
pub use bpf_map::*;
pub use bpf_program::*;
pub use bpf_program_map::*;
