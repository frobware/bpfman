//! Minimal read-only BPF ABI bridge for fields Aya's public API does not expose.
//! Only GET_FD_BY_ID and OBJ_GET_INFO_BY_FD are supported. No arbitrary command
//! or caller-supplied pointer can enter this module's safe interface.
#![allow(unsafe_code)]
use aya_obj::generated::{bpf_map_info, bpf_prog_info};
use std::{
    io,
    mem::{size_of, zeroed},
    os::fd::{AsRawFd, BorrowedFd, FromRawFd, OwnedFd},
};
#[repr(C)]
struct IdAttr {
    id: u32,
    next_id: u32,
    open_flags: u32,
}
#[repr(C)]
struct InfoAttr {
    fd: u32,
    len: u32,
    info: u64,
}
fn by_id(id: u32, command: u32) -> io::Result<OwnedFd> {
    let attr = IdAttr {
        id,
        next_id: 0,
        open_flags: 0,
    };
    // SAFETY: attr matches the Linux GET_FD_BY_ID ABI, is initialized and live
    // for this synchronous syscall. The kernel does not retain its address.
    let fd = unsafe { libc::syscall(libc::SYS_bpf, command, &attr, size_of::<IdAttr>()) };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: success returns a new owned descriptor, uniquely adopted here.
    Ok(unsafe { OwnedFd::from_raw_fd(fd as i32) })
}
fn info<T>(fd: BorrowedFd<'_>, value: &mut T) -> io::Result<u32> {
    let mut attr = InfoAttr {
        fd: fd.as_raw_fd() as u32,
        len: size_of::<T>() as u32,
        info: std::ptr::from_mut(value) as u64,
    };
    // SAFETY: private callers supply initialized generated BPF info structs.
    // value and any embedded buffers remain exclusively borrowed and live until
    // return. The kernel bounds all writes by attr.len and each buffer count.
    let rc = unsafe { libc::syscall(libc::SYS_bpf, 15u32, &mut attr, size_of::<InfoAttr>()) };
    if rc < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(attr.len)
    }
}
pub(super) fn program(id: u32) -> io::Result<(OwnedFd, bpf_prog_info, Vec<u32>, u32, bool)> {
    use std::os::fd::AsFd;
    let fd = by_id(id, 13)?;
    // SAFETY: generated C info structs contain only integers and integer arrays;
    // zero initializes every field and all kernel pointer/count pairs.
    let mut initial: bpf_prog_info = unsafe { zeroed() };
    info(fd.as_fd(), &mut initial)?;
    if initial.nr_map_ids > 1_048_576 || initial.xlated_prog_len > 16 * 1024 * 1024 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "unreasonable BPF info buffer length",
        ));
    }
    let mut maps = vec![0u32; initial.nr_map_ids as usize];
    let mut instructions = vec![0u8; initial.xlated_prog_len as usize];
    // SAFETY: same all-integer layout; start fresh so unrelated output lengths
    // from the first query cannot become input lengths with null pointers.
    let mut result: bpf_prog_info = unsafe { zeroed() };
    result.nr_map_ids = maps.len() as u32;
    result.map_ids = if maps.is_empty() {
        0
    } else {
        maps.as_mut_ptr() as u64
    };
    result.xlated_prog_len = instructions.len() as u32;
    result.xlated_prog_insns = if instructions.is_empty() {
        0
    } else {
        instructions.as_mut_ptr() as u64
    };
    let len = info(fd.as_fd(), &mut result)?;
    if result.id != id || result.nr_map_ids as usize != maps.len() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "BPF program identity or map count changed",
        ));
    }
    let restricted = !instructions.is_empty() && result.xlated_prog_insns == 0;
    // Never let pointers to temporary buffers escape, even inside private data.
    result.map_ids = 0;
    result.xlated_prog_insns = 0;
    Ok((fd, result, maps, len, restricted))
}
pub(super) fn map(id: u32) -> io::Result<(OwnedFd, bpf_map_info)> {
    use std::os::fd::AsFd;
    let fd = by_id(id, 14)?;
    // SAFETY: bpf_map_info is an all-integer generated C layout.
    let mut result: bpf_map_info = unsafe { zeroed() };
    info(fd.as_fd(), &mut result)?;
    if result.id != id {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "BPF map identity mismatch",
        ));
    }
    Ok((fd, result))
}
