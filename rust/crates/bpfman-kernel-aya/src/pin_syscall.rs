//! Narrow Linux BPF and namespace boundary for XDP attachment and owned pinning.
#![allow(unsafe_code)]

use std::{
    ffi::CString,
    io,
    mem::size_of,
    os::{
        fd::{AsRawFd, BorrowedFd, FromRawFd, OwnedFd},
        unix::ffi::OsStrExt,
    },
    path::Path,
};

const XDP_FLAGS_SKB_MODE: u32 = 2;
const XDP_FLAGS_DRV_MODE: u32 = 4;
const XDP_FLAGS_HW_MODE: u32 = 8;

fn fd_result(rc: libc::c_long) -> io::Result<OwnedFd> {
    if rc < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: successful BPF fd-producing calls return a new owned descriptor.
    Ok(unsafe { OwnedFd::from_raw_fd(rc as i32) })
}

pub(super) fn interface(name: &str) -> io::Result<u32> {
    let name = CString::new(name)?;
    // SAFETY: name is a live NUL-terminated string for the duration of the call.
    let index = unsafe { libc::if_nametoindex(name.as_ptr()) };
    if index == 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(index)
    }
}

pub(super) fn outer(
    program: BorrowedFd<'_>,
    ifindex: u32,
    mode: bpfman_model::XdpMode,
) -> io::Result<OwnedFd> {
    let flags = match mode {
        bpfman_model::XdpMode::Skb => XDP_FLAGS_SKB_MODE,
        bpfman_model::XdpMode::Drv => XDP_FLAGS_DRV_MODE,
        bpfman_model::XdpMode::Hw => XDP_FLAGS_HW_MODE,
    };
    let attr = [program.as_raw_fd() as u32, ifindex, 37, flags];
    // SAFETY: BPF_LINK_CREATE's fixed prefix: program, ifindex, BPF_XDP, mode.
    // No netlink fallback or replacement of a foreign attachment is attempted.
    fd_result(unsafe { libc::syscall(libc::SYS_bpf, 28u32, attr.as_ptr(), size_of::<[u32; 4]>()) })
}

pub(super) fn detach(fd: BorrowedFd<'_>) -> io::Result<()> {
    let attr = [fd.as_raw_fd() as u32];
    // SAFETY: BPF_LINK_DETACH reads one initialized link_fd; fd stays borrowed.
    let rc = unsafe { libc::syscall(libc::SYS_bpf, 34u32, attr.as_ptr(), size_of::<u32>()) };
    if rc < 0 {
        let e = io::Error::last_os_error();
        // A prior successful detach can precede a failed unpin. Explicit retry
        // may then observe ENOLINK; it still must remove only the owned pin.
        if e.raw_os_error() != Some(libc::ENOLINK) {
            return Err(e);
        }
    }
    Ok(())
}

pub(super) fn replace(
    link: BorrowedFd<'_>,
    old: BorrowedFd<'_>,
    new: BorrowedFd<'_>,
) -> io::Result<()> {
    let attr = [
        link.as_raw_fd() as u32,
        new.as_raw_fd() as u32,
        4u32,
        old.as_raw_fd() as u32,
    ];
    // SAFETY: BPF_LINK_UPDATE reads four initialized u32 fields: link_fd,
    // new_prog_fd, BPF_F_REPLACE, old_prog_fd. All descriptors stay borrowed.
    // The expected-old check and update occur atomically in the kernel.
    let rc = unsafe { libc::syscall(libc::SYS_bpf, 29u32, attr.as_ptr(), size_of::<[u32; 4]>()) };
    if rc < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

#[repr(C)]
struct ObjectAttr {
    path: u64,
    fd: u32,
    flags: u32,
}

pub(super) fn pin(fd: BorrowedFd<'_>, path: &Path) -> io::Result<()> {
    let path = CString::new(path.as_os_str().as_bytes())?;
    let attr = ObjectAttr {
        path: path.as_ptr() as u64,
        fd: fd.as_raw_fd() as u32,
        flags: 0,
    };
    // SAFETY: initialized BPF_OBJ_PIN attr, borrowed fd and live CString.
    let rc = unsafe { libc::syscall(libc::SYS_bpf, 6u32, &attr, size_of::<ObjectAttr>()) };
    if rc < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

pub(super) fn open(path: &Path) -> io::Result<OwnedFd> {
    let path = CString::new(path.as_os_str().as_bytes())?;
    let attr = ObjectAttr {
        path: path.as_ptr() as u64,
        fd: 0,
        flags: 0,
    };
    // SAFETY: initialized BPF_OBJ_GET input and live pathname buffer.
    fd_result(unsafe { libc::syscall(libc::SYS_bpf, 7u32, &attr, size_of::<ObjectAttr>()) })
}

pub(super) fn by_id(id: u32) -> io::Result<OwnedFd> {
    let attr = [id, 0u32, 0u32];
    // SAFETY: BPF_LINK_GET_FD_BY_ID reads initialized id, next_id and open_flags.
    fd_result(unsafe { libc::syscall(libc::SYS_bpf, 30u32, attr.as_ptr(), size_of::<[u32; 3]>()) })
}

#[repr(C)]
#[derive(Default)]
pub(super) struct LinkInfo {
    pub kind: u32,
    pub id: u32,
    pub program: u32,
    pad: u32,
    pub data: [u32; 6],
}

#[repr(C)]
struct InfoAttr {
    fd: u32,
    len: u32,
    info: u64,
}

pub(super) fn info(fd: BorrowedFd<'_>) -> io::Result<LinkInfo> {
    let mut info = LinkInfo::default();
    let mut attr = InfoAttr {
        fd: fd.as_raw_fd() as u32,
        len: size_of::<LinkInfo>() as u32,
        info: (&raw mut info) as u64,
    };
    // SAFETY: BPF_OBJ_GET_INFO_BY_FD bounds writes by len to this live all-integer
    // prefix of bpf_link_info. The fixed XDP/tracing union members have no pointers.
    let rc = unsafe { libc::syscall(libc::SYS_bpf, 15u32, &mut attr, size_of::<InfoAttr>()) };
    if rc < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(info)
    }
}

pub(super) fn require_netns(fd: BorrowedFd<'_>) -> io::Result<()> {
    // SAFETY: NS_GET_NSTYPE takes no pointer arguments and borrows a live fd.
    let kind = unsafe { libc::ioctl(fd.as_raw_fd(), libc::NS_GET_NSTYPE) };
    if kind == libc::CLONE_NEWNET {
        Ok(())
    } else if kind < 0 {
        Err(io::Error::last_os_error())
    } else {
        Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "expected a network namespace descriptor",
        ))
    }
}

pub(super) fn enter_netns(fd: BorrowedFd<'_>) -> io::Result<()> {
    // SAFETY: called only on a newly spawned disposable thread, with a live
    // validated network namespace fd; mount/user namespaces are never changed.
    if unsafe { libc::setns(fd.as_raw_fd(), libc::CLONE_NEWNET) } == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}
