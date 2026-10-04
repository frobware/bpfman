//! One shared MR–SW scenario, using the same active-store setup as the CLI.
#![allow(clippy::expect_used)]

use bpfman_fs::{RuntimeDirectory, RuntimeLayout};
use bpfman_lock::AcquireOptions;
use bpfman_model::Symbol;
use bpfman_runtime::{ActiveStore, Bpfman};
use bpfman_store::{CommitLoad, OpenStore, ProgramReader, TracepointRecord};
use std::{collections::BTreeMap, num::NonZeroU32, sync::mpsc, time::Duration};

const BUDGET: Duration = Duration::from_secs(5);

fn exercise<S: OpenStore + CommitLoad + Copy + Send + Sync>(backend: S) {
    let temporary = tempfile::tempdir().expect("runtime directory");
    let layout = RuntimeLayout::try_from(temporary.path().to_owned()).expect("layout");
    let store = ActiveStore::open(backend, &layout, BUDGET).expect("startup");
    let shared = Bpfman::new(
        ActiveStore::open(backend, &layout, BUDGET).expect("shared application"),
        Duration::from_millis(50),
    );
    let runtime = RuntimeDirectory::open_existing(layout.clone())
        .expect("open runtime")
        .expect("existing runtime");

    runtime
        .with_writer(
            AcquireOptions {
                timeout: BUDGET,
                cancelled: None,
            },
            |writer| {
                std::thread::scope(|scope| {
                    let mut workers = Vec::new();

                    for _ in 0..4 {
                        let (request, requests) = mpsc::channel();
                        let (reply, replies) = mpsc::channel();
                        let layout = &layout;
                        let shared = &shared;

                        scope.spawn(move || {
                            // Startup and opening a fresh reader must complete while
                            // the other thread owns the giant lock, before it releases it.
                            let active =
                                ActiveStore::open(backend, layout, Duration::from_millis(50))
                                    .expect("reader startup must not wait for writer");
                            let runtime = RuntimeDirectory::open_existing(layout.clone())
                                .expect("read-only runtime")
                                .expect("runtime");
                            let mut reader = active
                                .open_reader(&runtime)
                                .expect("reader")
                                .expect("store");

                            let bpfman = Bpfman::new(active, Duration::from_millis(50));
                            assert!(
                                bpfman
                                    .list_entries(&Default::default())
                                    .expect("full listing must not wait for writer")
                                    .is_empty()
                            );
                            let missing = bpfman
                                .get(NonZeroU32::new(u32::MAX).expect("id"))
                                .expect_err("missing record, without waiting for writer");

                            assert_eq!(
                                missing.kind(),
                                bpfman_runtime::ObservationErrorKind::NotFound
                            );
                            reply.send(()).expect("ready");

                            while requests.recv().is_ok() {
                                let records = reader
                                    .read_records()
                                    .expect("complete snapshot during publication");
                                let mut ids: Vec<_> =
                                    records.iter().map(|row| row.id.get()).collect();
                                ids.sort_unstable();

                                assert_eq!(ids, (1..=ids.len() as u32).collect::<Vec<_>>());
                                for row in records {
                                    assert_eq!(row.map_set, row.id);
                                    assert_eq!(row.metadata["sequence"], row.id.to_string());
                                    assert_eq!(row.spec.name().as_str(), "trace");
                                }

                                // Exercise application reads too; separate calls may
                                // legitimately observe different committed generations.
                                bpfman
                                    .list(&Default::default())
                                    .expect("listing must not wait for writer");
                                shared
                                    .list(&Default::default())
                                    .expect("shared instance permits concurrent readers");
                                reply.send(()).expect("read completed");
                            }
                        });

                        workers.push((request, replies));
                    }

                    for (_, replies) in &workers {
                        replies
                            .recv_timeout(BUDGET)
                            .expect("readers opened with writer held");
                    }

                    for sequence in 1..=32 {
                        for (request, _) in &workers {
                            request.send(()).expect("read concurrently");
                        }

                        store
                            .commit_tracepoint(
                                &writer,
                                TracepointRecord {
                                    globals: &Default::default(),
                                    id: NonZeroU32::new(sequence).expect("id"),
                                    name: &Symbol::try_from("trace").expect("symbol"),
                                    source: "/source.o",
                                    license: "GPL",
                                    created_at: "2026-10-03T12:00:00Z",
                                    metadata: &BTreeMap::from([(
                                        "sequence".into(),
                                        sequence.to_string(),
                                    )]),
                                },
                            )
                            .expect("single writer commit");

                        for (_, replies) in &workers {
                            replies
                                .recv_timeout(BUDGET)
                                .expect("readers progressed with writer held");
                        }
                    }
                });
            },
        )
        .expect("writer scope");

    assert_eq!(
        shared
            .list(&Default::default())
            .expect("latest state through retained handle")
            .len(),
        32
    );
    let mut reader = store.open_reader(&runtime).expect("reader").expect("store");
    assert_eq!(
        reader.read_records().expect("final committed state").len(),
        32
    );
}

#[test]
fn sqlite_multiple_readers_single_writer() {
    exercise(bpfman_store_sqlite::Backend);
}

#[test]
fn json_multiple_readers_single_writer() {
    exercise(bpfman_store_json::Backend);
}

fn concurrent_startup<S: OpenStore + Copy + Send>(backend: S) {
    let temporary = tempfile::tempdir().expect("tempdir");
    let layout = RuntimeLayout::try_from(temporary.path().join("runtime")).expect("layout");

    let start = std::sync::Barrier::new(4);

    std::thread::scope(|scope| {
        let start = &start;
        let mut threads = Vec::new();

        for _ in 0..4 {
            let layout = &layout;
            threads.push(scope.spawn(move || {
                start.wait();
                let active = ActiveStore::open(backend, layout, BUDGET).expect("startup");
                let bpfman = Bpfman::new(active, BUDGET);
                let records = bpfman
                    .list(&Default::default())
                    .expect("complete initialized store");

                assert!(records.is_empty());
            }));
        }

        for thread in threads {
            thread.join().expect("startup thread");
        }
    });
}

#[test]
fn sqlite_concurrent_startup() {
    concurrent_startup(bpfman_store_sqlite::Backend);
}

#[test]
fn json_concurrent_startup() {
    concurrent_startup(bpfman_store_json::Backend);
}
