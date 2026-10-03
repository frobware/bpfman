use super::*;
use crate::{RuntimeDirectory, RuntimeLayout};
use bpfman_lock::AcquireOptions;
use std::{fs, os::unix::fs::symlink, time::Duration};

fn options() -> AcquireOptions<'static> {
    AcquireOptions {
        timeout: Duration::from_secs(1),
        cancelled: None,
    }
}

#[test]
fn failed_publication_preserves_state_and_rejects_stale_observations()
-> Result<(), Box<dyn std::error::Error>> {
    let temporary = tempfile::tempdir()?;
    let runtime =
        RuntimeDirectory::open_or_create(RuntimeLayout::try_from(temporary.path().to_owned())?)?;

    runtime.with_writer(
        options(),
        |writer| -> Result<(), Box<dyn std::error::Error>> {
            let snapshot = writer.open_store_snapshot()?;
            writer.publish_store_snapshot(&snapshot, None, b"old")?;
            let outside = temporary.path().join("outside");
            fs::write(&outside, b"sentinel")?;
            let pending = temporary.path().join("db").join(PENDING);
            symlink(&outside, &pending)?;

            assert!(
                writer
                    .publish_store_snapshot(&snapshot, Some(b"old"), b"new")
                    .is_err()
            );
            assert_eq!(snapshot.read()?, Some(b"old".to_vec()));
            assert_eq!(fs::read(&outside)?, b"sentinel");

            fs::rename(&pending, temporary.path().join("saved-symlink"))?;
            fs::write(&pending, b"interrupted staging bytes")?;
            writer.publish_store_snapshot(&snapshot, Some(b"old"), b"new")?;
            assert_eq!(snapshot.read()?, Some(b"new".to_vec()));
            assert!(!pending.exists());
            assert!(
                writer
                    .publish_store_snapshot(&snapshot, Some(b"old"), b"stale")
                    .is_err()
            );
            assert_eq!(snapshot.read()?, Some(b"new".to_vec()));
            Ok(())
        },
    )??;

    Ok(())
}

#[test]
fn snapshot_authority_is_root_bound_and_refuses_special_files()
-> Result<(), Box<dyn std::error::Error>> {
    let temporary = tempfile::tempdir()?;
    let layout = RuntimeLayout::try_from(temporary.path().join("one"))?;
    let one = RuntimeDirectory::open_or_create(layout.clone())?;
    let two =
        RuntimeDirectory::open_or_create(RuntimeLayout::try_from(temporary.path().join("two"))?)?;
    let snapshot = one.with_writer(options(), |writer| writer.open_store_snapshot())??;

    two.with_writer(options(), |writer| {
        assert!(
            writer
                .publish_store_snapshot(&snapshot, None, b"wrong root")
                .is_err()
        );
    })?;
    assert_eq!(snapshot.read()?, None);

    let path = layout.database_path();
    let outside = temporary.path().join("outside");
    fs::write(&outside, b"sentinel")?;
    symlink(&outside, &path)?;
    assert!(snapshot.read().is_err());
    fs::rename(&path, temporary.path().join("saved-link"))?;
    fs::hard_link(&outside, &path)?;
    assert!(snapshot.read().is_err());
    fs::rename(&path, temporary.path().join("saved-hardlink"))?;
    fs::create_dir(&path)?;
    assert!(snapshot.read().is_err());
    assert_eq!(fs::read(&outside)?, b"sentinel");

    Ok(())
}

#[test]
fn replacement_of_store_directory_cannot_redirect_publication()
-> Result<(), Box<dyn std::error::Error>> {
    let temporary = tempfile::tempdir()?;
    let runtime =
        RuntimeDirectory::open_or_create(RuntimeLayout::try_from(temporary.path().to_owned())?)?;

    runtime.with_writer(
        options(),
        |writer| -> Result<(), Box<dyn std::error::Error>> {
            let snapshot = writer.open_store_snapshot()?;
            writer.publish_store_snapshot(&snapshot, None, b"old")?;
            fs::rename(
                temporary.path().join("db"),
                temporary.path().join("original"),
            )?;
            fs::create_dir(temporary.path().join("db"))?;

            assert!(
                writer
                    .publish_store_snapshot(&snapshot, Some(b"old"), b"new")
                    .is_err()
            );
            assert_eq!(snapshot.read()?, Some(b"old".to_vec()));
            assert!(!temporary.path().join("db/store.db").exists());
            Ok(())
        },
    )??;

    Ok(())
}
