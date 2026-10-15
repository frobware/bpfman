//! Shared map ownership through the production interpreter and both real stores.
use super::*;
use std::num::NonZeroU32;

fn request<S: Store>(f: &Fixture<S>, shared: bool) -> PreparedProgram {
    let path = f
        .temp
        .path()
        .join(if shared { "shared.o" } else { "source.o" });
    if shared {
        std::fs::write(&path, b"fake shared object").expect("source");
    }
    PreparedProgram::new(
        &f.kernel,
        &path,
        ProgramSpec::Tracepoint("entry".try_into().expect("name")),
        Default::default(),
    )
    .expect("prepare")
}

fn map<S: Store>(f: &Fixture<S>, id: NonZeroU32) -> u32 {
    f.app
        .get(id)
        .expect("program")
        .kernel
        .map_ids
        .expect("maps")[0]
}

pub(super) fn lifecycle<S: Store>(backend: S) {
    for shared in [false, true] {
        let f = Fixture::new(backend);
        let owner = f.app.load(request(&f, shared)).expect("owner").record.id;
        let dependent = f
            .app
            .load(request(&f, false).with_map_owner(owner))
            .expect("dependent")
            .record
            .id;
        assert_eq!(map(&f, owner), map(&f, dependent));
        let original = map(&f, owner);
        let independent = f
            .app
            .load(request(&f, shared))
            .expect("independent")
            .record
            .id;
        assert_eq!(map(&f, independent) == original, shared);
        f.app
            .attach_tracepoint(tracepoint(owner))
            .expect("owner link");
        f.app
            .attach_tracepoint(tracepoint(dependent))
            .expect("dependent link");
        assert_eq!(
            f.app
                .unload(owner)
                .expect("owner unload first")
                .unresolved(),
            0
        );
        assert_eq!(map(&f, dependent), original);
        let late = f
            .app
            .load(request(&f, false).with_map_owner(owner))
            .expect("join surviving set")
            .record
            .id;
        assert_eq!(map(&f, late), original);
        let before = f.kernel.events();
        f.app
            .load(request(&f, false).with_map_owner(dependent))
            .expect_err("dependent is not a map set");
        assert_eq!(
            f.kernel.events(),
            before,
            "bad owners fail before kernel effects"
        );
        let _report = f.app.unload(dependent).expect("dependent unload");
        assert_eq!(map(&f, late), original);
        let _report = f.app.unload(late).expect("last map-set user");
        assert!(f.app.get(independent).is_ok());
        let _report = f.app.unload(independent).expect("last named user");
        f.clean();
    }
}

pub(super) fn recovery<S: Store>(backend: S) {
    let pair = |f: &Fixture<S>| {
        let path = f.temp.path().join("pair.o");
        std::fs::write(&path, b"fake shared pair").expect("source");
        PreparedProgram::new(
            &f.kernel,
            &path,
            ProgramSpec::Tracepoint("entry".try_into().expect("name")),
            Default::default(),
        )
        .expect("prepare pair")
    };
    for phase in [Phase::Before, Phase::After] {
        let f = Fixture::new(backend);
        f.kernel.fail_after(Point::PinSharedMap, phase, 1);
        let error = f
            .app
            .load(pair(&f))
            .expect_err("second name-pin acquisition fails");
        assert_eq!(error.unresolved(), 0);
        f.kernel.clear();
        f.clean();
    }
    let f = Fixture::new(backend);
    let token = Cancellation::new();
    f.kernel.cancel_at(Point::PinSharedMap, &token);
    let error = f
        .app
        .load_with_cancellation(pair(&f), &token)
        .expect_err("cancel between name pins");
    assert_eq!(error.kind(), bpfman_runtime::LoadErrorKind::Cancelled);
    assert_eq!(error.unresolved(), 0);
    assert_eq!(
        f.kernel
            .events()
            .iter()
            .filter(|p| **p == Point::PinSharedMap)
            .count(),
        1
    );
    assert!(!f.kernel.events().contains(&Point::Load));
    f.kernel.clear();
    f.clean();

    let f = Fixture::new(backend);
    let batch = request(&f, true)
        .with_additional_programs(
            &f.kernel,
            vec![ProgramSpec::Tracepoint("second".try_into().expect("name"))],
        )
        .expect("batch");
    f.store.set(Some(StorePoint::Commit));
    let error = f
        .app
        .load_batch(batch)
        .expect_err("shared-name batch commit failure");
    assert_eq!(error.unresolved(), 0);
    f.store.set(None);
    f.clean();

    for (point, phase) in [
        (Point::PrepareMaps, Phase::Before),
        (Point::PinSharedMap, Phase::Before),
        (Point::PinSharedMap, Phase::After),
        (Point::Load, Phase::Before),
        (Point::PinProgram, Phase::Before),
        (Point::PinProgram, Phase::After),
        (Point::Directory, Phase::Before),
        (Point::Directory, Phase::After),
        (Point::PinMap, Phase::Before),
        (Point::PinMap, Phase::After),
    ] {
        let f = Fixture::new(backend);
        f.kernel.fail(point, phase);
        let error = f
            .app
            .load(request(&f, true))
            .expect_err("acquisition fails");
        assert_eq!(error.unresolved(), 0, "{point:?}/{phase:?}: {error}");
        f.kernel.clear();
        f.clean();
    }
    for point in [
        Point::PinSharedMap,
        Point::Load,
        Point::PinProgram,
        Point::Directory,
        Point::PinMap,
    ] {
        let f = Fixture::new(backend);
        let token = Cancellation::new();
        f.kernel.cancel_at(point, &token);
        let error = f
            .app
            .load_with_cancellation(request(&f, true), &token)
            .expect_err("cancelled");
        assert_eq!(error.kind(), bpfman_runtime::LoadErrorKind::Cancelled);
        assert_eq!(error.unresolved(), 0);
        f.kernel.clear();
        f.clean();
    }
    for shared in [false, true] {
        let f = Fixture::new(backend);
        let owner = f.app.load(request(&f, shared)).expect("owner").record.id;
        let original = map(&f, owner);
        f.store.set(Some(StorePoint::Commit));
        let error = f
            .app
            .load(request(&f, false).with_map_owner(owner))
            .expect_err("commit failure");
        assert_eq!(error.unresolved(), 0);
        f.store.set(None);
        assert_eq!(map(&f, owner), original);
        let _report = f.app.unload(owner).expect("owner intact");
        f.clean();
    }

    // The first attempt owns both the new name pin and its per-program alias.
    // A later committed user adopts the unchanged name pin before explicit retry.
    let f = Fixture::new(backend);
    f.store.set(Some(StorePoint::Commit));
    f.kernel.fail(Point::RemoveMap, Phase::Before);
    let error = f
        .app
        .load(request(&f, true))
        .expect_err("commit and map cleanup failures");
    assert!(error.unresolved() >= 3);
    f.store.set(None);
    f.kernel.clear();
    let user = f
        .app
        .load(request(&f, true))
        .expect("new committed user")
        .record
        .id;
    let app = Bpfman::new(
        ActiveStore::open(f.store.clone(), &f.layout, Duration::from_secs(1)).expect("reopen"),
        f.kernel.clone(),
        Duration::from_secs(1),
    );
    let error = app.retry_load_cleanup(error);
    assert_eq!(
        error.unresolved(),
        0,
        "new user inherits name-pin authority"
    );
    assert!(app.get(user).is_ok());
    let _report = app.unload(user).expect("last user removes inherited pin");
    f.clean();

    // Last-user unload can pause with pins retained; a new explicit owner join
    // inherits their unchanged identities when cleanup resumes.
    let f = Fixture::new(backend);
    let owner = f.app.load(request(&f, true)).expect("owner").record.id;
    f.kernel.fail(Point::RemoveMap, Phase::Before);
    let report = f.app.unload(owner).expect("post-record warning");
    assert!(report.unresolved() >= 3);
    f.kernel.clear();
    let user = f
        .app
        .load(request(&f, false).with_map_owner(owner))
        .expect("join retained set")
        .record
        .id;
    let report = f
        .app
        .retry_unload_cleanup(report)
        .expect("transfer cleanup authority");
    assert_eq!(report.unresolved(), 0);
    assert!(f.app.get(user).is_ok());
    let _report = f.app.unload(user).expect("last dependent");
    f.clean();
}
