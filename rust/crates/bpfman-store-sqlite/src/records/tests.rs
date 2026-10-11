//! Decode actual SQL projections using a connection-local Go schema.

use super::*;
use crate::{ErrorKind, test_support};

type Result = std::result::Result<(), Box<dyn std::error::Error>>;

#[test]
fn timestamps_match_go_rfc3339nano_without_losing_precision() -> Result {
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
        assert_eq!(timestamp(input.into(), 7)?, expected);
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
        let mut connection = test_support::database()?;
        test_support::seed(&connection)?;
        connection.execute(update, [])?;

        assert_eq!(
            Error::from(read(&mut connection).expect_err(update)).kind(),
            ErrorKind::InvalidData
        );
    }

    let mut connection = test_support::database()?;
    test_support::seed(&connection)?;
    connection.execute_batch("PRAGMA foreign_keys=OFF; DELETE FROM map_sets;")?;

    assert!(read(&mut connection).is_err());

    Ok(())
}
