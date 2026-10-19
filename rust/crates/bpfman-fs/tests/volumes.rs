//! CSI filesystem admission never follows caller or managed-directory aliases.
#![allow(clippy::expect_used)]
use bpfman_fs::{ErrorKind, RuntimeDirectory, RuntimeLayout, VolumeRequest};
use std::{fs, os::unix::fs::symlink, time::Duration};
#[test]
fn volume_target_and_backing_symlinks_are_preserved() {
    let temporary = tempfile::tempdir().expect("fixture");
    let runtime = RuntimeDirectory::open_or_create(
        RuntimeLayout::try_from(temporary.path().join("runtime")).expect("layout"),
    )
    .expect("runtime");
    let outside = temporary.path().join("outside");
    fs::create_dir(&outside).expect("outside");
    fs::write(outside.join("sentinel"), b"foreign").expect("sentinel");
    let target = temporary.path().join("target");
    symlink(&outside, &target).expect("target alias");
    let request = VolumeRequest::new("volume", &target, "map", false, None).expect("request");
    runtime
        .with_writer(
            bpfman_lock::AcquireOptions {
                timeout: Duration::from_secs(1),
                cancelled: None,
            },
            |writer| {
                assert_eq!(
                    writer
                        .volume_state(&request)
                        .expect_err("refuse target symlink")
                        .kind(),
                    ErrorKind::UnsafeLayout
                );
                assert!(writer.unpublish_volume("volume", &target).is_err());
                let clean_target = temporary.path().join("missing-target");
                symlink(&outside, runtime.layout().root().join("csi")).expect("backing alias");
                assert!(writer.unpublish_volume("volume", &clean_target).is_err());
            },
        )
        .expect("writer");
    assert_eq!(
        fs::read(outside.join("sentinel")).expect("preserved"),
        b"foreign"
    );
    assert!(
        fs::symlink_metadata(&target)
            .expect("alias")
            .file_type()
            .is_symlink()
    );
    assert_eq!(fs::read_dir(outside).expect("outside").count(), 1);
}
