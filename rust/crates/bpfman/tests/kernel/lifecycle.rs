//! The same live lifecycle scenarios can run against any conforming store.
use super::{
    faults::{Faults, Point},
    support::*,
};
use bpfman_model::{ObservedProgram, Symbol};
use bpfman_store::{CommitLoad, OpenStore, UnloadStore};
use std::{collections::BTreeMap, fs};
fn load<S: OpenStore + CommitLoad>(
    store: &S,
    c: &Context,
) -> Result<ObservedProgram, bpfman_runtime::LoadError> {
    bpfman_runtime::load_tracepoint(
        store,
        &c.layout,
        &fixture("tracepoint_counter.bpf.o"),
        Symbol::try_from(NAME).expect("symbol"),
        &BTreeMap::new(),
        TIMEOUT,
    )
}
pub(super) fn exercise<S: OpenStore + CommitLoad + UnloadStore>(backend: S) {
    let c = Context::new();
    let store = Faults::new(backend);
    let unrelated = load(&store, &c).expect("unrelated load").record.id;
    let pid = load(&store, &c).expect("load").record.id;
    c.present(pid);
    c.present(unrelated);
    // Failed scope observation precedes every destructive effect.
    store.set(Some(Point::ObserveUnload));
    assert!(bpfman_runtime::unload_tracepoint(&store, &c.layout, pid, TIMEOUT).is_err());
    assert_eq!(store.count(Point::DeleteProgram), 0);
    c.present(pid);
    c.present(unrelated);
    // Record deletion fails after unpinning. Independent bytecode cleanup runs.
    store.set(Some(Point::DeleteProgram));
    let error = bpfman_runtime::unload_tracepoint(&store, &c.layout, pid, TIMEOUT)
        .expect_err("delete failure");
    assert!(!c.layout.program_pin_path(pid).exists());
    assert!(!c.layout.bytecode_path(pid).exists());
    assert!(c.layout.map_directory_path(pid).exists());
    let entries =
        bpfman_runtime::list_program_entries(&store, &c.layout, &Default::default(), TIMEOUT)
            .expect("list");
    assert!(
        entries
            .iter()
            .any(|e| e.record.id == pid && e.kernel.is_none())
    );
    c.present(unrelated);
    let error = c
        .writer(|w| error.retry(&store, w))
        .expect_err("unchanged fault");
    assert_eq!(store.count(Point::DeleteProgram), 2);
    assert_eq!(store.count(Point::DeleteMapSet), 0);
    store.set(None);
    let report = c
        .writer(|w| error.retry(&store, w))
        .expect("retry after fault removed");
    assert_eq!(report.unresolved(), 0);
    c.absent(pid);
    c.present(unrelated);
    assert_eq!(
        store.count(Point::ObserveUnload),
        2,
        "retries must use retained evidence"
    );

    let pid = load(&store, &c).expect("load").record.id;
    store.set(Some(Point::DeleteMapSet));
    let report = bpfman_runtime::unload_tracepoint(&store, &c.layout, pid, TIMEOUT)
        .expect("successful unload with GC warning");
    assert_eq!(report.unresolved(), 1);
    c.absent(pid);
    c.present(unrelated);
    assert!(report.attempts().iter().any(|a| a.outcome.is_err()));
    let missing = bpfman_runtime::unload_tracepoint(&store, &c.layout, pid, TIMEOUT)
        .expect_err("record already gone");
    assert_eq!(missing.kind(), bpfman_runtime::UnloadErrorKind::NotFound);
    store.set(None);
    assert_eq!(
        c.writer(|w| report.retry_cleanup(&store, w))
            .expect("finish GC")
            .unresolved(),
        0
    );
    assert_eq!(
        bpfman_runtime::unload_tracepoint(&store, &c.layout, unrelated, TIMEOUT)
            .expect("unrelated unload")
            .unresolved(),
        0
    );
    c.no_artifacts();

    // The failure is a domain commit failure, independent of storage format.
    store.set(Some(Point::Commit));
    let error = load(&store, &c).expect_err("commit failure");
    assert_eq!(error.unresolved(), 0);
    assert!(format!("{:#}", anyhow::Error::new(error)).contains("injected Commit"));
    assert!(
        bpfman_runtime::list_programs(&store, &c.layout, &Default::default(), TIMEOUT)
            .expect("list")
            .is_empty()
    );
    c.no_artifacts();
    store.set(None);

    // Publication failure must not reach commit or follow a staging symlink.
    let commits = store.count(Point::Commit);
    let root = c.layout.root();
    let staging = root.join(".staging");
    fs::rename(&staging, root.join("saved-staging")).expect("save staging");
    let outside = root.parent().expect("temporary parent").join("outside");
    fs::create_dir(&outside).expect("outside");
    fs::write(outside.join("sentinel"), b"keep").expect("sentinel");
    std::os::unix::fs::symlink(&outside, &staging).expect("symlink");
    let error = load(&store, &c).expect_err("publication failure");
    assert_eq!(error.unresolved(), 0);
    assert_eq!(store.count(Point::Commit), commits);
    assert_eq!(names(&outside), ["sentinel"]);
    fs::rename(&staging, root.join("rejected-staging-link")).expect("save symlink");
    fs::rename(root.join("saved-staging"), &staging).expect("restore staging");
    c.no_artifacts();

    store.set(Some(Point::ReadAfterCommit));
    let error = load(&store, &c).expect_err("post-commit read failure");
    assert!(format!("{:#}", anyhow::Error::new(error)).contains("was committed"));
    store.set(None);
    let entries =
        bpfman_runtime::list_program_entries(&store, &c.layout, &Default::default(), TIMEOUT)
            .expect("committed record");
    assert_eq!(entries.len(), 1);
    assert!(entries[0].kernel.is_some());
    let pid = entries[0].record.id;
    c.present(pid);
    bpfman_runtime::get_program(&store, &c.layout, pid, TIMEOUT).expect("get after fault removed");
    assert_eq!(
        bpfman_runtime::unload_tracepoint(&store, &c.layout, pid, TIMEOUT)
            .expect("unload")
            .unresolved(),
        0
    );
    c.no_artifacts();
    assert!(
        bpfman_runtime::list_programs(&store, &c.layout, &Default::default(), TIMEOUT)
            .expect("empty records")
            .is_empty()
    );
}
