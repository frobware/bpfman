//! The same pending-link protocol runs against both stores through public contracts.
//! No database statements or persistence-format fixtures belong in this suite.
#![allow(clippy::expect_used)]

use bpfman_fs::{RuntimeDirectory, RuntimeLayout, RuntimeWriter};
use bpfman_lock::AcquireOptions;
use bpfman_model::{LinkDetails, LinkState, StoredLink, Symbol, Tracepoint};
use bpfman_runtime::ActiveStore;
use bpfman_store::{
    CommitLoad, LinkReader, LinkStore, LoadRecord, OpenStore, PendingTracepoint, ProgramReader,
    UnloadStore,
};
use std::{collections::BTreeMap, num::NonZeroU32, time::Duration};

fn id(raw: u32) -> NonZeroU32 {
    NonZeroU32::new(raw).expect("nonzero")
}

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

fn commit<S: CommitLoad>(store: &S, writer: &RuntimeWriter<'_>, raw: u32) {
    store
        .commit_program(
            writer,
            LoadRecord {
                map_owner: None,
                shared_maps: &[],
                globals: &Default::default(),
                id: id(raw),
                spec: &bpfman_model::ProgramSpec::Tracepoint(
                    Symbol::try_from("trace").expect("symbol"),
                ),
                source: "/trace.o",
                license: "GPL",
                created_at: "2026-10-04T09:00:00Z",
                metadata: &BTreeMap::new(),
            },
        )
        .expect("commit tracepoint");
}

fn pending<S: LinkStore>(
    store: &S,
    writer: &RuntimeWriter<'_>,
    program: u32,
    event: &str,
) -> (StoredLink, S::LinkReceipt) {
    store
        .create_pending_tracepoint(
            writer,
            PendingTracepoint {
                program_id: id(program),
                target: &event.parse::<Tracepoint>().expect("tracepoint"),
                metadata: &BTreeMap::from([("owner".into(), "O'Reilly; :id \"λ\"".into())]),
                created_at: "2026-10-04T09:01:02.1200Z",
            },
        )
        .expect("pending link")
}

fn lifecycle<S>(backend: S)
where
    S: OpenStore + CommitLoad + LinkStore + UnloadStore,
    S::Reader: LinkReader,
{
    let temporary = tempfile::tempdir().expect("tempdir");
    let layout = RuntimeLayout::try_from(temporary.path().join("runtime")).expect("layout");
    let runtime = RuntimeDirectory::open_or_create(layout.clone()).expect("runtime");
    let other = RuntimeDirectory::open_or_create(
        RuntimeLayout::try_from(temporary.path().join("other")).expect("layout"),
    )
    .expect("other runtime");
    let store = ActiveStore::open(backend, &layout, Duration::from_secs(1)).expect("active store");
    let mut reader = store
        .open_reader(&runtime)
        .expect("reader")
        .expect("present");

    let (first, receipt, stale_program, maps) = writer(&runtime, |w| {
        commit(&store, w, 42);
        let (program, maps) = store
            .observe_unload(w, id(42))
            .expect("unload observation")
            .expect("present");
        let (first, receipt) = pending(&store, w, 42, "sched/sched_switch");

        assert_eq!(first.state, LinkState::Pending);
        assert_eq!(first.program_id, id(42));
        assert_eq!(first.created_at, "2026-10-04T09:01:02.12Z");
        assert_eq!(first.metadata["owner"], "O'Reilly; :id \"λ\"");
        assert_eq!(
            first.details,
            LinkDetails::Tracepoint("sched/sched_switch".parse().expect("target"))
        );
        assert_eq!(
            first.pin_path,
            layout.link_pin_path(first.id).to_str().expect("path")
        );
        let (linked_program, _) = store
            .observe_unload(w, id(42))
            .expect("linked preflight")
            .expect("program");
        assert!(
            store.delete_program(w, linked_program).is_err(),
            "links block deletion"
        );

        let blocked = store
            .delete_program(w, program)
            .expect_err("pending link blocks even an older unload receipt");
        let observed = reader.read_links().expect("links");
        let programs = reader.read_records().expect("programs");
        let summaries = reader.read_programs().expect("summaries");

        assert_eq!(observed, std::slice::from_ref(&first));
        assert_eq!(programs[0].links, [first.id]);
        assert_eq!(summaries[0].links(), [first.id]);

        (first, receipt, blocked.remaining, maps)
    });

    let receipt = writer(&other, |w| {
        store
            .finalise_link(w, receipt, id(400))
            .expect_err("wrong runtime")
            .remaining
    });

    writer(&runtime, |w| {
        let (_, stale_pending) = store
            .observe_link(w, first.id)
            .expect("observe")
            .expect("present");
        let attached = store
            .finalise_link(w, receipt, id(400))
            .map_err(|e| e.cause)
            .expect("finalise");

        assert_eq!(attached.state, LinkState::Attached { kernel_id: id(400) });
        assert_eq!(attached.id, first.id);
        assert_eq!(
            reader.read_links().expect("fresh snapshot"),
            std::slice::from_ref(&attached)
        );
        assert!(store.delete_link(w, stale_pending).is_err());
        let (linked_program, _) = store
            .observe_unload(w, id(42))
            .expect("linked preflight")
            .expect("program");
        assert!(
            store.delete_program(w, linked_program).is_err(),
            "links block deletion"
        );

        let (_, finalised) = store
            .observe_link(w, first.id)
            .expect("observe")
            .expect("present");
        let error = store
            .finalise_link(w, finalised, id(401))
            .expect_err("cannot re-finalise");

        assert_eq!(reader.read_links().expect("unchanged"), [attached]);

        // The second pending link must survive a failed finalisation. The
        // conflicting kernel identity is a portable failure, without SQL hooks.
        let (second, second_receipt) = pending(&store, w, 42, "syscalls/sys_enter_kill");
        let duplicate = store
            .finalise_link(w, second_receipt, id(400))
            .expect_err("kernel identity is unique");
        let records = reader.read_links().expect("records");

        assert_eq!(records.len(), 2);
        assert_eq!(records[1], second);

        store
            .delete_link(w, duplicate.remaining)
            .map_err(|e| e.cause)
            .expect("compensate pending intent");
        store
            .delete_link(w, error.remaining)
            .map_err(|e| e.cause)
            .expect("delete finalised record");

        assert!(reader.read_links().expect("empty").is_empty());
        assert!(
            reader.read_records().expect("program remains")[0]
                .links
                .is_empty()
        );
        assert!(store.observe_link(w, first.id).expect("absent").is_none());

        // IDs must not be recycled after deletion. Other program mutations do
        // not invalidate an unrelated link's ownership evidence.
        let (third, receipt) = pending(&store, w, 42, "sched/sched_switch");

        assert!(third.id > second.id);

        commit(&store, w, 7);
        store
            .delete_link(w, receipt)
            .map_err(|e| e.cause)
            .expect("unrelated commit preserves receipt");
        store
            .delete_program(w, stale_program)
            .map_err(|e| e.cause)
            .expect("unload prerequisite now satisfied");
        store
            .delete_map_set(w, maps)
            .map_err(|e| e.cause)
            .expect("unused private maps");

        assert_eq!(
            reader.read_records().expect("remaining program")[0].id,
            id(7)
        );
    });
}

fn failures<S>(backend: S)
where
    S: OpenStore + CommitLoad + LinkStore,
    S::Reader: LinkReader + 'static,
{
    let temporary = tempfile::tempdir().expect("tempdir");
    let layout = RuntimeLayout::try_from(temporary.path().to_owned()).expect("layout");
    let runtime = RuntimeDirectory::open_or_create(layout.clone()).expect("runtime");
    let store = ActiveStore::open(backend, &layout, Duration::from_secs(1)).expect("active store");
    let mut reader = store
        .open_reader(&runtime)
        .expect("reader")
        .expect("present");

    writer(&runtime, |w| {
        let target: Tracepoint = "sched/sched_switch".parse().expect("target");
        let metadata = BTreeMap::new();

        assert!(
            store
                .create_pending_tracepoint(
                    w,
                    PendingTracepoint {
                        program_id: id(42),
                        target: &target,
                        metadata: &metadata,
                        created_at: "2026-10-04T12:00:00Z",
                    }
                )
                .is_err()
        );
        assert!(reader.read_links().expect("no orphan intent").is_empty());

        commit(&store, w, 42);

        assert!(
            store
                .create_pending_tracepoint(
                    w,
                    PendingTracepoint {
                        program_id: id(42),
                        target: &target,
                        metadata: &metadata,
                        created_at: "not a timestamp",
                    }
                )
                .is_err()
        );
        assert!(reader.read_links().expect("atomic rejection").is_empty());
        assert!(reader.read_records().expect("records")[0].links.is_empty());

        let (link, receipt) = pending(&store, w, 42, "sched/sched_switch");
        let (_, stale) = store
            .observe_link(w, link.id)
            .expect("observe")
            .expect("present");
        store
            .delete_link(w, receipt)
            .map_err(|e| e.cause)
            .expect("delete");
        let (new, new_receipt) = pending(&store, w, 42, "sched/sched_switch");

        assert!(store.delete_link(w, stale).is_err());
        assert_eq!(
            reader.read_links().expect("new link survives"),
            std::slice::from_ref(&new)
        );

        // A reader with a retained handle can execute on another thread while
        // the writer lock is held. It observes complete intent, without taking it.
        let mut concurrent = reader.clone();
        let (send, recv) = std::sync::mpsc::channel();
        let thread = std::thread::spawn(move || {
            send.send(concurrent.read_links())
                .expect("send observations");
        });
        let result = recv
            .recv_timeout(Duration::from_secs(2))
            .expect("reader does not acquire writer lock")
            .expect("read links");
        thread.join().expect("reader thread");

        assert_eq!(result, [new]);

        store
            .delete_link(w, new_receipt)
            .map_err(|e| e.cause)
            .expect("cleanup");
    });
}

#[test]
fn sqlite_pending_link_lifecycle() {
    lifecycle(bpfman_store_sqlite::Backend);
}

#[test]
fn json_pending_link_lifecycle() {
    lifecycle(bpfman_store_json::Backend);
}

#[test]
fn sqlite_atomic_failures_and_lock_free_readers() {
    failures(bpfman_store_sqlite::Backend);
}

#[test]
fn json_atomic_failures_and_lock_free_readers() {
    failures(bpfman_store_json::Backend);
}

fn kprobe_lifecycle<S>(backend: S)
where
    S: OpenStore + CommitLoad + LinkStore + UnloadStore + Copy,
    S::Reader: LinkReader,
{
    for retprobe in [false, true] {
        let temp = tempfile::tempdir().expect("tempdir");
        let layout = RuntimeLayout::try_from(temp.path().join("runtime")).expect("layout");
        let runtime = RuntimeDirectory::open_or_create(layout.clone()).expect("runtime");
        let other = RuntimeDirectory::open_or_create(
            RuntimeLayout::try_from(temp.path().join("other")).expect("layout"),
        )
        .expect("other");
        let store = ActiveStore::open(backend, &layout, Duration::from_secs(1)).expect("store");
        let name = "probe".try_into().expect("symbol");
        let spec = if retprobe {
            bpfman_model::ProgramSpec::Kretprobe(name)
        } else {
            bpfman_model::ProgramSpec::Kprobe(name)
        };
        let target = bpfman_model::KprobeLink {
            function: "bpfman_e2e_target_0".parse().expect("function"),
            offset: if retprobe { 0 } else { 17 },
            retprobe,
        };
        let pending = |w: &RuntimeWriter<'_>, target: &bpfman_model::KprobeLink| {
            store.create_pending_kprobe(
                w,
                bpfman_store::PendingKprobe {
                    program_id: id(42),
                    target,
                    metadata: &[("owner".into(), "probe".into())].into(),
                    created_at: "2026-10-15T12:00:00Z",
                },
            )
        };
        let (record, receipt) = writer(&runtime, |w| {
            assert!(pending(w, &target).is_err(), "missing program");
            store
                .commit_program(
                    w,
                    LoadRecord {
                        map_owner: None,
                        shared_maps: &[],
                        id: id(42),
                        spec: &spec,
                        source: "/probe.o",
                        globals: &Default::default(),
                        metadata: &Default::default(),
                        license: "GPL",
                        created_at: "2026-10-15T12:00:00Z",
                    },
                )
                .expect("load");
            let mut wrong = target.clone();
            wrong.retprobe = !retprobe;
            assert!(
                pending(w, &wrong).is_err(),
                "selection must match loaded kind"
            );
            wrong = target.clone();
            wrong.offset = u64::MAX;
            assert!(pending(w, &wrong).is_err(), "offset cannot truncate");
            let (record, receipt) = pending(w, &target).expect("pending");
            assert_eq!(record.details, LinkDetails::Kprobe(target.clone()));
            assert_eq!(record.state, LinkState::Pending);
            assert_eq!(
                record.pin_path,
                layout.link_pin_path(record.id).to_str().expect("path")
            );
            let (program, _) = store
                .observe_unload(w, id(42))
                .expect("observe")
                .expect("program");
            assert!(
                store.delete_program(w, program).is_err(),
                "pending link protects program"
            );
            (record, receipt)
        });
        let receipt = writer(&other, |w| {
            store
                .finalise_link(w, receipt, id(400))
                .expect_err("foreign runtime")
                .remaining
        });
        writer(&runtime, |w| {
            let (_, stale) = store
                .observe_link(w, record.id)
                .expect("observe")
                .expect("present");
            let attached = store
                .finalise_link(w, receipt, id(400))
                .map_err(|e| e.cause)
                .expect("finalise");
            assert_eq!(attached.details, record.details);
            assert!(
                store.delete_link(w, stale).is_err(),
                "stale pending evidence"
            );
        });
        let mut reader = backend
            .open_reader(&runtime)
            .expect("reader")
            .expect("exists");
        let observed = reader.read_links().expect("reopened intent");
        assert_eq!(observed.len(), 1);
        assert_eq!(observed[0].details, LinkDetails::Kprobe(target));
        assert_eq!(observed[0].metadata["owner"], "probe");
        assert_eq!(
            observed[0].state,
            LinkState::Attached { kernel_id: id(400) }
        );
        writer(&runtime, |w| {
            let (_, receipt) = store
                .observe_link(w, record.id)
                .expect("observe")
                .expect("present");
            store
                .delete_link(w, receipt)
                .map_err(|e| e.cause)
                .expect("delete unchanged");
            let (program, maps) = store
                .observe_unload(w, id(42))
                .expect("observe")
                .expect("present");
            store
                .delete_program(w, program)
                .map_err(|e| e.cause)
                .expect("delete program");
            store
                .delete_map_set(w, maps)
                .map_err(|e| e.cause)
                .expect("delete map set");
        });
        assert!(reader.read_links().expect("empty").is_empty());
        assert!(reader.read_records().expect("empty").is_empty());
    }
}

#[test]
fn sqlite_kprobe_pending_lifecycle() {
    kprobe_lifecycle(bpfman_store_sqlite::Backend);
}

#[test]
fn json_kprobe_pending_lifecycle() {
    kprobe_lifecycle(bpfman_store_json::Backend);
}

fn tcx_lifecycle<S>(backend: S)
where
    S: OpenStore + CommitLoad + LinkStore,
    S::Reader: LinkReader,
{
    let temp = tempfile::tempdir().expect("tempdir");
    let layout = RuntimeLayout::try_from(temp.path().join("runtime")).expect("layout");
    let other_layout = RuntimeLayout::try_from(temp.path().join("other")).expect("layout");
    let runtime = RuntimeDirectory::open_or_create(layout.clone()).expect("runtime");
    let other = RuntimeDirectory::open_or_create(other_layout).expect("other");
    let store = ActiveStore::open(backend, &layout, Duration::from_secs(1)).expect("store");
    let target = bpfman_model::TcxLink {
        key: bpfman_model::TcKey {
            nsid: std::num::NonZeroU64::MIN,
            ifindex: NonZeroU32::MIN,
            direction: bpfman_model::TcDirection::Ingress,
        },
        interface: "lo".parse().expect("interface"),
        netns: Default::default(),
        priority: 25,
        position: 0,
    };
    writer(&runtime, |w| {
        for raw in [42, 43] {
            store
                .commit_program(
                    w,
                    LoadRecord {
                        map_owner: None,
                        shared_maps: &[],
                        globals: &Default::default(),
                        id: id(raw),
                        spec: &bpfman_model::ProgramSpec::Tcx(
                            Symbol::try_from("native").expect("symbol"),
                        ),
                        source: "/native.o",
                        license: "GPL",
                        created_at: "2026-10-14T12:00:00Z",
                        metadata: &Default::default(),
                    },
                )
                .expect("native program");
        }
    });
    let pending = |w: &RuntimeWriter<'_>, program, target: &bpfman_model::TcxLink| {
        store.create_pending_tcx(
            w,
            bpfman_store::PendingTcx {
                program_id: program,
                target,
                metadata: &Default::default(),
                created_at: "2026-10-14T12:00:00Z",
            },
        )
    };
    let (first, receipt) = writer(&runtime, |w| {
        pending(w, id(42), &target).expect("pending native intent")
    });
    assert_eq!(
        first.pin_path,
        layout
            .tcx_link_path(target.key, id(42))
            .to_str()
            .expect("UTF8")
    );
    let receipt = writer(&other, |w| {
        store
            .finalise_link(w, receipt, id(80))
            .expect_err("foreign runtime")
            .remaining
    });
    let (_, stale) = writer(&runtime, |w| {
        store
            .observe_link(w, first.id)
            .expect("observe")
            .expect("present")
    });
    writer(&runtime, |w| {
        store
            .finalise_link(w, receipt, id(80))
            .map_err(|e| e.cause)
            .expect("finalise")
    });
    writer(&runtime, |w| {
        assert!(
            store.delete_link(w, stale).is_err(),
            "pending receipt cannot delete finalised intent"
        )
    });
    writer(&runtime, |w| {
        assert!(
            pending(w, id(42), &target).is_err(),
            "one native attachment per program/hook"
        )
    });
    let mut higher = target.clone();
    higher.priority = 0;
    let (second, receipt) = writer(&runtime, |w| {
        pending(w, id(43), &higher).expect("higher priority")
    });
    writer(&runtime, |w| {
        store
            .finalise_link(w, receipt, id(81))
            .map_err(|e| e.cause)
            .expect("finalise second")
    });
    let mut reader = store
        .open_reader(&runtime)
        .expect("reader")
        .expect("present");
    let records = reader.read_links().expect("consistent ranks");
    let LinkDetails::Tcx(first_target) = &records[0].details else {
        unreachable!()
    };
    assert_eq!(first_target.position, 1);
    let LinkDetails::Tcx(second_target) = &records[1].details else {
        unreachable!()
    };
    assert_eq!(second_target.position, 0);
    for record in [first, second] {
        let (_, receipt) = writer(&runtime, |w| {
            store
                .observe_link(w, record.id)
                .expect("observe")
                .expect("present")
        });
        writer(&runtime, |w| {
            store
                .delete_link(w, receipt)
                .map_err(|e| e.cause)
                .expect("conditional delete")
        });
    }
    assert!(reader.read_links().expect("empty links").is_empty());
    assert_eq!(reader.read_records().expect("programs retained").len(), 2);
}

#[test]
fn sqlite_tcx_intent_and_conditional_receipts() {
    tcx_lifecycle(bpfman_store_sqlite::Backend);
}
#[test]
fn json_tcx_intent_and_conditional_receipts() {
    tcx_lifecycle(bpfman_store_json::Backend);
}

fn uprobe_lifecycle<S>(backend: S)
where
    S: OpenStore + CommitLoad + LinkStore + UnloadStore + Copy,
    S::Reader: LinkReader,
{
    for (retprobe, container_pid, raw_target) in [
        (false, 0, "/host/binary"),
        (true, 0, "/host/binary"),
        (false, 4567, "/host/binary"),
        (true, 4567, "/host/binary"),
        (false, 0, "libc"),
        (true, 0, "libc.so.6"),
        (false, 4567, "libold"),
        (true, 4567, "libstdc++"),
    ] {
        let temp = tempfile::tempdir().expect("tempdir");
        let layout = RuntimeLayout::try_from(temp.path().join("runtime")).expect("layout");
        let runtime = RuntimeDirectory::open_or_create(layout.clone()).expect("runtime");
        let other = RuntimeDirectory::open_or_create(
            RuntimeLayout::try_from(temp.path().join("other")).expect("layout"),
        )
        .expect("other");
        let store = ActiveStore::open(backend, &layout, Duration::from_secs(1)).expect("store");
        let name = "probe".try_into().expect("symbol");
        let spec = if retprobe {
            bpfman_model::ProgramSpec::Uretprobe(name)
        } else {
            bpfman_model::ProgramSpec::Uprobe(name)
        };
        let target = bpfman_model::UprobeLink::new(
            raw_target,
            if retprobe {
                Some("entry".try_into().expect("symbol"))
            } else {
                None
            },
            if retprobe { 0 } else { 17 },
            1234,
            retprobe,
        )
        .expect("target")
        .with_container_pid(container_pid)
        .expect("container intent");
        let pending = |w: &RuntimeWriter<'_>, target: &bpfman_model::UprobeLink| {
            store.create_pending_uprobe(
                w,
                bpfman_store::PendingUprobe {
                    program_id: id(42),
                    target,
                    metadata: &[("owner".into(), "probe".into())].into(),
                    created_at: "2026-10-15T12:00:00Z",
                },
            )
        };
        let (record, receipt) = writer(&runtime, |w| {
            assert!(pending(w, &target).is_err(), "missing program");
            store
                .commit_program(
                    w,
                    LoadRecord {
                        map_owner: None,
                        shared_maps: &[],
                        id: id(42),
                        spec: &spec,
                        source: "/probe.o",
                        globals: &Default::default(),
                        metadata: &Default::default(),
                        license: "GPL",
                        created_at: "2026-10-15T12:00:00Z",
                    },
                )
                .expect("load");
            let wrong = bpfman_model::UprobeLink::new(
                target.target(),
                Some("entry".try_into().expect("symbol")),
                target.offset(),
                0,
                !retprobe,
            )
            .expect("wrong kind");
            assert!(
                pending(w, &wrong).is_err(),
                "selection must match loaded kind"
            );
            let (record, receipt) = pending(w, &target).expect("pending");
            assert_eq!(record.details, LinkDetails::Uprobe(target.clone()));
            assert_eq!(record.state, LinkState::Pending);
            assert_eq!(
                record.pin_path,
                layout.link_pin_path(record.id).to_str().expect("path")
            );
            let (program, _) = store
                .observe_unload(w, id(42))
                .expect("observe")
                .expect("program");
            assert!(
                store.delete_program(w, program).is_err(),
                "pending link protects program"
            );
            (record, receipt)
        });
        let receipt = writer(&other, |w| {
            store
                .finalise_link(w, receipt, id(400))
                .expect_err("foreign runtime")
                .remaining
        });
        writer(&runtime, |w| {
            let (_, stale) = store
                .observe_link(w, record.id)
                .expect("observe")
                .expect("present");
            let attached = store
                .finalise_link(w, receipt, id(400))
                .map_err(|e| e.cause)
                .expect("finalise");
            assert_eq!(attached.details, record.details);
            assert!(
                store.delete_link(w, stale).is_err(),
                "stale pending evidence"
            );
        });
        let mut reader = backend
            .open_reader(&runtime)
            .expect("reader")
            .expect("exists");
        let observed = reader.read_links().expect("reopened intent");
        assert_eq!(observed.len(), 1);
        assert_eq!(observed[0].details, LinkDetails::Uprobe(target));
        assert_eq!(observed[0].metadata["owner"], "probe");
        assert_eq!(
            observed[0].state,
            LinkState::Attached { kernel_id: id(400) }
        );
        writer(&runtime, |w| {
            let (_, receipt) = store
                .observe_link(w, record.id)
                .expect("observe")
                .expect("present");
            store
                .delete_link(w, receipt)
                .map_err(|e| e.cause)
                .expect("delete unchanged");
            let (program, maps) = store
                .observe_unload(w, id(42))
                .expect("observe")
                .expect("present");
            store
                .delete_program(w, program)
                .map_err(|e| e.cause)
                .expect("delete program");
            store
                .delete_map_set(w, maps)
                .map_err(|e| e.cause)
                .expect("delete map set");
        });
        assert!(reader.read_links().expect("empty").is_empty());
        assert!(reader.read_records().expect("empty").is_empty());
    }
}

#[test]
fn sqlite_uprobe_pending_lifecycle() {
    uprobe_lifecycle(bpfman_store_sqlite::Backend);
}

#[test]
fn json_uprobe_pending_lifecycle() {
    uprobe_lifecycle(bpfman_store_json::Backend);
}

fn function_tracing_lifecycle<S>(backend: S)
where
    S: OpenStore + CommitLoad + LinkStore + UnloadStore + Copy,
    S::Reader: LinkReader,
{
    for kind in [
        bpfman_model::TracingKind::Fentry,
        bpfman_model::TracingKind::Fexit,
    ] {
        let temp = tempfile::tempdir().expect("tempdir");
        let layout = RuntimeLayout::try_from(temp.path().join("runtime")).expect("layout");
        let runtime = RuntimeDirectory::open_or_create(layout.clone()).expect("runtime");
        let other = RuntimeDirectory::open_or_create(
            RuntimeLayout::try_from(temp.path().join("other")).expect("layout"),
        )
        .expect("other");
        let store = ActiveStore::open(backend, &layout, Duration::from_secs(1)).expect("store");
        let name = "probe".try_into().expect("symbol");
        let function: bpfman_model::Symbol = "bpfman_e2e_target_0".try_into().expect("target");
        let spec = match kind {
            bpfman_model::TracingKind::Fentry => bpfman_model::ProgramSpec::Fentry {
                name,
                target: function.clone(),
            },
            bpfman_model::TracingKind::Fexit => bpfman_model::ProgramSpec::Fexit {
                name,
                target: function.clone(),
            },
        };
        let target = bpfman_model::TracingLink { function, kind };
        let pending = |w: &RuntimeWriter<'_>, target: &bpfman_model::TracingLink| {
            store.create_pending_tracing(
                w,
                bpfman_store::PendingTracing {
                    program_id: id(42),
                    target,
                    metadata: &[("owner".into(), "probe".into())].into(),
                    created_at: "2026-10-15T12:00:00Z",
                },
            )
        };
        let (record, receipt) = writer(&runtime, |w| {
            assert!(pending(w, &target).is_err(), "missing program");
            store
                .commit_program(
                    w,
                    LoadRecord {
                        map_owner: None,
                        shared_maps: &[],
                        id: id(42),
                        spec: &spec,
                        source: "/probe.o",
                        globals: &Default::default(),
                        metadata: &Default::default(),
                        license: "GPL",
                        created_at: "2026-10-15T12:00:00Z",
                    },
                )
                .expect("load");
            let mut wrong = target.clone();
            wrong.kind = match kind {
                bpfman_model::TracingKind::Fentry => bpfman_model::TracingKind::Fexit,
                bpfman_model::TracingKind::Fexit => bpfman_model::TracingKind::Fentry,
            };
            assert!(
                pending(w, &wrong).is_err(),
                "selection must match loaded kind"
            );
            wrong = target.clone();
            wrong.function = "other_function".try_into().expect("symbol");
            assert!(
                pending(w, &wrong).is_err(),
                "function must match load target"
            );
            let (record, receipt) = pending(w, &target).expect("pending");
            assert_eq!(record.details, LinkDetails::Tracing(target.clone()));
            assert_eq!(record.state, LinkState::Pending);
            assert_eq!(
                record.pin_path,
                layout.link_pin_path(record.id).to_str().expect("path")
            );
            let (program, _) = store
                .observe_unload(w, id(42))
                .expect("observe")
                .expect("program");
            assert!(
                store.delete_program(w, program).is_err(),
                "pending link protects program"
            );
            (record, receipt)
        });
        let receipt = writer(&other, |w| {
            store
                .finalise_link(w, receipt, id(400))
                .expect_err("foreign runtime")
                .remaining
        });
        writer(&runtime, |w| {
            let (_, stale) = store
                .observe_link(w, record.id)
                .expect("observe")
                .expect("present");
            let attached = store
                .finalise_link(w, receipt, id(400))
                .map_err(|e| e.cause)
                .expect("finalise");
            assert_eq!(attached.details, record.details);
            assert!(
                store.delete_link(w, stale).is_err(),
                "stale pending evidence"
            );
        });
        let mut reader = backend
            .open_reader(&runtime)
            .expect("reader")
            .expect("exists");
        let observed = reader.read_links().expect("reopened intent");
        assert_eq!(observed.len(), 1);
        assert_eq!(observed[0].details, LinkDetails::Tracing(target));
        assert_eq!(observed[0].metadata["owner"], "probe");
        assert_eq!(
            observed[0].state,
            LinkState::Attached { kernel_id: id(400) }
        );
        writer(&runtime, |w| {
            let (_, receipt) = store
                .observe_link(w, record.id)
                .expect("observe")
                .expect("present");
            store
                .delete_link(w, receipt)
                .map_err(|e| e.cause)
                .expect("delete unchanged");
            let (program, maps) = store
                .observe_unload(w, id(42))
                .expect("observe")
                .expect("present");
            store
                .delete_program(w, program)
                .map_err(|e| e.cause)
                .expect("delete program");
            store
                .delete_map_set(w, maps)
                .map_err(|e| e.cause)
                .expect("delete map set");
        });
        assert!(reader.read_links().expect("empty").is_empty());
        assert!(reader.read_records().expect("empty").is_empty());
    }
}

#[test]
fn sqlite_function_tracing_pending_lifecycle() {
    function_tracing_lifecycle(bpfman_store_sqlite::Backend);
}
#[test]
fn json_function_tracing_pending_lifecycle() {
    function_tracing_lifecycle(bpfman_store_json::Backend);
}
