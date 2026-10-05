//! Batch outcomes through the public API, identical for both store backends.
use super::{
    faults::{Faults, Point},
    support::*,
};
use bpfman_runtime::{ActiveStore, Bpfman, PreparedProgram, PreparedPrograms};
use bpfman_store::{CommitLoad, LinkReader, LinkStore, OpenStore, UnloadStore};

fn prepare() -> PreparedPrograms {
    PreparedProgram::new(
        &fixture("multi_prog_tracepoint_kmod_counter.bpf.o"),
        bpfman_model::ProgramSpec::Tracepoint("tp_c".try_into().expect("symbol")),
        Default::default(),
    )
    .expect("first")
    .with_additional_programs(vec![
        bpfman_model::ProgramSpec::Tracepoint("tp_a".try_into().expect("symbol")),
        bpfman_model::ProgramSpec::Tracepoint("tp_b".try_into().expect("symbol")),
    ])
    .expect("batch")
}

pub(super) fn exercise<S>(backend: S)
where
    S: OpenStore + CommitLoad + UnloadStore + LinkStore + Clone,
    S::Reader: LinkReader,
{
    let c = Context::new();
    let store = Faults::new(backend);
    let app = Bpfman::new(
        ActiveStore::open(store.clone(), &c.layout, TIMEOUT).expect("store"),
        TIMEOUT,
    );
    let loaded = app.load_batch(prepare()).expect("batch");
    assert_eq!(
        loaded
            .iter()
            .map(|p| p.record.spec.name().as_str())
            .collect::<Vec<_>>(),
        ["tp_c", "tp_a", "tp_b"]
    );
    assert_eq!(store.count(Point::Commit), 1);
    assert_eq!(app.list(&Default::default()).expect("list").len(), 3);
    // Each selection has private map ownership and can be unloaded independently.
    for member in &loaded {
        assert_eq!(member.record.map_set, member.record.id);
        assert_eq!(
            app.unload(member.record.id).expect("unload").unresolved(),
            0
        );
        c.absent(member.record.id);
    }
    c.no_artifacts();

    let commits = store.count(Point::Commit);
    let bad = PreparedProgram::new(
        &fixture("tracepoint_batch_bad.bpf.o"),
        bpfman_model::ProgramSpec::Tracepoint("good".try_into().expect("symbol")),
        Default::default(),
    )
    .expect("good")
    .with_additional_programs(vec![bpfman_model::ProgramSpec::Tracepoint(
        "bad".try_into().expect("symbol"),
    )])
    .expect("valid ELF, verifier rejects second");
    let failure = app
        .load_batch(bad)
        .expect_err("second program verifier failure");
    assert_eq!(failure.unresolved(), 0);
    assert_eq!(store.count(Point::Commit), commits);
    assert!(app.list(&Default::default()).expect("list").is_empty());
    c.no_artifacts();

    store.set(Some(Point::Commit));
    let failure = app.load_batch(prepare()).expect_err("batch commit failure");
    assert_eq!(failure.unresolved(), 0);
    assert!(app.list(&Default::default()).expect("list").is_empty());
    c.no_artifacts();

    // An earlier member's unresolved bytecode must survive batch cleanup and
    // cancellation of a later explicit retry; all other members are removed.
    store.set(Some(Point::CommitWithBlockedCleanup));
    let failure = app
        .load_batch(prepare())
        .expect_err("blocked earlier cleanup");
    let unresolved = failure.unresolved();
    assert!(unresolved > 0);
    let cancellation = bpfman_runtime::Cancellation::new();
    cancellation.cancel();
    let failure = app.retry_load_cleanup_with_cancellation(failure, &cancellation);
    assert_eq!(failure.unresolved(), unresolved);
    assert_eq!(
        failure.retry_lock_error().expect("cancelled retry").kind(),
        bpfman_runtime::ErrorKind::Cancelled
    );
    let bytecode = c.layout.bytecode_path(store.blocked_bytecode());
    let directory = bytecode.parent().expect("directory");
    std::fs::rename(directory, c.layout.root().join("rejected-bytecode-link"))
        .expect("save symlink");
    std::fs::rename(c.layout.root().join("saved-bytecode"), directory).expect("restore bytecode");
    store.set(None);
    let failure = app.retry_load_cleanup(failure);
    assert_eq!(failure.unresolved(), 0);
    assert!(
        format!("{:#}", anyhow::Error::new(failure)).contains("injected CommitWithBlockedCleanup")
    );
    c.no_artifacts();

    store.set(Some(Point::ReadAfterCommit));
    let failure = app
        .load_batch(prepare())
        .expect_err("post-commit observation failure");
    assert!(format!("{:#}", anyhow::Error::new(failure)).contains("was committed"));
    store.set(None);
    let records = app.list(&Default::default()).expect("committed batch");
    assert_eq!(records.len(), 3);
    for record in records {
        assert_eq!(
            app.unload(record.id())
                .expect("unload committed member")
                .unresolved(),
            0
        );
    }
    c.no_artifacts();
}

pub(super) fn cli(store: &'static str) {
    let c = Context::with_store(store);
    let source = fixture("multi_prog_tracepoint_kmod_counter.bpf.o");
    let args = [
        "program",
        "load",
        "file",
        source.to_str().expect("path"),
        "--programs",
        "tracepoint:tp_c,tracepoint:tp_a,tracepoint:tp_b",
        "-o",
        "json",
    ];

    // The entire selection is validated before even creating the runtime.
    let mut invalid = args;
    invalid[5] = "tracepoint:tp_c,tracepoint:missing";
    c.run(&rust(), &invalid, false);
    assert!(!c.layout.root().exists());

    let loaded = c.json(&rust(), &args);
    let programs = loaded["programs"].as_array().expect("batch envelope");
    assert_eq!(programs.len(), 3);
    for (program, expected) in programs.iter().zip(["tp_c", "tp_a", "tp_b"]) {
        let pid = id(program);
        let observed = c.json(&rust(), &["program", "get", &pid.to_string(), "-o", "json"]);
        assert_eq!(observed["record"], program["record"]);
        // Verify full ELF selection ordering through public text output as well.
        let text = c.run(&rust(), &["program", "get", &pid.to_string()], true);
        assert!(
            String::from_utf8(text.stdout)
                .expect("text")
                .contains(expected)
        );
        c.run(&rust(), &["program", "unload", &pid.to_string()], true);
    }
    c.no_artifacts();

    // A delivery error cannot trigger compensation after the batch committed.
    let output = c
        .command(&rust(), &args)
        .stdout(std::process::Stdio::from(
            std::fs::OpenOptions::new()
                .write(true)
                .open("/dev/full")
                .expect("full sink"),
        ))
        .output()
        .expect("CLI");
    assert_eq!(output.status.code(), Some(1));
    let list = c.json(&rust(), &["program", "list", "-o", "json"]);
    let programs = list["programs"].as_array().expect("retained batch");
    assert_eq!(programs.len(), 3);
    for program in programs {
        c.run(
            &rust(),
            &["program", "unload", &id(program).to_string()],
            true,
        );
    }
    c.no_artifacts();
}
