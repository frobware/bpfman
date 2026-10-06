//! JSON-specific replacement publication, format, and complete-receipt guarantees.
#![allow(clippy::expect_used)]

use bpfman_fs::{RuntimeDirectory, RuntimeLayout, RuntimeWriter};
use bpfman_lock::AcquireOptions;
use bpfman_model::{ProgramSpec, XdpKey, XdpLink};
use bpfman_store::{
    CommitLoad, LinkReader, LoadRecord, OpenStore, XdpCommit, XdpDispatcherReader, XdpMemberCommit,
    XdpMemberId, XdpReplace, XdpReplacementStore, XdpStore,
};
use bpfman_store_json::{Backend, Reader};
use std::{
    fs,
    num::{NonZeroU32, NonZeroU64},
    os::unix::fs::symlink,
    time::Duration,
};

type Result = std::result::Result<(), Box<dyn std::error::Error>>;

fn id(n: u32) -> NonZeroU32 {
    NonZeroU32::new(n).expect("nonzero")
}

fn scope(test: impl FnOnce(&RuntimeWriter<'_>, &mut Reader, &XdpCommit<'_>) -> Result) -> Result {
    let temp = tempfile::tempdir()?;
    let runtime =
        RuntimeDirectory::open_or_create(RuntimeLayout::try_from(temp.path().to_owned())?)?;
    runtime.with_writer(
        AcquireOptions {
            timeout: Duration::from_secs(1),
            cancelled: None,
        },
        |w| -> Result {
            let mut reader = Backend.open(&w)?;
            Backend.commit_program(
                &w,
                LoadRecord {
                    id: id(42),
                    spec: &ProgramSpec::Xdp("pass".try_into()?),
                    source: "source.o",
                    license: "GPL",
                    created_at: "2026-10-05T00:00:00Z",
                    globals: &Default::default(),
                    metadata: &Default::default(),
                },
            )?;
            let details = XdpLink {
                slot: bpfman_model::XdpSlot::FIRST,
                key: XdpKey {
                    nsid: NonZeroU64::MIN,
                    ifindex: id(7),
                },
                interface: "veth0".parse()?,
                priority: 50,
                proceed_on: Default::default(),
                dispatcher_id: id(55),
                revision: id(1),
            };
            test(
                &w,
                &mut reader,
                &XdpCommit {
                    program_id: id(42),
                    details: &details,
                    extension_link_id: id(66),
                    outer_link_id: id(77),
                    metadata: &Default::default(),
                    created_at: "2026-10-05T00:00:00Z",
                },
            )
        },
    )??;
    Ok(())
}

#[test]
fn blocked_replacement_retains_bytes_ids_and_retry_evidence() -> Result {
    scope(|w, reader, request| {
        let link = Backend.commit_xdp(w, XdpCommit { ..*request })?;
        let old = reader.read_xdp_dispatcher(request.details.key)?;
        let (_, receipt) = Backend
            .observe_xdp_dispatcher(w, request.details.key)?
            .expect("snapshot");
        let mut details = request.details.clone();
        details.dispatcher_id = id(56);
        details.revision = id(2);
        let mut second = details.clone();
        second.slot = 1usize.try_into()?;
        let members = [
            XdpMemberCommit {
                identity: XdpMemberId::Existing(link.id),
                attachment: XdpCommit {
                    details: &details,
                    extension_link_id: id(67),
                    ..*request
                },
            },
            XdpMemberCommit {
                identity: XdpMemberId::New,
                attachment: XdpCommit {
                    details: &second,
                    extension_link_id: id(68),
                    ..*request
                },
            },
        ];
        let replace = || XdpReplace {
            members: &members,
            updated_at: request.created_at,
        };
        let before = fs::read(w.database_path())?;
        let sentinel = w.layout().root().join("sentinel");
        fs::write(&sentinel, b"untouched")?;
        let pending = w.layout().root().join("db/store.next");
        symlink(&sentinel, &pending)?;
        let failure = Backend
            .replace_xdp(w, receipt, replace())
            .expect_err("blocked publish");
        assert_eq!(fs::read(w.database_path())?, before);
        assert_eq!(reader.read_xdp_dispatcher(request.details.key)?, old);
        assert_eq!(fs::read(&sentinel)?, b"untouched");
        fs::rename(pending, w.layout().root().join("saved-obstruction"))?;
        let snapshot = Backend
            .replace_xdp(w, failure.remaining, replace())
            .map_err(|e| e.cause)?;
        assert_eq!(snapshot.members()[0].member.id, link.id);
        assert_eq!(snapshot.members()[1].member.id.get(), link.id.get() + 1);
        assert_eq!(
            reader.read_xdp_dispatcher(request.details.key)?,
            Some(snapshot)
        );
        Ok(())
    })
}

#[test]
fn format_four_remains_single_member_without_implicit_upgrade() -> Result {
    scope(|w, reader, request| {
        let mut state: serde_json::Value = serde_json::from_slice(&fs::read(w.database_path())?)?;
        state["version"] = 4.into();
        fs::write(w.database_path(), serde_json::to_vec(&state)?)?;
        let link = Backend.commit_xdp(w, XdpCommit { ..*request })?;
        let before = fs::read(w.database_path())?;
        let (_, receipt) = Backend
            .observe_xdp_dispatcher(w, request.details.key)?
            .expect("snapshot");
        let mut details = request.details.clone();
        details.revision = id(2);
        details.dispatcher_id = id(56);
        let members = [XdpMemberCommit {
            identity: XdpMemberId::Existing(link.id),
            attachment: XdpCommit {
                details: &details,
                extension_link_id: id(67),
                ..*request
            },
        }];
        let failure = Backend
            .replace_xdp(
                w,
                receipt,
                XdpReplace {
                    members: &members,
                    updated_at: request.created_at,
                },
            )
            .expect_err("format four refuses replacement");
        assert_eq!(fs::read(w.database_path())?, before);
        assert_eq!(reader.read_links()?, std::slice::from_ref(&link));
        Backend
            .delete_xdp(w, failure.remaining)
            .map_err(|e| e.cause)?;
        let state: serde_json::Value = serde_json::from_slice(&fs::read(w.database_path())?)?;
        assert_eq!(state["version"], 4);
        Ok(())
    })
}

#[test]
fn malformed_members_and_changed_complete_evidence_are_refused() -> Result {
    scope(|w, reader, request| {
        let link = Backend.commit_xdp(w, XdpCommit { ..*request })?;
        let mut details = request.details.clone();
        details.revision = id(2);
        details.dispatcher_id = id(56);
        let mut second = details.clone();
        second.slot = 1usize.try_into()?;
        let members = [
            XdpMemberCommit {
                identity: XdpMemberId::Existing(link.id),
                attachment: XdpCommit {
                    details: &details,
                    extension_link_id: id(67),
                    ..*request
                },
            },
            XdpMemberCommit {
                identity: XdpMemberId::New,
                attachment: XdpCommit {
                    details: &second,
                    extension_link_id: id(68),
                    ..*request
                },
            },
        ];
        let (_, receipt) = Backend
            .observe_xdp_dispatcher(w, request.details.key)?
            .expect("snapshot");
        Backend
            .replace_xdp(
                w,
                receipt,
                XdpReplace {
                    members: &members,
                    updated_at: request.created_at,
                },
            )
            .map_err(|e| e.cause)?;
        let original = fs::read(w.database_path())?;
        let value: serde_json::Value = serde_json::from_slice(&original)?;
        for (field, invalid) in [
            ("position", serde_json::json!(0)),
            ("position", serde_json::json!(10)),
            ("revision", serde_json::json!(3)),
            ("dispatcher_id", serde_json::json!(57)),
            ("outer_link_id", serde_json::json!(78)),
            ("extension_link_id", serde_json::json!(77)),
            ("interface", serde_json::json!("veth1")),
            ("link_id", serde_json::json!(link.id.get())),
        ] {
            let mut bad = value.clone();
            bad["xdp"][1][field] = invalid;
            fs::write(w.database_path(), serde_json::to_vec(&bad)?)?;
            assert!(
                reader.read_xdp_dispatcher(request.details.key).is_err(),
                "{field}"
            );
            assert!(reader.read_links().is_err(), "{field}");
            assert!(
                Backend
                    .observe_xdp_dispatcher(w, request.details.key)
                    .is_err(),
                "{field}"
            );
        }
        fs::write(w.database_path(), &original)?;
        let (_, receipt) = Backend
            .observe_xdp_dispatcher(w, request.details.key)?
            .expect("snapshot");
        let mut changed = value.clone();
        changed["xdp"][1]["metadata"] = serde_json::json!({"changed": "second member"});
        let changed = serde_json::to_vec(&changed)?;
        fs::write(w.database_path(), &changed)?;
        let failure = Backend
            .delete_xdp(w, receipt)
            .expect_err("receipt covers second member too");
        assert_eq!(fs::read(w.database_path())?, changed);
        fs::write(w.database_path(), &original)?;
        let mut changed = value;
        changed["identity"] = "ffffffffffffffffffffffffffffffff".into();
        fs::write(w.database_path(), serde_json::to_vec(&changed)?)?;
        let failure = Backend
            .delete_xdp(w, failure.remaining)
            .expect_err("store identity changed");
        fs::write(w.database_path(), original)?;
        Backend
            .delete_xdp(w, failure.remaining)
            .map_err(|e| e.cause)?;
        assert!(reader.read_xdp_dispatcher(request.details.key)?.is_none());
        Ok(())
    })
}
