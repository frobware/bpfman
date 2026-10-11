//! Connection-local fixtures for SQL and decoding unit tests.

use rusqlite::Connection;

#[path = "../../../tests/support/schema.rs"]
mod schema;

pub(super) fn database() -> rusqlite::Result<Connection> {
    let mut connection = Connection::open_in_memory()?;
    schema::initialize(&mut connection)?;
    Ok(connection)
}

pub(super) fn seed(connection: &Connection) -> rusqlite::Result<()> {
    schema::seed(connection)
}
