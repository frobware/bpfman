use crate::{Backend, error::Failure, queries::dispatcher as queries};
use bpfman_fs::RuntimeWriter;
use bpfman_model::DispatcherKey;
use bpfman_store::{DispatcherStore, Error};
use rusqlite::{Connection, OpenFlags, TransactionBehavior};

impl DispatcherStore for Backend {
    #[tracing::instrument(name = "store.delete_dispatcher", level = "debug", skip_all, err)]
    fn delete_dispatcher(
        &self,
        writer: &RuntimeWriter<'_>,
        key: DispatcherKey,
    ) -> Result<bool, Error> {
        (|| -> Result<bool, Failure> {
            let mut connection = Connection::open_with_flags(
                writer.database_path(),
                OpenFlags::SQLITE_OPEN_READ_WRITE,
            )?;
            connection.busy_timeout(std::time::Duration::from_secs(5))?;
            connection.pragma_update(None, "foreign_keys", true)?;
            let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
            crate::open::require_supported(crate::open::schema_version(&tx)?)?;
            if !queries::delete(&tx, key)? {
                return Ok(false);
            }
            tx.commit()?;
            Ok(true)
        })()
        .map_err(crate::Error::from)
        .map_err(Into::into)
    }
}
