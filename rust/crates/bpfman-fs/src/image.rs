//! A single envelope prevents readers combining bytecode and metadata generations.
use crate::{
    Error, RuntimeDirectory, RuntimeWriter,
    artifacts::{Entry, entry, identity, proc_path},
    directory::{CONFINED, DIRECTORY, ensure_directory},
    error::{Failure, io},
};
use bpfman_core::EffectFailure;
use rustix::fs::{AtFlags, CWD, FileType, Mode, OFlags, fstat, linkat, openat2, renameat};
use std::{
    fs::File,
    io::{Read, Write},
};

const COLLECTION: &str = "images-v1";
const LIMIT: u64 = 32 * 1024 * 1024;

/// A full SHA256 cache identity with no caller-controlled path components.
#[derive(Clone, Copy, Debug)]
pub struct ImageCacheKey([u8; 32]);

impl ImageCacheKey {
    /// Construct from the hash of the normalized reference and platform.
    pub fn new(digest: [u8; 32]) -> Self {
        Self(digest)
    }
    fn name(&self) -> String {
        self.0.iter().map(|b| format!("{b:02x}")).collect()
    }
}

/// Ownership of a fully written cache stage. Drop does not remove it.
#[derive(Debug)]
pub struct ImageCacheStage {
    entry: Box<Entry>,
    key: ImageCacheKey,
}

impl RuntimeDirectory {
    /// Read an envelope without a writer. Only absence is a cache miss.
    pub fn read_image_cache(&self, key: ImageCacheKey) -> Result<Option<Vec<u8>>, Error> {
        let fd = match openat2(
            &self.root,
            format!("{COLLECTION}/{}", key.name()),
            OFlags::RDONLY | OFlags::CLOEXEC | OFlags::NONBLOCK,
            Mode::empty(),
            CONFINED,
        ) {
            Ok(fd) => fd,
            Err(rustix::io::Errno::NOENT) => return Ok(None),
            Err(e) => return Err(io("open cached image", e)),
        };
        let stat = fstat(&fd).map_err(|e| io("inspect cached image", e))?;
        if FileType::from_raw_mode(stat.st_mode) != FileType::RegularFile || stat.st_nlink != 1 {
            return Err(
                Failure::Unsafe("image cache entry must be a singly linked regular file").into(),
            );
        }
        let mut bytes = Vec::new();
        File::from(fd)
            .take(LIMIT + 1)
            .read_to_end(&mut bytes)
            .map_err(|e| io("read cached image", e))?;
        if bytes.len() as u64 > LIMIT {
            return Err(Failure::Unsafe("image cache entry exceeds size limit").into());
        }
        Ok(Some(bytes))
    }
}

impl RuntimeWriter<'_> {
    /// Sync an unnamed file before acquiring a named stage. Failed writes close
    /// automatically. Once linked, the returned receipt owns its cleanup.
    pub fn stage_image_cache(
        &self,
        key: ImageCacheKey,
        bytes: &[u8],
    ) -> Result<ImageCacheStage, Error> {
        if bytes.len() as u64 > LIMIT {
            return Err(Failure::Unsafe("image envelope exceeds size limit").into());
        }
        let parent = ensure_directory(&self.runtime.root, COLLECTION, CONFINED)?;
        let owned = entry(
            self,
            &parent,
            COLLECTION.into(),
            format!(".stage-{}", key.name()),
            false,
        )?;
        let fd = openat2(
            &parent,
            ".",
            OFlags::RDWR | OFlags::TMPFILE | OFlags::CLOEXEC,
            Mode::from_raw_mode(0o600),
            CONFINED,
        )
        .map_err(|e| io("create unnamed cache stage", e))?;
        let mut file = File::from(fd);
        file.write_all(bytes)
            .map_err(|e| io("write image cache stage", e))?;
        file.sync_all()
            .map_err(|e| io("sync image cache stage", e))?;
        let mut stage = ImageCacheStage {
            entry: Box::new(owned),
            key,
        };
        let retained =
            rustix::io::fcntl_dupfd_cloexec(&file, 0).map_err(|e| io("retain image stage", e))?;
        stage.entry.identity = Some(identity(&retained)?);
        stage.entry.check_writer(self)?;
        linkat(
            CWD,
            proc_path(&retained),
            &parent,
            &stage.entry.name,
            AtFlags::SYMLINK_FOLLOW,
        )
        .map_err(|e| io("link image cache stage", e))?;
        Ok(stage)
    }

    /// Atomically replace a complete envelope. Failed publication returns stage
    /// ownership; a failed post-rename sync retains no stage.
    pub fn publish_image_cache(
        &self,
        stage: ImageCacheStage,
    ) -> Result<(), EffectFailure<Option<ImageCacheStage>, Error>> {
        let result = (|| {
            stage.entry.check_writer(self)?;
            let _opened = crate::removal::open_owned(&stage.entry)?;
            let _existing = self.directory().read_image_cache(stage.key)?;
            renameat(
                &stage.entry.parent,
                &stage.entry.name,
                &stage.entry.parent,
                stage.key.name(),
            )
            .map_err(|e| io("publish image cache envelope", e))
        })();
        if let Err(cause) = result {
            return Err(EffectFailure {
                cause,
                remaining: Some(stage),
            });
        }
        let directory = openat2(&stage.entry.parent, ".", DIRECTORY, Mode::empty(), CONFINED)
            .map_err(|e| EffectFailure {
                cause: io("open published cache directory", e),
                remaining: None,
            })?;
        File::from(directory).sync_all().map_err(|e| EffectFailure {
            cause: io("sync published image cache", e),
            remaining: None,
        })
    }

    /// Remove exactly the retained stage, preserving ownership on failure.
    pub fn remove_image_cache_stage(
        &self,
        stage: ImageCacheStage,
    ) -> Result<(), EffectFailure<ImageCacheStage, Error>> {
        match crate::removal::remove(self, &stage.entry) {
            Ok(()) => Ok(()),
            Err(cause) => Err(EffectFailure {
                cause,
                remaining: stage,
            }),
        }
    }
}
