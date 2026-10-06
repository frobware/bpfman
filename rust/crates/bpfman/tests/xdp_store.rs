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
        slot: bpfman_model::XdpSlot::FIRST,
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

use bpfman_store::{
    XdpDispatcherReader, XdpMemberCommit, XdpMemberId, XdpReplace, XdpReplacementStore,
};

fn replacement_contract<S: OpenStore + CommitLoad + XdpReplacementStore + Clone>(backend: S)
where
    S::Reader: LinkReader + XdpReader + XdpDispatcherReader,
{
    for survivor in [0, 1] {
        let temp = tempfile::tempdir().expect("tempdir");
        let layout = RuntimeLayout::try_from(temp.path().join("one")).expect("layout");
        let runtime = RuntimeDirectory::open_or_create(layout.clone()).expect("runtime");
        let other = RuntimeDirectory::open_or_create(
            RuntimeLayout::try_from(temp.path().join("two")).expect("layout"),
        )
        .expect("other");
        let mut reader = writer(&runtime, |w| backend.open(w).expect("open"));
        writer(&other, |w| backend.open(w).expect("open other"));
        let key = XdpKey {
            nsid: NonZeroU64::MIN,
            ifindex: n(7),
        };
        let metadata = BTreeMap::from([("owner".into(), "replacement".into())]);
        let timestamp = "2026-10-05T00:00:00Z";
        let initial = XdpLink {
            slot: bpfman_model::XdpSlot::FIRST,
            key,
            interface: "veth0".parse().expect("interface"),
            priority: 100,
            proceed_on: Default::default(),
            dispatcher_id: n(55),
            revision: n(1),
        };
        writer(&runtime, |w| {
            for (id, name) in [(42, "pass"), (43, "counter")] {
                backend
                    .commit_program(
                        w,
                        LoadRecord {
                            id: n(id),
                            spec: &ProgramSpec::Xdp(name.try_into().expect("symbol")),
                            source: "source.o",
                            license: "GPL",
                            created_at: timestamp,
                            metadata: &metadata,
                            globals: &BTreeMap::new(),
                        },
                    )
                    .expect("program");
            }
            let link = backend
                .commit_xdp(
                    w,
                    XdpCommit {
                        program_id: n(42),
                        details: &initial,
                        extension_link_id: n(66),
                        outer_link_id: n(77),
                        metadata: &metadata,
                        created_at: timestamp,
                    },
                )
                .expect("first member");
            let (old, receipt) = backend
                .observe_xdp_dispatcher(w, key)
                .expect("observe")
                .expect("present");
            let (_, stale_delete) = backend
                .observe_xdp_dispatcher(w, key)
                .expect("observe")
                .expect("present");
            let (_, stale_replace) = backend
                .observe_xdp_dispatcher(w, key)
                .expect("observe")
                .expect("present");
            let mut second = initial.clone();
            second.dispatcher_id = n(56);
            second.revision = n(2);
            second.priority = 0;
            second.proceed_on = 1u32.try_into().expect("mask");
            let mut existing = initial.clone();
            existing.dispatcher_id = n(56);
            existing.revision = n(2);
            existing.slot = 1usize.try_into().expect("slot");
            // Put the new member first to exercise slot reassignment of the survivor.
            let desired = [
                XdpMemberCommit {
                    identity: XdpMemberId::New,
                    attachment: XdpCommit {
                        program_id: n(43),
                        details: &second,
                        extension_link_id: n(67),
                        outer_link_id: n(77),
                        metadata: &metadata,
                        created_at: timestamp,
                    },
                },
                XdpMemberCommit {
                    identity: XdpMemberId::Existing(link.id),
                    attachment: XdpCommit {
                        program_id: n(42),
                        details: &existing,
                        extension_link_id: n(68),
                        outer_link_id: n(77),
                        metadata: &metadata,
                        created_at: timestamp,
                    },
                },
            ];
            let request = || XdpReplace {
                members: &desired,
                updated_at: timestamp,
            };
            let error = writer(&other, |other| {
                backend.replace_xdp(other, receipt, request())
            })
            .expect_err("foreign runtime");
            assert_eq!(
                reader.read_xdp_dispatcher(key).expect("unchanged"),
                Some(old.clone())
            );
            let error = backend
                .replace_xdp(
                    w,
                    error.remaining,
                    XdpReplace {
                        members: &desired,
                        updated_at: "invalid",
                    },
                )
                .expect_err("bad publication time");
            assert_eq!(
                reader.read_xdp_dispatcher(key).expect("unchanged"),
                Some(old)
            );
            let mut receipt = error.remaining;
            for case in 0..15 {
                let mut bad_details = second.clone();
                let mut old_details = existing.clone();
                let bad_metadata = BTreeMap::new();
                match case {
                    0 => bad_details.revision = n(4),
                    1 => bad_details.dispatcher_id = n(55),
                    2 => bad_details.key.ifindex = n(8),
                    3 => bad_details.interface = "veth1".parse().expect("interface"),
                    4 => bad_details.priority = i32::MAX as u32 + 1,
                    5 => old_details.priority = 99,
                    _ => {}
                }
                let mut bad = vec![
                    XdpMemberCommit {
                        identity: XdpMemberId::New,
                        attachment: XdpCommit {
                            details: &bad_details,
                            program_id: if case == 6 { n(999) } else { n(43) },
                            extension_link_id: if case == 7 { n(68) } else { n(67) },
                            outer_link_id: if case == 8 { n(78) } else { n(77) },
                            created_at: if case == 9 { "invalid" } else { timestamp },
                            ..desired[0].attachment
                        },
                    },
                    XdpMemberCommit {
                        identity: if case == 10 {
                            XdpMemberId::New
                        } else if case == 11 {
                            XdpMemberId::Existing(NonZeroU64::new(999).expect("id"))
                        } else {
                            XdpMemberId::Existing(link.id)
                        },
                        attachment: XdpCommit {
                            details: &old_details,
                            metadata: if case == 12 { &bad_metadata } else { &metadata },
                            ..desired[1].attachment
                        },
                    },
                ];
                if case == 13 {
                    bad.clear();
                }
                if case == 14 {
                    for _ in 0..9 {
                        bad.push(XdpMemberCommit {
                            identity: XdpMemberId::Existing(link.id),
                            attachment: XdpCommit {
                                ..desired[1].attachment
                            },
                        });
                    }
                }
                let error = backend
                    .replace_xdp(
                        w,
                        receipt,
                        XdpReplace {
                            members: &bad,
                            updated_at: timestamp,
                        },
                    )
                    .expect_err("invalid desired snapshot");
                receipt = error.remaining;
                assert_eq!(
                    reader
                        .read_xdp_dispatcher(key)
                        .expect("unchanged")
                        .expect("present")
                        .members()[0]
                        .member,
                    link,
                    "case {case}"
                );
            }
            let snapshot = backend
                .replace_xdp(w, receipt, request())
                .map_err(|e| e.cause)
                .expect("replace");
            assert_eq!(snapshot.members().len(), 2);
            assert_eq!(snapshot.members()[1].member.id, link.id);
            assert_ne!(snapshot.members()[0].member.id, link.id);
            assert_eq!(snapshot.members()[1].member.metadata, link.metadata);
            assert_eq!(snapshot.members()[1].member.created_at, link.created_at);
            assert_eq!(snapshot.members()[0].program_name.as_str(), "counter");
            for (index, member) in snapshot.members().iter().enumerate() {
                assert_eq!(member.details.slot.index(), index);
                assert_eq!(member.outer_link_id, n(77));
                assert_eq!(
                    member.member.pin_path,
                    layout
                        .xdp_slot_path(key, n(2), member.details.slot)
                        .to_str()
                        .expect("path")
                );
            }
            assert_eq!(
                reader.read_xdp_dispatcher(key).expect("read"),
                Some(snapshot.clone())
            );
            let mut reopened = backend
                .open_reader(&runtime)
                .expect("reopen")
                .expect("exists");
            assert_eq!(
                reopened
                    .read_xdp_dispatcher(key)
                    .expect("reopened snapshot"),
                Some(snapshot.clone())
            );
            assert_eq!(reader.read_links().expect("links").len(), 2);
            for program in reader.read_records().expect("programs") {
                assert_eq!(program.links.len(), 1);
            }
            // The existing kernel lifecycle must never treat two members as one.
            assert!(reader.read_xdp(key).is_err());
            for member in snapshot.members() {
                assert!(backend.observe_xdp(w, member.member.id).is_err());
            }
            assert!(backend.delete_xdp(w, stale_delete).is_err());
            assert!(backend.replace_xdp(w, stale_replace, request()).is_err());
            assert_eq!(
                reader.read_xdp_dispatcher(key).expect("unchanged"),
                Some(snapshot.clone())
            );

            let keep = &snapshot.members()[survivor];
            let mut details = keep.details.clone();
            details.revision = n(3);
            details.slot = bpfman_model::XdpSlot::FIRST;
            details.dispatcher_id = n(57);
            let desired = [XdpMemberCommit {
                identity: XdpMemberId::Existing(keep.member.id),
                attachment: XdpCommit {
                    program_id: keep.member.program_id,
                    details: &details,
                    extension_link_id: n(69),
                    outer_link_id: keep.outer_link_id,
                    metadata: &keep.member.metadata,
                    created_at: &keep.member.created_at,
                },
            }];
            let (_, receipt) = backend
                .observe_xdp_dispatcher(w, key)
                .expect("observe")
                .expect("present");
            let one = backend
                .replace_xdp(
                    w,
                    receipt,
                    XdpReplace {
                        members: &desired,
                        updated_at: timestamp,
                    },
                )
                .map_err(|e| e.cause)
                .expect("remove either member");
            assert_eq!(one.members().len(), 1);
            assert_eq!(one.members()[0].member.id, keep.member.id);
            assert_eq!(one.members()[0].details.slot.index(), 0);
            assert_eq!(
                reader.read_links().expect("one link"),
                [one.members()[0].member.clone()]
            );
            let (_, receipt) = backend
                .observe_xdp(w, keep.member.id)
                .expect("singleton again")
                .expect("present");
            backend
                .delete_xdp(w, receipt)
                .map_err(|e| e.cause)
                .expect("last detach");
            assert!(reader.read_xdp_dispatcher(key).expect("absent").is_none());
            assert!(reader.read_links().expect("no residue").is_empty());
        });
    }
}

#[test]
fn sqlite_replacement_contract() {
    replacement_contract(bpfman_store_sqlite::Backend);
}

#[test]
fn json_replacement_contract() {
    replacement_contract(bpfman_store_json::Backend);
}

fn capacity_contract<S: OpenStore + CommitLoad + XdpReplacementStore>(backend: S)
where
    S::Reader: XdpDispatcherReader + LinkReader,
{
    let temp = tempfile::tempdir().expect("tempdir");
    let runtime = RuntimeDirectory::open_or_create(
        RuntimeLayout::try_from(temp.path().to_owned()).expect("layout"),
    )
    .expect("runtime");
    writer(&runtime, |w| {
        let mut reader = backend.open(w).expect("open");
        let timestamp = "2026-10-05T00:00:00Z";
        let metadata = BTreeMap::new();
        let initial = XdpLink {
            slot: bpfman_model::XdpSlot::FIRST,
            key: XdpKey {
                nsid: NonZeroU64::MIN,
                ifindex: n(7),
            },
            interface: "veth0".parse().expect("interface"),
            priority: 100,
            proceed_on: Default::default(),
            dispatcher_id: n(50),
            revision: n(1),
        };
        for program in 100..111 {
            backend
                .commit_program(
                    w,
                    LoadRecord {
                        id: n(program),
                        spec: &ProgramSpec::Xdp("pass".try_into().expect("symbol")),
                        source: "source.o",
                        license: "GPL",
                        created_at: timestamp,
                        metadata: &metadata,
                        globals: &BTreeMap::new(),
                    },
                )
                .expect("program");
        }
        backend
            .commit_xdp(
                w,
                XdpCommit {
                    program_id: n(100),
                    details: &initial,
                    extension_link_id: n(200),
                    outer_link_id: n(77),
                    metadata: &metadata,
                    created_at: timestamp,
                },
            )
            .expect("first");
        for count in 2..=11 {
            let (old, receipt) = backend
                .observe_xdp_dispatcher(w, initial.key)
                .expect("observe")
                .expect("present");
            let mut details = initial.clone();
            details.revision = n(count);
            details.dispatcher_id = n(50 + count);
            let mut existing = Vec::new();
            for (slot, member) in old.members().iter().enumerate() {
                let mut d = member.details.clone();
                d.revision = details.revision;
                d.dispatcher_id = details.dispatcher_id;
                // At capacity, append the invalid eleventh member instead.
                d.slot = if count == 11 { slot } else { slot + 1 }
                    .try_into()
                    .expect("slot");
                existing.push(d);
            }
            let mut members: Vec<_> = old
                .members()
                .iter()
                .zip(&existing)
                .enumerate()
                .map(|(slot, (member, d))| XdpMemberCommit {
                    identity: XdpMemberId::Existing(member.member.id),
                    attachment: XdpCommit {
                        program_id: member.member.program_id,
                        details: d,
                        extension_link_id: n(200 + count * 10 + slot as u32),
                        outer_link_id: n(77),
                        metadata: &member.member.metadata,
                        created_at: &member.member.created_at,
                    },
                })
                .collect();
            let new = XdpMemberCommit {
                identity: XdpMemberId::New,
                attachment: XdpCommit {
                    program_id: n(99 + count),
                    details: &details,
                    extension_link_id: n(400 + count),
                    outer_link_id: n(77),
                    metadata: &metadata,
                    created_at: timestamp,
                },
            };
            if count == 11 {
                members.push(new);
            } else {
                members.insert(0, new);
            }
            let result = backend.replace_xdp(
                w,
                receipt,
                XdpReplace {
                    members: &members,
                    updated_at: timestamp,
                },
            );
            if count == 11 {
                let error = result.expect_err("capacity");
                assert_eq!(
                    reader.read_xdp_dispatcher(initial.key).expect("unchanged"),
                    Some(old)
                );
                backend
                    .delete_xdp(w, error.remaining)
                    .map_err(|e| e.cause)
                    .expect("delete complete snapshot");
                assert!(reader.read_links().expect("no residual members").is_empty());
            } else {
                let snapshot = result.map_err(|e| e.cause).expect("grow");
                assert_eq!(snapshot.members().len(), count as usize);
                for (slot, member) in snapshot.members().iter().enumerate() {
                    assert_eq!(member.details.slot.index(), slot);
                    assert_eq!(member.details.revision, n(count));
                }
                for (prior, next) in old.members().iter().zip(&snapshot.members()[1..]) {
                    assert_eq!(prior.member.id, next.member.id);
                }
                assert_eq!(
                    reader.read_xdp_dispatcher(initial.key).expect("read"),
                    Some(snapshot)
                );
                assert_eq!(reader.read_links().expect("links").len(), count as usize);
            }
        }
    });
}

#[test]
fn sqlite_replacement_capacity_contract() {
    capacity_contract(bpfman_store_sqlite::Backend);
}

#[test]
fn json_replacement_capacity_contract() {
    capacity_contract(bpfman_store_json::Backend);
}
