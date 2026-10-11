//! Summary validation through the real queries with a connection-local schema.

use super::*;
use crate::{ErrorKind, test_support};

#[test]
fn rejects_malformed_stored_values() -> Result<(), Box<dyn std::error::Error>> {
    for update in [
        "UPDATE managed_programs SET program_type = 'bogus' WHERE program_id = 7",
        "UPDATE managed_programs SET program_id = 0 WHERE program_id = 7",
        "UPDATE managed_programs SET metadata_json = '{\"x\":2}' WHERE program_id = 7",
    ] {
        let mut connection = test_support::database()?;
        test_support::seed(&connection)?;
        connection.execute(update, [])?;

        assert_eq!(
            Error::from(read(&mut connection).expect_err("invalid stored value")).kind(),
            ErrorKind::InvalidData
        );
    }

    Ok(())
}
