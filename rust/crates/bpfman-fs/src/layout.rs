use std::path::{Component, Path, PathBuf};

use crate::{LayoutError, RuntimeLayout};

impl TryFrom<PathBuf> for RuntimeLayout {
    type Error = LayoutError;

    /// Validate and lexically normalise an absolute root, as Go's Layout does.
    ///
    /// This does not canonicalise through the filesystem or resolve symlinks.
    /// Non-UTF-8 path components are preserved.
    fn try_from(path: PathBuf) -> Result<Self, Self::Error> {
        if path.as_os_str().is_empty() {
            return Err(LayoutError::EmptyRoot);
        }
        if !path.is_absolute() {
            return Err(LayoutError::RelativeRoot(path));
        }
        let mut root = PathBuf::new();
        for component in path.components() {
            match component {
                Component::CurDir => {}
                Component::ParentDir => {
                    root.pop();
                }
                other => root.push(other.as_os_str()),
            }
        }
        if root.parent().is_none() {
            return Err(LayoutError::FilesystemRoot);
        }
        Ok(Self { root })
    }
}

impl RuntimeLayout {
    /// Validated runtime root, used directly without appending a bpfman suffix.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Global cross-process writer lock shared with the Go implementation.
    pub fn lock_path(&self) -> PathBuf {
        self.root.join(".lock")
    }

    /// SQLite database shared with the Go implementation.
    pub fn database_path(&self) -> PathBuf {
        self.root.join("db").join("store.db")
    }
}
