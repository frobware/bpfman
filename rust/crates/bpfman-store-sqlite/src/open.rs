use std::{io, path::Path};

use rusqlite::{Connection, OpenFlags};

use crate::{Error, SCHEMA_VERSION, Store, error::Failure};

impl Store {
    /// Observe an existing store without creating or migrating it.
    ///
    /// `None` means the path was absent. Existing empty/corrupt databases,
    /// dangling symlinks, and permission errors are failures, not absence.
    /// Call under the writer lock when this observation drives opening decisions.
    pub fn inspect(path: &Path) -> Result<Option<Self>, Error> {
        match std::fs::symlink_metadata(path) {
            Ok(_) => Self::open_observed(path).map(Some).map_err(Error::from),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(source) => Err(Failure::Filesystem {
                path: path.to_owned(),
                source,
            }
            .into()),
        }
    }

    /// Open a required existing store and validate the supported schema.
    ///
    /// Never creates files or applies migrations. Runtime uses this to
    /// check the postcondition after creation while still under the lock.
    pub fn open(path: &Path) -> Result<Self, Error> {
        let store = Self::open_observed(path)?;
        require_supported(store.schema_version)?;

        Ok(store)
    }

    /// Schema version observed when this handle was opened.
    ///
    /// It is an observation, not a guarantee against later external changes.
    pub fn schema_version(&self) -> i64 {
        self.schema_version
    }

    fn open_observed(path: &Path) -> Result<Self, Failure> {
        let connection = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        connection.busy_timeout(std::time::Duration::from_secs(5))?;
        let schema_version = schema_version(&connection)?;

        Ok(Self {
            connection,
            schema_version,
        })
    }
}

pub(super) fn schema_version(connection: &Connection) -> Result<i64, Failure> {
    connection
        .query_row("SELECT MAX(version_id) FROM goose_db_version", [], |row| {
            row.get(0)
        })
        .map_err(Failure::SchemaVersion)
}

pub(super) fn require_supported(found: i64) -> Result<(), Failure> {
    if found != SCHEMA_VERSION {
        return Err(Failure::UnsupportedSchema { found });
    }

    Ok(())
}
