//! Full observations decoded from the unchanged Go schema.

use bpfman_model::{ImagePullPolicy, ProgramSource, ProgramSpec};
use bpfman_store_sqlite::{ErrorKind, Store};

#[path = "../../../tests/support/mod.rs"]
mod support;

type Result = std::result::Result<(), Box<dyn std::error::Error>>;

#[test]
fn full_records_preserve_nullable_values_and_payloads_without_writing() -> Result {
    let db = support::database()?;
    support::seed(&db)?;
    db.connection.execute_batch(
        r#"
        UPDATE managed_programs SET source_path='original.o', owner='alice',
            description='test program', license='GPL', gpl_compatible=1,
            global_data='{"nil":null,"empty":"","bytes":"AAH/"}',
            created_at='2026-07-07T12:00:00.139163360Z',
            updated_at='0001-01-01T00:00:00Z' WHERE program_id=7;
        UPDATE managed_programs SET program_type='fentry', attach_func='do_open',
            image_source='{"url":"example.test/image","digest":"sha256:abc","pull_policy":"Never"}'
            WHERE program_id=42;
    "#,
    )?;
    let before = std::fs::read(&db.path)?;
    let records = Store::open(&db.path)?.read_records()?;

    assert_eq!(
        records.iter().map(|r| r.id.get()).collect::<Vec<_>>(),
        [7, 42]
    );

    let p = &records[0];

    assert_eq!(p.source, ProgramSource::File(Some("original.o".into())));
    assert_eq!(p.owner, "alice");
    assert_eq!(p.description, "test program");
    assert_eq!(p.license, "GPL");
    assert!(p.gpl_compatible);
    assert_eq!(p.created_at, "2026-07-07T12:00:00.13916336Z");
    assert_eq!(p.updated_at.as_deref(), Some("0001-01-01T00:00:00Z"));
    assert_eq!(p.globals.get("nil"), Some(&None));
    assert_eq!(p.globals.get("empty"), Some(&Some(Vec::new())));
    assert_eq!(p.globals.get("bytes"), Some(&Some(vec![0, 1, 255])));
    assert_eq!(p.map_set.get(), 42);
    assert_eq!(p.map_path, "/run/bpfman/fs/maps/42");
    assert!(p.links.is_empty());

    let p = &records[1];

    assert!(matches!(&p.spec, ProgramSpec::Fentry{target,..} if target.as_str()=="do_open"));
    assert_eq!(
        p.source,
        ProgramSource::Image {
            url: "example.test/image".into(),
            digest: "sha256:abc".into(),
            pull_policy: ImagePullPolicy::Never,
        }
    );
    assert_eq!(p.links.iter().map(|l| l.get()).collect::<Vec<_>>(), [2, 10]);
    assert!(p.updated_at.is_none());
    assert!(p.globals.is_empty());
    assert_eq!(std::fs::read(&db.path)?, before);
    assert_eq!(std::fs::read_dir(db.runtime.join("db"))?.count(), 1);

    Ok(())
}

#[test]
fn timestamps_match_go_rfc3339nano_without_losing_precision() -> Result {
    let db = support::database()?;
    support::seed(&db)?;

    for (input, expected) in [
        ("2026-07-07T12:00:00.000000000Z", "2026-07-07T12:00:00Z"),
        (
            "2026-07-07T12:00:00.000000001Z",
            "2026-07-07T12:00:00.000000001Z",
        ),
        (
            "2026-07-07T12:00:00.120000000+01:00",
            "2026-07-07T12:00:00.12+01:00",
        ),
        ("2026-07-07T12:00:00+00:00", "2026-07-07T12:00:00Z"),
    ] {
        db.connection.execute(
            "UPDATE managed_programs SET created_at=? WHERE program_id=7",
            [input],
        )?;

        assert_eq!(
            Store::open(&db.path)?.read_records()?[0].created_at,
            expected
        );
    }

    Ok(())
}

#[test]
fn malformed_full_records_fail_instead_of_being_omitted() -> Result {
    for update in [
        "UPDATE managed_programs SET created_at='broken' WHERE program_id=7",
        "UPDATE managed_programs SET updated_at='broken' WHERE program_id=7",
        "UPDATE managed_programs SET global_data='{\"x\":\"not base64!\"}' WHERE program_id=7",
        "UPDATE managed_programs SET global_data='[]' WHERE program_id=7",
        "UPDATE managed_programs SET image_source='{\"url\":\"image\",\"pull_policy\":\"Sometimes\"}' WHERE program_id=7",
        "UPDATE managed_programs SET program_type='fentry', attach_func=NULL WHERE program_id=7",
        "UPDATE managed_programs SET program_id=0 WHERE program_id=7",
    ] {
        let db = support::database()?;
        support::seed(&db)?;
        db.connection.execute(update, [])?;

        assert_eq!(
            Store::open(&db.path)?
                .read_records()
                .expect_err(update)
                .kind(),
            ErrorKind::InvalidData
        );
    }

    let db = support::database()?;
    support::seed(&db)?;
    db.connection
        .execute_batch("PRAGMA foreign_keys=OFF; DELETE FROM map_sets;")?;

    assert!(Store::open(&db.path)?.read_records().is_err());

    Ok(())
}

#[test]
fn full_reads_recheck_the_schema_on_the_open_handle() -> Result {
    let db = support::database()?;
    let mut store = Store::open(&db.path)?;
    db.connection
        .execute("UPDATE goose_db_version SET version_id=3", [])?;

    assert_eq!(
        store.read_records().expect_err("changed schema").kind(),
        ErrorKind::IncompatibleSchema
    );

    Ok(())
}
