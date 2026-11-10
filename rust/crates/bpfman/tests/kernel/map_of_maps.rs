//! Shared outer maps retain inner references through failure and reopened cleanup.

use super::{
    faults::{Faults, Point},
    support::*,
};
use aya::maps::{Array, ArrayOfMaps, HashOfMaps, IterableMap, Map, MapData, MapInfo};
use bpfman_runtime::{ActiveStore, Bpfman, Cancellation, PreparedProgram};
use bpfman_store::{
    CommitLoad, LinkReader, LinkStore, OpenStore, TcStore, UnloadStore, XdpReplacementStore,
};
use std::{fs, os::fd::AsFd};

fn prepared(object: &str, program: &str) -> PreparedProgram {
    PreparedProgram::new(
        &bpfman_kernel_aya::Kernel::default(),
        &fixture(object),
        bpfman_model::ProgramSpec::Kprobe(program.try_into().expect("symbol")),
        Default::default(),
    )
    .expect("map-of-maps ELF")
}

fn shared() -> PreparedProgram {
    prepared("map_of_maps_shared.bpf.o", "mom_counter")
}

fn descriptors() -> usize {
    fs::read_dir("/proc/self/fd").expect("descriptors").count()
}

fn value(inner: MapData) -> u64 {
    let values: Array<_, u64> = Map::Array(inner).try_into().expect("inner array");
    values.get(&0, 0).expect("inner value")
}

pub(super) fn initializers<S>(backend: S)
where
    S: OpenStore + CommitLoad + UnloadStore + LinkStore + TcStore + XdpReplacementStore + Clone,
    S::Reader: LinkReader,
{
    let c = Context::new();
    let store = Faults::new(backend.clone());
    let app = Bpfman::new(
        ActiveStore::open(store.clone(), &c.layout, TIMEOUT).expect("store"),
        bpfman_kernel_aya::Kernel::default(),
        TIMEOUT,
    );
    let request = || prepared("map_of_maps_initializers.bpf.o", "mom_seed_counter");
    let warm = app.load(request()).expect("warm initialization");
    assert_eq!(
        app.unload(warm.record.id)
            .expect("warm unload")
            .unresolved(),
        0
    );
    c.no_artifacts();
    let baseline = descriptors();

    for (object, selection) in [
        ("map_of_maps_initializers_invalid.bpf.o", "mom_seed_counter"),
        ("map_of_maps_initializers.bpf.o", "mom_seed_bad"),
    ] {
        let failure = app
            .load(prepared(object, selection))
            .expect_err("kernel refusal");
        assert_eq!(failure.unresolved(), 0);
        drop(failure);
        c.no_artifacts();
        assert_eq!(descriptors(), baseline, "partly populated map leaked");
    }

    let cancelled = Cancellation::new();
    cancelled.cancel();
    let failure = app
        .load_with_cancellation(request(), &cancelled)
        .expect_err("cancelled admission");
    assert_eq!(failure.kind(), bpfman_runtime::LoadErrorKind::Cancelled);
    assert_eq!(failure.unresolved(), 0);
    drop(failure);

    let late = Cancellation::new();
    store.cancel_at(Point::Commit, &late);
    store.set(Some(Point::Commit));
    let failure = app
        .load_with_cancellation(request(), &late)
        .expect_err("commit failure");
    assert!(late.is_cancelled());
    assert_eq!(failure.unresolved(), 0);
    drop(failure);
    store.set(None);
    c.no_artifacts();
    assert_eq!(descriptors(), baseline);

    store.set(Some(Point::CommitWithBlockedCleanup));
    let failure = app.load(request()).expect_err("blocked compensation");
    let pending = failure.unresolved();
    assert!(pending > 0);
    let failure = app.retry_load_cleanup_with_cancellation(failure, &cancelled);
    assert_eq!(failure.unresolved(), pending);
    let bytecode = c.layout.bytecode_path(store.blocked_bytecode());
    let directory = bytecode.parent().expect("bytecode directory");
    fs::rename(directory, c.layout.root().join("rejected-bytecode-link"))
        .expect("save blocked link");
    fs::rename(c.layout.root().join("saved-bytecode"), directory).expect("restore bytecode");
    store.set(None);
    let failure = app.retry_load_cleanup(failure);
    assert_eq!(failure.unresolved(), 0);
    drop(failure);
    c.no_artifacts();
    assert_eq!(descriptors(), baseline);

    let owner = app.load(request()).expect("initialized private maps");
    let directory = c.layout.map_directory_path(owner.record.id);
    {
        let a = MapData::from_pin(directory.join("mom_seed_a")).expect("first inner");
        let b = MapData::from_pin(directory.join("mom_seed_b")).expect("second inner");
        let a_id = a.info().expect("identity").id();
        let b_id = b.info().expect("identity").id();
        assert_ne!(a_id, b_id);
        let mut array: ArrayOfMaps<_, MapData> =
            Map::ArrayOfMaps(MapData::from_pin(directory.join("mom_seed_array")).expect("array"))
                .try_into()
                .expect("array of maps");
        let hash: HashOfMaps<_, u32, MapData> =
            Map::HashOfMaps(MapData::from_pin(directory.join("mom_seed_hash")).expect("hash"))
                .try_into()
                .expect("hash of maps");
        for (key, expected) in [(1, a_id), (3, b_id)] {
            assert_eq!(
                array
                    .get(&key, 0)
                    .expect("array seed")
                    .info()
                    .expect("identity")
                    .id(),
                expected
            );
            assert_eq!(
                hash.get(&key, 0)
                    .expect("hash seed")
                    .info()
                    .expect("identity")
                    .id(),
                expected
            );
        }
        assert!(array.get(&0, 0).is_err());
        assert!(array.get(&2, 0).is_err());
        assert!(hash.get(&0, 0).is_err());
        let mut keys = hash.keys().collect::<Result<Vec<_>, _>>().expect("keys");
        keys.sort();
        assert_eq!(keys, [1, 3]);

        // Refuse initializer replay onto live shared outers, retaining an
        // operator's deliberate replacement and every original map identity.
        array.set(1, &b, 0).expect("operator update");
        let failure = app
            .load(request().with_map_owner(owner.record.id))
            .expect_err("initializer replay refusal");
        assert_eq!(failure.kind(), bpfman_runtime::LoadErrorKind::Unsupported);
        assert_eq!(failure.unresolved(), 0);
        drop(failure);
        assert_eq!(
            array
                .get(&1, 0)
                .expect("preserved update")
                .info()
                .expect("identity")
                .id(),
            b_id
        );
        assert_eq!(
            hash.get(&1, 0)
                .expect("preserved hash")
                .info()
                .expect("identity")
                .id(),
            a_id
        );
    }
    drop(app);
    let reopened = Bpfman::new(
        ActiveStore::open(backend, &c.layout, TIMEOUT).expect("reopen"),
        bpfman_kernel_aya::Kernel::default(),
        TIMEOUT,
    );
    assert_eq!(
        reopened
            .unload(owner.record.id)
            .expect("reopened unload")
            .unresolved(),
        0
    );
    c.absent(owner.record.id);
    c.no_artifacts();
    assert_eq!(descriptors(), baseline);
}

pub(super) fn exercise<S>(backend: S)
where
    S: OpenStore + CommitLoad + UnloadStore + LinkStore + TcStore + XdpReplacementStore + Clone,
    S::Reader: LinkReader,
{
    let c = Context::new();
    let store = Faults::new(backend);
    let app = Bpfman::new(
        ActiveStore::open(store.clone(), &c.layout, TIMEOUT).expect("store"),
        bpfman_kernel_aya::Kernel::default(),
        TIMEOUT,
    );
    let array_pin = c.layout.shared_maps_path().join("mom_array");
    let hash_pin = c.layout.shared_maps_path().join("mom_hash");

    // Warm the ordinary store/feature caches before measuring descriptor release.
    let warm = app.load(shared()).expect("warm load");
    assert_eq!(
        app.unload(warm.record.id)
            .expect("warm unload")
            .unresolved(),
        0
    );
    c.no_artifacts();
    let baseline = descriptors();

    for (object, selection) in [
        ("map_of_maps_invalid.bpf.o", "mom_counter"),
        ("map_of_maps_shared.bpf.o", "mom_bad"),
    ] {
        let failure = app
            .load(prepared(object, selection))
            .expect_err("kernel refusal");
        assert_eq!(failure.unresolved(), 0);
        drop(failure);
        c.no_artifacts();
        assert_eq!(
            descriptors(),
            baseline,
            "template or outer descriptor leaked"
        );
    }

    // Inject a wrong existing second pin. Preparation must create the first
    // outer, reject the second, remove only its new pin and preserve the alien.
    let control = app
        .load(prepared("map_of_maps_private.bpf.o", "mom_counter"))
        .expect("private control");
    let inner_pin = c
        .layout
        .map_directory_path(control.record.id)
        .join("mom_values");
    let alien_id = MapInfo::from_pin(&inner_pin)
        .expect("control identity")
        .id();
    fs::rename(&inner_pin, &hash_pin).expect("inject incompatible second pin");
    let failure = app.load(shared()).expect_err("partial shared acquisition");
    assert_eq!(failure.unresolved(), 0);
    assert!(!array_pin.exists(), "first acquisition must be compensated");
    assert_eq!(
        MapInfo::from_pin(&hash_pin).expect("alien preserved").id(),
        alien_id
    );
    assert!(
        format!("{:#}", anyhow::Error::new(failure))
            .contains("incompatible shared map \"mom_hash\""),
        "failure must reach the second shared-map preparation"
    );
    fs::rename(&hash_pin, &inner_pin).expect("restore control pin");
    assert_eq!(
        app.unload(control.record.id)
            .expect("control unload")
            .unresolved(),
        0
    );
    c.no_artifacts();
    assert_eq!(descriptors(), baseline);

    // A failed admitted commit wins over concurrent cancellation and compensates
    // both newly created outers. Cancelling before admission performs no effects.
    let cancelled = Cancellation::new();
    cancelled.cancel();
    let failure = app
        .load_with_cancellation(shared(), &cancelled)
        .expect_err("cancel admission");
    assert_eq!(failure.kind(), bpfman_runtime::LoadErrorKind::Cancelled);
    assert_eq!(failure.unresolved(), 0);
    drop(failure);
    c.no_artifacts();

    let late = Cancellation::new();
    store.cancel_at(Point::Commit, &late);
    store.set(Some(Point::Commit));
    let failure = app
        .load_with_cancellation(shared(), &late)
        .expect_err("failed commit");
    assert!(late.is_cancelled());
    assert_eq!(failure.kind(), bpfman_runtime::LoadErrorKind::Unavailable);
    assert_eq!(failure.unresolved(), 0);
    drop(failure);
    store.set(None);
    c.no_artifacts();
    assert_eq!(descriptors(), baseline);

    // Block one cleanup dependency. Successful map cleanup must not repeat when
    // a fresh caller retries the retained bytecode receipt after cancellation.
    store.set(Some(Point::CommitWithBlockedCleanup));
    let failure = app.load(shared()).expect_err("blocked compensation");
    let unresolved = failure.unresolved();
    assert!(unresolved > 0);
    assert!(!array_pin.exists() && !hash_pin.exists());
    let failure = app.retry_load_cleanup_with_cancellation(failure, &cancelled);
    assert_eq!(failure.unresolved(), unresolved);
    assert_eq!(
        failure.retry_lock_error().expect("cancelled retry").kind(),
        bpfman_runtime::ErrorKind::Cancelled
    );
    let bytecode = c.layout.bytecode_path(store.blocked_bytecode());
    let directory = bytecode.parent().expect("bytecode directory");
    fs::rename(directory, c.layout.root().join("rejected-bytecode-link")).expect("save link");
    fs::rename(c.layout.root().join("saved-bytecode"), directory).expect("restore bytecode");
    store.set(None);
    let failure = app.retry_load_cleanup(failure);
    assert_eq!(failure.unresolved(), 0);
    assert!(
        format!("{:#}", anyhow::Error::new(failure)).contains("injected CommitWithBlockedCleanup")
    );
    c.no_artifacts();
    assert_eq!(descriptors(), baseline);

    let owner = app.load(shared()).expect("named owner");
    let array_id = MapInfo::from_pin(&array_pin).expect("array").id();
    let hash_id = MapInfo::from_pin(&hash_pin).expect("hash").id();
    let inner_pin = c
        .layout
        .map_directory_path(owner.record.id)
        .join("mom_values");
    let inner = MapData::from_pin(&inner_pin).expect("real inner");
    let inner_id = inner.info().expect("inner identity").id();
    assert!(
        bpfman_kernel::ProgramObservations::map(&bpfman_kernel_aya::Kernel::default(), inner_id)
            .expect("inner observation")
            .btf_id
            .is_some(),
        "ordinary inner maps retain BTF"
    );
    let mut values: Array<_, u64> =
        Map::Array(MapData::from_pin(&inner_pin).expect("inner values"))
            .try_into()
            .expect("inner array");
    values.set(0, 12345, 0).expect("set inner contents");
    drop(values);
    let mut array: ArrayOfMaps<_, MapData> =
        Map::ArrayOfMaps(MapData::from_pin(&array_pin).expect("outer array"))
            .try_into()
            .expect("array of maps");
    let mut hash: HashOfMaps<_, u32, MapData> =
        Map::HashOfMaps(MapData::from_pin(&hash_pin).expect("outer hash"))
            .try_into()
            .expect("hash of maps");
    array.set(0, &inner, 0).expect("populate array");
    hash.insert(7, &inner, 0).expect("populate hash");

    // Replacing either entry with the wrong inner value size must fail without
    // changing the old entry. Kernel insertion checks its retained template.
    let wrong_array = Array::<MapData, u32>::create(1, 0).expect("incompatible inner");
    let wrong = MapData::from_fd(
        wrong_array
            .map()
            .fd()
            .as_fd()
            .try_clone_to_owned()
            .expect("duplicate inner descriptor"),
    )
    .expect("wrong inner map");
    assert!(array.set(0, &wrong, 0).is_err());
    assert!(hash.insert(7, &wrong, 0).is_err());
    assert_eq!(
        array
            .get(&0, 0)
            .expect("preserved array entry")
            .info()
            .expect("info")
            .id(),
        inner_id
    );
    assert_eq!(
        hash.get(&7, 0)
            .expect("preserved hash entry")
            .info()
            .expect("info")
            .id(),
        inner_id
    );
    drop(wrong);
    drop(wrong_array);

    store.set(Some(Point::Commit));
    let failure = app
        .load(shared())
        .expect_err("reuse followed by commit failure");
    assert_eq!(failure.unresolved(), 0);
    assert_eq!(
        MapInfo::from_pin(&array_pin).expect("preserved array").id(),
        array_id
    );
    assert_eq!(
        MapInfo::from_pin(&hash_pin).expect("preserved hash").id(),
        hash_id
    );
    assert_eq!(
        value(array.get(&0, 0).expect("array after failed reuse")),
        12345
    );
    assert_eq!(
        value(hash.get(&7, 0).expect("hash after failed reuse")),
        12345
    );
    store.set(None);

    let dependent = app.load(shared()).expect("independent named user");
    assert_eq!(
        app.unload(owner.record.id)
            .expect("owner unload")
            .unresolved(),
        0
    );
    assert!(!inner_pin.exists(), "inner pin has independent lifetime");
    drop(inner);
    assert_eq!(
        array
            .get(&0, 0)
            .expect("outer retains unpinned inner")
            .info()
            .expect("info")
            .id(),
        inner_id
    );
    let reopened = Bpfman::new(
        ActiveStore::open(store, &c.layout, TIMEOUT).expect("reopen"),
        bpfman_kernel_aya::Kernel::default(),
        TIMEOUT,
    );
    assert_eq!(
        reopened
            .unload(dependent.record.id)
            .expect("last user unload")
            .unresolved(),
        0
    );
    c.no_artifacts();
    // External outer descriptors still own their entries after managed unpinning.
    assert_eq!(
        hash.get(&7, 0)
            .expect("external outer retains inner")
            .info()
            .expect("info")
            .id(),
        inner_id
    );
    assert_eq!(
        value(array.get(&0, 0).expect("inner after last unload")),
        12345
    );
    drop(array);
    drop(hash);
}
