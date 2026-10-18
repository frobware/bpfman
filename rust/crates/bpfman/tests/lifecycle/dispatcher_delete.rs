//! Record deletion never grants authority over live kernel resources.
use super::*;
use bpfman_model::{DispatcherKey, LinkState};
use bpfman_store::XdpCommit;

pub(super) fn exercise<S: Store>(backend: S) {
    for boundary in [
        None,
        Some(StorePoint::Validate),
        Some(StorePoint::DispatcherDelete),
    ] {
        let f = Fixture::new(backend);
        let id = f.app.load(f.request(true)).expect("program").record.id;
        let link = f.app.attach_xdp(xdp(id)).expect("link");
        let key = xdp_key(&link);
        let snapshot = f.app.get_xdp_dispatcher(key).expect("snapshot");
        let held = f.kernel.hold_outer();
        let events = f.kernel.events();
        let cancellation = Cancellation::new();
        if let Some(boundary) = boundary {
            f.store.cancel_at(boundary, &cancellation);
        } else {
            cancellation.cancel();
        }
        let result = f
            .app
            .delete_dispatcher_with_cancellation(DispatcherKey::Xdp(key), &cancellation);
        if boundary == Some(StorePoint::DispatcherDelete) {
            assert!(result.expect("admitted commit wins"));
            assert!(cancellation.is_cancelled());
        } else {
            assert_eq!(
                result.expect_err("cancelled").kind(),
                bpfman_runtime::ErrorKind::Cancelled
            );
            assert_eq!(f.app.get_xdp_dispatcher(key).expect("preserved"), snapshot);
            f.store.set(Some(StorePoint::DispatcherDelete));
            assert_eq!(
                f.app
                    .delete_dispatcher(DispatcherKey::Xdp(key))
                    .expect_err("store fault")
                    .kind(),
                bpfman_runtime::ErrorKind::Unavailable
            );
            assert_eq!(f.app.get_xdp_dispatcher(key).expect("preserved"), snapshot);
            f.store.set(None);
            assert!(
                f.app
                    .delete_dispatcher(DispatcherKey::Xdp(key))
                    .expect("delete")
            );
        }
        assert_eq!(
            f.kernel.events(),
            events,
            "no kernel effects or observations"
        );
        assert!(held.attached());
        // The fake tracks dispatcher pins as owned resources rather than native
        // bpffs entries. Restored detach below must consume those same resources;
        // the native contract separately checks every canonical pin path.
        assert!(f.app.list_link_records().expect("links").is_empty());
        assert!(
            f.app
                .list_xdp_dispatchers()
                .expect("dispatchers")
                .is_empty()
        );
        assert!(f.app.get(id).expect("program retained").links.is_empty());
        assert!(
            !f.app
                .delete_dispatcher(DispatcherKey::Xdp(key))
                .expect("missing")
        );

        // Reopen before restoring the original stored intent through domain APIs.
        // Restoration is fixture cleanup, not authority granted by record deletion.
        let app = Bpfman::new(
            ActiveStore::open(backend, &f.layout, Duration::from_secs(1)).expect("reopen"),
            f.kernel.clone(),
            Duration::from_secs(1),
        );
        assert!(app.list_link_records().expect("reopened links").is_empty());
        let old = &snapshot.members()[0];
        let runtime = RuntimeDirectory::open_existing(f.layout.clone())
            .expect("runtime")
            .expect("exists");
        let restored = runtime
            .with_writer(
                bpfman_lock::AcquireOptions {
                    timeout: Duration::from_secs(1),
                    cancelled: None,
                },
                |w| {
                    backend.commit_xdp(
                        &w,
                        XdpCommit {
                            program_id: id,
                            details: &old.details,
                            extension_link_id: match old.member.state {
                                LinkState::Attached { kernel_id } => kernel_id,
                                _ => unreachable!(),
                            },
                            outer_link_id: old.outer_link_id,
                            metadata: &old.member.metadata,
                            created_at: &old.member.created_at,
                        },
                    )
                },
            )
            .expect("writer")
            .expect("restore");
        app.detach_xdp(restored.id).expect("detach");
        assert!(!held.attached());
        drop(held);
        assert_eq!(app.unload(id).expect("unload").unresolved(), 0);
        f.clean();
    }
}
