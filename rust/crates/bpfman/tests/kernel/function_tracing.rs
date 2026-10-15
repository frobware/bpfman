//! Native vmlinux/module tracing traffic, compensation and owned-object lifetime.
use super::{
    faults::{Faults, Point},
    support::*,
};
use bpfman_kernel::{LinkObservations, ProgramObservations, TracingLinks};
use bpfman_model::{LinkState, ProgramSpec};
use bpfman_runtime::{ActiveStore, Bpfman, Cancellation, PreparedProgram, TracingAttach};
use bpfman_store::*;
use std::num::NonZeroU32;

fn request(id: NonZeroU32, kind: bpfman_model::TracingKind) -> TracingAttach {
    TracingAttach {
        program_id: id,
        kind,
        metadata: Default::default(),
    }
}

fn absent(id: NonZeroU32) {
    assert_eq!(
        bpfman_kernel_aya::Kernel::default()
            .program(id)
            .expect_err("released program")
            .kind(),
        bpfman_kernel::ErrorKind::Missing
    );
}

pub(super) fn exercise<S>(backend: S)
where
    S: OpenStore
        + CommitLoad
        + UnloadStore
        + LinkStore
        + XdpReplacementStore
        + TcStore
        + Clone
        + 'static,
    S::Reader: LinkReader + XdpDispatcherReader,
{
    for kind in [
        bpfman_model::TracingKind::Fentry,
        bpfman_model::TracingKind::Fexit,
    ] {
        let c = Context::new();
        let store = Faults::new(backend.clone());
        let app = Bpfman::new(
            ActiveStore::open(store.clone(), &c.layout, TIMEOUT).expect("store"),
            bpfman_kernel_aya::Kernel::default(),
            TIMEOUT,
        );
        let prepared = |target: &str| {
            let name = match kind {
                bpfman_model::TracingKind::Fentry => "test_fentry",
                bpfman_model::TracingKind::Fexit => "test_fexit",
            }
            .try_into()
            .expect("name");
            let target = target.try_into().expect("target");
            PreparedProgram::new(
                &bpfman_kernel_aya::Kernel::default(),
                &fixture("fentry_counter.bpf.o"),
                match kind {
                    bpfman_model::TracingKind::Fentry => ProgramSpec::Fentry { name, target },
                    bpfman_model::TracingKind::Fexit => ProgramSpec::Fexit { name, target },
                },
                Default::default(),
            )
            .expect("prepare")
        };
        let error = app
            .load(prepared("bpfman_function_that_does_not_exist"))
            .expect_err("missing BTF target");
        assert_eq!(error.unresolved(), 0);
        assert!(app.list_link_records().expect("no intent").is_empty());
        c.no_artifacts();
        store.set(Some(Point::Commit));
        let error = app
            .load(prepared("do_unlinkat"))
            .expect_err("load publication failure");
        assert_eq!(error.unresolved(), 0);
        store.set(None);
        c.no_artifacts();
        let built_in = app.load(prepared("do_unlinkat")).expect("vmlinux target");
        let built_in_id = built_in.record.id;
        let link = app
            .attach_tracing(request(built_in_id, kind))
            .expect("vmlinux attach");
        assert!(app.get_link(link.id).expect("observed").pin_present);
        let map = aya::maps::MapData::from_pin(
            c.layout
                .map_directory_path(built_in_id)
                .join("fentry_stats_map"),
        )
        .expect("percpu counter");
        let counts = aya::maps::PerCpuArray::<_, u64>::try_from(aya::maps::Map::PerCpuArray(map))
            .expect("percpu array");
        let key = match kind {
            bpfman_model::TracingKind::Fentry => 0,
            bpfman_model::TracingKind::Fexit => 1,
        };
        let before: u64 = counts.get(&key, 0).expect("count").iter().sum();
        tempfile::NamedTempFile::new()
            .expect("unlink target")
            .close()
            .expect("trigger unlink");
        let after: u64 = counts.get(&key, 0).expect("traffic").iter().sum();
        assert!(after > before, "native vmlinux tracing executed");
        drop(counts);
        assert_eq!(app.detach(link.id).expect("vmlinux detach").unresolved(), 0);
        assert_eq!(
            app.unload(built_in_id)
                .expect("vmlinux unload")
                .unresolved(),
            0
        );
        absent(built_in_id);
        c.no_artifacts();
        let load = || {
            let (object, name) = match kind {
                bpfman_model::TracingKind::Fentry => ("fentry_kmod_exact.bpf.o", "test_fentry"),
                bpfman_model::TracingKind::Fexit => ("fexit_kmod_exact.bpf.o", "test_fexit"),
            };
            let name = name.try_into().expect("name");
            // ELF targets slot zero; loading slot one proves requested target override.
            let target = "bpfman_e2e_target_1".try_into().expect("target");
            app.load(
                PreparedProgram::new(
                    &bpfman_kernel_aya::Kernel::default(),
                    &fixture(object),
                    match kind {
                        bpfman_model::TracingKind::Fentry => ProgramSpec::Fentry { name, target },
                        bpfman_model::TracingKind::Fexit => ProgramSpec::Fexit { name, target },
                    },
                    Default::default(),
                )
                .expect("prepare")
                .with_globals(
                    &bpfman_kernel_aya::Kernel::default(),
                    [("weight".into(), 37u64.to_ne_bytes().to_vec())].into(),
                )
                .expect("globals"),
            )
            .expect("load")
            .record
            .id
        };
        let id = load();
        c.writer(|writer| {
            let wrong_target = bpfman_model::TracingLink {
                function: "bpfman_e2e_target_0".try_into().expect("other function"),
                kind,
            };
            assert!(
                bpfman_kernel_aya::Kernel::default()
                    .prepare_tracing(writer, id, &wrong_target)
                    .is_err(),
                "native BTF target must match stored intent"
            );
        });
        let wrong_kind = match kind {
            bpfman_model::TracingKind::Fentry => bpfman_model::TracingKind::Fexit,
            bpfman_model::TracingKind::Fexit => bpfman_model::TracingKind::Fentry,
        };
        let error = app
            .attach_tracing(request(id, wrong_kind))
            .expect_err("wrong family");
        assert!(error.report().is_none());
        assert!(
            app.list_link_records()
                .expect("no wrong-family intent")
                .is_empty()
        );
        store.set(Some(Point::FinaliseLink));
        let error = app
            .attach_tracing(request(id, kind))
            .expect_err("finalise failure");
        assert_eq!(error.report().expect("compensation").unresolved(), 0);
        store.set(None);
        assert!(
            app.list_link_records()
                .expect("no pending intent")
                .is_empty()
        );

        let token = Cancellation::new();
        store.cancel_at(Point::CreateLink, &token);
        let error = app
            .attach_tracing_with_cancellation(request(id, kind), &token)
            .expect_err("cancel after intent");
        assert_eq!(error.report().expect("compensation").unresolved(), 0);
        assert!(
            app.list_link_records()
                .expect("no cancelled intent")
                .is_empty()
        );

        let first = app.attach_tracing(request(id, kind)).expect("first");
        let duplicate = app
            .attach_tracing(request(id, kind))
            .expect_err("the kernel refuses a second link for one tracing program");
        assert_eq!(duplicate.report().expect("compensation").unresolved(), 0);
        assert_eq!(app.list_link_records().expect("original retained").len(), 1);
        let other_id = load();
        let second = app
            .attach_tracing(request(other_id, kind))
            .expect("independent program");
        let map_name = match kind {
            bpfman_model::TracingKind::Fentry => "fe_count",
            bpfman_model::TracingKind::Fexit => "fx_count",
        };
        let map = aya::maps::MapData::from_pin(c.layout.map_directory_path(id).join(map_name))
            .expect("counter");
        let counter =
            aya::maps::Array::<_, u64>::try_from(aya::maps::Map::Array(map)).expect("array");
        let before = counter.get(&0, 0).expect("count");
        for _ in 0..3 {
            std::fs::write("/sys/kernel/debug/bpfman_e2e/trigger_000", b"1")
                .expect("ELF target trigger");
        }
        assert_eq!(
            counter.get(&0, 0).expect("count"),
            before,
            "ELF target was overridden"
        );
        for _ in 0..3 {
            std::fs::write("/sys/kernel/debug/bpfman_e2e/trigger_001", b"1")
                .expect("requested target trigger");
        }
        assert_eq!(counter.get(&0, 0).expect("traffic"), before + 3 * 37);
        drop(counter);
        let saved = c.layout.root().join("fs/saved-probe-link");
        let swap = || {
            std::fs::rename(&first.pin_path, &saved).expect("save");
            std::fs::rename(&second.pin_path, &first.pin_path).expect("replace");
            std::fs::rename(&saved, &second.pin_path).expect("swap");
        };
        swap();
        let error = app
            .unload(id)
            .expect_err("changed pin identity refuses before effects");
        assert!(error.report().is_none());
        assert!(std::path::Path::new(&first.pin_path).exists());
        assert!(std::path::Path::new(&second.pin_path).exists());
        swap();

        // Failed record deletion retains the program after pin removal. Reopen
        // and retry the retained record without adopting the completed pin.
        store.set(Some(Point::DeleteLink));
        let error = app.unload(id).expect_err("record failure");
        assert!(
            error.report().is_some(),
            "teardown reached the injected record deletion"
        );
        assert!(bpfman_kernel_aya::Kernel::default().program(id).is_ok());
        assert!(
            std::path::Path::new(&second.pin_path).exists(),
            "independent program retained"
        );
        {
            let link = &first;
            assert!(!std::path::Path::new(&link.pin_path).exists());
            let LinkState::Attached { kernel_id } = link.state else {
                unreachable!()
            };
            assert_eq!(
                bpfman_kernel_aya::Kernel::default()
                    .extension_link(kernel_id)
                    .expect_err("owned link released")
                    .kind(),
                bpfman_kernel::ErrorKind::Missing
            );
        }
        store.set(None);
        let reopened = Bpfman::new(
            ActiveStore::open(store.clone(), &c.layout, TIMEOUT).expect("reopen"),
            bpfman_kernel_aya::Kernel::default(),
            TIMEOUT,
        );
        assert_eq!(
            reopened
                .retry_unload(error)
                .expect("record-only retry")
                .unresolved(),
            0
        );
        absent(id);
        assert_eq!(
            app.unload(other_id)
                .expect("other program unload")
                .unresolved(),
            0
        );
        absent(other_id);
        c.no_artifacts();

        // A genuine external holder protects the program until its close.
        let id = load();
        let held_program = aya::programs::ProgramInfo::from_pin(c.layout.program_pin_path(id))
            .expect("program")
            .fd()
            .expect("holder");
        app.attach_tracing(request(id, kind))
            .expect("control attach");
        assert_eq!(app.unload(id).expect("held program unload").unresolved(), 0);
        assert!(bpfman_kernel_aya::Kernel::default().program(id).is_ok());
        drop(held_program);
        absent(id);
        c.no_artifacts();
    }
}
