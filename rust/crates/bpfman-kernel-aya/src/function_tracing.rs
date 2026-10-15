//! Function tracing: Aya handles vmlinux; the module bridge reuses its maps.
use crate::{
    AyaLink, AyaTracing, Kernel,
    failure::{LoadCause, filesystem},
    pin_syscall, syscall,
    tracing_btf::Parts,
};
use bpfman_fs::{
    KernelResult, PinSource, PinnedProgram, PreparedTracingAttach, ProgramInspection,
    RuntimeWriter, TracingKernel, TracingProgram,
};
use bpfman_kernel::{Error, TracingLinks};
use bpfman_model::{TracingKind, TracingLink};
use std::{
    io,
    num::NonZeroU32,
    os::fd::{AsFd, AsRawFd, OwnedFd},
};

fn failure(source: impl std::error::Error + Send + Sync + 'static) -> LoadCause {
    LoadCause::Tracing(Box::new(source))
}

pub(super) fn attach_type(kind: TracingKind) -> u32 {
    match kind {
        TracingKind::Fentry => 24,
        TracingKind::Fexit => 25,
    }
}

struct Target {
    id: u32,
    module: Option<OwnedFd>,
    base: Vec<u8>,
}

fn resolve(function: &str) -> io::Result<Target> {
    let base = std::fs::read("/sys/kernel/btf/vmlinux")?;
    let parts = Parts::parse(&base)?;
    let (base_count, found) = parts.function(function, None)?;
    if let Some(id) = found {
        return Ok(Target {
            id,
            module: None,
            base,
        });
    }
    let mut current = 0;
    while let Some(id) = syscall::btf_next(current)? {
        current = id;
        let (fd, bytes, name, kernel) = match syscall::btf(id) {
            Ok(value) => value,
            // Objects may disappear between enumeration and opening.
            Err(error) if error.raw_os_error() == Some(libc::ENOENT) => continue,
            Err(error) => return Err(error),
        };
        if !kernel || name == "vmlinux" {
            continue;
        }
        let (_, found) = Parts::parse(&bytes)?.function(function, Some((&parts, base_count)))?;
        if let Some(id) = found {
            return Ok(Target {
                id,
                module: Some(fd),
                base,
            });
        }
    }
    Err(io::Error::new(
        io::ErrorKind::NotFound,
        format!("kernel function {function:?} not found in vmlinux or module BTF"),
    ))
}

pub(super) fn load(
    bpf: &mut aya::Ebpf,
    bytes: &[u8],
    name: &str,
    function: &str,
    kind: TracingKind,
) -> Result<Option<OwnedFd>, LoadCause> {
    let target = resolve(function).map_err(failure)?;
    let base = aya::Btf::parse(&target.base, Default::default()).map_err(failure)?;
    let Some(module) = target.module else {
        let program = bpf
            .program_mut(name)
            .ok_or_else(|| LoadCause::Invalid("selected tracing program disappeared".into()))?;
        match kind {
            TracingKind::Fentry => {
                let p: &mut aya::programs::FEntry = program
                    .try_into()
                    .map_err(|e| LoadCause::Program(Box::new(e)))?;
                p.load(function, &base)
                    .map_err(|e| LoadCause::Program(Box::new(e)))?;
            }
            TracingKind::Fexit => {
                let p: &mut aya::programs::FExit = program
                    .try_into()
                    .map_err(|e| LoadCause::Program(Box::new(e)))?;
                p.load(function, &base)
                    .map_err(|e| LoadCause::Program(Box::new(e)))?;
            }
        }
        return Ok(None);
    };

    // Aya 0.14 keeps attach_btf_obj_fd private for FEntry/FExit. Retain its
    // allocated/reused maps, and relocate the selected program with aya-obj's
    // public operations before the narrow native load. No Aya layout access.
    let mut object = aya_obj::Object::parse(bytes).map_err(|e| LoadCause::Parse(Box::new(e)))?;
    // Module tracing already requires kernel BTF FUNC/DATASEC support. Lower
    // optional newer BTF kinds to the portable forms Aya itself supplies.
    let features =
        aya_obj::btf::BtfFeatures::new(true, true, true, false, false, false, false, false);
    object.fixup_and_sanitize_btf(&features).map_err(failure)?;
    object.relocate_btf(&base).map_err(failure)?;
    let maps = object.maps.clone();
    let mut relocated = Vec::new();
    for (map_name, map) in &maps {
        let data = bpf
            .map(map_name)
            .ok_or_else(|| LoadCause::Invalid("tracing map disappeared".into()))?;
        relocated.push((
            map_name.as_str(),
            crate::object::map_data(data).fd().as_fd().as_raw_fd(),
            map,
        ));
    }
    let sections = object
        .functions
        .keys()
        .map(|(section, _)| *section)
        .collect();
    object
        .relocate_maps(relocated.into_iter(), &sections)
        .map_err(failure)?;
    object.relocate_calls(&sections).map_err(failure)?;
    let program = object
        .programs
        .get(name)
        .ok_or_else(|| LoadCause::Invalid("tracing program disappeared".into()))?;
    let code = object
        .functions
        .get(&program.function_key())
        .ok_or_else(|| LoadCause::Invalid("tracing instructions disappeared".into()))?;
    let btf = object
        .btf
        .as_ref()
        .map(|b| pin_syscall::load_btf(&b.to_bytes()))
        .transpose()
        .map_err(failure)?;
    let flags = match program.section {
        aya_obj::ProgramSection::FEntry { sleepable }
        | aya_obj::ProgramSection::FExit { sleepable } => {
            if sleepable {
                1 << 4
            } else {
                0
            }
        }
        _ => return Err(LoadCause::Invalid("tracing section changed".into())),
    };
    pin_syscall::load_tracing(
        name,
        program,
        code,
        btf.as_ref().map(AsFd::as_fd),
        module.as_fd(),
        target.id,
        kind,
        flags,
    )
    .map(Some)
    .map_err(failure)
}

impl TracingLinks for Kernel {
    type PreparedTracing = PreparedTracingAttach<AyaTracing>;

    fn prepare_tracing(
        &self,
        w: &RuntimeWriter<'_>,
        id: NonZeroU32,
        target: &TracingLink,
    ) -> Result<Self::PreparedTracing, Error> {
        w.prepare_tracing_attach(self, id, target)
            .map_err(filesystem)
    }

    fn attach_tracing(
        &self,
        w: &RuntimeWriter<'_>,
        prepared: Self::PreparedTracing,
        target: &TracingLink,
    ) -> Result<Self::LiveTracepoint, Error> {
        prepared.attach(w, target).map_err(filesystem)
    }
}

impl TracingKernel for Kernel {
    type Tracing = AyaTracing;

    fn tracing_at(
        &self,
        source: PinSource<'_>,
        target: &TracingLink,
    ) -> KernelResult<(PinnedProgram, AyaTracing)> {
        let observation = self.program_at(source)?;
        let fd = pin_syscall::open(source.path())?;
        let info = syscall::program_info(fd.as_fd())?;
        let expected = resolve(target.function.as_str())?;
        let module_id = match &expected.module {
            Some(fd) => syscall::btf_id(fd.as_fd())?,
            None => {
                let (_, _, name, kernel) = syscall::btf(info.attach_btf_obj_id)?;
                if !kernel || name != "vmlinux" {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "tracing target is not vmlinux",
                    )
                    .into());
                }
                info.attach_btf_obj_id
            }
        };
        if info.id != observation.id
            || info.type_ != 26
            || info.attach_btf_id != expected.id
            || info.attach_btf_obj_id != module_id
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "pinned tracing program differs from its load-time target",
            )
            .into());
        }
        // bpf_prog_info does not expose expected_attach_type. The kernel enforces
        // the requested entry/exit type again during BPF_LINK_CREATE.
        Ok((
            observation,
            AyaTracing {
                fd,
                kind: target.kind,
            },
        ))
    }
}

impl TracingProgram for AyaTracing {
    type Link = AyaLink;

    fn attach(&mut self) -> KernelResult<AyaLink> {
        let fd = pin_syscall::attach_tracing(self.fd.as_fd(), self.kind)?;
        Ok(AyaLink(crate::NativeLink::Tracing(fd)))
    }
}
