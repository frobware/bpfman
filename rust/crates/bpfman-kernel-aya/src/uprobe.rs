//! Native user-space entry/return probes with confined pin adoption.
use crate::{AyaLink, AyaUprobe, Kernel, PreparedUprobe, failure::filesystem};
use bpfman_fs::{
    KernelResult, PinSource, PinnedProgram, ProgramInspection, RuntimeWriter, UprobeKernel,
    UprobeProgram,
};
use bpfman_kernel::{Error, UprobeLinks};
use bpfman_model::UprobeLink;
use std::{
    ffi::OsStr,
    num::NonZeroU32,
    os::{
        fd::AsRawFd,
        unix::{ffi::OsStrExt, fs::OpenOptionsExt},
    },
    path::PathBuf,
};

impl UprobeLinks for Kernel {
    type PreparedUprobe = PreparedUprobe;

    fn prepare_uprobe(
        &self,
        writer: &RuntimeWriter<'_>,
        program: NonZeroU32,
        target: &UprobeLink,
    ) -> Result<Self::PreparedUprobe, Error> {
        let program = writer
            .prepare_uprobe_attach(self, program, target.retprobe())
            .map_err(filesystem)?;
        let file = match (&self.uprobe_opener, target.container_pid()) {
            (Some(open), Some(_)) => Some(
                open(writer, target)
                    .and_then(regular)
                    .map_err(target_error)?,
            ),
            (None, Some(_)) => {
                return Err(target_error(std::io::Error::other(
                    "container target opener is not configured",
                )));
            }
            (_, None) => None,
        };
        Ok(PreparedUprobe {
            program,
            target: file,
            intent: target.clone(),
        })
    }

    fn attach_uprobe(
        &self,
        writer: &RuntimeWriter<'_>,
        prepared: Self::PreparedUprobe,
        target: &UprobeLink,
    ) -> Result<Self::LiveTracepoint, Error> {
        if &prepared.intent != target {
            return Err(target_error(std::io::Error::other(
                "user-space target changed since preparation",
            )));
        }
        let file = match prepared.target {
            Some(file) => file,
            None => open_host(target).and_then(regular).map_err(target_error)?,
        };
        prepared
            .program
            .attach(writer, target, &file)
            .map_err(filesystem)
    }
}

impl UprobeKernel for Kernel {
    type Uprobe = AyaUprobe;

    fn uprobe_at(
        &self,
        source: PinSource<'_>,
        retprobe: bool,
    ) -> KernelResult<(PinnedProgram, AyaUprobe)> {
        let info = self.program_at(source)?;
        let kind = if retprobe {
            aya::programs::ProbeKind::Return
        } else {
            aya::programs::ProbeKind::Entry
        };
        let program = aya::programs::UProbe::from_pin(source.path(), kind)?;
        Ok((info, AyaUprobe(program)))
    }
}

impl UprobeProgram for AyaUprobe {
    type Link = AyaLink;

    fn attach(&mut self, target: &UprobeLink, file: &std::fs::File) -> KernelResult<AyaLink> {
        let scope = target.pid().map_or(
            aya::programs::uprobe::UProbeScope::AllProcesses,
            aya::programs::uprobe::UProbeScope::OneProcess,
        );
        let point = match target.function() {
            Some(name) => {
                aya::programs::uprobe::UProbeAttachLocation::SymbolOffset(name, target.offset())
            }
            None => aya::programs::uprobe::UProbeAttachLocation::AbsoluteOffset(target.offset()),
        };

        let path = format!("/proc/self/fd/{}", file.as_raw_fd());
        let id = self.0.attach(point, path, scope)?;
        let link: aya::programs::links::FdLink = self.0.take_link(id)?.try_into()?;
        Ok(AyaLink(link))
    }
}

pub(super) type TargetOpener =
    dyn Fn(&RuntimeWriter<'_>, &UprobeLink) -> std::io::Result<std::fs::File> + Send + Sync;

impl Kernel {
    /// Inject container target lookup. The callback returns an owned regular-file
    /// descriptor; attachment and pinning remain in this backend and its caller.
    /// The caller owns process policy and cooperative cancellation for the lookup.
    pub fn with_uprobe_target_opener(
        open: impl Fn(&RuntimeWriter<'_>, &UprobeLink) -> std::io::Result<std::fs::File>
        + Send
        + Sync
        + 'static,
    ) -> Self {
        Self {
            uprobe_opener: Some(Box::new(open)),
        }
    }
}

fn open_host(target: &UprobeLink) -> std::io::Result<std::fs::File> {
    let mapped = if let Some(pid) = target.pid() {
        let bytes = resolution_bytes(std::path::Path::new(&format!("/proc/{pid}/maps")))?;
        bpfman_core::uprobe_library_from_maps(&bytes, target.target())
            .map(|path| PathBuf::from(OsStr::from_bytes(path)))
    } else {
        None
    };
    let path = match mapped {
        Some(path) => path,
        None if target.is_library() => {
            let bytes = resolution_bytes(std::path::Path::new("/etc/ld.so.cache"))?;
            let path = bpfman_core::uprobe_library_from_cache(&bytes, target.target())
                .map_err(std::io::Error::other)?
                .ok_or_else(|| {
                    std::io::Error::other(format!(
                        "library {:?} not found in /etc/ld.so.cache",
                        target.target()
                    ))
                })?;
            PathBuf::from(OsStr::from_bytes(path))
        }
        None => PathBuf::from(target.target()),
    };
    std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(rustix::fs::OFlags::NONBLOCK.bits() as i32)
        .open(path)
}

fn regular(file: std::fs::File) -> std::io::Result<std::fs::File> {
    if !file.metadata()?.is_file() {
        return Err(std::io::Error::other(
            "uprobe target must be a regular file",
        ));
    }
    Ok(file)
}

fn target_error(error: std::io::Error) -> Error {
    Error::new(
        crate::ErrorKind::Unavailable,
        "open user-space probe target",
        error,
    )
}

impl std::fmt::Debug for Kernel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Kernel").finish_non_exhaustive()
    }
}

#[derive(Debug, thiserror::Error)]
#[error("read user-space target resolution data at {path:?}")]
struct ResolutionReadError {
    path: PathBuf,
    #[source]
    cause: std::io::Error,
}

fn resolution_bytes(path: &std::path::Path) -> std::io::Result<Vec<u8>> {
    std::fs::read(path).map_err(|cause| {
        std::io::Error::other(ResolutionReadError {
            path: path.into(),
            cause,
        })
    })
}
