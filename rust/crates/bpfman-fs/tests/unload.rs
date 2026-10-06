//! Existing-artifact adoption must prove confinement before deletion authority.
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
    NonZeroU32::new(42).expect("fixture")
}

fn runtime(
    path: &std::path::Path,
) -> std::result::Result<RuntimeDirectory, Box<dyn std::error::Error>> {
    Ok(RuntimeDirectory::open_or_create(RuntimeLayout::try_from(
        path.to_owned(),
    )?)?)
}

#[test]
fn adopt_existing_bytecode_and_absence_without_creating_collections() -> Result {
    let temporary = tempfile::tempdir()?;
    let root = runtime(temporary.path())?;
    root.with_writer(options(), |writer| -> Result {
        let found = writer.observe_unload(&NoKernel, id())?;

        assert!(found.program.is_none() && found.directory.is_none() && found.bytecode.is_none());
        assert!(!temporary.path().join("fs").exists());
        assert!(!temporary.path().join("programs").exists());

        let created = writer
            .publish_bytecode(id(), b"bytes", b"provenance")
            .map_err(|f| f.cause)?;
        drop(created); // committed ownership survives receipt release
        let found = writer.observe_unload(&NoKernel, id())?;
        writer
            .remove_bytecode(found.bytecode.expect("existing bytecode"))
            .map_err(|f| f.cause)?;

        assert!(writer.observe_unload(&NoKernel, id())?.bytecode.is_none());
        assert!(temporary.path().join("programs").is_dir());
        assert!(temporary.path().join(".lock").is_file());

        Ok(())
    })??;

    Ok(())
}

#[test]
fn bytecode_adoption_refuses_symlink_ancestors_children_hardlinks_and_unexpected_names() -> Result {
    for case in [
        "ancestor",
        "directory",
        "child",
        "hardlink",
        "unknown",
        "nested",
        "fifo",
    ] {
        let temporary = tempfile::tempdir()?;
        let outside = tempfile::tempdir()?;
        let root = runtime(temporary.path())?;
        let sentinel = outside.path().join("sentinel");
        std::fs::write(&sentinel, b"keep")?;
        root.with_writer(options(), |writer| -> Result {
            let dir = temporary.path().join("programs/42");

            match case {
                "ancestor" => symlink(outside.path(), temporary.path().join("programs"))?,
                "directory" => {
                    std::fs::create_dir(temporary.path().join("programs"))?;
                    symlink(outside.path(), &dir)?;
                }
                _ => {
                    std::fs::create_dir_all(&dir)?;

                    match case {
                        "child" => symlink(&sentinel, dir.join("bytecode.o"))?,
                        "hardlink" => std::fs::hard_link(&sentinel, dir.join("bytecode.o"))?,
                        "unknown" => std::fs::write(dir.join("unrelated"), b"keep")?,
                        "nested" => std::fs::create_dir(dir.join("bytecode.o"))?,
                        "fifo" => {
                            assert!(
                                std::process::Command::new("mkfifo")
                                    .arg(dir.join("bytecode.o"))
                                    .status()?
                                    .success()
                            );
                        }
                        _ => unreachable!(),
                    }
                }
            }

            assert!(
                writer.observe_unload(&NoKernel, id()).is_err(),
                "case {case}"
            );
            assert_eq!(std::fs::read(&sentinel)?, b"keep");

            Ok(())
        })??;
    }

    Ok(())
}

#[test]
fn observed_receipts_refuse_replacement_and_wrong_root() -> Result {
    let temporary = tempfile::tempdir()?;
    let other = tempfile::tempdir()?;
    let root = runtime(temporary.path())?;
    let other_root = runtime(other.path())?;
    root.with_writer(options(), |writer| -> Result {
        drop(
            writer
                .publish_bytecode(id(), b"bytes", b"provenance")
                .map_err(|f| f.cause)?,
        );
        let receipt = writer
            .observe_unload(&NoKernel, id())?
            .bytecode
            .expect("bytecode");
        let receipt = other_root.with_writer(options(), |other_writer| {
            other_writer
                .remove_bytecode(receipt)
                .expect_err("wrong root refused")
                .remaining
        })?;
        let path = temporary.path().join("programs/42/bytecode.o");
        std::fs::rename(&path, temporary.path().join("saved-bytecode"))?;
        std::fs::write(&path, b"replacement")?;
        let failed = writer
            .remove_bytecode(receipt)
            .expect_err("replacement refused");

        assert_eq!(std::fs::read(&path)?, b"replacement");

        // Other independent owned file was still removed, but its container remains.
        assert!(
            !temporary
                .path()
                .join("programs/42/provenance.json")
                .exists()
        );
        assert!(temporary.path().join("programs/42").is_dir());
        drop(failed);

        Ok(())
    })??;

    Ok(())
}

#[test]
fn existing_fs_must_be_bpffs_and_not_a_symlink() -> Result {
    for symlinked in [false, true] {
        let temporary = tempfile::tempdir()?;
        let outside = tempfile::tempdir()?;
        let root = runtime(temporary.path())?;

        if symlinked {
            symlink(outside.path(), temporary.path().join("fs"))?;
        } else {
            std::fs::create_dir(temporary.path().join("fs"))?;
        }

        root.with_writer(options(), |writer| {
            assert!(writer.observe_unload(&NoKernel, id()).is_err());
        })?;

        assert!(outside.path().read_dir()?.next().is_none());
    }

    Ok(())
}

// These cases contain no valid bpffs pins. Confinement must reject invalid
// filesystem entries before consulting the kernel.
struct NoKernel;

impl bpfman_fs::ProgramInspection for NoKernel {
    fn program_at(
        &self,
        _: bpfman_fs::PinSource<'_>,
    ) -> bpfman_fs::KernelResult<bpfman_fs::PinnedProgram> {
        unreachable!("filesystem validation must precede kernel observation")
    }

    fn map_at(&self, _: bpfman_fs::PinSource<'_>) -> bpfman_fs::KernelResult<u32> {
        unreachable!("filesystem validation must precede kernel observation")
    }
}
