//! JSON-specific format and publication guarantees. Behaviour lives in the
//! shared store and outside-in lifecycle suites, run against both backends.
#![allow(clippy::expect_used)]

use bpfman_fs::{RuntimeDirectory, RuntimeLayout, RuntimeWriter};
use bpfman_lock::AcquireOptions;
use bpfman_model::Symbol;
use bpfman_store::{
    CommitLoad, ErrorKind, LinkReader, LinkStore, LoadRecord, OpenStore, PendingTracepoint,
    ProgramReader, UnloadStore,
};
use bpfman_store_json::Backend;
use std::{collections::BTreeMap, fs, num::NonZeroU32, os::unix::fs::symlink, time::Duration};

fn writer<T>(runtime: &RuntimeDirectory, work: impl FnOnce(&RuntimeWriter<'_>) -> T) -> T {
    runtime
        .with_writer(
            AcquireOptions {
                timeout: Duration::from_secs(1),
                cancelled: None,
            },
            |w| work(&w),
        )
        .expect("writer")
}

fn commit(writer: &RuntimeWriter<'_>) -> Result<(), bpfman_store::Error> {
    Backend.commit_program(
        writer,
        LoadRecord {
            globals: &Default::default(),
            id: NonZeroU32::new(42).expect("id"),
            spec: &bpfman_model::ProgramSpec::Tracepoint(
                Symbol::try_from("trace").expect("symbol"),
            ),
            source: "/source.o",
            license: "GPL",
            created_at: "2026-10-03T12:00:00Z",
            metadata: &BTreeMap::new(),
        },
    )?;

    Ok(())
}

#[test]
fn malformed_and_future_snapshots_are_never_replaced() {
    let temporary = tempfile::tempdir().expect("tempdir");
    let layout = RuntimeLayout::try_from(temporary.path().to_owned()).expect("layout");
    let runtime = RuntimeDirectory::open_or_create(layout.clone()).expect("runtime");
    writer(&runtime, |w| {
        Backend.open(w).expect("create");
    });
    let pristine = fs::read(layout.database_path()).expect("snapshot");

    for (bytes, kind) in [
        (b"{".as_slice(), ErrorKind::InvalidData),
        (b"{\"version\":8}".as_slice(), ErrorKind::IncompatibleState),
        (b"SQLite format 3\0".as_slice(), ErrorKind::InvalidData),
    ] {
        fs::write(layout.database_path(), bytes).expect("fixture");
        writer(&runtime, |w| {
            let error = Backend.open(w).map(|_| ()).expect_err("reject state");

            assert_eq!(error.kind(), kind);
        });

        assert_eq!(fs::read(layout.database_path()).expect("unchanged"), bytes);
    }

    fs::write(layout.database_path(), &pristine).expect("restore");
    let mut reader = writer(&runtime, |w| Backend.open(w).expect("open"));
    fs::write(layout.database_path(), b"{\"version\":9}").expect("change version");

    assert_eq!(
        reader
            .read_records()
            .expect_err("revalidate version")
            .kind(),
        ErrorKind::IncompatibleState
    );
}

#[test]
fn publication_failure_never_commits_half_a_load() {
    let temporary = tempfile::tempdir().expect("tempdir");
    let layout = RuntimeLayout::try_from(temporary.path().to_owned()).expect("layout");
    let runtime = RuntimeDirectory::open_or_create(layout.clone()).expect("runtime");

    writer(&runtime, |w| {
        let mut reader = Backend.open(w).expect("create");
        let previous = fs::read(layout.database_path()).expect("snapshot");
        let outside = temporary.path().join("outside");
        fs::write(&outside, b"sentinel").expect("outside");
        let pending = temporary.path().join("db/store.next");
        symlink(&outside, &pending).expect("blocked publication");

        assert!(commit(w).is_err());
        assert!(reader.read_records().expect("records").is_empty());
        assert_eq!(
            fs::read(layout.database_path()).expect("snapshot"),
            previous
        );
        assert_eq!(fs::read(&outside).expect("sentinel"), b"sentinel");

        fs::rename(&pending, temporary.path().join("saved")).expect("remove obstruction");
        commit(w).expect("commit after obstruction removed");

        let published = fs::read(layout.database_path()).expect("published");
        let state: serde_json::Value = serde_json::from_slice(&published).expect("JSON");

        assert_eq!(state["programs"].as_array().expect("programs").len(), 1);
        assert_eq!(state["map_sets"].as_array().expect("maps").len(), 1);

        assert!(commit(w).is_err());
        assert_eq!(
            fs::read(layout.database_path()).expect("unchanged"),
            published
        );
    });
}

#[test]
fn invalid_relationships_are_refused_before_teardown() {
    let temporary = tempfile::tempdir().expect("tempdir");
    let layout = RuntimeLayout::try_from(temporary.path().to_owned()).expect("layout");
    let runtime = RuntimeDirectory::open_or_create(layout.clone()).expect("runtime");
    writer(&runtime, |w| {
        Backend.open(w).expect("create");
        commit(w).expect("commit");
    });
    let original: serde_json::Value =
        serde_json::from_slice(&fs::read(layout.database_path()).expect("read")).expect("JSON");

    for mutation in 0..4 {
        let mut state = original.clone();

        match mutation {
            0 => state["map_sets"] = serde_json::json!([]),
            1 => state["programs"][0]["created_at"] = serde_json::json!("invalid"),
            2 => state["programs"][0]["links"] = serde_json::json!([1]),
            _ => state["programs"]
                .as_array_mut()
                .expect("programs")
                .push(original["programs"][0].clone()),
        }

        let bytes = serde_json::to_vec(&state).expect("encode");
        fs::write(layout.database_path(), &bytes).expect("fixture");

        writer(&runtime, |w| {
            assert!(
                Backend
                    .observe_unload(w, NonZeroU32::new(42).expect("id"))
                    .is_err()
            );
        });

        assert_eq!(fs::read(layout.database_path()).expect("unchanged"), bytes);
    }
}

#[test]
fn receipts_reject_recreated_records_and_replaced_store_identity() {
    let temporary = tempfile::tempdir().expect("tempdir");
    let layout = RuntimeLayout::try_from(temporary.path().to_owned()).expect("layout");
    let runtime = RuntimeDirectory::open_or_create(layout.clone()).expect("runtime");
    let id = NonZeroU32::new(42).expect("id");

    writer(&runtime, |w| {
        Backend.open(w).expect("create");
        commit(w).expect("commit");

        let (stale_program, stale_maps) = Backend
            .observe_unload(w, id)
            .expect("observe")
            .expect("present");
        let (program, maps) = Backend
            .observe_unload(w, id)
            .expect("observe")
            .expect("present");
        Backend
            .delete_program(w, program)
            .map_err(|e| e.cause)
            .expect("delete");
        Backend
            .delete_map_set(w, maps)
            .map_err(|e| e.cause)
            .expect("delete map set");

        // Same ID, timestamp and contents, but a new generation owns the artifacts.
        commit(w).expect("recreate identical record");

        assert!(Backend.delete_program(w, stale_program).is_err());

        let (program, maps) = Backend
            .observe_unload(w, id)
            .expect("observe")
            .expect("present");
        Backend
            .delete_program(w, program)
            .map_err(|e| e.cause)
            .expect("delete new generation");

        assert!(Backend.delete_map_set(w, stale_maps).is_err());
        Backend
            .delete_map_set(w, maps)
            .map_err(|e| e.cause)
            .expect("delete new map generation");

        commit(w).expect("commit");

        let (program, _) = Backend
            .observe_unload(w, id)
            .expect("observe")
            .expect("present");
        fs::rename(layout.database_path(), temporary.path().join("old-store"))
            .expect("replace store");
        Backend.open(w).expect("create independent store");
        commit(w).expect("same record in new store");

        assert!(Backend.delete_program(w, program).is_err());
        assert_eq!(
            Backend
                .open(w)
                .expect("reopen")
                .read_records()
                .expect("records")
                .len(),
            1
        );
    });
}

#[test]
fn version_one_programs_remain_usable_without_implicit_upgrade() {
    let temporary = tempfile::tempdir().expect("tempdir");
    let layout = RuntimeLayout::try_from(temporary.path().to_owned()).expect("layout");
    let runtime = RuntimeDirectory::open_or_create(layout.clone()).expect("runtime");

    writer(&runtime, |w| {
        Backend.open(w).expect("create");
    });
    let mut legacy: serde_json::Value =
        serde_json::from_slice(&fs::read(layout.database_path()).expect("read")).expect("JSON");
    legacy["version"] = serde_json::json!(1);
    let bytes = serde_json::to_vec(&legacy).expect("legacy snapshot");
    fs::write(layout.database_path(), &bytes).expect("version 1 fixture");

    writer(&runtime, |w| {
        let mut reader = Backend.open(w).expect("version 1 still opens");

        assert!(reader.read_links().expect("no links").is_empty());
        assert_eq!(fs::read(layout.database_path()).expect("unchanged"), bytes);

        commit(w).expect("legacy program load");
        let committed = fs::read(layout.database_path()).expect("committed");
        let target = "sched/sched_switch".parse().expect("target");
        let error = Backend
            .create_pending_tracepoint(
                w,
                PendingTracepoint {
                    program_id: NonZeroU32::new(42).expect("id"),
                    target: &target,
                    metadata: &BTreeMap::new(),
                    created_at: "2026-10-04T12:00:00Z",
                },
            )
            .map(|_| ())
            .expect_err("version 1 has no links");

        assert_eq!(error.kind(), ErrorKind::IncompatibleState);
        assert_eq!(
            fs::read(layout.database_path()).expect("no implicit upgrade"),
            committed
        );

        let (program, maps) = Backend
            .observe_unload(w, NonZeroU32::new(42).expect("id"))
            .expect("legacy unload")
            .expect("present");
        Backend
            .delete_program(w, program)
            .map_err(|e| e.cause)
            .expect("delete program");
        Backend
            .delete_map_set(w, maps)
            .map_err(|e| e.cause)
            .expect("delete map set");
        let state: serde_json::Value =
            serde_json::from_slice(&fs::read(layout.database_path()).expect("read")).expect("JSON");

        assert_eq!(state["version"], 1);
        assert!(state.get("links").is_none());
        assert!(state.get("next_link_id").is_none());
        assert!(reader.read_records().expect("empty").is_empty());
    });
}

#[test]
fn failed_link_publication_retains_previous_state_and_receipts() {
    let temporary = tempfile::tempdir().expect("tempdir");
    let layout = RuntimeLayout::try_from(temporary.path().to_owned()).expect("layout");
    let runtime = RuntimeDirectory::open_or_create(layout.clone()).expect("runtime");
    let outside = temporary.path().join("outside");
    fs::write(&outside, b"sentinel").expect("outside");
    let pending = temporary.path().join("db/store.next");
    let block = || symlink(&outside, &pending).expect("block publication");
    let unblock = |step: &str| fs::rename(&pending, temporary.path().join(step)).expect("unblock");

    writer(&runtime, |w| {
        let mut reader = Backend.open(w).expect("create");
        commit(w).expect("commit");
        let target = "sched/sched_switch".parse().expect("target");
        let metadata = BTreeMap::new();
        let request = || PendingTracepoint {
            program_id: NonZeroU32::new(42).expect("id"),
            target: &target,
            metadata: &metadata,
            created_at: "2026-10-04T12:00:00Z",
        };
        let previous = fs::read(layout.database_path()).expect("snapshot");
        block();

        assert!(Backend.create_pending_tracepoint(w, request()).is_err());
        assert_eq!(
            fs::read(layout.database_path()).expect("unchanged"),
            previous
        );
        assert!(reader.read_links().expect("no intent").is_empty());

        unblock("create-obstruction");
        let (record, receipt) = Backend
            .create_pending_tracepoint(w, request())
            .expect("intent");
        let previous = fs::read(layout.database_path()).expect("snapshot");
        block();
        let error = Backend
            .finalise_link(w, receipt, NonZeroU32::new(7).expect("kernel ID"))
            .expect_err("finalisation not committed");

        assert_eq!(
            fs::read(layout.database_path()).expect("unchanged"),
            previous
        );
        assert_eq!(reader.read_links().expect("still pending"), [record]);

        unblock("finalise-obstruction");
        let attached = Backend
            .finalise_link(w, error.remaining, NonZeroU32::new(7).expect("kernel ID"))
            .map_err(|e| e.cause)
            .expect("explicit finalisation attempt");
        let (_, receipt) = Backend
            .observe_link(w, attached.id)
            .expect("observe")
            .expect("present");
        let previous = fs::read(layout.database_path()).expect("snapshot");
        block();
        let error = Backend
            .delete_link(w, receipt)
            .expect_err("deletion not committed");

        assert_eq!(
            fs::read(layout.database_path()).expect("unchanged"),
            previous
        );
        assert_eq!(reader.read_links().expect("record retained"), [attached]);

        unblock("delete-obstruction");
        Backend
            .delete_link(w, error.remaining)
            .map_err(|e| e.cause)
            .expect("explicit deletion attempt");

        assert!(reader.read_links().expect("empty").is_empty());
        assert_eq!(fs::read(&outside).expect("sentinel"), b"sentinel");
    });
}

#[test]
fn legacy_versions_refuse_xdp_without_upgrade_or_partial_commit() {
    for version in [1, 2] {
        let temporary = tempfile::tempdir().expect("tempdir");
        let layout = RuntimeLayout::try_from(temporary.path().to_owned()).expect("layout");
        let runtime = RuntimeDirectory::open_or_create(layout.clone()).expect("runtime");
        writer(&runtime, |w| {
            Backend.open(w).expect("create");
        });
        let mut state: serde_json::Value =
            serde_json::from_slice(&fs::read(layout.database_path()).expect("read")).expect("JSON");
        assert_eq!(state["version"], 7);
        state["version"] = version.into();
        fs::write(
            layout.database_path(),
            serde_json::to_vec(&state).expect("encode"),
        )
        .expect("fixture");
        writer(&runtime, |w| {
            let mut reader = Backend.open(w).expect("open legacy");
            commit(w).expect("tracepoint works without upgrade");
            if version == 2 {
                Backend
                    .create_pending_tracepoint(
                        w,
                        PendingTracepoint {
                            program_id: NonZeroU32::new(42).expect("id"),
                            target: &"sched/sched_switch".parse().expect("target"),
                            metadata: &BTreeMap::new(),
                            created_at: "2026-10-05T00:00:00Z",
                        },
                    )
                    .expect("version 2 retains link support");
            }
            let previous = fs::read(layout.database_path()).expect("snapshot");
            let xdp = bpfman_model::ProgramSpec::Xdp(Symbol::try_from("pass").expect("symbol"));
            let tracepoint =
                bpfman_model::ProgramSpec::Tracepoint(Symbol::try_from("trace").expect("symbol"));
            let metadata = BTreeMap::new();
            let globals = BTreeMap::new();
            let record = |id, spec| LoadRecord {
                id: NonZeroU32::new(id).expect("id"),
                spec,
                source: "source.o",
                license: "GPL",
                metadata: &metadata,
                globals: &globals,
                created_at: "2026-10-05T00:00:00Z",
            };
            let failure = Backend
                .commit_programs(w, &[record(43, &tracepoint), record(44, &xdp)])
                .expect_err("requires explicit new format");
            assert_eq!(failure.kind(), ErrorKind::IncompatibleState);
            assert_eq!(
                fs::read(layout.database_path()).expect("unchanged"),
                previous
            );
            assert_eq!(reader.read_records().expect("records").len(), 1);
            // A forged kind field cannot smuggle an extension into an old format.
            let mut invalid: serde_json::Value = serde_json::from_slice(&previous).expect("JSON");
            invalid["programs"][0]["kind"] = "xdp".into();
            let invalid = serde_json::to_vec(&invalid).expect("encode");
            fs::write(layout.database_path(), &invalid).expect("fixture");
            assert_eq!(
                reader
                    .read_records()
                    .expect_err("reject wrong version")
                    .kind(),
                ErrorKind::IncompatibleState
            );
            assert_eq!(
                fs::read(layout.database_path()).expect("unchanged"),
                invalid
            );
        });
    }
}

#[test]
fn older_formats_refuse_dispatcher_publication_without_migration() {
    use bpfman_model::{ProgramSpec, XdpKey, XdpLink};
    use bpfman_store::{XdpCommit, XdpStore};
    for version in 1..=3 {
        let temporary = tempfile::tempdir().expect("tempdir");
        let layout = RuntimeLayout::try_from(temporary.path().to_owned()).expect("layout");
        let runtime = RuntimeDirectory::open_or_create(layout.clone()).expect("runtime");
        writer(&runtime, |w| {
            Backend.open(w).expect("create");
        });
        let mut state: serde_json::Value =
            serde_json::from_slice(&fs::read(layout.database_path()).expect("read")).expect("JSON");
        state["version"] = version.into();
        fs::write(
            layout.database_path(),
            serde_json::to_vec(&state).expect("encode"),
        )
        .expect("fixture");
        writer(&runtime, |w| {
            Backend.open(w).expect("legacy store");
            if version == 3 {
                Backend
                    .commit_program(
                        w,
                        LoadRecord {
                            id: NonZeroU32::new(42).expect("id"),
                            spec: &ProgramSpec::Xdp("pass".try_into().expect("symbol")),
                            source: "source.o",
                            license: "GPL",
                            created_at: "2026-10-05T00:00:00Z",
                            metadata: &BTreeMap::new(),
                            globals: &BTreeMap::new(),
                        },
                    )
                    .expect("version 3 still loads XDP");
            }
            let previous = fs::read(layout.database_path()).expect("snapshot");
            let key = XdpKey {
                nsid: std::num::NonZeroU64::MIN,
                ifindex: NonZeroU32::MIN,
            };
            assert_eq!(
                Backend
                    .preflight_xdp(w, key, NonZeroU32::new(42).expect("id"))
                    .expect_err("attachment requires new format")
                    .kind(),
                ErrorKind::IncompatibleState
            );
            let details = XdpLink {
                netns: Default::default(),
                slot: bpfman_model::XdpSlot::FIRST,
                key,
                interface: "eth0".parse().expect("interface"),
                priority: 50,
                proceed_on: Default::default(),
                dispatcher_id: NonZeroU32::new(55).expect("dispatcher"),
                revision: NonZeroU32::MIN,
            };
            let result = Backend.commit_xdp(
                w,
                XdpCommit {
                    program_id: NonZeroU32::new(42).expect("id"),
                    details: &details,
                    extension_link_id: NonZeroU32::new(66).expect("extension"),
                    outer_link_id: NonZeroU32::new(77).expect("outer"),
                    metadata: &BTreeMap::new(),
                    created_at: "2026-10-05T00:00:00Z",
                },
            );
            assert_eq!(
                result.expect_err("no upgrade on commit").kind(),
                ErrorKind::IncompatibleState
            );
            assert_eq!(
                fs::read(layout.database_path()).expect("unchanged"),
                previous
            );
        });
    }
}

#[test]
fn old_formats_refuse_tc_without_upgrade_or_partial_publication() {
    use bpfman_store::{CommitLoad, LoadRecord};
    for version in 1..=6 {
        let temp = tempfile::tempdir().expect("temp");
        let layout = RuntimeLayout::try_from(temp.path().join("runtime")).expect("layout");
        let runtime = RuntimeDirectory::open_or_create(layout.clone()).expect("runtime");
        writer(&runtime, |w| {
            Backend.open(w).expect("new store");
        });
        let mut state: serde_json::Value =
            serde_json::from_slice(&fs::read(layout.database_path()).expect("snapshot"))
                .expect("JSON");
        state["version"] = version.into();
        let original = serde_json::to_vec(&state).expect("legacy snapshot");
        fs::write(layout.database_path(), &original).expect("legacy fixture");
        writer(&runtime, |w| {
            Backend.open(w).expect("old format opens");
            let id = std::num::NonZeroU32::MIN;
            let spec = bpfman_model::ProgramSpec::Tc("tc_ingress".try_into().expect("symbol"));
            let empty = std::collections::BTreeMap::new();
            assert!(
                Backend
                    .commit_program(
                        w,
                        LoadRecord {
                            id,
                            spec: &spec,
                            source: "tc.o",
                            license: "GPL",
                            created_at: "2026-10-08T00:00:00Z",
                            metadata: &empty,
                            globals: &Default::default()
                        }
                    )
                    .is_err()
            );
        });
        assert_eq!(
            fs::read(layout.database_path()).expect("unchanged"),
            original
        );
    }
}
