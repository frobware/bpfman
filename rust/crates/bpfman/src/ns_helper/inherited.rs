//! Audited exec descriptor boundary. Only dup2 runs in the post-fork callback.
#![allow(unsafe_code)]
use std::{
    io,
    os::{
        fd::{AsRawFd, BorrowedFd, FromRawFd, OwnedFd},
        unix::process::CommandExt,
    },
    process::Command,
};

pub(super) fn configure(
    command: &mut Command,
    socket: BorrowedFd<'_>,
    lock: BorrowedFd<'_>,
    namespace: BorrowedFd<'_>,
) -> io::Result<()> {
    // Sources above destination range prevent dup2 collisions. They remain
    // CLOEXEC in the parent, including while other threads launch processes.
    let socket = rustix::io::fcntl_dupfd_cloexec(socket, 10)?;
    let lock = rustix::io::fcntl_dupfd_cloexec(lock, 10)?;
    let namespace = rustix::io::fcntl_dupfd_cloexec(namespace, 10)?;
    // SAFETY: the callback performs only async-signal-safe dup2 syscalls and
    // error construction. It captures owned descriptors, with no locks or I/O.
    unsafe {
        command.pre_exec(move || {
            for (source, destination) in [
                (socket.as_raw_fd(), 3),
                (lock.as_raw_fd(), 4),
                (namespace.as_raw_fd(), 5),
            ] {
                if libc::dup2(source, destination) < 0 {
                    return Err(io::Error::last_os_error());
                }
            }
            Ok(())
        });
    }
    Ok(())
}

pub(super) fn take(fd: i32) -> io::Result<OwnedFd> {
    // SAFETY: called once in the single-threaded helper, after checking that all
    // inherited slots are distinct and not standard I/O. fcntl verifies validity
    // before ownership is adopted; no borrowed references to these slots exist.
    unsafe {
        if libc::fcntl(fd, libc::F_GETFD) < 0 {
            return Err(io::Error::last_os_error());
        }
        let owned = OwnedFd::from_raw_fd(fd);
        rustix::io::fcntl_setfd(&owned, rustix::io::FdFlags::CLOEXEC)?;
        Ok(owned)
    }
}
