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
        let attempts = 4;
        let failures = 2 - successful_links;
        assert_eq!(report.attempts().len(), attempts);
        for (index, attempt) in report.attempts().iter().enumerate() {
            assert_eq!(
                attempt.outcome.is_err(),
                index % 2 == 1 && index / 2 >= successful_links
            );
        }
        c.present(id);

        let observed = app.get(id).expect("partly detached program");
        assert_eq!(observed.links.len(), 2 - successful_links);
        assert!(!observed.links[0].pin_present);
        assert!(observed.links[0].kernel.is_none());
        assert!(
            observed
                .links
                .iter()
                .all(|link| !link.pin_present && link.kernel.is_none())
        );

        // The unchanged fault cannot heal itself. No successful step is repeated.
        let error = app.retry_unload(error).expect_err("same fault remains");
        let report = error.report().expect("retained work");
        assert_eq!(report.attempts().len(), attempts + failures);
        let failed_ids: Vec<_> = (successful_links..2).map(|i| 2 * i + 1).collect();
        assert_eq!(
            report.attempts()[attempts..]
                .iter()
                .map(|a| a.id)
                .collect::<Vec<_>>(),
            failed_ids
        );
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
            attempts + failures
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
            failures * 2
        );
        assert!(
            report.attempts()[attempts + failures..]
                .iter()
                .all(|a| a.outcome.is_ok())
        );
        for link in &links {
            assert_eq!(
                report
                    .attempts()
                    .iter()
                    .filter(|a| a.kind == UnloadKind::LinkPin(link.id))
                    .count(),
                1,
                "successful unpin must never repeat"
            );
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
        // Probe only this fixture's ID. Enumerating every kernel program opens
        // transient FDs to other concurrent fixtures and can extend their lifetime.
        // Held-link teardown releases our owned references before returning.
        assert_program_absent(id);
        c.no_artifacts();
    }
}
