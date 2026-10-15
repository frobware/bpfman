//! Narrow Linux BPF boundary for links, owned pins, BTF and module tracing loads.
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

// Aya requires a valid ProgramId for ID-relative ordering. Validate its type and
// retain its descriptor through attach, keeping this checked conversion inside
// the existing narrow syscall boundary.
pub(super) fn tcx_order(
    order: bpfman_model::TcxOrder,
) -> io::Result<(aya::programs::LinkOrder, Option<OwnedFd>)> {
    use bpfman_model::TcxOrder;
    let (id, before) = match order {
        TcxOrder::First => return Ok((aya::programs::LinkOrder::first(), None)),
        TcxOrder::Before(id) => (id, true),
        TcxOrder::After(id) => (id, false),
    };
    let (fd, info, _, _, _) = crate::syscall::program(id.get())?;
    if info.type_ != 3 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "TCX anchor is not a classifier",
        ));
    }
    // SAFETY: GET_FD_BY_ID validated this program ID; fd keeps it live through attach.
    let id = unsafe { aya::programs::ProgramId::new(id.get()) };
    let order = if before {
        aya::programs::LinkOrder::before_program_id(id)
    } else {
        aya::programs::LinkOrder::after_program_id(id)
    };
    Ok((order, Some(fd)))
}

// Aya exposes map creation with a BTF descriptor, but keeps BTF loading private.
#[repr(C)]
struct BtfAttr {
    btf: u64,
    log: u64,
    size: u32,
    log_size: u32,
    log_level: u32,
    log_true_size: u32,
}

#[derive(Debug, thiserror::Error)]
#[error("BTF verifier rejection: {log}")]
struct BtfLoadError {
    #[source]
    error: io::Error,
    log: String,
}

pub(super) fn load_btf(bytes: &[u8]) -> io::Result<OwnedFd> {
    let size = u32::try_from(bytes.len()).map_err(|_| {
        io::Error::new(io::ErrorKind::InvalidInput, "BTF exceeds kernel size range")
    })?;
    let mut log = vec![0u8; 1024 * 1024];
    let attr = BtfAttr {
        btf: bytes.as_ptr() as u64,
        log: log.as_mut_ptr() as u64,
        size,
        log_size: log.len() as u32,
        log_level: 1,
        log_true_size: 0,
    };
    // SAFETY: initialized BPF_BTF_LOAD ABI; bytes and the exclusive writable log
    // buffer stay live for this synchronous call. No pointer escapes or persists.
    let fd = unsafe { libc::syscall(libc::SYS_bpf, 18u32, &attr, size_of::<BtfAttr>()) };
    if fd < 0 {
        let error = io::Error::last_os_error();
        let end = log.iter().position(|b| *b == 0).unwrap_or(log.len());
        return Err(io::Error::new(
            error.kind(),
            BtfLoadError {
                error,
                log: String::from_utf8_lossy(&log[..end]).into_owned(),
            },
        ));
    }
    // SAFETY: a successful BPF_BTF_LOAD returns one new, uniquely owned fd.
    fd_result(fd)
}

#[derive(Debug, thiserror::Error)]
#[error("function tracing verifier rejection: {log}")]
struct TracingLoadError {
    #[source]
    error: io::Error,
    log: String,
}

#[allow(clippy::too_many_arguments)]
pub(super) fn load_tracing(
    name: &str,
    program: &aya_obj::Program,
    code: &aya_obj::Function,
    btf: Option<BorrowedFd<'_>>,
    module: BorrowedFd<'_>,
    target: u32,
    kind: bpfman_model::TracingKind,
    flags: u32,
) -> io::Result<OwnedFd> {
    use aya_obj::generated::bpf_attr__bindgen_ty_4;
    // SAFETY: generated Linux BPF_PROG_LOAD input contains only integers/unions
    // of integers. Zero initializes reserved fields and unused pointer pairs.
    let mut attr: bpf_attr__bindgen_ty_4 = unsafe { std::mem::zeroed() };
    let count = |len| {
        u32::try_from(len).map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "tracing data exceeds kernel size range",
            )
        })
    };
    let mut log = vec![0u8; 1024 * 1024];
    attr.prog_type = 26;
    attr.insn_cnt = count(code.instructions.len())?;
    attr.insns = code.instructions.as_ptr() as u64;
    attr.license = program.license.as_ptr() as u64;
    attr.kern_version = program.kernel_version.unwrap_or(0);
    attr.prog_flags = flags;
    for (out, byte) in attr.prog_name.iter_mut().take(15).zip(name.bytes()) {
        *out = byte as libc::c_char;
    }
    attr.expected_attach_type = crate::function_tracing::attach_type(kind);
    attr.attach_btf_id = target;
    attr.__bindgen_anon_1.attach_btf_obj_fd = module.as_raw_fd() as u32;
    if let Some(btf) = btf {
        attr.prog_btf_fd = btf.as_raw_fd() as u32;
        attr.func_info_rec_size = size_of::<aya_obj::generated::bpf_func_info>() as u32;
        attr.func_info = code.func_info.func_info.as_ptr() as u64;
        attr.func_info_cnt = count(code.func_info.func_info.len())?;
        attr.line_info_rec_size = size_of::<aya_obj::generated::bpf_line_info>() as u32;
        attr.line_info = code.line_info.line_info.as_ptr() as u64;
        attr.line_info_cnt = count(code.line_info.line_info.len())?;
    }
    attr.log_buf = log.as_mut_ptr() as u64;
    attr.log_size = log.len() as u32;
    attr.log_level = 1;
    // SAFETY: initialized generated BPF_PROG_LOAD ABI. All instructions, BTF
    // records, license and writable log buffers remain live through this call.
    // Borrowed map, BTF and module descriptors are held by the caller.
    let result = unsafe {
        libc::syscall(
            libc::SYS_bpf,
            5u32,
            &attr,
            size_of::<bpf_attr__bindgen_ty_4>(),
        )
    };
    if result < 0 {
        let error = io::Error::last_os_error();
        let end = log.iter().position(|b| *b == 0).unwrap_or(log.len());
        return Err(io::Error::new(
            error.kind(),
            TracingLoadError {
                error,
                log: String::from_utf8_lossy(&log[..end]).into_owned(),
            },
        ));
    }
    fd_result(result)
}

pub(super) fn attach_tracing(
    program: BorrowedFd<'_>,
    kind: bpfman_model::TracingKind,
) -> io::Result<OwnedFd> {
    let attr = [
        program.as_raw_fd() as u32,
        0,
        crate::function_tracing::attach_type(kind),
        0,
    ];
    // SAFETY: fixed initialized BPF_LINK_CREATE prefix. Target is bound at load;
    // the kernel verifies the requested attach type against that program.
    fd_result(unsafe { libc::syscall(libc::SYS_bpf, 28u32, attr.as_ptr(), size_of::<[u32; 4]>()) })
}
