// SPDX-License-Identifier: Apache-2.0
// Copyright Authors of bpfman

use anyhow::{Context, anyhow, bail};
use chrono::NaiveDateTime;
use diesel::prelude::*;
use log::info;

use crate::{
    k32::KernelU32, oci_utils::image_manager::ImageManager, setup, types::Location,
    uintblob::U64Blob,
};

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

#[derive(
    Clone,
    Debug,
    Eq,
    PartialEq,
    serde::Serialize,
    serde::Deserialize,
    AsChangeset,
    Insertable,
    Identifiable,
    Queryable,
)]
#[diesel(table_name = crate::schema::bpf_programs)]
#[diesel(primary_key(id))]
pub struct BpfProgram {
    pub id: KernelU32,
    pub name: String,
    pub kind: String,
    pub state: String,
    pub location_type: String,
    pub file_path: Option<String>,
    pub image_url: Option<String>,
    pub image_pull_policy: Option<String>,
    pub username: Option<String>,
    pub password: Option<String>,
    pub map_pin_path: String,
    pub map_owner_id: Option<KernelU32>,
    #[diesel(sql_type = diesel::sql_types::Binary)]
    #[serde(skip)]
    pub program_bytes: Vec<u8>,
    pub metadata: String,
    pub global_data: String,
    pub retprobe: Option<bool>,
    pub fn_name: Option<String>,
    pub kernel_name: Option<String>,
    pub kernel_program_type: Option<KernelU32>,
    pub kernel_loaded_at: Option<String>,
    pub kernel_tag: U64Blob,
    pub kernel_gpl_compatible: Option<bool>,
    pub kernel_btf_id: Option<KernelU32>,
    pub kernel_bytes_xlated: Option<KernelU32>,
    pub kernel_jited: Option<bool>,
    pub kernel_bytes_jited: Option<KernelU32>,
    pub kernel_verified_insns: Option<KernelU32>,
    pub kernel_bytes_memlock: Option<KernelU32>,
    pub created_at: NaiveDateTime,
    pub updated_at: Option<NaiveDateTime>,
}

#[derive(Debug, AsChangeset, Insertable, Identifiable, Queryable)]
#[diesel(belongs_to(BpfProgram, foreign_key = program_id))]
#[diesel(table_name = crate::schema::bpf_links)]
#[diesel(primary_key(id))]
pub struct BpfLink {
    pub id: KernelU32,
    pub program_id: KernelU32,
    pub link_type: Option<String>,
    pub target: Option<String>,
    pub state: String,
    pub created_at: NaiveDateTime,
    pub updated_at: Option<NaiveDateTime>,
}

#[derive(
    Debug, AsChangeset, Insertable, Identifiable, Queryable, serde::Serialize, serde::Deserialize,
)]
#[diesel(table_name = crate::schema::bpf_maps)]
#[diesel(primary_key(id))]
pub struct BpfMap {
    pub id: KernelU32,
    pub name: String,
    pub map_type: Option<String>,
    pub key_size: KernelU32,
    pub value_size: KernelU32,
    pub max_entries: KernelU32,
    pub created_at: NaiveDateTime,
    pub updated_at: Option<NaiveDateTime>,
}

#[derive(Debug, Queryable, Selectable, Associations)]
#[diesel(belongs_to(BpfProgram, foreign_key = program_id))]
#[diesel(belongs_to(BpfMap, foreign_key = map_id))]
#[diesel(table_name = crate::schema::bpf_program_maps)]
pub struct BpfProgramMap {
    pub program_id: KernelU32,
    pub map_id: KernelU32,
}

/// BPF Program database operations.
///
/// This implementation provides a thin convenience layer over the
/// underlying database operations, isolating direct calls to Diesel.
/// The functions are intentionally simple, handling only basic CRUD
/// (Create, Read, Update, Delete) operations.
///
/// # Transaction Handling
///
/// These functions do not manage database transactions. Transaction
/// control should be handled at a higher level where operation
/// grouping and rollback behaviour can only be determined by the
/// caller.
///
/// # Error Handling
///
/// All functions return `QueryResult<T>`, propagating any database
/// errors to the caller for handling.
impl BpfProgram {
    /// Inserts a new record in the database.
    ///
    /// Sets created_at and updated_at timestamps before insertion.
    pub fn insert_record(
        conn: &mut SqliteConnection,
        program: &BpfProgram,
    ) -> QueryResult<BpfProgram> {
        diesel::insert_into(crate::schema::bpf_programs::table)
            .values(program)
            .returning(crate::schema::bpf_programs::all_columns)
            .get_result(conn)
    }

    /// Returns all BPF programs in the database.
    pub fn find_all(conn: &mut SqliteConnection) -> QueryResult<Vec<BpfProgram>> {
        use crate::schema::bpf_programs::dsl::*;
        bpf_programs.load(conn)
    }

    /// Finds a BPF program by its ID.
    pub fn find_record(
        conn: &mut SqliteConnection,
        search_id: KernelU32,
    ) -> QueryResult<BpfProgram> {
        use crate::schema::bpf_programs::dsl::*;
        bpf_programs.filter(id.eq(search_id)).first(conn)
    }

    /// Updates an existing BPF program record.
    pub fn update_record(&mut self, conn: &mut SqliteConnection) -> QueryResult<()> {
        use crate::schema::bpf_programs::dsl::*;

        let updated: BpfProgram = diesel::update(bpf_programs.filter(id.eq(self.id)))
            .set(&*self)
            .get_result(conn)?;

        *self = updated;
        Ok(())
    }

    /// Deletes a BPF program by its ID. Returns true if a record was
    /// deleted, false if no record matched the ID.
    pub fn delete_record(conn: &mut SqliteConnection, delete_id: KernelU32) -> QueryResult<bool> {
        use crate::schema::bpf_programs::dsl::*;

        let num_deleted = diesel::delete(bpf_programs.filter(id.eq(delete_id))).execute(conn)?;

        Ok(num_deleted > 0)
    }
}

impl BpfMap {
    /// Inserts a map record into the database, ignoring conflicts if
    /// the record already exists.
    ///
    /// This method is particularly useful when dealing with shared
    /// maps between multiple programs. If a map with the same ID
    /// already exists in the database, the insertion is silently
    /// skipped without raising an error.
    ///
    /// # Arguments
    ///
    /// * `conn` - A mutable reference to an active SQLite connection
    /// * `map` - The BpfMap record to insert
    ///
    /// # Returns
    ///
    /// * `QueryResult<usize>` - On success, returns the number of rows affected (1 if inserted, 0 if skipped).
    /// * On failure, returns a Diesel error (e.g., for connection
    ///   issues or constraint violations).
    pub fn insert_record_on_conflict_do_nothing(
        conn: &mut SqliteConnection,
        map: &BpfMap,
    ) -> QueryResult<usize> {
        diesel::insert_into(crate::schema::bpf_maps::table)
            .values(map)
            .on_conflict_do_nothing()
            .execute(conn)
    }
}

impl BpfLink {
    pub fn insert_record(
        conn: &mut SqliteConnection,
        link: &BpfLink,
    ) -> Result<(), diesel::result::Error> {
        diesel::insert_into(crate::schema::bpf_links::table)
            .values(link)
            .execute(conn)?;

        Ok(())
    }
}

impl BpfProgramMap {
    pub fn insert_record(
        conn: &mut SqliteConnection,
        program_id: KernelU32,
        map_id: KernelU32,
    ) -> Result<(), diesel::result::Error> {
        use crate::schema::bpf_program_maps;

        diesel::insert_into(bpf_program_maps::table)
            .values((
                bpf_program_maps::program_id.eq(program_id),
                bpf_program_maps::map_id.eq(map_id),
            ))
            .execute(conn)?;

        Ok(())
    }
}

pub fn get_program_bytes_and_validate(
    location: &Location,
    image_manager: &mut ImageManager,
    requested_programs: &[(String, Vec<String>)],
) -> anyhow::Result<(Vec<u8>, Vec<String>)> {
    // XXX(frobware) - We need to refactor get_program_bytes() to not
    // require a SLED db. For the moment just continue to pass a SLED
    // DB handle.
    let (_config, root_db) = setup()?;

    let (program_bytes, function_names) = location
        .get_program_bytes(&root_db, image_manager)
        .context("Failed to retrieve eBPF program bytes")?;

    if let Location::Image(image) = location {
        info!(
            "Loading program bytecode from container image: {}",
            image.get_url()
        );

        for (prog_type, parts) in requested_programs {
            let name = parts
                .first()
                .ok_or_else(|| anyhow!("Missing program name for type '{}'", prog_type))?;

            if !function_names.contains(name) {
                bail!(
                    "Function '{}' not found in eBPF Image '{}'. Available: {:?}",
                    name,
                    image.get_url(),
                    function_names
                );
            }
        }
    } else if let Location::File(path) = location {
        info!("Loading program bytecode from file: {}", path);
    }

    Ok((program_bytes, function_names))
}

#[cfg(test)]
/// This is intentionally limited to `#[cfg(test)]` to prevent misuse
/// in production code where a fully-initialised `BpfProgram` is the
/// expected norm.
impl Default for BpfProgram {
    fn default() -> Self {
        Self {
            id: 0u32.into(),
            name: "".to_owned(),
            kind: "".to_owned(),
            state: "".to_owned(),
            location_type: "".to_owned(),
            file_path: None,
            image_url: None,
            image_pull_policy: None,
            username: None,
            password: None,
            map_pin_path: "".to_owned(),
            map_owner_id: None,
            program_bytes: vec![],
            metadata: "{}".to_owned(),
            global_data: "{}".to_owned(),
            retprobe: None,
            fn_name: None,
            kernel_name: None,
            kernel_program_type: None,
            kernel_loaded_at: None,
            kernel_tag: U64Blob::from(0u64),
            kernel_gpl_compatible: None,
            kernel_btf_id: None,
            kernel_bytes_xlated: None,
            kernel_jited: None,
            kernel_bytes_jited: None,
            kernel_verified_insns: None,
            kernel_bytes_memlock: None,
            created_at: Default::default(),
            updated_at: Default::default(),
        }
    }
}

#[cfg(test)]
/// This is intentionally limited to `#[cfg(test)]` to prevent misuse
/// in production code where a fully-initialised `BpfLink` is the
/// expected norm.
impl Default for BpfLink {
    fn default() -> Self {
        Self {
            id: Default::default(),
            program_id: Default::default(),
            link_type: None,
            target: None,
            state: "".to_owned(),
            created_at: Default::default(),
            updated_at: Default::default(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{establish_sqlite_connection, models::BpfProgram};

    fn setup_test_db() -> SqliteConnection {
        let database_url = ":memory:";
        establish_sqlite_connection(database_url)
            .expect("Failed to establish in-memory SQLite connection")
    }

    /// Verifies that the SQLite PRAGMA foreign_keys setting is
    /// enabled (value = 1) when establishing database connections
    /// through our standard method.
    ///
    /// This test is critical because:
    /// 1. SQLite does NOT enforce foreign key constraints by default.
    /// 2. Our schema depends heavily on ON DELETE CASCADE behaviors for referential integrity.
    /// 3. Without this setting enabled, cascading deletes silently fail, causing orphaned data.
    ///
    /// If this test fails, cascade operations like automatic map
    /// cleanup won't work properly. We also want to regress early
    /// should the pragma be removed from establishing().
    #[test]
    fn test_foreign_keys_pragma_enabled() {
        let mut conn = setup_test_db();

        #[derive(QueryableByName, Debug)]
        struct ForeignKeySetting {
            #[diesel(sql_type = diesel::sql_types::Integer)]
            foreign_keys: i32,
        }

        let result = diesel::sql_query("PRAGMA foreign_keys")
            .load::<ForeignKeySetting>(&mut conn)
            .expect("Failed to query foreign_keys PRAGMA");

        let foreign_keys_enabled = result
            .first()
            .expect("Expected one row from PRAGMA foreign_keys")
            .foreign_keys;

        assert_eq!(
            foreign_keys_enabled, 1,
            "PRAGMA foreign_keys is not enabled! This will break cascade deletes and referential integrity."
        );
    }

    #[test]
    /// Tests the insertion and retrieval of BPF programs in the
    /// database.
    ///
    /// This test verifies several aspects in sequence:
    ///
    /// 1. Program Creation:
    ///    - Creates a minimal but valid BPF program with required fields
    ///    - Verifies default timestamps are set to epoch
    ///
    /// 2. Program Insertion:
    ///    - Tests successful insertion into the database
    ///    - Verifies timestamps are updated (no longer epoch)
    ///    - After syncing timestamps, confirms complete equality between
    ///      input and inserted program
    ///
    /// 3. Default Values and JSON Validity:
    ///    - Confirms metadata defaults to "{}"
    ///    - Confirms global_data defaults to "{}"
    ///    - Verifies all are valid JSON structures
    ///
    /// 4. Record Retrieval:
    ///    - Tests the find_record operation by ID
    ///    - Verifies complete equality between inserted and retrieved records
    ///
    /// The test uses Eq for complete record comparison after
    /// synchronising timestamps, providing thorough verification of
    /// all fields through the database round-trip.
    fn test_insert_and_find_bpf_program() {
        let mut db_conn = setup_test_db();

        // Setup test program with minimal required fields.
        // Ensure BpfProgram derives Clone (if not, add #[derive(Clone)] to its definition).
        let prog = BpfProgram {
            id: 100u32.into(),
            name: "xdp_test_program".to_owned(),
            kind: "xdp".to_owned(),
            state: "pre_load".to_owned(),
            location_type: "file".to_owned(),
            file_path: Some("/path/to/test_program.o".to_owned()),
            map_pin_path: "/sys/fs/bpf/test_program".to_owned(),
            program_bytes: vec![0xAA, 0xBB, 0xCC],
            ..Default::default()
        };

        // Verify default timestamps are epoch.
        let epoch: NaiveDateTime = Default::default();
        assert_eq!(prog.created_at, epoch, "Default created_at should be epoch");
        assert_eq!(prog.updated_at, None, "Default updated_at should be None");

        // Clone prog so we have a copy to compare later.
        let mut prog_for_assert = prog.clone();

        // Insert program.
        // Note: insert_record now takes ownership of `prog`.
        let inserted_program =
            BpfProgram::insert_record(&mut db_conn, &prog).expect("Insert failed");

        // Sync timestamps to enable Eq comparisons.
        prog_for_assert.created_at = inserted_program.created_at;
        prog_for_assert.updated_at = inserted_program.updated_at;

        // Assert that the modified copy equals the inserted record.
        assert_eq!(prog_for_assert, inserted_program);

        // Verify JSON field defaults and validity.
        {
            assert_eq!(inserted_program.metadata, "{}");
            assert_eq!(inserted_program.global_data, "{}");

            serde_json::from_str::<serde_json::Value>(&inserted_program.metadata)
                .expect("metadata should be valid JSON");
            serde_json::from_str::<serde_json::Value>(&inserted_program.global_data)
                .expect("global_data should be valid JSON");
        }

        // Verify record retrieval using full Eq comparison.
        {
            let found_program = BpfProgram::find_record(&mut db_conn, prog_for_assert.id)
                .expect("Failed to find program");
            assert_eq!(found_program, inserted_program);
        }
    }

    #[test]
    /// This test verifies the serialisation, deserialisation, and
    /// database persistence of BpfProgram structs. It ensures that:
    ///
    /// - The Serde derive macros work correctly for all field types
    ///   (KernelU32, String, Option<String>, Vec<u8>, etc.).
    /// - No data is lost in the JSON conversion.
    /// - Diesel's type mappings are correct for all fields.
    /// - The database schema matches the struct.
    /// - No data is lost or corrupted during database operations.
    /// - Timestamps are handled correctly.
    /// - Optional fields are preserved.
    /// - Binary data is stored and retrieved accurately.
    /// - JSON string fields (metadata, global_data, kernel_map_ids)
    ///   maintain their format.
    ///
    /// It performs two round-trip tests:
    ///
    /// 1. **JSON round-trip:**
    ///    - Creates a BpfProgram with all fields populated.
    ///    - Serialises it to JSON.
    ///    - Deserialises back to a BpfProgram.
    ///    - Verifies all fields match the original.
    ///
    /// 2. **Database round-trip:**
    ///    - Takes the same BpfProgram.
    ///    - Inserts it into SQLite.
    ///    - Retrieves it.
    ///    - Serialises to JSON.
    ///    - Deserialises back to a BpfProgram.
    ///    - Verifies all fields match.
    fn test_bpf_program_serde_roundtrip() {
        let prog = BpfProgram {
            id: 100u32.into(),
            name: "xdp_test_program".to_owned(),
            kind: "xdp".to_owned(),
            state: "pre_load".to_owned(),
            location_type: "file".to_owned(),
            file_path: Some("/path/to/test_program.o".to_owned()),
            image_url: Some("registry.example.com/image:tag".to_owned()),
            image_pull_policy: Some("Always".to_owned()),
            username: Some("testuser".to_owned()),
            password: Some("testpass".to_owned()),
            map_pin_path: "/sys/fs/bpf/test_program".to_owned(),
            map_owner_id: Some(1234u32.into()),
            program_bytes: vec![0xAA, 0xBB, 0xCC],
            metadata: "{}".to_owned(),
            global_data: "{}".to_owned(),
            retprobe: Some(true),
            fn_name: Some("test_function".to_owned()),
            kernel_name: Some("test_kernel_prog".to_owned()),
            kernel_program_type: Some(123u32.into()),
            kernel_loaded_at: Some("2024-02-18T12:00:00Z".to_owned()),
            kernel_tag: U64Blob::from(u64::MAX),
            kernel_gpl_compatible: Some(true),
            kernel_btf_id: Some(456u32.into()),
            kernel_bytes_xlated: Some(1024u32.into()),
            kernel_jited: Some(true),
            kernel_bytes_jited: Some(2048u32.into()),
            kernel_verified_insns: Some(100u32.into()),
            kernel_bytes_memlock: Some(4096u32.into()),
            ..Default::default()
        };

        // Test JSON serialisation round-trip.
        {
            let json = serde_json::to_string(&prog).expect("Failed to serialize to JSON");

            let mut deserialized: BpfProgram =
                serde_json::from_str(&json).expect("Failed to deserialize from JSON");

            // Manually restore the skipped field for comparison.
            deserialized.program_bytes = prog.program_bytes.clone();

            assert_eq!(prog, deserialized);
        }

        // Test database round-trip.
        {
            let mut db_conn = setup_test_db();

            let inserted =
                BpfProgram::insert_record(&mut db_conn, &prog).expect("Failed to insert");

            let json_after_db =
                serde_json::to_string(&inserted).expect("Failed to serialize after DB");

            let mut deserialized_after_db: BpfProgram =
                serde_json::from_str(&json_after_db).expect("Failed to deserialize after DB");

            // Manually restore the skipped field for comparison.
            deserialized_after_db.program_bytes = prog.program_bytes.clone();

            assert_eq!(inserted, deserialized_after_db);
        }
    }

    #[test]
    /// Verifies cascade + trigger behaviour across multiple programs
    /// sharing the same BPF map. Ensures the map is only deleted once
    /// all referencing programs are removed.
    ///
    /// Test plan:
    ///
    /// 1. Insert two BPF programs: prog1 and prog2.
    /// 2. Insert one shared map.
    /// 3. Link both programs to the map.
    /// 4. Confirm both program_map links exist.
    /// 5. Delete prog1 — program_map row is deleted, map remains.
    /// 6. Confirm only prog2's mapping remains.
    /// 7. Delete prog2 — remaining mapping and map are deleted.
    /// 8. Confirm bpf_maps is now empty.
    fn test_program_map_cascade_deletes_map_only_when_unused() {
        use crate::{
            models::{BpfMap, BpfProgram, BpfProgramMap},
            schema::*,
        };

        let mut conn = setup_test_db();

        // Shared map inserted once.
        let shared_map = BpfMap {
            id: 900u32.into(),
            name: "shared_map".to_string(),
            map_type: Some("Array".to_string()),
            key_size: 4u32.into(),
            value_size: 64u32.into(),
            max_entries: 128u32.into(),
            created_at: Default::default(),
            updated_at: Default::default(),
        };
        BpfMap::insert_record_on_conflict_do_nothing(&mut conn, &shared_map).unwrap();

        // Insert first program.
        let prog1 = BpfProgram {
            id: 101u32.into(),
            name: "prog1".into(),
            kind: "tracepoint".into(),
            state: "pre_load".into(),
            location_type: "file".into(),
            file_path: Some("/tmp/prog1.o".into()),
            map_pin_path: "/sys/fs/bpf/prog1".into(),
            program_bytes: vec![0x1],
            metadata: "{}".into(),
            global_data: "{}".into(),
            ..Default::default()
        };
        BpfProgram::insert_record(&mut conn, &prog1).unwrap();

        // Insert second program.
        let prog2 = BpfProgram {
            id: 102u32.into(),
            name: "prog2".into(),
            kind: "tracepoint".into(),
            state: "pre_load".into(),
            location_type: "file".into(),
            file_path: Some("/tmp/prog2.o".into()),
            map_pin_path: "/sys/fs/bpf/prog2".into(),
            program_bytes: vec![0x2],
            metadata: "{}".into(),
            global_data: "{}".into(),
            ..Default::default()
        };
        BpfProgram::insert_record(&mut conn, &prog2).unwrap();

        // Link both programs to the shared map.
        BpfProgramMap::insert_record(&mut conn, prog1.id, shared_map.id).unwrap();
        BpfProgramMap::insert_record(&mut conn, prog2.id, shared_map.id).unwrap();

        // Confirm both join rows exist.
        let mappings: Vec<(KernelU32, KernelU32)> = bpf_program_maps::table
            .select((bpf_program_maps::program_id, bpf_program_maps::map_id))
            .order_by(bpf_program_maps::program_id)
            .load(&mut conn)
            .unwrap();
        assert_eq!(
            mappings,
            vec![(prog1.id, shared_map.id), (prog2.id, shared_map.id)],
            "Expected both program_map rows to exist"
        );

        // Delete first program.
        BpfProgram::delete_record(&mut conn, prog1.id).unwrap();

        // Confirm only second mapping remains.
        let mappings: Vec<(KernelU32, KernelU32)> = bpf_program_maps::table
            .select((bpf_program_maps::program_id, bpf_program_maps::map_id))
            .load(&mut conn)
            .unwrap();
        assert_eq!(
            mappings,
            vec![(prog2.id, shared_map.id)],
            "Expected only prog2 mapping to remain"
        );

        // Confirm map still exists.
        let maps: Vec<BpfMap> = bpf_maps::table.load(&mut conn).unwrap();
        assert_eq!(maps.len(), 1, "Expected shared map to still exist");

        // Delete second program.
        BpfProgram::delete_record(&mut conn, prog2.id).unwrap();

        // Confirm join table is empty.
        let mappings: Vec<(KernelU32, KernelU32)> = bpf_program_maps::table
            .select((bpf_program_maps::program_id, bpf_program_maps::map_id))
            .load(&mut conn)
            .unwrap();
        assert!(
            mappings.is_empty(),
            "Expected all program_map rows to be gone"
        );

        // Confirm shared map is now deleted.
        let maps: Vec<BpfMap> = bpf_maps::table.load(&mut conn).unwrap();
        assert!(
            maps.is_empty(),
            "Expected shared map to be deleted after all program references removed"
        );
    }
}
