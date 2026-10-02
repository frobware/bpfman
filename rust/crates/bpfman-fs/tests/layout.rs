//! Layout contracts are independent of runtime setup and kernel privileges.

use std::path::{Path, PathBuf};

use bpfman_fs::{DEFAULT_RUNTIME_ROOT, LayoutError, RuntimeLayout};

#[test]
fn rejects_empty_and_relative_roots() {
    assert_eq!(
        RuntimeLayout::try_from(PathBuf::new()),
        Err(LayoutError::EmptyRoot)
    );
    for root in [".", "..", "runtime", "./runtime", "../runtime"] {
        let path = PathBuf::from(root);
        assert_eq!(
            RuntimeLayout::try_from(path.clone()),
            Err(LayoutError::RelativeRoot(path))
        );
    }
}

#[test]
fn owns_the_go_compatible_path_contract() -> Result<(), LayoutError> {
    let default = RuntimeLayout::try_from(PathBuf::from(DEFAULT_RUNTIME_ROOT))?;
    assert_eq!(default.root(), Path::new("/run/bpfman"));
    assert_eq!(default.lock_path(), Path::new("/run/bpfman/.lock"));
    assert_eq!(
        default.database_path(),
        Path::new("/run/bpfman/db/store.db")
    );
    let custom = RuntimeLayout::try_from(PathBuf::from("/tmp/custom"))?;
    assert_eq!(custom.root(), Path::new("/tmp/custom"));
    assert_eq!(custom.lock_path(), Path::new("/tmp/custom/.lock"));
    assert_eq!(custom.database_path(), Path::new("/tmp/custom/db/store.db"));
    Ok(())
}

#[test]
fn normalises_lexically_without_escaping_the_root() -> Result<(), LayoutError> {
    for (input, expected) in [
        ("/run//bpfman/./", "/run/bpfman"),
        ("/tmp/../run/bpfman", "/run/bpfman"),
        ("/../../runtime", "/runtime"),
    ] {
        let layout = RuntimeLayout::try_from(PathBuf::from(input))?;
        assert_eq!(layout.root(), Path::new(expected));
    }
    Ok(())
}

#[test]
fn refuses_filesystem_root_and_lexical_aliases_without_io() {
    for path in [
        "/",
        "//",
        "/./",
        "/..",
        "/run/..",
        "/run/../../",
        "/one/two/../..",
    ] {
        assert_eq!(
            RuntimeLayout::try_from(PathBuf::from(path)),
            Err(LayoutError::FilesystemRoot)
        );
    }
}

#[test]
fn construction_does_not_create_or_require_directories() -> Result<(), Box<dyn std::error::Error>> {
    let directory = tempfile::tempdir()?;
    let root = directory.path().join("does-not-exist/runtime");
    let layout = RuntimeLayout::try_from(root.clone())?;
    assert_eq!(layout.root(), root);
    assert!(!layout.root().exists());
    assert!(!layout.lock_path().exists());
    assert!(!layout.database_path().exists());
    assert_eq!(std::fs::read_dir(directory.path())?.count(), 0);
    Ok(())
}

#[test]
fn preserves_non_utf8_paths() -> Result<(), LayoutError> {
    use std::{ffi::OsString, os::unix::ffi::OsStringExt};
    let root = PathBuf::from(OsString::from_vec(b"/tmp/runtime-\xff".to_vec()));
    let layout = RuntimeLayout::try_from(root.clone())?;
    assert_eq!(layout.root(), root);
    assert_eq!(layout.lock_path(), root.join(".lock"));
    assert_eq!(layout.database_path(), root.join("db/store.db"));
    Ok(())
}
