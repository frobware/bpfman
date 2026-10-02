//! Runtime capabilities validate filesystem state before lending authority.

use std::{cell::Cell, time::Duration};

use bpfman_fs::{ErrorKind, RuntimeDirectory, RuntimeLayout};
use bpfman_lock::AcquireOptions;

#[test]
fn invalid_database_parent_is_rejected_before_lending_writer()
-> Result<(), Box<dyn std::error::Error>> {
    let directory = tempfile::tempdir()?;
    let layout = RuntimeLayout::try_from(directory.path().to_owned())?;
    let database = layout.database_path();
    let parent = database.parent().ok_or("missing database parent")?;
    std::fs::write(parent, b"preserve me")?;
    let runtime = RuntimeDirectory::open_or_create(layout)?;
    let called = Cell::new(false);
    let error = runtime
        .with_writer(
            AcquireOptions {
                timeout: Duration::from_secs(1),
                cancelled: None,
            },
            |_| called.set(true),
        )
        .expect_err("invalid database parent");
    assert_eq!(error.kind(), ErrorKind::UnsafeLayout);
    assert!(!called.get());
    assert_eq!(std::fs::read(parent)?, b"preserve me");
    Ok(())
}
