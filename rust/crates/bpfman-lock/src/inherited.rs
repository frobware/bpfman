use std::os::fd::OwnedFd;

use rustix::fs::{FlockOperation, flock};

use crate::{Error, InheritedWriteLock, WritePermit, acquire::ActiveScope, error::io_error};

impl WritePermit<'_> {
    /// Duplicate the held descriptor for explicit handoff to a child process.
    ///
    /// The copy is close-on-exec. The process launcher must explicitly map it
    /// into the child and set [`crate::WRITER_LOCK_FD_ENV`]. Ending the parent's callback
    /// does not release the lock while an inherited copy remains open.
    pub fn duplicate_fd(&self) -> Result<OwnedFd, Error> {
        self.file
            .try_clone()
            .map(OwnedFd::from)
            .map_err(|e| io_error("duplicate writer lock descriptor", e))
    }
}

impl InheritedWriteLock {
    /// Accept an owned descriptor from the parent; never reopen the lock path.
    ///
    /// As in Go, verification proves exclusive ownership *now*, not that the
    /// parent held it previously. The parent must duplicate through a WritePermit.
    pub fn from_fd(fd: OwnedFd) -> Result<Self, Error> {
        let file = std::fs::File::from(fd);
        flock(&file, FlockOperation::NonBlockingLockExclusive)
            .map_err(|e| io_error("verify inherited writer lock descriptor", e))?;

        Ok(Self { file })
    }

    /// Borrow proof of the inherited lock for one callback.
    ///
    /// The lock remains owned until this object and every duplicate are dropped.
    pub fn with_permit<T>(
        &self,
        work: impl for<'lock> FnOnce(WritePermit<'lock>) -> T,
    ) -> Result<T, Error> {
        let _active = ActiveScope::enter(&self.file)?;

        Ok(work(WritePermit { file: &self.file }))
    }
}
