use std::path::{Component, Path, PathBuf};

use crate::{LayoutError, RuntimeLayout};

pub(super) const LOCK_FILE: &str = ".lock";
pub(super) const DATABASE_DIRECTORY: &str = "db";
pub(super) const DATABASE_FILE: &str = "store.db";

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
        self.root.join(LOCK_FILE)
    }

    /// SQLite database shared with the Go implementation.
    pub fn database_path(&self) -> PathBuf {
        self.root.join(DATABASE_DIRECTORY).join(DATABASE_FILE)
    }
}

impl RuntimeLayout {
    /// Go-compatible standalone link pin location, for persistence and presentation.
    pub fn link_pin_path(&self, id: std::num::NonZeroU64) -> PathBuf {
        self.root.join("fs/links").join(id.to_string())
    }

    /// Go-compatible program pin path, for stored records and presentation only.
    pub fn program_pin_path(&self, id: std::num::NonZeroU32) -> PathBuf {
        self.root.join("fs").join(format!("prog_{id}"))
    }

    /// Go-compatible private map directory, for stored records only.
    pub fn map_directory_path(&self, id: std::num::NonZeroU32) -> PathBuf {
        self.root.join("fs/maps").join(id.to_string())
    }

    /// Published local ELF path, for stored records only.
    pub fn bytecode_path(&self, id: std::num::NonZeroU32) -> PathBuf {
        self.root
            .join("programs")
            .join(id.to_string())
            .join("bytecode.o")
    }
}

impl RuntimeLayout {
    /// Go-compatible XDP revision directory, for persistence and presentation.
    pub fn xdp_revision_path(
        &self,
        key: bpfman_model::XdpKey,
        revision: std::num::NonZeroU32,
    ) -> PathBuf {
        self.root.join("fs/xdp").join(format!(
            "dispatcher_{}_{}_{}",
            key.nsid, key.ifindex, revision
        ))
    }

    /// XDP dispatcher program pin path.
    pub fn xdp_program_path(
        &self,
        key: bpfman_model::XdpKey,
        revision: std::num::NonZeroU32,
    ) -> PathBuf {
        self.xdp_revision_path(key, revision).join("dispatcher")
    }

    /// First extension's canonical link pin path.
    pub fn xdp_extension_path(
        &self,
        key: bpfman_model::XdpKey,
        revision: std::num::NonZeroU32,
    ) -> PathBuf {
        self.xdp_revision_path(key, revision).join("link_0")
    }

    /// Stable outer interface link pin path.
    pub fn xdp_outer_path(&self, key: bpfman_model::XdpKey) -> PathBuf {
        self.root
            .join("fs/xdp")
            .join(format!("dispatcher_{}_{}_link", key.nsid, key.ifindex))
    }
}
