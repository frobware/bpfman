//! Real filesystem and store contracts for local tracepoint loading.
#![allow(clippy::expect_used)]

use bpfman_fs::{RuntimeDirectory, RuntimeLayout};
use bpfman_lock::AcquireOptions;
use std::{num::NonZeroU32, os::unix::fs::symlink, time::Duration};

type Result = std::result::Result<(), Box<dyn std::error::Error>>;

fn options() -> AcquireOptions<'static> {
    AcquireOptions {
        timeout: Duration::from_secs(1),
        cancelled: None,
    }
}

fn id() -> NonZeroU32 {
    NonZeroU32::new(42).expect("nonzero fixture")
}

fn runtime(
    path: &std::path::Path,
) -> std::result::Result<RuntimeDirectory, Box<dyn std::error::Error>> {
    Ok(RuntimeDirectory::open_or_create(RuntimeLayout::try_from(
        path.to_owned(),
    )?)?)
}

#[test]
fn published_bytes_and_owned_cleanup_preserve_collections_and_lock() -> Result {
    let temp = tempfile::tempdir()?;
    let root = runtime(temp.path())?;
    root.with_writer(options(), |writer| -> Result {
        let receipt = writer
            .publish_bytecode(id(), b"ELF bytes", b"provenance")
            .map_err(|f| f.cause)?;

        assert_eq!(
            std::fs::read(temp.path().join("programs/42/bytecode.o"))?,
            b"ELF bytes"
        );
        writer.remove_bytecode(receipt).map_err(|f| f.cause)?;
        assert!(!temp.path().join("programs/42").exists());
        assert!(temp.path().join("programs").is_dir());
        assert!(temp.path().join(".staging").is_dir());
        assert!(temp.path().join(".lock").is_file());

        Ok(())
    })??;

    Ok(())
}

#[test]
fn publication_collision_returns_staging_without_replacing_existing_state() -> Result {
    let temp = tempfile::tempdir()?;
    let root = runtime(temp.path())?;
    std::fs::create_dir_all(temp.path().join("programs/42"))?;
    std::fs::write(temp.path().join("programs/42/sentinel"), b"keep")?;
    root.with_writer(options(), |writer| -> Result {
        let failure = writer
            .publish_bytecode(id(), b"ELF", b"{}")
            .expect_err("collision");

        assert_eq!(failure.remaining.len(), 1);

        for receipt in failure.remaining {
            writer.remove_bytecode(receipt).map_err(|f| f.cause)?;
        }

        assert_eq!(
            std::fs::read(temp.path().join("programs/42/sentinel"))?,
            b"keep"
        );
        assert_eq!(std::fs::read_dir(temp.path().join(".staging"))?.count(), 0);

        Ok(())
    })??;

    Ok(())
}

#[test]
fn wrong_runtime_refuses_then_original_writer_can_retry() -> Result {
    let temp = tempfile::tempdir()?;
    let a = runtime(&temp.path().join("a"))?;
    let b = runtime(&temp.path().join("b"))?;
    let receipt = a
        .with_writer(options(), |w| w.publish_bytecode(id(), b"ELF", b"{}"))?
        .map_err(|f| f.cause)?;
    let failure = b
        .with_writer(options(), |w| w.remove_bytecode(receipt))?
        .expect_err("wrong root");

    assert!(temp.path().join("a/programs/42/bytecode.o").is_file());
    a.with_writer(options(), |w| w.remove_bytecode(failure.remaining))?
        .map_err(|f| f.cause)?;

    Ok(())
}

#[test]
fn replaced_child_is_preserved_other_child_is_cleaned_and_retry_works() -> Result {
    let temp = tempfile::tempdir()?;
    let root = runtime(temp.path())?;
    root.with_writer(options(), |writer| -> Result {
        let receipt = writer
            .publish_bytecode(id(), b"owned", b"{}")
            .map_err(|f| f.cause)?;
        let path = temp.path().join("programs/42/bytecode.o");
        let moved = temp.path().join("saved");
        std::fs::rename(&path, &moved)?;
        std::fs::write(&path, b"unrelated")?;
        let failure = writer
            .remove_bytecode(receipt)
            .expect_err("inode replacement");

        assert_eq!(std::fs::read(&path)?, b"unrelated");
        assert!(!temp.path().join("programs/42/provenance.json").exists());
        std::fs::rename(&path, temp.path().join("unrelated"))?;
        std::fs::rename(&moved, &path)?;
        writer
            .remove_bytecode(failure.remaining)
            .map_err(|f| f.cause)?;

        Ok(())
    })??;

    Ok(())
}

#[test]
fn moved_parent_and_symlinked_collection_cannot_redirect_cleanup() -> Result {
    let temp = tempfile::tempdir()?;
    let root_path = temp.path().join("runtime");
    let root = runtime(&root_path)?;
    root.with_writer(options(), |writer| -> Result {
        let receipt = writer
            .publish_bytecode(id(), b"keep", b"{}")
            .map_err(|f| f.cause)?;
        let moved = temp.path().join("outside");
        std::fs::rename(root_path.join("programs"), &moved)?;
        symlink(&moved, root_path.join("programs"))?;
        let failure = writer
            .remove_bytecode(receipt)
            .expect_err("moved collection");

        assert_eq!(std::fs::read(moved.join("42/bytecode.o"))?, b"keep");
        std::fs::rename(root_path.join("programs"), temp.path().join("saved-link"))?;
        std::fs::rename(&moved, root_path.join("programs"))?;
        writer
            .remove_bytecode(failure.remaining)
            .map_err(|f| f.cause)?;

        Ok(())
    })??;

    Ok(())
}

#[test]
fn unexpected_children_prevent_directory_removal_without_recursive_deletion() -> Result {
    let temp = tempfile::tempdir()?;
    let root = runtime(temp.path())?;
    root.with_writer(options(), |writer| -> Result {
        let receipt = writer
            .publish_bytecode(id(), b"owned", b"{}")
            .map_err(|f| f.cause)?;
        let unexpected = temp.path().join("programs/42/unrelated");
        std::fs::write(&unexpected, b"keep")?;
        let failure = writer
            .remove_bytecode(receipt)
            .expect_err("directory not empty");

        assert_eq!(std::fs::read(&unexpected)?, b"keep");
        assert!(!temp.path().join("programs/42/bytecode.o").exists());
        std::fs::rename(&unexpected, temp.path().join("unrelated"))?;
        writer
            .remove_bytecode(failure.remaining)
            .map_err(|f| f.cause)?;

        Ok(())
    })??;

    Ok(())
}

#[test]
fn symlinked_staging_is_rejected_before_publication() -> Result {
    let temp = tempfile::tempdir()?;
    let root = runtime(&temp.path().join("runtime"))?;
    let outside = temp.path().join("outside");
    std::fs::create_dir(&outside)?;
    symlink(&outside, temp.path().join("runtime/.staging"))?;
    root.with_writer(options(), |writer| {
        let failure = writer
            .publish_bytecode(id(), b"ELF", b"{}")
            .expect_err("symlink staging");

        assert!(failure.remaining.is_empty());
    })?;

    assert_eq!(std::fs::read_dir(outside)?.count(), 0);

    Ok(())
}
