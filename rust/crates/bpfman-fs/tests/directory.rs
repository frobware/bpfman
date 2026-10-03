//! Runtime capabilities validate filesystem state before lending authority.

use std::{cell::Cell, time::Duration};

use bpfman_fs::{ErrorKind, RuntimeDirectory, RuntimeLayout};
use bpfman_lock::AcquireOptions;

type Result<T = ()> = std::result::Result<T, Box<dyn std::error::Error>>;

fn options() -> AcquireOptions<'static> {
    AcquireOptions {
        timeout: Duration::from_millis(80),
        cancelled: None,
    }
}

fn assert_refused(runtime: &RuntimeDirectory) -> Result {
    let called = Cell::new(false);
    let error = runtime
        .with_writer(options(), |_| called.set(true))
        .err()
        .ok_or("unsafe layout lent writer authority")?;

    assert_eq!(error.kind(), ErrorKind::UnsafeLayout);
    assert!(!called.get());

    Ok(())
}

#[test]
fn read_only_open_never_creates_runtime_state() -> Result {
    let temporary = tempfile::tempdir()?;
    let layout = RuntimeLayout::try_from(temporary.path().join("absent/runtime"))?;

    assert!(RuntimeDirectory::open_existing(layout.clone())?.is_none());
    assert!(!temporary.path().join("absent").exists());

    std::fs::create_dir_all(layout.root())?;
    let runtime = RuntimeDirectory::open_existing(layout.clone())?.ok_or("runtime missing")?;

    assert!(runtime.open_store_snapshot()?.is_none());
    assert_eq!(std::fs::read_dir(layout.root())?.count(), 0);

    std::os::unix::fs::symlink(temporary.path(), layout.root().join("db"))?;
    assert!(runtime.open_store_snapshot().is_err());

    let alias = temporary.path().join("alias");
    std::os::unix::fs::symlink(layout.root(), &alias)?;
    assert!(RuntimeDirectory::open_existing(RuntimeLayout::try_from(alias)?).is_err());

    Ok(())
}

#[test]
fn constructing_a_directory_does_not_acquire_or_create_the_lock() -> Result {
    let temporary = tempfile::tempdir()?;
    let layout = RuntimeLayout::try_from(temporary.path().join("nested/runtime"))?;
    let runtime = RuntimeDirectory::open_or_create(layout.clone())?;

    assert!(layout.root().is_dir());
    assert!(!layout.lock_path().exists());
    assert!(
        !layout
            .database_path()
            .parent()
            .ok_or("database parent")?
            .exists()
    );

    // An independent pathname-based contender can acquire the same lock now.
    bpfman_lock::with_write_lock(&layout.lock_path(), options(), |_| ())?;
    runtime.with_writer(options(), |writer| {
        assert_eq!(writer.database_path(), layout.database_path())
    })?;

    Ok(())
}

#[test]
fn rejects_symlinked_roots_and_ancestors_without_touching_the_target() -> Result {
    use std::os::unix::fs::symlink;
    let temporary = tempfile::tempdir()?;
    let outside = temporary.path().join("outside");
    std::fs::create_dir(&outside)?;
    std::fs::write(outside.join("sentinel"), b"preserve me")?;
    let alias = temporary.path().join("alias");
    symlink(&outside, &alias)?;

    for root in [alias.clone(), alias.join("missing/runtime")] {
        let error = RuntimeDirectory::open_or_create(RuntimeLayout::try_from(root)?)
            .err()
            .ok_or("accepted a symlink")?;

        assert_eq!(error.kind(), ErrorKind::UnsafeLayout);
    }

    assert_eq!(std::fs::read_dir(&outside)?.count(), 1);
    assert_eq!(std::fs::read(outside.join("sentinel"))?, b"preserve me");

    // Refuse a symlink to / at traversal, without issuing any mutation there.

    let filesystem_alias = temporary.path().join("filesystem-root");
    symlink("/", &filesystem_alias)?;

    assert!(RuntimeDirectory::open_or_create(RuntimeLayout::try_from(filesystem_alias)?).is_err());

    Ok(())
}

#[test]
fn lock_and_database_symlinks_are_rejected_after_adoption_and_after_use() -> Result {
    use std::os::unix::fs::symlink;

    for replacing_existing in [false, true] {
        for replace_lock in [false, true] {
            let temporary = tempfile::tempdir()?;
            let outside = temporary.path().join("outside");
            std::fs::create_dir(&outside)?;
            let sentinel = outside.join("sentinel");
            std::fs::write(&sentinel, b"preserve me")?;
            let layout = RuntimeLayout::try_from(temporary.path().join("runtime"))?;
            let runtime = RuntimeDirectory::open_or_create(layout.clone())?;

            if replacing_existing {
                runtime.with_writer(options(), |_| ())?;
            }

            let path = if replace_lock {
                layout.lock_path()
            } else {
                layout
                    .database_path()
                    .parent()
                    .ok_or("database parent")?
                    .to_owned()
            };

            if replacing_existing {
                std::fs::rename(&path, temporary.path().join("original"))?;
            }

            symlink(if replace_lock { &sentinel } else { &outside }, &path)?;
            assert_refused(&runtime)?;

            assert!(std::fs::symlink_metadata(path)?.file_type().is_symlink());
            assert_eq!(std::fs::read(&sentinel)?, b"preserve me");
            assert_eq!(std::fs::read_dir(&outside)?.count(), 1);
        }
    }

    Ok(())
}

#[test]
fn refuses_hard_linked_and_directory_lock_entries() -> Result {
    for hard_link in [false, true] {
        let temporary = tempfile::tempdir()?;
        let layout = RuntimeLayout::try_from(temporary.path().join("runtime"))?;
        let runtime = RuntimeDirectory::open_or_create(layout.clone())?;
        let sentinel = temporary.path().join("sentinel");
        std::fs::write(&sentinel, b"preserve me")?;

        if hard_link {
            std::fs::hard_link(&sentinel, layout.lock_path())?;
        } else {
            std::fs::create_dir(layout.lock_path())?;
        }

        assert_refused(&runtime)?;

        assert_eq!(std::fs::read(sentinel)?, b"preserve me");
        assert!(
            !layout
                .database_path()
                .parent()
                .ok_or("database parent")?
                .exists()
        );
    }

    Ok(())
}

#[test]
fn writer_authorities_remain_bound_to_their_own_runtimes() -> Result {
    let temporary = tempfile::tempdir()?;
    let a = RuntimeLayout::try_from(temporary.path().join("a"))?;
    let b = RuntimeLayout::try_from(temporary.path().join("b"))?;
    let first = RuntimeDirectory::open_or_create(a.clone())?;
    let second = RuntimeDirectory::open_or_create(b.clone())?;
    first.with_writer(options(), |writer_a| {
        second.with_writer(options(), |writer_b| {
            assert_eq!(writer_a.database_path(), a.database_path());
            assert_eq!(writer_b.database_path(), b.database_path());
            assert_ne!(writer_a.database_path(), writer_b.database_path());
        })
    })??;

    Ok(())
}

#[test]
fn callback_errors_release_authority_and_cancellation_never_lends_it() -> Result {
    let temporary = tempfile::tempdir()?;
    let layout = RuntimeLayout::try_from(temporary.path().join("runtime"))?;
    let runtime = RuntimeDirectory::open_or_create(layout.clone())?;
    let cancelled = std::sync::atomic::AtomicBool::new(true);
    let called = Cell::new(false);
    let error = runtime
        .with_writer(
            AcquireOptions {
                timeout: Duration::ZERO,
                cancelled: Some(&cancelled),
            },
            |_| called.set(true),
        )
        .expect_err("cancelled");

    assert_eq!(error.kind(), ErrorKind::Cancelled);
    assert!(!called.get());
    assert!(
        !layout
            .database_path()
            .parent()
            .ok_or("database parent")?
            .exists()
    );

    let failure = runtime.with_writer(options(), |_| Err::<(), _>("work failed"))?;

    assert_eq!(failure, Err("work failed"));
    runtime.with_writer(options(), |_| ())?;

    Ok(())
}

#[test]
fn pathname_lock_peer() -> Result {
    let Some(path) = std::env::var_os("BPFMAN_FS_TEST_LOCK") else {
        return Ok(());
    };
    let result = bpfman_lock::with_write_lock(std::path::Path::new(&path), options(), |_| ());

    if std::env::var("BPFMAN_FS_TEST_LOCK_BUSY")? == "yes" {
        assert_eq!(
            result.expect_err("parent holds lock").kind(),
            bpfman_lock::ErrorKind::TimedOut
        );
    } else {
        result?;
    }

    Ok(())
}

#[test]
fn writer_contends_with_go_compatible_pathname_lock_in_another_process() -> Result {
    let temporary = tempfile::tempdir()?;
    let layout = RuntimeLayout::try_from(temporary.path().join("runtime"))?;
    let runtime = RuntimeDirectory::open_or_create(layout.clone())?;
    let peer = |busy| -> Result {
        let output = std::process::Command::new(std::env::current_exe()?)
            .args(["--exact", "pathname_lock_peer", "--nocapture"])
            .env("BPFMAN_FS_TEST_LOCK", layout.lock_path())
            .env("BPFMAN_FS_TEST_LOCK_BUSY", if busy { "yes" } else { "no" })
            .output()?;

        assert!(
            output.status.success(),
            "stdout={} stderr={}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );

        Ok(())
    };
    runtime.with_writer(options(), |_| peer(true))??;
    peer(false)
}

#[test]
fn invalid_database_parent_is_rejected_before_lending_writer() -> Result {
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
