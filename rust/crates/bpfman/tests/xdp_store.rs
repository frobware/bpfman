//! The same atomic XDP snapshot contract applies to every backend.
#![allow(clippy::expect_used)]

use bpfman_fs::{RuntimeDirectory, RuntimeLayout, RuntimeWriter};
use bpfman_lock::AcquireOptions;
use bpfman_model::{InterfaceName, LinkDetails, LinkState, ProgramSpec, XdpKey, XdpLink};
use bpfman_store::{
    CommitLoad, LinkReader, LoadRecord, OpenStore, ProgramReader, XdpCommit, XdpReader, XdpStore,
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

fn contract<S: OpenStore + CommitLoad + XdpStore + Clone>(backend: S)
where
    S::Reader: LinkReader + XdpReader,
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
    let details = XdpLink {
        key,
        interface: "veth0".parse::<InterfaceName>().expect("name"),
        priority: 100,
        proceed_on: Default::default(),
        dispatcher_id: n(55),
        revision: NonZeroU32::MIN,
    };
    let metadata = BTreeMap::from([("owner".into(), "contract".into())]);
    let request = || XdpCommit {
        program_id: n(42),
        details: &details,
        extension_link_id: n(66),
        outer_link_id: n(77),
        metadata: &metadata,
        created_at: "2026-10-05T00:00:00Z",
    };
    let mut reader = writer(&runtime, |w| backend.open(w).expect("open"));
    writer(&runtime, |w| {
        assert!(backend.preflight_xdp(w, key, n(42)).is_err());
        assert!(backend.commit_xdp(w, request()).is_err());
        backend
            .commit_program(
                w,
                LoadRecord {
                    id: n(42),
                    spec: &ProgramSpec::Xdp("pass".try_into().expect("symbol")),
                    source: "source.o",
                    license: "GPL",
                    created_at: "2026-10-05T00:00:00Z",
                    metadata: &BTreeMap::new(),
                    globals: &BTreeMap::new(),
                },
            )
            .expect("program");
        backend.preflight_xdp(w, key, n(42)).expect("vacant");
        let mut wrong = details.clone();
        wrong.revision = n(2);
        assert!(
            backend
                .commit_xdp(
                    w,
                    XdpCommit {
                        details: &wrong,
                        ..request()
                    }
                )
                .is_err()
        );
        wrong.revision = NonZeroU32::MIN;
        wrong.priority = i32::MAX as u32 + 1;
        assert!(
            backend
                .commit_xdp(
                    w,
                    XdpCommit {
                        details: &wrong,
                        ..request()
                    }
                )
                .is_err()
        );
        assert!(
            backend
                .commit_xdp(
                    w,
                    XdpCommit {
                        outer_link_id: n(66),
                        ..request()
                    }
                )
                .is_err()
        );
        assert!(reader.read_xdp(key).expect("still vacant").is_none());
        assert!(reader.read_links().expect("no partial links").is_empty());

        let link = backend.commit_xdp(w, request()).expect("snapshot");
        assert_eq!(link.details, LinkDetails::Xdp(details.clone()));
        assert_eq!(link.state, LinkState::Attached { kernel_id: n(66) });
        assert_eq!(link.metadata, metadata);
        assert_eq!(
            link.pin_path,
            layout
                .xdp_extension_path(key, NonZeroU32::MIN)
                .to_str()
                .expect("path")
        );
        let snapshot = reader.read_xdp(key).expect("read").expect("snapshot");
        assert_eq!(snapshot.member, link);
        assert_eq!(snapshot.program_name.as_str(), "pass");
        assert_eq!(
            reader.read_links().expect("links"),
            std::slice::from_ref(&link)
        );
        assert_eq!(reader.read_records().expect("programs")[0].links, [link.id]);
        assert!(backend.preflight_xdp(w, key, n(42)).is_err());
        assert!(backend.commit_xdp(w, request()).is_err());
        assert_eq!(reader.read_xdp(key).expect("unchanged"), Some(snapshot));
        let (_, receipt) = backend
            .observe_xdp(w, link.id)
            .expect("observe")
            .expect("present");
        let (_, stale) = backend
            .observe_xdp(w, link.id)
            .expect("observe")
            .expect("present");
        let error =
            writer(&other, |other| backend.delete_xdp(other, receipt)).expect_err("wrong runtime");
        backend
            .delete_xdp(w, error.remaining)
            .map_err(|e| e.cause)
            .expect("correct runtime");
        assert!(reader.read_links().expect("empty").is_empty());
        assert!(reader.read_xdp(key).expect("absent").is_none());
        assert!(
            reader.read_records().expect("program survives")[0]
                .links
                .is_empty()
        );
        let fresh = backend.commit_xdp(w, request()).expect("reattach");
        assert_ne!(fresh.id, link.id);
        assert!(backend.delete_xdp(w, stale).is_err());
        assert_eq!(reader.read_links().expect("retained"), [fresh]);
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
