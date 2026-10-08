//! Attached-program teardown runs unchanged against both storage implementations.

use super::{
    faults::{Faults, Point},
    links::{assert_gone, request},
    support::*,
};
use bpfman_core::UnloadKind;
use bpfman_model::LinkState;
use bpfman_runtime::{ActiveStore, Bpfman, Cancellation, PreparedProgram, UnloadErrorKind};
use bpfman_store::{CommitLoad, LinkReader, LinkStore, OpenStore, UnloadStore};

pub(super) fn exercise<S>(backend: S)
where
    S: OpenStore
        + CommitLoad
        + UnloadStore
        + bpfman_store::XdpReplacementStore
        + bpfman_store::TcStore
        + LinkStore
        + Clone,
    S::Reader: LinkReader,
{
    let c = Context::new();
    let store = Faults::new(backend);
    let app = Bpfman::new(
        ActiveStore::open(store.clone(), &c.layout, TIMEOUT).expect("startup"),
        bpfman_kernel_aya::Kernel,
        TIMEOUT,
    );

    for successful_links in 0..2 {
        let loaded = app
            .load(
                PreparedProgram::new(
                    &bpfman_kernel_aya::Kernel,
                    &fixture("tracepoint_counter.bpf.o"),
                    bpfman_model::ProgramSpec::Tracepoint(NAME.try_into().expect("symbol")),
                    Default::default(),
                )
                .expect("request"),
            )
            .expect("load");
        let id = loaded.record.id;
        let first = app.attach_tracepoint(request(id)).expect("first attach");
        let mut second_request = request(id);
        second_request.target = "syscalls/sys_enter_tkill".parse().expect("second target");
        let second = app
            .attach_tracepoint(second_request)
            .expect("second attach");
        let links = [first, second];

        // Cancellation during link preflight leaves every attachment intact.
        let cancellation = Cancellation::new();
        store.cancel_at(Point::ObserveLink, &cancellation);
        let error = app
            .unload_with_cancellation(id, &cancellation)
            .expect_err("cancel preflight");
        assert_eq!(error.kind(), UnloadErrorKind::Cancelled);
        assert!(error.report().is_none());
        c.present(id);
        for link in &links {
            let observed = app.get_link(link.id).expect("preserved link");
            assert!(observed.kernel.is_some() && observed.pin_present);
        }

        // Fail either the first or second record deletion, after real unpinning.
        store.fail_after(Point::DeleteLink, successful_links);
        let error = app.unload(id).expect_err("link record deletion");
        let report = error.report().expect("teardown report");
        let attempts = 2 * (successful_links + 1);
        assert_eq!(report.attempts().len(), attempts);
        assert!(
            report.attempts()[..attempts - 1]
                .iter()
                .all(|a| a.outcome.is_ok())
        );
        assert!(report.attempts()[attempts - 1].outcome.is_err());
        assert_eq!(
            report.attempts()[attempts - 1].kind,
            UnloadKind::LinkRecord(links[successful_links].id)
        );
        c.present(id);

        let observed = app.get(id).expect("partly detached program");
        assert_eq!(observed.links.len(), 2 - successful_links);
        assert!(!observed.links[0].pin_present);
        assert!(observed.links[0].kernel.is_none());
        if successful_links == 0 {
            assert!(observed.links[1].pin_present);
            assert!(observed.links[1].kernel.is_some());
        }

        // The unchanged fault cannot heal itself. No successful step is repeated.
        let error = app.retry_unload(error).expect_err("same fault remains");
        let report = error.report().expect("retained work");
        assert_eq!(report.attempts().len(), attempts + 1);
        assert_eq!(report.attempts()[attempts].id, attempts - 1);
        let remaining = report.unresolved();
        c.present(id);

        let cancellation = Cancellation::new();
        cancellation.cancel();
        let error = app
            .retry_unload_with_cancellation(error, &cancellation)
            .expect_err("cancel retry admission");
        assert_eq!(error.kind(), UnloadErrorKind::Cancelled);
        assert_eq!(
            error.report().expect("history").attempts().len(),
            attempts + 1
        );
        assert_eq!(error.report().expect("receipts").unresolved(), remaining);

        // Repair the fault explicitly. Cancellation after admission must not
        // strand program pins, records, maps, or bytecode midway through the pass.
        store.set(None);
        let cancellation = Cancellation::new();
        store.cancel_at(Point::DeleteLink, &cancellation);
        let report = app
            .retry_unload_with_cancellation(error, &cancellation)
            .expect("finish admitted teardown");
        assert!(cancellation.is_cancelled());
        assert_eq!(report.unresolved(), 0);
        assert_eq!(
            report
                .attempts()
                .iter()
                .filter(|a| a.outcome.is_err())
                .count(),
            2
        );
        for (offset, attempt) in report.attempts()[attempts + 1..].iter().enumerate() {
            assert_eq!(attempt.id, attempts - 1 + offset);
            assert!(attempt.outcome.is_ok());
        }

        assert!(app.list_link_records().expect("links").is_empty());
        assert!(app.list(&Default::default()).expect("programs").is_empty());
        c.absent(id);
        for link in links {
            let LinkState::Attached { kernel_id } = link.state else {
                unreachable!()
            };
            assert_gone(kernel_id);
        }
        // Perf-event teardown and final program reclamation can outlive unpin.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        loop {
            let present = aya::programs::loaded_programs()
                .collect::<Result<Vec<_>, _>>()
                .expect("kernel programs")
                .iter()
                .any(|p| p.id() == id.get());
            if !present {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "unloaded program remains in kernel"
            );
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        c.no_artifacts();
    }
}
