//! The same atomic TC snapshot contract applies to every backend.
#![allow(clippy::expect_used)]

use bpfman_fs::{RuntimeDirectory, RuntimeLayout, RuntimeWriter};
use bpfman_lock::AcquireOptions;
use bpfman_model::{InterfaceName, LinkDetails, LinkState, ProgramSpec, TcLink, XdpKey};
use bpfman_store::{
    CommitLoad, LinkReader, LoadRecord, OpenStore, ProgramReader, TcCommit, TcStore,
};
use std::{
    collections::BTreeMap,
    num::{NonZeroU32, NonZeroU64},
    time::Duration,
};

fn n(raw: u32) -> NonZeroU32 {
    NonZeroU32::new(raw).expect("id")
}

fn writer<T>(r: &RuntimeDirectory, f: impl FnOnce(&RuntimeWriter<'_>) -> T) -> T {
    r.with_writer(
        AcquireOptions {
            timeout: Duration::from_secs(1),
            cancelled: None,
        },
        |w| f(&w),
    )
    .expect("writer")
}

fn desired<'a>(
    base: TcCommit<'a>,
    first: &'a TcLink,
    second: &'a TcLink,
    id: NonZeroU64,
) -> Vec<bpfman_store::TcMemberCommit<'a>> {
    vec![
        bpfman_store::TcMemberCommit {
            identity: bpfman_store::XdpMemberId::Existing(id),
            attachment: TcCommit {
                details: first,
                extension_link_id: n(90),
                ..base
            },
        },
        bpfman_store::TcMemberCommit {
            identity: bpfman_store::XdpMemberId::New,
            attachment: TcCommit {
                details: second,
                extension_link_id: n(91),
                ..base
            },
        },
    ]
}

fn contract<S: OpenStore + CommitLoad + TcStore + Clone>(backend: S)
where
    S::Reader: LinkReader,
{
    let temp = tempfile::tempdir().expect("tempdir");
    let layout = RuntimeLayout::try_from(temp.path().join("one")).expect("layout");
    let runtime = RuntimeDirectory::open_or_create(layout.clone()).expect("runtime");
    let other = RuntimeDirectory::open_or_create(
        RuntimeLayout::try_from(temp.path().join("two")).expect("layout"),
    )
    .expect("other");
    let key = XdpKey {
        nsid: NonZeroU64::MIN,
        ifindex: n(7),
    };
    let details = TcLink {
        revision: NonZeroU32::MIN,
        slot: bpfman_model::XdpSlot::FIRST,
        netns: Default::default(),
        key,
        interface: "veth0".parse::<InterfaceName>().expect("name"),
        priority: 100,
        proceed_on: ((1 << 0) | (1 << 3) | (1 << 31))
            .try_into()
            .expect("TC mask"),
        dispatcher_id: n(55),
        filter_handle: n(77),
        filter_priority: 50,
    };
    let metadata = BTreeMap::from([("owner".into(), "contract".into())]);
    let request = || TcCommit {
        program_id: n(42),
        details: &details,
        extension_link_id: n(66),
        metadata: &metadata,
        created_at: "2026-10-05T00:00:00Z",
    };
    let mut reader = writer(&runtime, |w| backend.open(w).expect("open"));
    writer(&runtime, |w| {
        assert!(backend.preflight_tc(w, key, n(42)).is_err());
        assert!(backend.commit_tc(w, request()).is_err());
        backend
            .commit_program(
                w,
                LoadRecord {
                    id: n(42),
                    spec: &ProgramSpec::Tc("pass".try_into().expect("symbol")),
                    source: "source.o",
                    license: "GPL",
                    created_at: "2026-10-05T00:00:00Z",
                    metadata: &BTreeMap::new(),
                    globals: &BTreeMap::new(),
                },
            )
            .expect("program");
        backend.preflight_tc(w, key, n(42)).expect("vacant");
        let mut wrong = details.clone();
        wrong.filter_priority = 0;
        assert!(
            backend
                .commit_tc(
                    w,
                    TcCommit {
                        details: &wrong,
                        ..request()
                    }
                )
                .is_err()
        );
        wrong.filter_priority = 50;
        wrong.priority = i32::MAX as u32 + 1;
        assert!(
            backend
                .commit_tc(
                    w,
                    TcCommit {
                        details: &wrong,
                        ..request()
                    }
                )
                .is_err()
        );
        assert!(reader.read_links().expect("no partial links").is_empty());

        let link = backend.commit_tc(w, request()).expect("snapshot");
        assert_eq!(link.details, LinkDetails::Tc(details.clone()));
        assert_eq!(link.state, LinkState::Attached { kernel_id: n(66) });
        assert_eq!(link.metadata, metadata);
        assert_eq!(
            link.pin_path,
            layout.tc_extension_path(key).to_str().expect("path")
        );
        let (snapshot, _) = backend
            .observe_tc(w, link.id)
            .expect("read")
            .expect("snapshot");
        assert_eq!(snapshot.member, link);
        assert_eq!(snapshot.program_name.as_str(), "pass");
        assert_eq!(
            reader.read_links().expect("links"),
            std::slice::from_ref(&link)
        );
        assert_eq!(reader.read_records().expect("programs")[0].links, [link.id]);
        assert!(backend.preflight_tc(w, key, n(42)).is_err());
        assert!(backend.commit_tc(w, request()).is_err());
        assert_eq!(
            backend
                .observe_tc(w, link.id)
                .expect("unchanged")
                .map(|s| s.0),
            Some(snapshot)
        );
        let (_, receipt) = backend
            .observe_tc(w, link.id)
            .expect("observe")
            .expect("present");
        let (_, stale) = backend
            .observe_tc(w, link.id)
            .expect("observe")
            .expect("present");
        let error =
            writer(&other, |other| backend.delete_tc(other, receipt)).expect_err("wrong runtime");
        backend
            .delete_tc(w, error.remaining)
            .map_err(|e| e.cause)
            .expect("correct runtime");
        assert!(reader.read_links().expect("empty").is_empty());
        assert!(backend.observe_tc(w, link.id).expect("absent").is_none());
        assert!(
            reader.read_records().expect("program survives")[0]
                .links
                .is_empty()
        );
        let fresh = backend.commit_tc(w, request()).expect("reattach");
        assert_ne!(fresh.id, link.id);
        assert!(backend.delete_tc(w, stale).is_err());
        assert_eq!(
            reader.read_links().expect("retained"),
            std::slice::from_ref(&fresh)
        );
        let (before, receipt) = backend
            .observe_tc_dispatcher(w, key)
            .expect("observe complete")
            .expect("present");
        let (_, stale) = backend
            .observe_tc_dispatcher(w, key)
            .expect("observe stale")
            .expect("present");
        let mut next = details.clone();
        next.revision = n(2);
        next.dispatcher_id = n(88);
        let mut second = next.clone();
        second.slot = 1usize.try_into().expect("slot");
        second.priority = 200;
        // Rejection returns the same receipt, with no partial member or header publication.
        let mut wrong = second.clone();
        wrong.filter_handle = n(999);
        let invalid = desired(request(), &next, &wrong, fresh.id);
        let receipt = backend
            .replace_tc(
                w,
                receipt,
                bpfman_store::TcReplace {
                    updated_at: "2026-10-08T00:00:00Z",
                    members: &invalid,
                },
            )
            .expect_err("changed filter refused")
            .remaining;
        assert_eq!(
            backend
                .observe_tc_dispatcher(w, key)
                .expect("unchanged")
                .expect("present")
                .0,
            before
        );
        let valid = desired(request(), &next, &second, fresh.id);
        let receipt = writer(&other, |other| {
            backend.replace_tc(
                other,
                receipt,
                bpfman_store::TcReplace {
                    updated_at: "2026-10-08T00:00:00Z",
                    members: &valid,
                },
            )
        })
        .expect_err("foreign writer")
        .remaining;
        let published = backend
            .replace_tc(
                w,
                receipt,
                bpfman_store::TcReplace {
                    updated_at: "2026-10-08T00:00:00Z",
                    members: &valid,
                },
            )
            .map_err(|f| f.cause)
            .expect("complete publication");
        assert_eq!(published.members().len(), 2);
        assert_eq!(published.members()[0].member.id, fresh.id);
        assert_ne!(published.members()[1].member.id, fresh.id);
        assert_eq!(published.members()[0].member.metadata, metadata);
        assert_eq!(reader.read_links().expect("whole snapshot").len(), 2);
        assert_eq!(reader.read_records().expect("all links")[0].links.len(), 2);
        assert!(
            backend.observe_tc(w, fresh.id).is_err(),
            "singleton API cannot truncate membership"
        );
        assert!(
            backend.delete_tc(w, stale).is_err(),
            "old singleton cannot delete replaced dispatcher"
        );
        let (published_again, receipt) = backend
            .observe_tc_member_dispatcher(w, fresh.id)
            .expect("member lookup")
            .expect("present");
        assert_eq!(published_again, published);
        assert_eq!(
            reader.read_tc_dispatchers().expect("read only"),
            std::slice::from_ref(&published)
        );
        let survivor = &published.members()[1];
        let mut last = survivor.details.clone();
        last.revision = n(3);
        last.slot = bpfman_model::XdpSlot::FIRST;
        last.dispatcher_id = n(92);
        let remaining = [bpfman_store::TcMemberCommit {
            identity: bpfman_store::XdpMemberId::Existing(survivor.member.id),
            attachment: TcCommit {
                program_id: survivor.member.program_id,
                details: &last,
                extension_link_id: n(93),
                metadata: &survivor.member.metadata,
                created_at: &survivor.member.created_at,
            },
        }];
        let published = backend
            .replace_tc(
                w,
                receipt,
                bpfman_store::TcReplace {
                    updated_at: "2026-10-08T00:01:00Z",
                    members: &remaining,
                },
            )
            .map_err(|f| f.cause)
            .expect("survivor publication");
        assert_eq!(published.members()[0].member.id, survivor.member.id);
        let (_, receipt) = backend
            .observe_tc(w, survivor.member.id)
            .expect("last")
            .expect("present");
        backend
            .delete_tc(w, receipt)
            .map_err(|f| f.cause)
            .expect("last delete");
        assert!(reader.read_tc_dispatchers().expect("empty").is_empty());
    });
}

#[test]
fn sqlite_snapshot_contract() {
    contract(bpfman_store_sqlite::Backend);
}

#[test]
fn json_snapshot_contract() {
    contract(bpfman_store_json::Backend);
}
