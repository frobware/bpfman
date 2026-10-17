//! Cache publication and cleanup exercise real descriptor confinement.
use bpfman_fs::{ImageCacheKey, RuntimeDirectory, RuntimeLayout};
use bpfman_lock::AcquireOptions;
use std::{fs, io::Read, time::Duration};

type Result = std::result::Result<(), Box<dyn std::error::Error>>;
fn options() -> AcquireOptions<'static> {
    AcquireOptions {
        timeout: Duration::ZERO,
        cancelled: None,
    }
}

#[test]
fn complete_generations_remain_readable_across_atomic_replacement() -> Result {
    let temp = tempfile::tempdir()?;
    let root =
        RuntimeDirectory::open_or_create(RuntimeLayout::try_from(temp.path().join("cache"))?)?;
    let key = ImageCacheKey::new([1; 32]);
    root.with_cache_writer(options(), |writer| {
        let stage = writer.stage_image_cache(key, b"old").expect("stage");
        assert_eq!(root.read_image_cache(key).expect("read"), None);
        writer
            .publish_image_cache(stage)
            .map_err(|failure| failure.cause)
            .expect("publish");
        let file = fs::read_dir(root.layout().root().join("images-v1"))
            .expect("entries")
            .next()
            .expect("entry")
            .expect("entry")
            .path();
        let mut retained = fs::File::open(file).expect("retained reader");
        let stage = writer.stage_image_cache(key, b"new").expect("new stage");
        assert_eq!(
            root.read_image_cache(key).expect("read").expect("old"),
            b"old"
        );
        writer
            .publish_image_cache(stage)
            .map_err(|failure| failure.cause)
            .expect("replace");
        assert_eq!(
            root.read_image_cache(key).expect("read").expect("new"),
            b"new"
        );
        let mut bytes = Vec::new();
        retained.read_to_end(&mut bytes).expect("old reader");
        assert_eq!(bytes, b"old");
    })?;
    assert!(!root.layout().database_path().exists());
    assert_eq!(
        fs::read_dir(root.layout().root().join("images-v1"))?.count(),
        1
    );
    Ok(())
}

#[test]
fn replaced_stage_and_parent_retain_cleanup_authority_for_explicit_retry() -> Result {
    let temp = tempfile::tempdir()?;
    let root =
        RuntimeDirectory::open_or_create(RuntimeLayout::try_from(temp.path().join("cache"))?)?;
    let foreign =
        RuntimeDirectory::open_or_create(RuntimeLayout::try_from(temp.path().join("foreign"))?)?;
    root.with_cache_writer(options(), |writer| {
        let key = ImageCacheKey::new([2; 32]);
        let stage = writer.stage_image_cache(key, b"complete").expect("stage");
        let stage = foreign
            .with_cache_writer(options(), |other| other.remove_image_cache_stage(stage))
            .expect("writer")
            .expect_err("wrong root")
            .remaining;
        let collection = root.layout().root().join("images-v1");
        let saved = root.layout().root().join("saved");
        fs::rename(&collection, &saved).expect("move parent");
        fs::create_dir(&collection).expect("replace parent");
        let failure = writer
            .publish_image_cache(stage)
            .expect_err("parent changed");
        let stage = failure.remaining.expect("retained stage");
        let stage = writer
            .remove_image_cache_stage(stage)
            .expect_err("cleanup refused")
            .remaining;
        // Preserve the foreign empty replacement; restore the retained parent.
        fs::rename(&collection, root.layout().root().join("foreign-parent")).expect("save foreign");
        fs::rename(&saved, &collection).expect("restore parent");
        writer
            .remove_image_cache_stage(stage)
            .map_err(|failure| failure.cause)
            .expect("retry");
        assert_eq!(fs::read_dir(collection).expect("entries").count(), 0);
    })?;
    Ok(())
}

#[test]
fn symlink_and_hardlink_destinations_are_refused_without_removal() -> Result {
    let temp = tempfile::tempdir()?;
    let root =
        RuntimeDirectory::open_or_create(RuntimeLayout::try_from(temp.path().join("cache"))?)?;
    for symlink in [true, false] {
        root.with_cache_writer(options(), |writer| {
            let digest = if symlink { [3; 32] } else { [4; 32] };
            let key = ImageCacheKey::new(digest);
            let stage = writer.stage_image_cache(key, b"new").expect("stage");
            let outside = temp.path().join(if symlink {
                "symlink-target"
            } else {
                "hardlink-target"
            });
            fs::write(&outside, b"foreign").expect("foreign");
            let name: String = digest.iter().map(|b| format!("{b:02x}")).collect();
            let target = root.layout().root().join("images-v1").join(name);
            if symlink {
                std::os::unix::fs::symlink(&outside, &target).expect("symlink");
            } else {
                fs::hard_link(&outside, &target).expect("hardlink");
            }
            assert!(root.read_image_cache(key).is_err());
            let failure = writer
                .publish_image_cache(stage)
                .expect_err("unsafe target");
            writer
                .remove_image_cache_stage(failure.remaining.expect("stage"))
                .map_err(|failure| failure.cause)
                .expect("stage cleanup");
            assert_eq!(fs::read(&outside).expect("preserved"), b"foreign");
            assert!(target.symlink_metadata().is_ok());
        })?;
    }
    Ok(())
}
