//! Namespace entry is confined to disposable worker threads. The caller retains
//! writer authority and owns every returned descriptor; no worker pins objects.

use bpfman_model::{InterfaceName, NetworkNamespace, XdpKey, XdpMode};
use std::{
    fs::{File, OpenOptions},
    io,
    num::{NonZeroU32, NonZeroU64},
    os::{
        fd::{AsFd, BorrowedFd, OwnedFd},
        unix::fs::{MetadataExt, OpenOptionsExt},
    },
};

/// Owned namespace and interface evidence for a prepared XDP attachment.
/// Its descriptor keeps the namespace alive across staging and failed recovery.
pub struct XdpNamespace {
    fd: File,
    selector: NetworkNamespace,
    interface: InterfaceName,
    device: u64,
    key: XdpKey,
}

#[derive(Debug)]
struct ModeAttachError {
    requested: XdpMode,
    requested_error: io::Error,
    fallback_error: io::Error,
}

impl std::fmt::Display for ModeAttachError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "XDP {} attach failed ({}); SKB fallback failed",
            self.requested.as_str(),
            self.requested_error
        )
    }
}

impl std::error::Error for ModeAttachError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.fallback_error)
    }
}

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

fn open(selector: &NetworkNamespace) -> io::Result<File> {
    let path = if selector.as_str().is_empty() {
        "/proc/thread-self/ns/net"
    } else {
        selector.as_str()
    };
    let fd = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NONBLOCK)
        .open(path)?;
    crate::pin_syscall::require_netns(fd.as_fd())?;
    Ok(fd)
}

// A fresh thread enters once and exits; it never returns to a pool or restores
// another thread's namespace. Joining keeps all borrowed authority in scope.
fn run<T: Send>(
    fd: BorrowedFd<'_>,
    explicit: bool,
    action: impl FnOnce() -> io::Result<T> + Send,
) -> io::Result<T> {
    if !explicit {
        return action();
    }
    std::thread::scope(|scope| {
        std::thread::Builder::new()
            .name("bpfman-netns".into())
            .spawn_scoped(scope, move || {
                crate::pin_syscall::enter_netns(fd)?;
                action()
            })?
            .join()
            .map_err(|_| io::Error::other("network namespace worker panicked"))?
    })
}

impl XdpNamespace {
    pub(super) fn open(interface: &InterfaceName, selector: &NetworkNamespace) -> io::Result<Self> {
        let fd = open(selector)?;
        let metadata = fd.metadata()?;
        let nsid =
            NonZeroU64::new(metadata.ino()).ok_or_else(|| invalid("zero namespace identity"))?;
        let ifindex = run(fd.as_fd(), !selector.as_str().is_empty(), || {
            crate::pin_syscall::interface(interface.as_str())
        })?;
        let ifindex = NonZeroU32::new(ifindex).ok_or_else(|| invalid("zero interface identity"))?;
        Ok(Self {
            fd,
            selector: selector.clone(),
            interface: interface.clone(),
            device: metadata.dev(),
            key: XdpKey { nsid, ifindex },
        })
    }

    pub(super) fn key(&self) -> XdpKey {
        self.key
    }

    pub(super) fn validate(&self) -> io::Result<()> {
        let current = open(&self.selector)?.metadata()?;
        if current.dev() != self.device || current.ino() != self.key.nsid.get() {
            return Err(invalid("network namespace path changed since admission"));
        }
        Ok(())
    }

    pub(super) fn attach(
        &self,
        program: BorrowedFd<'_>,
        key: XdpKey,
        mode: XdpMode,
    ) -> io::Result<OwnedFd> {
        self.validate()?;
        if key != self.key {
            return Err(invalid(
                "namespace evidence belongs to another attach point",
            ));
        }
        run(self.fd.as_fd(), !self.selector.as_str().is_empty(), || {
            if crate::pin_syscall::interface(self.interface.as_str())? != key.ifindex.get() {
                return Err(invalid("network interface changed since admission"));
            }
            match crate::pin_syscall::outer(program, key.ifindex.get(), mode) {
                Ok(fd) => Ok(fd),
                Err(requested_error) if mode != XdpMode::Skb => {
                    match crate::pin_syscall::outer(program, key.ifindex.get(), XdpMode::Skb) {
                        Ok(fd) => Ok(fd),
                        Err(fallback_error) => Err(io::Error::other(ModeAttachError {
                            requested: mode,
                            requested_error,
                            fallback_error,
                        })),
                    }
                }
                Err(error) => Err(error),
            }
        })
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};

    #[test]
    fn failed_worker_entry_does_not_run_effects_or_change_the_caller() {
        let original = std::fs::read_link("/proc/thread-self/ns/net").expect("caller namespace");
        let wrong = File::open("/proc/thread-self/ns/mnt").expect("mount namespace");
        let called = AtomicBool::new(false);
        let result = run(wrong.as_fd(), true, || {
            called.store(true, Ordering::SeqCst);
            Ok(())
        });
        assert!(result.is_err());
        assert!(!called.load(Ordering::SeqCst));
        assert_eq!(
            std::fs::read_link("/proc/thread-self/ns/net").expect("caller"),
            original
        );
    }
}
