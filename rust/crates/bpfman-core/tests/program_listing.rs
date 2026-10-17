//! Go's managed and kernel-only listing/filter contracts over complete observations.
#![allow(clippy::expect_used)]

#[path = "../../../tests/observation.rs"]
mod sample;

use bpfman_core::{ProgramFilter, list_program_entries};
use bpfman_model::{ProgramListEntry, ProgramSpec, ProgramType};
use std::num::NonZeroU32;

#[test]
fn managed_identity_precedes_filtering_and_missing_kernel_records_survive() {
    let mut record = sample::record();
    record.spec = ProgramSpec::Tcx("classifier".try_into().expect("symbol"));
    record
        .metadata
        .insert("bpfman.io/application".into(), "app".into());
    let mut observed = sample::kernel();
    observed.kind = "schedcls".into();
    let mut stray = observed.clone();
    stray.id = NonZeroU32::new(1).expect("id");
    let mut missing = record.clone();
    missing.id = NonZeroU32::new(100).expect("id");
    let rows = list_program_entries(
        vec![missing.clone(), record.clone()],
        vec![observed.clone(), stray.clone()],
        &Default::default(),
    );

    assert_eq!(
        rows.iter().map(|p| p.id().get()).collect::<Vec<_>>(),
        [1, 42, 100]
    );
    assert_eq!(rows[0], ProgramListEntry::KernelOnly(stray.clone()));
    assert!(
        matches!(&rows[2], ProgramListEntry::Managed(p) if p.kernel.is_none() && p.record == missing)
    );

    let selected = list_program_entries(
        vec![record.clone()],
        vec![observed.clone(), stray.clone()],
        &ProgramFilter {
            types: vec![ProgramType::Tc],
            application: None,
        },
    );
    assert_eq!(selected, [ProgramListEntry::KernelOnly(stray.clone())]);

    for application in [
        Some("app".into()),
        Some("other".into()),
        Some(String::new()),
    ] {
        let rows = list_program_entries(
            vec![record.clone()],
            vec![stray.clone(), observed.clone()],
            &ProgramFilter {
                types: Vec::new(),
                application: application.clone(),
            },
        );
        match application.as_deref() {
            Some("app") => assert!(matches!(rows.as_slice(), [ProgramListEntry::Managed(_)])),
            Some("other") => assert!(rows.is_empty()),
            _ => assert_eq!(rows.len(), 2),
        }
    }
}

#[test]
fn fine_types_project_to_kernel_taxonomy_and_unknown_types_remain_visible() {
    for kind in ProgramType::ALL {
        let expected = match kind {
            ProgramType::Tc | ProgramType::Tcx => "schedcls",
            ProgramType::Kprobe
            | ProgramType::Kretprobe
            | ProgramType::Uprobe
            | ProgramType::Uretprobe => "kprobe",
            ProgramType::Fentry | ProgramType::Fexit => "tracing",
            _ => kind.as_str(),
        };
        let mut kernel = sample::kernel();
        kernel.kind = expected.into();
        let mut unknown = kernel.clone();
        unknown.id = NonZeroU32::new(99).expect("id");
        unknown.kind = "programtype(99)".into();
        let filter = ProgramFilter {
            types: vec![kind],
            application: None,
        };
        assert_eq!(
            list_program_entries(Vec::new(), vec![unknown.clone(), kernel.clone()], &filter),
            [ProgramListEntry::KernelOnly(kernel)]
        );
        assert_eq!(
            list_program_entries(Vec::new(), vec![unknown], &Default::default()).len(),
            1
        );
    }
}
