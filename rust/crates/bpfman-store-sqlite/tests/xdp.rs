//! SQLite-specific atomic publication and refusal of malformed dispatcher rows.
#![allow(clippy::expect_used)]

use bpfman_fs::{RuntimeDirectory, RuntimeLayout, RuntimeWriter};
use bpfman_lock::AcquireOptions;
use bpfman_model::{ProgramSpec, XdpKey, XdpLink};
use bpfman_store::{CommitLoad, LinkReader, LoadRecord, OpenStore, XdpCommit, XdpReader, XdpStore};
use bpfman_store_sqlite::{Backend, Store};
use rusqlite::Connection;
use std::{
    num::{NonZeroU32, NonZeroU64},
    time::Duration,
};

type Result = std::result::Result<(), Box<dyn std::error::Error>>;

fn id(n: u32) -> NonZeroU32 {
    NonZeroU32::new(n).expect("nonzero")
}

fn scope(
    test: impl FnOnce(&RuntimeWriter<'_>, &Connection, &mut Store, &XdpCommit<'_>) -> Result,
) -> Result {
    let temp = tempfile::tempdir()?;
    let layout = RuntimeLayout::try_from(temp.path().to_owned())?;
    let runtime = RuntimeDirectory::open_or_create(layout.clone())?;
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
                revision: NonZeroU32::MIN,
            };
            let request = XdpCommit {
                program_id: id(42),
                details: &details,
                extension_link_id: id(66),
                outer_link_id: id(77),
                metadata: &Default::default(),
                created_at: "2026-10-05T00:00:00Z",
            };
            let db = Connection::open(layout.database_path())?;

            test(&w, &db, &mut reader, &request)
        },
    )??;

    Ok(())
}

fn commit(w: &RuntimeWriter<'_>, request: &XdpCommit<'_>) -> bpfman_model::StoredLink {
    Backend
        .commit_xdp(w, XdpCommit { ..*request })
        .expect("commit snapshot")
}

#[test]
fn publication_and_ignored_deletion_never_leave_half_a_snapshot() -> Result {
    scope(|w, db, reader, request| {
        db.execute_batch("CREATE TRIGGER reject_member BEFORE INSERT ON link_xdp_details BEGIN SELECT RAISE(ABORT, 'injected'); END")?;

        assert!(Backend.commit_xdp(w, XdpCommit { ..*request }).is_err());
        assert!(reader.read_links()?.is_empty());
        assert!(reader.read_xdp(request.details.key)?.is_none());
        let headers: u32 = db.query_row("SELECT count(*) FROM dispatchers", [], |r| r.get(0))?;
        assert_eq!(headers, 0);

        db.execute_batch("DROP TRIGGER reject_member")?;
        for table in ["links", "dispatchers"] {
            let link = commit(w, request);
            let snapshot = reader.read_xdp(request.details.key)?;
            db.execute_batch(&format!("CREATE TRIGGER ignore_delete BEFORE DELETE ON {table} BEGIN SELECT RAISE(IGNORE); END"))?;
            let (_, receipt) = Backend.observe_xdp(w, link.id)?.expect("receipt");

            let failure = Backend.delete_xdp(w, receipt).expect_err("ignored delete");
            assert_eq!(reader.read_xdp(request.details.key)?, snapshot);
            assert_eq!(reader.read_links()?, [link]);

            db.execute_batch("DROP TRIGGER ignore_delete")?;
            // Retained evidence remains valid after the rejected transaction.
            Backend
                .delete_xdp(w, failure.remaining)
                .map_err(|e| e.cause)?;
            assert!(reader.read_xdp(request.details.key)?.is_none());
        }
        Ok(())
    })
}

#[test]
fn malformed_dispatcher_members_are_errors_not_absence() -> Result {
    scope(|w, db, reader, request| {
        let link = commit(w, request);
        db.execute_batch("CREATE TEMP TABLE saved_details AS SELECT * FROM link_xdp_details")?;
        let mutations = [
            (
                "UPDATE dispatchers SET netns='/run/netns/foreign'",
                "UPDATE dispatchers SET netns=''",
            ),
            (
                "UPDATE link_xdp_details SET netns='/run/netns/foreign'",
                "UPDATE link_xdp_details SET netns=''",
            ),
            (
                "UPDATE link_xdp_details SET position=1",
                "UPDATE link_xdp_details SET position=0",
            ),
            (
                "UPDATE link_xdp_details SET position=1; UPDATE links SET pin_path=replace(pin_path,'link_0','link_1')",
                "UPDATE link_xdp_details SET position=0; UPDATE links SET pin_path=replace(pin_path,'link_1','link_0')",
            ),
            (
                "UPDATE link_xdp_details SET proceed_on='[5]'",
                "UPDATE link_xdp_details SET proceed_on='[2,31]'",
            ),
            (
                "UPDATE dispatchers SET created_at='invalid'",
                "UPDATE dispatchers SET created_at='2026-10-05T00:00:00Z'",
            ),
            (
                "UPDATE links SET kernel_link_id=77",
                "UPDATE links SET kernel_link_id=66",
            ),
            (
                "DELETE FROM link_xdp_details",
                "INSERT INTO link_xdp_details SELECT * FROM saved_details",
            ),
        ];
        for (mutation, restore) in mutations {
            db.execute_batch(mutation)?;

            assert!(reader.read_links().is_err(), "{mutation}");
            assert!(reader.read_xdp(request.details.key).is_err(), "{mutation}");
            assert!(Backend.observe_xdp(w, link.id).is_err(), "{mutation}");

            db.execute_batch(restore)?;
            assert_eq!(reader.read_links()?, std::slice::from_ref(&link));
        }
        Ok(())
    })
}

#[test]
fn replacement_rolls_back_every_dml_failure_and_retains_receipt() -> Result {
    use bpfman_store::{
        XdpDispatcherReader, XdpMemberCommit, XdpMemberId, XdpReplace, XdpReplacementStore,
    };
    scope(|w, db, reader, request| {
        let link = commit(w, request);
        let old = reader.read_xdp_dispatcher(request.details.key)?;
        let mut details = request.details.clone();
        details.dispatcher_id = id(56);
        details.revision = id(2);
        let members = [XdpMemberCommit {
            identity: XdpMemberId::Existing(link.id),
            attachment: XdpCommit {
                details: &details,
                extension_link_id: id(67),
                ..*request
            },
        }];
        let replacement = || XdpReplace {
            members: &members,
            updated_at: request.created_at,
        };
        let (_, mut receipt) = Backend
            .observe_xdp_dispatcher(w, request.details.key)?
            .expect("snapshot");
        for (operation, table) in [
            ("DELETE", "links"),
            ("DELETE", "link_xdp_details"),
            ("UPDATE", "dispatchers"),
            ("INSERT", "links"),
            ("INSERT", "link_xdp_details"),
        ] {
            for action in ["ABORT, 'injected'", "IGNORE"] {
                db.execute_batch(&format!("CREATE TRIGGER inject BEFORE {operation} ON {table} BEGIN SELECT RAISE({action}); END"))?;
                let failure = Backend
                    .replace_xdp(w, receipt, replacement())
                    .expect_err("DML fault");
                receipt = failure.remaining;
                assert_eq!(
                    reader.read_xdp_dispatcher(request.details.key)?,
                    old,
                    "{operation} {table} {action}"
                );
                assert_eq!(reader.read_links()?, std::slice::from_ref(&link));
                db.execute_batch("DROP TRIGGER inject")?;
            }
        }
        let snapshot = Backend
            .replace_xdp(w, receipt, replacement())
            .map_err(|e| e.cause)?;
        assert_eq!(snapshot.members()[0].member.id, link.id);
        assert_eq!(snapshot.members()[0].details.revision, id(2));
        assert_eq!(
            reader.read_xdp_dispatcher(request.details.key)?,
            Some(snapshot)
        );
        Ok(())
    })
}
