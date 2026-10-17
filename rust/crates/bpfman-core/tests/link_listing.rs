//! Stored link selection joins owner metadata without interpreting attachment state.
#![allow(clippy::expect_used)]

use bpfman_core::{LinkFilter, ProgramFilter, list_links};
use bpfman_model::{
    LinkDetails, LinkKind, LinkState, ProgramType, StoredLink, StoredProgramSummary,
};
use std::{
    collections::BTreeMap,
    num::{NonZeroU32, NonZeroU64},
};

fn link(id: u64, owner: u32) -> StoredLink {
    StoredLink {
        id: NonZeroU64::new(id).expect("link"),
        program_id: NonZeroU32::new(owner).expect("program"),
        details: LinkDetails::Tracepoint("syscalls/sys_enter_kill".parse().expect("target")),
        state: LinkState::Pending,
        pin_path: "/unobserved".into(),
        metadata: BTreeMap::from([("owner".into(), "link-owner".into())]),
        created_at: "2026-10-17T00:00:00Z".into(),
    }
}

#[test]
fn owner_and_link_filters_compose_without_losing_pending_or_orphan_intent() {
    let first = link(2, 42);
    let mut second = link(1, 43);
    second.details = LinkDetails::Kprobe(bpfman_model::KprobeLink {
        function: "target".parse().expect("function"),
        offset: 0,
        retprobe: true,
    });
    second.state = LinkState::Attached {
        kernel_id: NonZeroU32::MIN,
    };
    let orphan = link(3, 44);
    let links = vec![orphan.clone(), first.clone(), second.clone()];
    let programs = vec![
        StoredProgramSummary::new(
            first.program_id,
            "trace".into(),
            ProgramType::Tracepoint,
            BTreeMap::from([
                ("owner".into(), "program-owner".into()),
                ("bpfman.io/application".into(), "demo".into()),
                ("empty".into(), "".into()),
                ("arbitrary key".into(), "raw=value with spaces".into()),
            ]),
            vec![],
        ),
        StoredProgramSummary::new(
            second.program_id,
            "probe".into(),
            ProgramType::Kretprobe,
            BTreeMap::from([("owner".into(), "other".into())]),
            vec![],
        ),
    ];
    let select = |filter: LinkFilter| {
        list_links(links.clone(), programs.clone(), &filter)
            .into_iter()
            .map(|link| link.id.get())
            .collect::<Vec<_>>()
    };
    assert_eq!(select(Default::default()), [1, 2, 3]);
    assert_eq!(
        select(LinkFilter {
            program_id: Some(orphan.program_id),
            ..Default::default()
        }),
        [3]
    );
    assert_eq!(
        select(LinkFilter {
            kinds: vec![LinkKind::Kprobe],
            ..Default::default()
        }),
        []
    );
    assert_eq!(
        select(LinkFilter {
            kinds: vec![LinkKind::Kretprobe],
            ..Default::default()
        }),
        [1]
    );
    assert_eq!(
        select(LinkFilter {
            kinds: vec![LinkKind::Kretprobe, LinkKind::Tracepoint],
            ..Default::default()
        }),
        [1, 2, 3]
    );
    assert_eq!(
        select(LinkFilter {
            program: Some(Default::default()),
            ..Default::default()
        }),
        [1, 2]
    );

    let matching = ProgramFilter {
        application: Some("demo".into()),
        metadata: BTreeMap::from([
            ("bpfman.io/application".into(), "overridden".into()),
            ("owner".into(), "program-owner".into()),
            ("empty".into(), "".into()),
            ("arbitrary key".into(), "raw=value with spaces".into()),
        ]),
        ..Default::default()
    };
    let filter = LinkFilter {
        kinds: vec![LinkKind::Tracepoint],
        program_id: Some(first.program_id),
        program: Some(matching.clone()),
    };
    let selected = list_links(links.clone(), programs.clone(), &filter);
    assert_eq!(selected.as_slice(), std::slice::from_ref(&first));
    assert_eq!(selected[0].metadata["owner"], "link-owner");
    assert_eq!(selected[0].state, LinkState::Pending);

    for owner in [
        ProgramFilter {
            metadata: BTreeMap::from([("owner".into(), "link-owner".into())]),
            ..Default::default()
        },
        ProgramFilter {
            metadata: BTreeMap::from([("absent".into(), "".into())]),
            ..Default::default()
        },
        ProgramFilter {
            types: vec![ProgramType::Kretprobe],
            ..matching.clone()
        },
    ] {
        assert!(
            select(LinkFilter {
                program: Some(owner),
                ..filter.clone()
            })
            .is_empty()
        );
    }
    assert_eq!(
        select(LinkFilter {
            program: Some(ProgramFilter {
                types: vec![ProgramType::Kprobe, ProgramType::Kretprobe],
                ..Default::default()
            }),
            ..Default::default()
        }),
        [1]
    );
    assert_eq!(
        select(LinkFilter {
            program: Some(ProgramFilter {
                application: Some(String::new()),
                ..Default::default()
            }),
            ..Default::default()
        }),
        [1, 2]
    );
}

#[test]
fn probe_and_tracing_kinds_keep_entry_exit_and_lsm_distinct() {
    for (retprobe, expected) in [(false, LinkKind::Uprobe), (true, LinkKind::Uretprobe)] {
        let details = LinkDetails::Uprobe(
            bpfman_model::UprobeLink::new(
                "libc",
                Some("malloc".try_into().expect("symbol")),
                0,
                0,
                retprobe,
            )
            .expect("target"),
        );
        assert_eq!(details.kind(), expected);
    }
    for (kind, expected) in [
        (bpfman_model::TracingKind::Fentry, LinkKind::Fentry),
        (bpfman_model::TracingKind::Fexit, LinkKind::Fexit),
        (bpfman_model::TracingKind::Lsm, LinkKind::Lsm),
    ] {
        let details = LinkDetails::Tracing(bpfman_model::TracingLink {
            function: "target".try_into().expect("symbol"),
            kind,
        });
        assert_eq!(details.kind(), expected);
    }
    for kind in LinkKind::ALL {
        assert_eq!(kind.as_str().parse::<LinkKind>(), Ok(kind));
    }
    for raw in ["", "Kprobe", "kprobe ", "perf_event", "tracing", "schedcls"] {
        assert!(raw.parse::<LinkKind>().is_err());
    }
}
