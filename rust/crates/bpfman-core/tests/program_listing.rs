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
            ..Default::default()
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
                ..Default::default()
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
            ..Default::default()
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

#[test]
fn metadata_selection_is_identical_for_summaries_records_and_inventory() {
    use bpfman_core::{list_programs, select_records};
    use bpfman_model::StoredProgramSummary;
    use std::collections::BTreeMap;

    let mut record = sample::record();
    record.metadata = BTreeMap::from([
        ("bpfman.io/application".into(), "demo".into()),
        ("owner".into(), "acme".into()),
        ("arbitrary key".into(), "value=with spaces".into()),
        ("empty".into(), String::new()),
    ]);
    let summary = StoredProgramSummary::new(
        record.id,
        record.spec.name().as_str().into(),
        record.spec.kind(),
        record.metadata.clone(),
        Vec::new(),
    );
    let mut stray = sample::kernel();
    stray.id = NonZeroU32::new(43).expect("unmanaged id");
    let pairs = |key: &str, value: &str| BTreeMap::from([(key.into(), value.into())]);
    for (metadata, application, raw, expected) in [
        (pairs("owner", "acme"), None, "owner in (acme,other)", true),
        (pairs("owner", "other"), None, "owner=acme", false),
        (pairs("arbitrary key", "value=with spaces"), None, "", true),
        (pairs("empty", ""), None, "empty=", true),
        (pairs("absent", ""), None, "!absent", false),
        (BTreeMap::new(), None, "!absent", true),
        (BTreeMap::new(), None, "owner!=acme", false),
        (
            pairs("bpfman.io/application", "other"),
            Some("demo"),
            "",
            true,
        ),
        (
            pairs("bpfman.io/application", "other"),
            Some("demo"),
            "bpfman.io/application=other",
            false,
        ),
        (pairs("bpfman.io/application", "other"), Some(""), "", false),
    ] {
        let filter = ProgramFilter {
            metadata,
            application: application.map(Into::into),
            selector: raw.parse().expect("selector"),
            ..Default::default()
        };
        assert_eq!(
            list_programs(vec![summary.clone()], &filter).len(),
            usize::from(expected)
        );
        assert_eq!(
            select_records(vec![record.clone()], &filter).len(),
            usize::from(expected)
        );
        let rows = list_program_entries(vec![record.clone()], vec![stray.clone()], &filter);
        assert_eq!(rows.len(), usize::from(expected));
        assert!(
            rows.iter()
                .all(|row| matches!(row, ProgramListEntry::Managed(_)))
        );
    }

    // An empty selector remains unrestricted, but negative/existence requirements
    // never match unmanaged programs, even if they would match an empty map.
    for raw in [
        "",
        "  ",
        "!missing",
        "missing!=value",
        "missing notin (value)",
    ] {
        let filter = ProgramFilter {
            selector: raw.parse().expect("selector"),
            ..Default::default()
        };
        assert_eq!(
            list_program_entries(Vec::new(), vec![stray.clone()], &filter).len(),
            usize::from(raw.trim().is_empty())
        );
    }
}

#[test]
fn attachment_filters_use_kernel_links_and_exclude_unmanaged_rows() {
    use bpfman_core::{AttachmentFilter, select_program_attachments};
    use bpfman_model::{
        KernelLink, KernelLinkDetails, LinkDetails, LinkState, ObservedLink, StoredLink,
    };
    use std::num::NonZeroU64;

    let record = sample::record();
    let rows = list_program_entries(
        vec![record.clone()],
        vec![sample::kernel()],
        &Default::default(),
    );
    let stray = ProgramListEntry::KernelOnly(sample::kernel());
    let mut rows = rows;
    rows.push(stray);
    let mut link = ObservedLink {
        record: StoredLink {
            id: NonZeroU64::MIN,
            program_id: record.id,
            details: LinkDetails::Tracepoint("syscalls/sys_enter_kill".parse().expect("target")),
            state: LinkState::Attached {
                kernel_id: NonZeroU32::MIN,
            },
            pin_path: "/runtime/fs/links/1".into(),
            metadata: Default::default(),
            created_at: record.created_at,
        },
        kernel: None,
        pin_present: true,
    };

    // A stored kernel ID and a surviving pin never substitute for observation.
    assert!(
        select_program_attachments(rows.clone(), &[link.clone()], AttachmentFilter::Attached)
            .is_empty()
    );
    assert_eq!(
        select_program_attachments(rows.clone(), &[link.clone()], AttachmentFilter::Unattached)
            .len(),
        1
    );

    link.kernel = Some(KernelLink {
        id: NonZeroU32::MIN,
        program_id: record.id,
        details: KernelLinkDetails::PerfEvent,
    });
    link.pin_present = false;
    assert_eq!(
        select_program_attachments(rows.clone(), &[link.clone()], AttachmentFilter::Attached).len(),
        1
    );
    assert!(
        select_program_attachments(rows.clone(), &[link.clone()], AttachmentFilter::Unattached)
            .is_empty()
    );

    // Only a link associated with this managed program can establish attachment.
    link.record.program_id = NonZeroU32::MIN;
    assert!(
        select_program_attachments(rows.clone(), &[link], AttachmentFilter::Attached).is_empty()
    );
    assert_eq!(
        select_program_attachments(rows, &[], AttachmentFilter::Unattached).len(),
        1
    );
}
