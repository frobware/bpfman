//! Cascading deletion preserves ordering, partial progress and consuming retries.
use super::*;
use bpfman_runtime::{DeleteError, DeleteErrorKind, DeleteProgress, ProgramDelete};

pub(super) fn ordering<S: Store>(backend: S) {
    for recursive in [false, true] {
        let f = Fixture::new(backend);
        let owner = f.app.load(f.request(false)).expect("owner").record.id;
        let dep = f
            .app
            .load(f.request(false).with_map_owner(owner))
            .expect("dep")
            .record
            .id;
        let a = f
            .app
            .attach_tracepoint(tracepoint(owner))
            .expect("owner link")
            .id;
        let b = f
            .app
            .attach_tracepoint(tracepoint(dep))
            .expect("dep link")
            .id;
        let ids = if recursive {
            vec![owner, dep, owner]
        } else {
            vec![owner, dep]
        };
        let results = f
            .app
            .delete_programs(ProgramDelete::Ids {
                ids: &ids,
                recursive,
            })
            .expect("delete");
        assert_eq!(
            results.iter().map(|r| r.id).collect::<Vec<_>>(),
            [dep, owner]
        );
        let mut unloaded = Vec::new();
        let mut detached = Vec::new();
        for result in results {
            result.outcome.expect("deleted");
            for progress in result.progress {
                match progress {
                    DeleteProgress::Program { id, report } => {
                        assert_eq!(report.unresolved(), 0);
                        unloaded.push(id);
                    }
                    DeleteProgress::Link { id, .. } => detached.push(id),
                }
            }
        }
        assert_eq!(unloaded, [dep, owner]);
        assert_eq!(detached, [b, a]);
        f.clean();
    }

    // An unnamed dependant is included only when recursive (or --all) is used.
    for recursive in [false, true] {
        let f = Fixture::new(backend);
        let owner = f.app.load(f.request(false)).expect("owner").record.id;
        let dep = f
            .app
            .load(f.request(false).with_map_owner(owner))
            .expect("dep")
            .record
            .id;
        let a = f
            .app
            .attach_tracepoint(tracepoint(owner))
            .expect("owner link")
            .id;
        let b = f
            .app
            .attach_tracepoint(tracepoint(owner))
            .expect("second link")
            .id;
        let c = f
            .app
            .attach_tracepoint(tracepoint(dep))
            .expect("dep link")
            .id;
        let first = f.app.delete_links(&[a, a], recursive).expect("first link");
        for result in first {
            result.outcome.expect("in-batch duplicate succeeds");
        }
        assert_eq!(f.app.get(owner).expect("still linked").links.len(), 1);
        let ids = if recursive { vec![b, c, b] } else { vec![b, b] };
        for result in f
            .app
            .delete_links(&ids, recursive)
            .expect("last owner link")
        {
            result.outcome.expect("cascaded");
        }
        assert!(f.app.get(owner).is_err());
        if recursive {
            assert!(f.app.get(dep).is_err());
        } else {
            let survivor = f.app.get(dep).expect("surviving dependant");
            assert_eq!(survivor.record.map_set, owner);
            assert!(survivor.links[0].pin_present);
            for result in f.app.delete_programs(ProgramDelete::All).expect("all") {
                result.outcome.expect("cleanup");
            }
        }
        f.clean();
    }
}

pub(super) fn recovery<S: Store>(backend: S) {
    let f = Fixture::new(backend);
    let owner = f.app.load(f.request(false)).expect("owner").record.id;
    let dep = f
        .app
        .load(f.request(false).with_map_owner(owner))
        .expect("dep")
        .record
        .id;
    let owner_link = f
        .app
        .attach_tracepoint(tracepoint(owner))
        .expect("owner link")
        .id;
    f.app.attach_tracepoint(tracepoint(dep)).expect("dep link");
    let independent = f
        .app
        .load(f.request(true))
        .expect("independent XDP")
        .record
        .id;
    f.app.attach_xdp(xdp(independent)).expect("XDP");
    f.kernel.fail(Point::RemoveLink, Phase::Before);
    let mut results = f
        .app
        .delete_programs(ProgramDelete::Ids {
            ids: &[owner, independent],
            recursive: true,
        })
        .expect("batch")
        .into_iter();
    let failed = results.next().expect("failed owner");
    assert!(
        failed.progress.is_empty(),
        "dependant blocks owner before owner effects"
    );
    let DeleteError::Link { error, .. } = failed.outcome.expect_err("dependant teardown") else {
        panic!("link receipt");
    };
    results
        .next()
        .expect("independent")
        .outcome
        .expect("continued");
    assert!(f.app.get(owner).expect("owner untouched").links[0].pin_present);
    f.kernel.clear();
    let reopened = Bpfman::new(
        ActiveStore::open(f.store.clone(), &f.layout, Duration::from_secs(1)).expect("reopen"),
        f.kernel.clone(),
        Duration::from_secs(1),
    );
    let bpfman_runtime::DetachError::Standalone(error) = *error else {
        panic!("standalone");
    };
    assert_eq!(
        reopened
            .retry_link_cleanup(error)
            .expect("retained link retry")
            .unresolved(),
        0
    );
    for result in reopened
        .delete_programs(ProgramDelete::All)
        .expect("finish")
    {
        result.outcome.expect("cleanup");
    }
    assert!(reopened.get_link(owner_link).is_err());
    f.clean();

    // Detach can succeed before orphan unload fails. Both that progress and the
    // original unload receipt must survive, including duplicate link operands.
    let f = Fixture::new(backend);
    let program = f.app.load(f.request(false)).expect("program").record.id;
    let link = f
        .app
        .attach_tracepoint(tracepoint(program))
        .expect("link")
        .id;
    f.kernel.fail(Point::RemoveProgram, Phase::Before);
    let mut results = f
        .app
        .delete_links(&[link, link], false)
        .expect("batch")
        .into_iter();
    let failed = results.next().expect("first");
    assert!(matches!(failed.progress.as_slice(), [DeleteProgress::Link {id, ..}] if *id == link));
    let DeleteError::Program { error, .. } = failed.outcome.expect_err("unload fails") else {
        panic!("unload receipt");
    };
    results
        .next()
        .expect("duplicate detached in this batch")
        .outcome
        .expect("already detached");
    assert!(error.report().expect("retained").unresolved() > 0);
    f.kernel.clear();
    let reopened = Bpfman::new(
        ActiveStore::open(f.store.clone(), &f.layout, Duration::from_secs(1)).expect("reopen"),
        f.kernel.clone(),
        Duration::from_secs(1),
    );
    assert_eq!(
        reopened
            .retry_unload(error)
            .expect("consuming unload retry")
            .unresolved(),
        0
    );
    f.clean();

    // A dependant can finish before the owner fails. Its successful progress
    // must remain beside the owner's retained receipt across reopen.
    let f = Fixture::new(backend);
    let owner = f.app.load(f.request(false)).expect("owner").record.id;
    let dep = f
        .app
        .load(f.request(false).with_map_owner(owner))
        .expect("dep")
        .record
        .id;
    let owner_link = f
        .app
        .attach_tracepoint(tracepoint(owner))
        .expect("owner link")
        .id;
    let dep_link = f
        .app
        .attach_tracepoint(tracepoint(dep))
        .expect("dep link")
        .id;
    f.kernel.fail_after(Point::RemoveProgram, Phase::Before, 1);
    let mut results = f
        .app
        .delete_programs(ProgramDelete::Ids {
            ids: &[owner],
            recursive: true,
        })
        .expect("batch");
    let result = results.remove(0);
    assert!(matches!(result.progress.as_slice(), [
        DeleteProgress::Link {id: a, ..},
        DeleteProgress::Program {id: b, ..},
        DeleteProgress::Link {id: c, ..},
    ] if *a == dep_link && *b == dep && *c == owner_link));
    let DeleteError::Program { id, error } = result.outcome.expect_err("owner failure") else {
        panic!("owner receipt");
    };
    assert_eq!(id, owner);
    assert!(f.app.get(dep).is_err());
    assert!(f.app.get(owner).expect("retained owner").links.is_empty());
    f.kernel.clear();
    let reopened = Bpfman::new(
        ActiveStore::open(f.store.clone(), &f.layout, Duration::from_secs(1)).expect("reopen"),
        f.kernel.clone(),
        Duration::from_secs(1),
    );
    assert_eq!(
        reopened
            .retry_unload(error)
            .expect("retry owner only")
            .unresolved(),
        0
    );
    f.clean();

    // Failed overlapping resources are not retried inline: their original
    // receipts must remain the only authority for a later explicit pass.
    let f = Fixture::new(backend);
    let program = f.app.load(f.request(false)).expect("load").record.id;
    let link = f
        .app
        .attach_tracepoint(tracepoint(program))
        .expect("link")
        .id;
    f.kernel.fail(Point::RemoveLink, Phase::Before);
    let mut results = f
        .app
        .delete_links(&[link, link], false)
        .expect("batch")
        .into_iter();
    let DeleteError::Link { error, .. } =
        results.next().expect("first").outcome.expect_err("failed")
    else {
        panic!("link error");
    };
    let bpfman_runtime::DetachError::Standalone(error) = *error else {
        panic!("standalone receipt");
    };
    assert!(
        matches!(results.next().expect("duplicate").outcome, Err(DeleteError::BlockedLink(id)) if id == link)
    );
    f.kernel.clear();
    assert_eq!(
        f.app.retry_link_cleanup(error).expect("retry").unresolved(),
        0
    );
    let _report = f.app.unload(program).expect("cleanup");
    f.clean();
}

pub(super) fn warnings<S: Store>(backend: S) {
    let f = Fixture::new(backend);
    let owner = f.app.load(f.request(false)).expect("owner").record.id;
    f.app
        .load(f.request(false).with_map_owner(owner))
        .expect("dep");
    f.kernel.fail(Point::RemoveMap, Phase::Before);
    let results = f
        .app
        .delete_programs(ProgramDelete::Ids {
            ids: &[owner],
            recursive: true,
        })
        .expect("cascade");
    f.kernel.clear();
    let mut retained = 0;
    for result in results {
        result.outcome.expect("post-record warning is success");
        for step in result.progress {
            if let DeleteProgress::Program { report, .. } = step {
                if report.unresolved() > 0 {
                    retained += 1;
                }
                assert_eq!(
                    f.app
                        .retry_unload_cleanup(report)
                        .expect("retry warnings")
                        .unresolved(),
                    0
                );
            }
        }
    }
    assert_eq!(retained, 1, "last map-set user retains map cleanup");
    f.clean();
}

pub(super) fn cancellation<S: Store>(backend: S) {
    for links in [false, true] {
        for point in [None, Some(Point::RemoveLink), Some(Point::RemoveProgram)] {
            let f = Fixture::new(backend);
            let a = f.app.load(f.request(false)).expect("a").record.id;
            let b = f.app.load(f.request(false)).expect("b").record.id;
            let link = f.app.attach_tracepoint(tracepoint(a)).expect("link").id;
            let second = f
                .app
                .attach_tracepoint(tracepoint(b))
                .expect("second link")
                .id;
            let token = Cancellation::new();
            if let Some(point) = point {
                f.kernel.cancel_at(point, &token);
            } else {
                f.store.cancel_at(StorePoint::ReadLinks, &token);
            }
            let results: Vec<_> = if links {
                f.app
                    .delete_links_with_cancellation(&[link, second], false, &token)
                    .expect("admitted")
                    .into_iter()
                    .map(|r| (r.progress, r.outcome))
                    .collect()
            } else {
                f.app
                    .delete_programs_with_cancellation(
                        ProgramDelete::Ids {
                            ids: &[a, b],
                            recursive: false,
                        },
                        &token,
                    )
                    .expect("admitted")
                    .into_iter()
                    .map(|r| (r.progress, r.outcome))
                    .collect()
            };
            let mut results = results.into_iter();
            let (progress, outcome) = results.next().expect("first");
            if point == Some(Point::RemoveProgram) {
                outcome.expect("admitted unload completes");
                assert!(f.app.get(a).is_err());
            } else {
                assert_eq!(
                    outcome.expect_err("cancel next step").kind(),
                    DeleteErrorKind::Cancelled
                );
                assert!(f.app.get(a).is_ok());
                assert_eq!(f.app.get_link(link).is_ok(), point.is_none());
                assert_eq!(
                    progress.len(),
                    usize::from(point == Some(Point::RemoveLink))
                );
            }
            assert_eq!(
                results
                    .next()
                    .expect("second")
                    .1
                    .expect_err("not started")
                    .kind(),
                DeleteErrorKind::Cancelled
            );
            assert!(f.app.get(b).expect("unchanged second").links[0].pin_present);
            f.kernel.clear();
            for result in f.app.delete_programs(ProgramDelete::All).expect("cleanup") {
                result.outcome.expect("delete");
            }
            f.clean();
        }
    }
}

pub(super) fn observations<S: Store>(backend: S) {
    for point in [StorePoint::ReadRecords, StorePoint::ReadLinks] {
        let f = Fixture::new(backend);
        let id = f.app.load(f.request(false)).expect("load").record.id;
        f.app.attach_tracepoint(tracepoint(id)).expect("attach");
        let before = f.kernel.events();
        f.store.set(Some(point));
        let result = f.app.delete_programs(ProgramDelete::All);
        if point == StorePoint::ReadRecords {
            assert_eq!(
                result.err().expect("selection failure").kind(),
                bpfman_runtime::ErrorKind::Unavailable
            );
        } else {
            assert_eq!(
                result
                    .expect("selected")
                    .remove(0)
                    .outcome
                    .expect_err("link observation")
                    .kind(),
                DeleteErrorKind::Unavailable
            );
        }
        f.store.set(None);
        assert_eq!(
            f.kernel.events(),
            before,
            "no teardown after failed observation"
        );
        let token = Cancellation::new();
        f.store.cancel_at(StorePoint::ReadRecords, &token);
        assert_eq!(
            f.app
                .delete_programs_with_cancellation(ProgramDelete::All, &token)
                .err()
                .expect("cancel selection")
                .kind(),
            bpfman_runtime::ErrorKind::Cancelled
        );
        assert_eq!(f.kernel.events(), before);
        for result in f.app.delete_programs(ProgramDelete::All).expect("cleanup") {
            result.outcome.expect("delete");
        }
        f.clean();
    }
}
