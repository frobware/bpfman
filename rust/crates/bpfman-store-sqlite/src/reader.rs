//! Reuse one idle connection without holding a mutex during a read transaction.

use crate::{error::Failure, open};
use rusqlite::Connection;
use std::{
    path::{Path, PathBuf},
    sync::Mutex,
};

#[derive(Debug)]
pub(super) struct Reader {
    path: PathBuf,
    idle: Mutex<Option<Connection>>,
}

impl Reader {
    pub(super) fn new(path: &Path, connection: Connection) -> Self {
        Self {
            path: path.to_owned(),
            idle: Mutex::new(Some(connection)),
        }
    }

    pub(super) fn read<T>(
        &self,
        read: impl FnOnce(&mut Connection) -> Result<T, Failure>,
    ) -> Result<T, Failure> {
        // Poison recovery is safe: the mutex protects only an optional idle
        // connection. No queries or caller code execute while it is held.
        let connection = self.idle.lock().unwrap_or_else(|e| e.into_inner()).take();
        let reused = connection.is_some();
        let mut connection = match connection {
            Some(connection) => connection,
            None => open::connection(&self.path)?,
        };
        tracing::debug!(reused, "reader acquired");

        let result = read(&mut connection);

        // Transactions and statements are dropped before returning the connection.
        // Retain at most one idle connection; concurrent excess readers close here.
        let mut idle = self.idle.lock().unwrap_or_else(|e| e.into_inner());
        if idle.is_none() {
            *idle = Some(connection);
        }

        result
    }
}

#[cfg(test)]
mod tests;
