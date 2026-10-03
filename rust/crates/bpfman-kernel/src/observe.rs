use crate::{Error, ErrorKind, syscall};
use bpfman_model::{KernelMap, KernelProgram, ProgramStats};
use std::{
    collections::BTreeMap,
    num::NonZeroU32,
    os::fd::{AsRawFd, OwnedFd},
};
impl Error {
    /// Classify without exposing raw syscall/backend types.
    pub fn kind(&self) -> ErrorKind {
        match self.source.kind() {
            std::io::ErrorKind::NotFound => ErrorKind::Missing,
            std::io::ErrorKind::InvalidData => ErrorKind::InvalidData,
            _ => ErrorKind::Unavailable,
        }
    }
}
fn failure(operation: &'static str, source: std::io::Error) -> Error {
    Error { operation, source }
}
fn fdinfo(fd: &OwnedFd) -> BTreeMap<String, u64> {
    std::fs::read_to_string(format!("/proc/self/fdinfo/{}", fd.as_raw_fd()))
        .ok()
        .into_iter()
        .flat_map(|s| {
            s.lines()
                .filter_map(|line| {
                    let (key, value) = line.split_once(':')?;
                    Some((key.into(), value.trim().parse().ok()?))
                })
                .collect::<Vec<_>>()
        })
        .collect()
}
fn name(bytes: &[libc::c_char]) -> String {
    String::from_utf8_lossy(
        &bytes
            .iter()
            .take_while(|b| **b != 0)
            .map(|b| *b as u8)
            .collect::<Vec<_>>(),
    )
    .into_owned()
}
fn nonzero(value: u64) -> Option<u64> {
    (value != 0).then_some(value)
}
fn loaded_at(nanos: u64) -> Result<Option<String>, Error> {
    if nanos == 0 {
        return Ok(None);
    }
    let stat =
        std::fs::read_to_string("/proc/stat").map_err(|e| failure("read kernel boot time", e))?;
    let boot = stat
        .lines()
        .find_map(|line| line.strip_prefix("btime "))
        .and_then(|s| s.parse::<i64>().ok())
        .ok_or_else(|| {
            failure(
                "read kernel boot time",
                std::io::Error::new(std::io::ErrorKind::InvalidData, "missing btime"),
            )
        })?;
    let time = chrono::DateTime::from_timestamp(boot + (nanos / 1_000_000_000) as i64, 0)
        .ok_or_else(|| {
            failure(
                "convert kernel load time",
                std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "load time outside timestamp range",
                ),
            )
        })?;
    Ok(Some(
        time.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
    ))
}
/// Observe one live program by ID, preserving unavailable fields and counters.
/// A descriptor holds the object alive across both metadata queries and procfs.
pub fn observe_program(id: NonZeroU32) -> Result<(KernelProgram, Option<ProgramStats>), Error> {
    let (fd, info, maps, len, restricted) =
        syscall::program(id.get()).map_err(|e| failure("observe kernel program", e))?;
    let extra = fdinfo(&fd);
    let stats = (len as usize
        >= std::mem::offset_of!(aya_obj::generated::bpf_prog_info, run_cnt) + 8)
        .then_some(ProgramStats {
            runtime_ns: info.run_time_ns,
            run_count: info.run_cnt,
            recursion_misses: info.recursion_misses,
        });
    Ok((
        KernelProgram {
            id,
            name: name(&info.name),
            kind: program_kind(info.type_),
            tag: info.tag.iter().map(|b| format!("{b:02x}")).collect(),
            loaded_at: loaded_at(info.load_time)?,
            uid: (info.load_time != 0).then_some(info.created_by_uid),
            btf_id: (info.btf_id != 0).then_some(info.btf_id),
            map_ids: (info.load_time != 0).then_some(maps),
            jited_size: info.jited_prog_len,
            xlated_size: if restricted { 0 } else { info.xlated_prog_len },
            verified_insns: info.verified_insns,
            memlock: extra.get("memlock").copied().and_then(nonzero),
            restricted,
        },
        stats,
    ))
}
/// Observe a live map's metadata and procfs accounting/frozen flag.
pub fn observe_map(id: u32) -> Result<KernelMap, Error> {
    let (fd, info) = syscall::map(id).map_err(|e| failure("observe kernel map", e))?;
    let extra = fdinfo(&fd);
    Ok(KernelMap {
        id: info.id,
        name: name(&info.name),
        kind: map_kind(info.type_),
        key_size: info.key_size,
        value_size: info.value_size,
        max_entries: info.max_entries,
        flags: info.map_flags,
        btf_id: (info.btf_id != 0).then_some(info.btf_id),
        map_extra: nonzero(info.map_extra),
        memlock: extra.get("memlock").copied().and_then(nonzero),
        frozen: extra.get("frozen").is_some_and(|n| *n != 0),
    })
}
fn program_kind(kind: u32) -> String {
    const NAMES: &[&str] = &[
        "unspecifiedprogram",
        "socketfilter",
        "kprobe",
        "schedcls",
        "schedact",
        "tracepoint",
        "xdp",
        "perfevent",
        "cgroupskb",
        "cgroupsock",
        "lwtin",
        "lwtout",
        "lwtxmit",
        "sockops",
        "skskb",
        "cgroupdevice",
        "skmsg",
        "rawtracepoint",
        "cgroupsockaddr",
        "lwtseg6local",
        "lircmode2",
        "skreuseport",
        "flowdissector",
        "cgroupsysctl",
        "rawtracepointwritable",
        "cgroupsockopt",
        "tracing",
        "structops",
        "extension",
        "lsm",
        "sklookup",
        "syscall",
        "netfilter",
    ];
    NAMES
        .get(kind as usize)
        .map_or_else(|| format!("programtype({kind})"), |s| (*s).into())
}
fn map_kind(kind: u32) -> String {
    const NAMES: &[&str] = &[
        "unspecifiedmap",
        "hash",
        "array",
        "programarray",
        "perfeventarray",
        "percpuhash",
        "percpuarray",
        "stacktrace",
        "cgrouparray",
        "lruhash",
        "lrucpuhash",
        "lpmtrie",
        "arrayofmaps",
        "hashofmaps",
        "devmap",
        "sockmap",
        "cpumap",
        "xskmap",
        "sockhash",
        "cgroupstorage",
        "reuseportsockarray",
        "percpucgroupstorage",
        "queue",
        "stack",
        "skstorage",
        "devmaphash",
        "structopsmap",
        "ringbuf",
        "inodestorage",
        "taskstorage",
        "bloomfilter",
        "userringbuf",
        "cgroupstorage",
        "arena",
    ];
    NAMES
        .get(kind as usize)
        .map_or_else(|| format!("maptype({kind})"), |s| (*s).into())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn syscall_errors_preserve_absence_vs_denied_observation() {
        for (errno, kind) in [
            (libc::ENOENT, ErrorKind::Missing),
            (libc::EPERM, ErrorKind::Unavailable),
            (libc::EACCES, ErrorKind::Unavailable),
            (libc::EIO, ErrorKind::Unavailable),
        ] {
            assert_eq!(
                failure("test", std::io::Error::from_raw_os_error(errno)).kind(),
                kind
            );
        }
        assert_eq!(
            failure(
                "test",
                std::io::Error::from(std::io::ErrorKind::InvalidData)
            )
            .kind(),
            ErrorKind::InvalidData
        );
    }
    #[test]
    fn kernel_taxonomy_and_fixed_names_match_go_observations() {
        assert_eq!(program_kind(5), "tracepoint");
        assert_eq!(program_kind(3), "schedcls");
        assert_eq!(program_kind(14), "skskb");
        assert_eq!(program_kind(99), "programtype(99)");
        assert_eq!(map_kind(10), "lrucpuhash");
        assert_eq!(map_kind(26), "structopsmap");
        assert_eq!(map_kind(99), "maptype(99)");
        assert_eq!(name(&[b'a' as libc::c_char, 0, b'b' as libc::c_char]), "a");
        assert_eq!(nonzero(0), None);
        assert_eq!(nonzero(1024), Some(1024));
    }
}
