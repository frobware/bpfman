use bpfman_model::{KernelProgram, ProgramSpec, ProgramType, StoredProgram, Symbol};
use bpfman_proto::v1 as pb;
use grpcuds::Status;

pub(super) fn program_type(value: i32) -> Result<ProgramType, Status> {
    Ok(match value {
        0 => ProgramType::Xdp,
        1 => ProgramType::Tc,
        2 => ProgramType::Tracepoint,
        3 => ProgramType::Kprobe,
        4 => ProgramType::Uprobe,
        5 => ProgramType::Fentry,
        6 => ProgramType::Fexit,
        7 => ProgramType::Tcx,
        8 => ProgramType::Lsm,
        _ => return Err(Status::invalid_argument("unknown program type")),
    })
}

pub(super) fn wire_type(kind: ProgramType) -> u32 {
    match kind {
        ProgramType::Xdp => 0,
        ProgramType::Tc => 1,
        ProgramType::Tracepoint => 2,
        ProgramType::Kprobe | ProgramType::Kretprobe => 3,
        ProgramType::Uprobe | ProgramType::Uretprobe => 4,
        ProgramType::Fentry => 5,
        ProgramType::Fexit => 6,
        ProgramType::Tcx => 7,
        ProgramType::Lsm => 8,
    }
}

pub(super) fn invalid(error: impl std::fmt::Display) -> Status {
    Status::invalid_argument(error.to_string())
}

pub(super) fn spec(
    info: &pb::LoadInfo,
    metadata: &std::collections::BTreeMap<String, String>,
) -> Result<ProgramSpec, Status> {
    let name = Symbol::try_from(info.name.as_str()).map_err(invalid)?;
    let target = || {
        use pb::prog_specific_info::Info;
        let raw = match info.info.as_ref().and_then(|info| info.info.as_ref()) {
            Some(Info::FentryLoadInfo(info)) => &info.fn_name,
            Some(Info::FexitLoadInfo(info)) => &info.fn_name,
            Some(Info::LsmLoadInfo(info)) => &info.hook_name,
            None => return Err(Status::invalid_argument("load-time target is required")),
        };
        Symbol::try_from(raw.as_str()).map_err(invalid)
    };
    let requested = program_type(info.program_type)?;
    // Existing operator clients carry return-probe selection in this metadata
    // key because the protobuf enum has only KPROBE and UPROBE.
    let kind = metadata
        .get(&format!("bpfman.io/actual-type:{}", info.name))
        .and_then(|value| value.parse().ok())
        .unwrap_or(requested);
    Ok(match kind {
        ProgramType::Xdp => ProgramSpec::Xdp(name),
        ProgramType::Tc => ProgramSpec::Tc(name),
        ProgramType::Tcx => ProgramSpec::Tcx(name),
        ProgramType::Tracepoint => ProgramSpec::Tracepoint(name),
        ProgramType::Kprobe => ProgramSpec::Kprobe(name),
        ProgramType::Uprobe => ProgramSpec::Uprobe(name),
        ProgramType::Fentry => ProgramSpec::Fentry {
            name,
            target: target()?,
        },
        ProgramType::Fexit => ProgramSpec::Fexit {
            name,
            target: target()?,
        },
        ProgramType::Lsm => ProgramSpec::Lsm {
            name,
            hook: target()?,
        },
        ProgramType::Kretprobe => ProgramSpec::Kretprobe(name),
        ProgramType::Uretprobe => ProgramSpec::Uretprobe(name),
    })
}

pub(super) fn policy(image: &pb::BytecodeImage) -> Result<bpfman_model::ImagePullPolicy, Status> {
    Ok(match image.image_pull_policy {
        0 => bpfman_model::ImagePullPolicy::Always,
        1 => bpfman_model::ImagePullPolicy::IfNotPresent,
        2 => bpfman_model::ImagePullPolicy::Never,
        _ => return Err(Status::invalid_argument("invalid pull policy")),
    })
}

pub(super) fn image(image: &pb::BytecodeImage) -> pb::BytecodeImage {
    let mut image = image.clone();
    if image.username.as_deref().is_none_or(str::is_empty)
        || image.password.as_deref().is_none_or(str::is_empty)
    {
        image.username = None;
        image.password = None;
    }
    image
}

pub(super) fn info(
    record: &StoredProgram,
    users: &[std::num::NonZeroU32],
    links: bool,
) -> pb::ProgramInfo {
    pb::ProgramInfo {
        name: record.spec.name().as_str().into(),
        bytecode: Some(pb::BytecodeLocation {
            location: Some(pb::bytecode_location::Location::File(
                record.object_path.clone(),
            )),
        }),
        links: if links {
            record.links.iter().map(|id| id.get() as u32).collect()
        } else {
            Vec::new()
        },
        global_data: record
            .globals
            .iter()
            .map(|(k, v)| (k.clone(), v.clone().unwrap_or_default()))
            .collect(),
        map_owner_id: (record.map_set != record.id).then_some(record.map_set.get()),
        map_pin_path: record.map_path.clone(),
        map_used_by: users.iter().map(ToString::to_string).collect(),
        metadata: record.metadata.clone(),
    }
}

#[derive(Clone, Copy)]
pub(super) enum View {
    Load,
    List,
    Get,
}

pub(super) fn kernel(
    record: &StoredProgram,
    kernel: &KernelProgram,
    view: View,
) -> pb::KernelProgramInfo {
    pb::KernelProgramInfo {
        id: kernel.id.get(),
        name: kernel.name.clone(),
        program_type: wire_type(record.spec.kind()),
        loaded_at: kernel.loaded_at.clone().unwrap_or_default(),
        tag: kernel.tag.clone(),
        gpl_compatible: !matches!(view, View::List) && record.gpl_compatible,
        map_ids: kernel.map_ids.clone().unwrap_or_default(),
        btf_id: kernel.btf_id.unwrap_or_default(),
        bytes_xlated: kernel.xlated_size,
        bytes_jited: kernel.jited_size,
        jited: matches!(view, View::Load) && kernel.jited_size > 0,
        bytes_memlock: if matches!(view, View::Load) {
            kernel.memlock.unwrap_or_default() as u32
        } else {
            0
        },
        verified_insns: if matches!(view, View::Load) {
            kernel.verified_insns
        } else {
            0
        },
    }
}

pub(super) fn internal(error: impl std::error::Error) -> Status {
    Status::internal(diagnostic(&error))
}

fn diagnostic(error: &dyn std::error::Error) -> String {
    let mut parts = vec![error.to_string()];
    let mut next = error.source();
    while let Some(cause) = next {
        let message = cause.to_string();
        if parts.last() != Some(&message) {
            parts.push(message);
        }
        next = cause.source();
    }
    parts.join(": ")
}

pub(super) fn observation(error: bpfman_runtime::ObservationError) -> Status {
    use bpfman_runtime::ObservationErrorKind;
    match error.kind() {
        ObservationErrorKind::Cancelled => Status::cancelled("request cancelled"),
        ObservationErrorKind::NotFound => Status::not_found(error.to_string()),
        _ => internal(error),
    }
}

pub(super) fn link(kind: bpfman_runtime::LinkErrorKind, error: impl std::error::Error) -> Status {
    use bpfman_runtime::LinkErrorKind;
    match kind {
        LinkErrorKind::Cancelled => Status::cancelled("request cancelled"),
        LinkErrorKind::NotFound => Status::not_found(error.to_string()),
        LinkErrorKind::TimedOut => Status::deadline_exceeded("writer lock timeout"),
        _ => internal(error),
    }
}

pub(super) fn attachment(
    cause: Option<&bpfman_runtime::LinkCause>,
    kind: bpfman_runtime::LinkErrorKind,
    error: impl std::error::Error,
) -> Status {
    if cause.is_some_and(|cause| cause.is_program_mismatch()) {
        return Status::failed_precondition(error.to_string());
    }
    if cause.is_some_and(|cause| cause.is_invalid_input()) {
        return invalid(error);
    }
    link(kind, error)
}

pub(super) fn application(error: bpfman_runtime::Error) -> Status {
    match error.kind() {
        bpfman_runtime::ErrorKind::Cancelled => Status::cancelled("request cancelled"),
        bpfman_runtime::ErrorKind::TimedOut => Status::deadline_exceeded("writer lock timeout"),
        _ => internal(error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn operator_metadata_selects_return_probes_without_rewriting_wire_types() {
        let info = pb::LoadInfo {
            name: "ret".into(),
            program_type: 3,
            ..Default::default()
        };
        let metadata = std::collections::BTreeMap::from([(
            "bpfman.io/actual-type:ret".into(),
            "kretprobe".into(),
        )]);
        assert!(matches!(
            spec(&info, &metadata),
            Ok(ProgramSpec::Kretprobe(_))
        ));
        assert_eq!(wire_type(ProgramType::Kretprobe), 3);
        assert_eq!(wire_type(ProgramType::Uretprobe), 4);
        let mut invalid_info = info;
        invalid_info.program_type = 99;
        assert!(spec(&invalid_info, &metadata).is_err());
    }

    #[test]
    fn incomplete_registry_credentials_are_anonymous_at_the_frontend() {
        for (username, password) in [
            (None, None),
            (Some("user"), None),
            (None, Some("pass")),
            (Some(""), Some("pass")),
            (Some("user"), Some("")),
        ] {
            let input = pb::BytecodeImage {
                url: "registry/test".into(),
                username: username.map(str::to_owned),
                password: password.map(str::to_owned),
                ..Default::default()
            };
            let converted = image(&input);
            assert_eq!((converted.username, converted.password), (None, None));
        }
    }
}
