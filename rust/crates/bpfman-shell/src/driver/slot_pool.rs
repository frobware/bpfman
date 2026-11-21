//! Shared flock and provenance rules for the Go and Rust fixture pools.

use std::{
    fs::{self, DirBuilder, File, OpenOptions},
    io::{self, Read, Seek, SeekFrom, Write},
    ops::RangeInclusive,
    os::unix::fs::{DirBuilderExt, OpenOptionsExt},
    path::{Path, PathBuf},
};

use jiff::Timestamp;
use rustix::fs::{FlockOperation, flock};
use serde_json::{Map, Value as Json};

pub(super) fn candidates(root: &Path, indices: RangeInclusive<u8>) -> io::Result<Vec<Candidate>> {
    DirBuilder::new().recursive(true).mode(0o700).create(root)?;
    let mut slots = indices
        .map(|index| {
            let path = root.join(format!("{index:02}.lock"));
            let last_use = last_used_at(&path)?;
            Ok((last_use, Candidate { index, path }))
        })
        .collect::<io::Result<Vec<_>>>()?;
    slots.sort_by_key(|(last_use, _)| *last_use);
    Ok(slots.into_iter().map(|(_, candidate)| candidate).collect())
}

pub(super) struct Candidate {
    pub index: u8,
    path: PathBuf,
}

impl Candidate {
    pub(super) fn try_lock(&self) -> io::Result<Option<Lock>> {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .open(&self.path)?;
        match flock(&file, FlockOperation::NonBlockingLockExclusive) {
            Ok(()) => Ok(Some(Lock { file })),
            Err(rustix::io::Errno::WOULDBLOCK) => Ok(None),
            Err(error) => Err(error.into()),
        }
    }
}

pub(super) struct Lock {
    file: File,
}

impl Lock {
    pub(super) fn previous(&mut self) -> io::Result<Map<String, Json>> {
        let mut contents = Vec::new();
        self.file.seek(SeekFrom::Start(0))?;
        self.file.read_to_end(&mut contents)?;
        if contents.is_empty() {
            return Ok(Map::new());
        }
        serde_json::from_slice(&contents)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
    }

    pub(super) fn claim(mut self, metadata: Map<String, Json>) -> io::Result<Lease> {
        write_record(&mut self.file, &metadata)?;
        Ok(Lease {
            file: self.file,
            metadata,
        })
    }
}

pub(super) struct Lease {
    file: File,
    metadata: Map<String, Json>,
}

impl Lease {
    pub(super) fn release(mut self) -> io::Result<()> {
        self.metadata.insert(
            "released_at".to_owned(),
            Json::String(Timestamp::now().to_string()),
        );
        write_record(&mut self.file, &self.metadata)
    }
}

fn write_record(file: &mut File, metadata: &Map<String, Json>) -> io::Result<()> {
    file.set_len(0)?;
    file.seek(SeekFrom::Start(0))?;
    serde_json::to_writer(&mut *file, metadata)?;
    file.write_all(b"\n")
}

fn last_used_at(path: &Path) -> io::Result<Option<Timestamp>> {
    let info = match fs::metadata(path) {
        Ok(info) => info,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    let released = fs::read(path)
        .ok()
        .and_then(|body| serde_json::from_slice::<Json>(&body).ok())
        .and_then(|record| record.get("released_at")?.as_str()?.parse().ok());
    Ok(released.or_else(|| {
        info.modified()
            .ok()
            .and_then(|time| Timestamp::try_from(time).ok())
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_lock_is_fresh_but_malformed_provenance_is_an_error() -> io::Result<()> {
        let dir = tempfile::tempdir()?;
        let candidate = candidates(dir.path(), 0..=0)?.remove(0);
        let mut lock = candidate.try_lock()?.expect("fresh slot is available");
        assert!(lock.previous()?.is_empty());

        fs::write(&candidate.path, b"{\"origin\":")?;
        let error = lock.previous().expect_err("malformed provenance");
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        assert_eq!(fs::read(&candidate.path)?, b"{\"origin\":");
        Ok(())
    }
}
