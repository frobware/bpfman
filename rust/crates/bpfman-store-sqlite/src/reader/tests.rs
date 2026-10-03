#![allow(clippy::expect_used)]

use super::*;
use std::{sync::mpsc, time::Duration};

#[test]
fn sequential_reads_reuse_the_initial_connection_including_after_an_error() {
    let connection = Connection::open_in_memory().expect("connection");
    connection
        .execute_batch("CREATE TABLE marker (value); INSERT INTO marker VALUES (7)")
        .expect("connection-local state");
    let temp = tempfile::tempdir().expect("tempdir");
    let absent = temp.path().join("must-not-open.db");
    let reader = Reader::new(&absent, connection);

    for _ in 0..3 {
        let value: i64 = reader
            .read(|connection| {
                Ok(connection.query_row("SELECT value FROM marker", [], |row| row.get(0))?)
            })
            .expect("original connection reused");
        assert_eq!(value, 7);
    }

    let failed = reader.read(|connection| {
        let tx = connection.transaction()?;
        tx.execute_batch("SELECT * FROM absent_table")?;
        Ok(())
    });
    assert!(failed.is_err());

    let value: i64 = reader
        .read(|connection| {
            assert!(
                connection.is_autocommit(),
                "failed transaction was released"
            );
            Ok(connection.query_row("SELECT value FROM marker", [], |row| row.get(0))?)
        })
        .expect("connection remains reusable");

    assert_eq!(value, 7);
    assert!(!absent.exists());
}

#[test]
fn overlapping_reads_have_independent_connections_without_waiting_for_each_other() {
    let temp = tempfile::tempdir().expect("tempdir");
    let path = temp.path().join("store.db");
    let setup = Connection::open(&path).expect("database");
    setup
        .execute_batch("CREATE TABLE marker (value); INSERT INTO marker VALUES (7)")
        .expect("data");
    drop(setup);
    let reader = Reader::new(&path, open::connection(&path).expect("initial connection"));
    let budget = Duration::from_secs(5);

    std::thread::scope(|scope| {
        let mut workers = Vec::new();
        for _ in 0..2 {
            let (started, ready) = mpsc::channel();
            let (release, released) = mpsc::channel();
            let reader = &reader;
            let worker = scope.spawn(move || {
                reader.read(|connection| {
                    let tx = connection.transaction()?;
                    let value: i64 =
                        tx.query_row("SELECT value FROM marker", [], |row| row.get(0))?;
                    started.send(()).expect("entered read transaction");
                    released.recv_timeout(budget).expect("release read");
                    Ok(value)
                })
            });
            workers.push((ready, release, worker));
        }

        let entered: Vec<_> = workers
            .iter()
            .map(|(ready, _, _)| ready.recv_timeout(budget))
            .collect();
        for (_, release, _) in &workers {
            let _ = release.send(());
        }
        for result in entered {
            result.expect("both transactions entered before either was released");
        }
        for (_, _, worker) in workers {
            assert_eq!(worker.join().expect("worker").expect("read"), 7);
        }
    });
}
