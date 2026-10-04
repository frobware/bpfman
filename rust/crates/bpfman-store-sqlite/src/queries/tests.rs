//! Exercise statement reuse and binding against the actual Go-created schema.

use super::*;
use bpfman_fs::{RuntimeDirectory, RuntimeLayout};
use bpfman_lock::AcquireOptions;
use bpfman_model::Symbol;
use std::{collections::BTreeMap, time::Duration};

type Result = std::result::Result<(), Box<dyn std::error::Error>>;

#[test]
fn cached_statements_rebind_after_errors_and_keep_record_fields_distinct() -> Result {
    let temp = tempfile::tempdir()?;
    let layout = RuntimeLayout::try_from(temp.path().to_owned())?;
    let runtime = RuntimeDirectory::open_or_create(layout.clone())?;

    runtime.with_writer(
        AcquireOptions {
            timeout: Duration::from_secs(1),
            cancelled: None,
        },
        |writer| -> Result {
            crate::create_if_missing(&writer)?;
            let mut connection = Connection::open(writer.database_path())?;
            connection.pragma_update(None, "foreign_keys", true)?;
            let tx = connection.transaction()?;
            assert_eq!(schema_version(&tx)?, crate::SCHEMA_VERSION);

            let first = NonZeroU32::new(41).expect("id");
            let second = NonZeroU32::new(42).expect("id");
            let mut expected = Vec::new();

            for (id, license, gpl) in [(first, "GPL", true), (second, "MIT", false)] {
                let name = Symbol::try_from(format!("trace_{}", id.get()).as_str())?;
                let source = format!("source-{}'; DELETE FROM managed_programs; --.o", id.get());
                let object_path = format!("/bytecode/{id}/object.o");
                let pin_path = format!("/bpffs/prog_{id}");
                let map_path = format!("/bpffs/maps/{id}");
                let created_at = format!("2026-10-04T00:00:{id}Z");
                let metadata = BTreeMap::from([("label".into(), format!("quoted '{id}' ☃"))]);
                let metadata_json = serde_json::to_string(&metadata)?;
                let record = TracepointRecord {
                    globals: &Default::default(),
                    id,
                    name: &name,
                    source: &source,
                    license,
                    created_at: &created_at,
                    metadata: &metadata,
                };
                let map = || MapSetIdentity {
                    id,
                    pin_path: &map_path,
                    created_at: &created_at,
                };
                let program = || TracepointInsert {
                    globals: "{}",
                    record: &record,
                    object_path: &object_path,
                    pin_path: &pin_path,
                    metadata: &metadata_json,
                    gpl_compatible: gpl,
                };

                insert_map_set(&tx, map())?;
                insert_tracepoint(&tx, program())?;
                assert!(
                    insert_tracepoint(&tx, program()).is_err(),
                    "duplicate must fail without poisoning the cached statement"
                );

                let row = unload(&tx, id)?.expect("record");
                assert_eq!(row.kind, "tracepoint");
                assert_eq!(row.object, object_path);
                assert_eq!(row.pin, pin_path);
                assert_eq!(row.map_path, map_path);
                assert_eq!(row.map_set, i64::from(id.get()));
                assert_eq!(row.created, created_at);
                assert_eq!(row.map_created, created_at);
                assert_eq!((row.links, row.users, row.shared), (0, 1, 0));
                assert_eq!(
                    delete_map_set(&tx, map())?,
                    0,
                    "referenced map set is protected"
                );
                expected.push((
                    id,
                    name,
                    source,
                    object_path,
                    pin_path,
                    map_path,
                    created_at,
                    metadata_json,
                    license,
                    gpl,
                ));
            }

            let rows = programs(&tx)?;
            let summaries = summaries(&tx)?;
            assert_eq!(rows.len(), 2);
            assert_eq!(summaries.len(), 2);
            assert!(links(&tx)?.is_empty());

            for (
                (row, summary),
                (id, name, source, object, pin, map, created, metadata, license, gpl),
            ) in rows.into_iter().zip(summaries).zip(expected)
            {
                assert_eq!(row.id, i64::from(id.get()));
                assert_eq!(row.name, name.as_str());
                assert_eq!(row.source_path.as_deref(), Some(source.as_str()));
                assert_eq!(row.object_path, object);
                assert_eq!(row.pin_path, pin);
                assert_eq!(row.map_path, map);
                assert_eq!(row.map_set, i64::from(id.get()));
                assert_eq!(row.created_at, created);
                assert_eq!(row.license.as_deref(), Some(license));
                assert_eq!(row.gpl_compatible != 0, gpl);
                assert_eq!(row.metadata.as_deref(), Some(metadata.as_str()));
                assert!(row.updated_at.is_none());
                assert_eq!(summary.id, row.id);
                assert_eq!(summary.name, row.name);
                assert_eq!(summary.kind, row.kind);
                assert_eq!(summary.metadata, metadata);

                assert_eq!(delete_program(&tx, id)?, 1);
                assert!(unload(&tx, id)?.is_none());
                assert_eq!(
                    delete_map_set(
                        &tx,
                        MapSetIdentity {
                            id,
                            pin_path: &map,
                            created_at: "another-generation"
                        }
                    )?,
                    0
                );
                assert_eq!(
                    delete_map_set(
                        &tx,
                        MapSetIdentity {
                            id,
                            pin_path: "/another/path",
                            created_at: &created
                        }
                    )?,
                    0
                );
                assert_eq!(
                    delete_map_set(
                        &tx,
                        MapSetIdentity {
                            id,
                            pin_path: &map,
                            created_at: &created
                        }
                    )?,
                    1
                );
            }

            assert!(programs(&tx)?.is_empty());
            assert_eq!(schema_version(&tx)?, crate::SCHEMA_VERSION);
            tx.commit()?;
            connection.close().map_err(|(_, error)| error)?;

            Ok(())
        },
    )??;

    Ok(())
}
