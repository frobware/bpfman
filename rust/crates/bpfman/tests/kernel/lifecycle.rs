//! The same live lifecycle scenarios can run against any conforming store.

use super::{
    faults::{Faults, Point},
    support::*,
};
use bpfman_model::{ObservedProgram, Symbol};
use bpfman_store::{CommitLoad, OpenStore, UnloadStore};
use std::{collections::BTreeMap, fs};

fn load<S: OpenStore + CommitLoad>(
    bpfman: &bpfman_runtime::Bpfman<S>,
) -> Result<ObservedProgram, bpfman_runtime::LoadError> {
    let request = bpfman_runtime::PreparedTracepoint::new(
        &fixture("tracepoint_counter.bpf.o"),
        Symbol::try_from(NAME).expect("symbol"),
        BTreeMap::new(),
    )?;
    bpfman.load(request)
}

pub(super) fn exercise<S: OpenStore + CommitLoad + UnloadStore + Clone + Sync>(backend: S) {
    let c = Context::new();
    let store = Faults::new(backend);
    let active =
        bpfman_runtime::ActiveStore::open(store.clone(), &c.layout, TIMEOUT).expect("active store");
    let bpfman = bpfman_runtime::Bpfman::new(active, std::time::Duration::from_millis(50));
    let unrelated = load(&bpfman).expect("unrelated load").record.id;
    let pid = load(&bpfman).expect("load").record.id;
    c.present(pid);
    c.present(unrelated);

    // Full observations must proceed even while the giant writer lock is held.
    c.writer(|_| {
        let observed = bpfman.get(pid).expect("get while writer held");
        assert_eq!(observed.record.id, pid);
        assert!(observed.maps.iter().any(|map| map.present));

        let entries = bpfman
            .list_entries(&Default::default())
            .expect("full list while writer held");
        assert_eq!(entries.len(), 2);
        assert!(entries.iter().all(|entry| entry.kernel.is_some()));
    });

    let commits = store.count(Point::Commit);
    let denied = c
        .writer(|_| {
            std::thread::scope(|threads| {
                threads.spawn(|| load(&bpfman)).join().expect("load worker")
            })
        })
        .expect_err("load must acquire writer authority");

    assert_eq!(denied.kind(), bpfman_runtime::LoadErrorKind::Unavailable);
    assert_eq!(denied.unresolved(), 0);
    assert_eq!(store.count(Point::Commit), commits);

    // Failed scope observation precedes every destructive effect.
    store.set(Some(Point::ObserveUnload));

    assert!(bpfman.unload(pid).is_err());
    assert_eq!(store.count(Point::DeleteProgram), 0);
    c.present(pid);
    c.present(unrelated);

    // Record deletion fails after unpinning. Independent bytecode cleanup runs.
    store.set(Some(Point::DeleteProgram));

    let error = bpfman.unload(pid).expect_err("delete failure");

    assert!(!c.layout.program_pin_path(pid).exists());
    assert!(!c.layout.bytecode_path(pid).exists());
    assert!(c.layout.map_directory_path(pid).exists());

    let entries = bpfman.list_entries(&Default::default()).expect("list");

    assert!(
        entries
            .iter()
            .any(|e| e.record.id == pid && e.kernel.is_none())
    );
    c.present(unrelated);

    let error = bpfman.retry_unload(error).expect_err("unchanged fault");

    assert_eq!(store.count(Point::DeleteProgram), 2);
    assert_eq!(store.count(Point::DeleteMapSet), 0);
    store.set(None);

    let report = bpfman
        .retry_unload(error)
        .expect("retry after fault removed");

    assert_eq!(report.unresolved(), 0);
    c.absent(pid);
    c.present(unrelated);
    assert_eq!(
        store.count(Point::ObserveUnload),
        2,
        "retries must use retained evidence"
    );

    let pid = load(&bpfman).expect("load").record.id;
    store.set(Some(Point::DeleteMapSet));
    let report = bpfman
        .unload(pid)
        .expect("successful unload with GC warning");

    assert_eq!(report.unresolved(), 1);
    c.absent(pid);
    c.present(unrelated);
    assert!(report.attempts().iter().any(|a| a.outcome.is_err()));

    let missing = bpfman.unload(pid).expect_err("record already gone");

    assert_eq!(missing.kind(), bpfman_runtime::UnloadErrorKind::NotFound);
    store.set(None);
    assert_eq!(
        bpfman
            .retry_unload_cleanup(report)
            .expect("finish GC")
            .unresolved(),
        0
    );
    assert_eq!(
        bpfman
            .unload(unrelated)
            .expect("unrelated unload")
            .unresolved(),
        0
    );
    c.no_artifacts();

    // The failure is a domain commit failure, independent of storage format.
    store.set(Some(Point::Commit));

    let error = load(&bpfman).expect_err("commit failure");

    assert_eq!(error.unresolved(), 0);
    assert!(format!("{:#}", anyhow::Error::new(error)).contains("injected Commit"));
    assert!(bpfman.list(&Default::default()).expect("list").is_empty());
    c.no_artifacts();
    store.set(None);

    // A blocked explicit cleanup pass must retain the original load failure
    // and its real bytecode receipts, for both persistence backends.
    store.set(Some(Point::CommitWithBlockedCleanup));
    let error = load(&bpfman).expect_err("commit and bytecode cleanup failure");
    let remaining = error.unresolved();
    assert!(remaining > 0);

    let cancellation = bpfman_runtime::Cancellation::new();
    cancellation.cancel();
    let error = bpfman.retry_load_cleanup_with_cancellation(error, &cancellation);
    assert_eq!(error.unresolved(), remaining);
    assert_eq!(
        error
            .retry_lock_error()
            .expect("cancelled admission")
            .kind(),
        bpfman_runtime::ErrorKind::Cancelled
    );

    let error = c.writer(|_| {
        std::thread::scope(|threads| {
            threads
                .spawn(|| bpfman.retry_load_cleanup(error))
                .join()
                .expect("cleanup worker")
        })
    });

    assert_eq!(error.unresolved(), remaining);
    assert_eq!(
        error
            .retry_lock_error()
            .expect("lock admission failure")
            .kind(),
        bpfman_runtime::ErrorKind::TimedOut
    );

    let id = store.blocked_bytecode();
    let path = c.layout.bytecode_path(id);
    let directory = path.parent().expect("program directory");
    fs::rename(directory, c.layout.root().join("rejected-bytecode-link")).expect("save symlink");
    fs::rename(c.layout.root().join("saved-bytecode"), directory).expect("restore bytecode");
    store.set(None);
    let error = bpfman.retry_load_cleanup(error);

    assert_eq!(error.unresolved(), 0);
    assert!(error.retry_lock_error().is_none());
    assert!(
        format!("{:#}", anyhow::Error::new(error)).contains("injected CommitWithBlockedCleanup")
    );
    c.no_artifacts();

    // Publication failure must not reach commit or follow a staging symlink.

    let commits = store.count(Point::Commit);
    let root = c.layout.root();
    let staging = root.join(".staging");
    fs::rename(&staging, root.join("saved-staging")).expect("save staging");
    let outside = root.parent().expect("temporary parent").join("outside");
    fs::create_dir(&outside).expect("outside");
    fs::write(outside.join("sentinel"), b"keep").expect("sentinel");
    std::os::unix::fs::symlink(&outside, &staging).expect("symlink");
    let error = load(&bpfman).expect_err("publication failure");

    assert_eq!(error.unresolved(), 0);
    assert_eq!(store.count(Point::Commit), commits);
    assert_eq!(names(&outside), ["sentinel"]);
    fs::rename(&staging, root.join("rejected-staging-link")).expect("save symlink");
    fs::rename(root.join("saved-staging"), &staging).expect("restore staging");
    c.no_artifacts();

    store.set(Some(Point::ReadAfterCommit));

    let error = load(&bpfman).expect_err("post-commit read failure");

    assert!(format!("{:#}", anyhow::Error::new(error)).contains("was committed"));
    store.set(None);

    let entries = bpfman
        .list_entries(&Default::default())
        .expect("committed record");

    assert_eq!(entries.len(), 1);
    assert!(entries[0].kernel.is_some());

    let pid = entries[0].record.id;
    c.present(pid);
    bpfman.get(pid).expect("get after fault removed");

    assert_eq!(bpfman.unload(pid).expect("unload").unresolved(), 0);
    c.no_artifacts();
    assert!(
        bpfman
            .list(&Default::default())
            .expect("empty records")
            .is_empty()
    );
}
