//! XDP load has a temporary verification target but no attachment or dispatcher pin.
use super::{
    faults::{Faults, Point},
    support::*,
};
use bpfman_model::{ProgramSpec, ProgramType};
use bpfman_runtime::{ActiveStore, Bpfman, PreparedProgram};
use bpfman_store::{CommitLoad, LinkReader, LinkStore, OpenStore, UnloadStore};
use std::collections::BTreeSet;

fn request(object: &str, name: &str) -> PreparedProgram {
    PreparedProgram::new(
        &bpfman_kernel_aya::Kernel,
        &fixture(object),
        ProgramSpec::Xdp(name.try_into().expect("symbol")),
        Default::default(),
    )
    .expect("prepare XDP")
}

fn dispatchers() -> BTreeSet<u32> {
    aya::programs::loaded_programs()
        .filter_map(|p| {
            let p = p.expect("enumerate kernel programs");
            (p.name() == b"xdp_dispatcher").then_some(p.id())
        })
        .collect()
}

fn dispatchers_released(baseline: &BTreeSet<u32>) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        if dispatchers().is_subset(baseline) {
            return;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "temporary verification target leaked"
        );
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}

pub(super) fn exercise<S>(backend: S)
where
    S: OpenStore + CommitLoad + UnloadStore + bpfman_store::XdpReplacementStore + LinkStore + Clone,
    S::Reader: LinkReader,
{
    let c = Context::new();
    let store = Faults::new(backend);
    let app = Bpfman::new(
        ActiveStore::open(store.clone(), &c.layout, TIMEOUT).expect("store"),
        bpfman_kernel_aya::Kernel,
        TIMEOUT,
    );
    let baseline = dispatchers();

    // Both section forms become EXT programs, and each has private maps.
    for (object, name) in [
        ("xdp_pass.bpf.o", "pass"),
        ("xdp_frags_pass.bpf.o", "frags_pass"),
    ] {
        let loaded = app.load(request(object, name)).expect("load extension");
        let id = loaded.record.id;
        assert_eq!(loaded.record.spec.kind(), ProgramType::Xdp);
        let info = aya::programs::ProgramInfo::from_pin(c.layout.program_pin_path(id))
            .expect("live extension");
        assert_eq!(
            info.program_type(),
            aya::programs::ProgramType::Extension.into()
        );
        assert!(loaded.record.links.is_empty());
        assert_eq!(
            names(&c.layout.root().join("fs")),
            ["maps", &format!("prog_{id}")]
        );
        let got = app.get(id).expect("get");
        assert_eq!(got.record.spec, loaded.record.spec);
        assert!(got.maps.iter().all(|m| m.present));
        // Wrong attachment family is refused before creating a link.
        let attach = bpfman_runtime::TracepointAttach {
            program_id: id,
            target: "sched/sched_switch".parse().expect("target"),
            metadata: Default::default(),
        };
        assert!(app.attach_tracepoint(attach).is_err());
        assert_eq!(app.unload(id).expect("unload").unresolved(), 0);
        c.absent(id);
        dispatchers_released(&baseline);
    }

    // Failure of a later selected member compensates the earlier extension.
    let bad = request("multi_prog_one_bad.bpf.o", "good")
        .with_additional_programs(
            &bpfman_kernel_aya::Kernel,
            vec![ProgramSpec::Xdp("bad".try_into().expect("symbol"))],
        )
        .expect("batch");
    assert_eq!(
        app.load_batch(bad)
            .expect_err("verifier failure")
            .unresolved(),
        0
    );
    assert!(app.list(&Default::default()).expect("empty").is_empty());
    c.no_artifacts();
    dispatchers_released(&baseline);

    store.set(Some(Point::Commit));
    assert_eq!(
        app.load(request("xdp_pass.bpf.o", "pass"))
            .expect_err("commit failure")
            .unresolved(),
        0
    );
    assert!(app.list(&Default::default()).expect("empty").is_empty());
    c.no_artifacts();
    dispatchers_released(&baseline);
    store.set(None);

    // Teardown failure retains evidence and can be retried for XDP too.
    let id = app
        .load(request("xdp_pass.bpf.o", "pass"))
        .expect("load")
        .record
        .id;
    store.set(Some(Point::DeleteProgram));
    let failure = app.unload(id).expect_err("delete failure");
    assert!(!c.layout.program_pin_path(id).exists());
    assert!(c.layout.map_directory_path(id).exists());
    store.set(None);
    assert_eq!(app.retry_unload(failure).expect("retry").unresolved(), 0);
    c.no_artifacts();
    dispatchers_released(&baseline);
}
