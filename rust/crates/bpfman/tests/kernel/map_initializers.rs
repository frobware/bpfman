//! Initializer admission, observation-only reuse, and owned-map compensation.

use super::{
    faults::{Faults, Point},
    map_of_maps::{descriptors, prepared},
    support::*,
};
use aya::maps::{ArrayOfMaps, HashOfMaps, Map, MapData};
use bpfman_runtime::{ActiveStore, Bpfman, Cancellation};
use bpfman_store::{
    CommitLoad, LinkReader, LinkStore, OpenStore, TcStore, UnloadStore, XdpReplacementStore,
};
use std::fs;

// Pinning and mutation here deliberately model external operators. This Go-only
// reference contract records where Rust's no-overwrite policy differs.
pub(super) fn go_initializers() {
    let c = Context::new();
    let go = std::env::var_os("BPFMAN_GO_BIN")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| repository().join("bin/bpfman"));
    let load = |object: &str, selection: &str, owner: Option<std::num::NonZeroU32>, success| {
        let path = fixture(object);
        let mut args = vec![
            "program",
            "load",
            "file",
            path.to_str().expect("fixture path"),
            "--programs",
            selection,
            "-o",
            "json",
        ];
        let owner = owner.map(|id| id.to_string());
        if let Some(owner) = owner.as_deref() {
            args.extend(["--map-owner-id", owner]);
        }
        c.run(&go, &args, success)
    };
    let unload = |program: std::num::NonZeroU32| {
        c.run(&go, &["program", "unload", &program.to_string()], true);
        c.absent(program);
    };

    // Standalone NewMap cannot resolve ELF string references for a new named
    // outer, so Go refuses before it can publish either shared pin.
    let failure = load(
        "map_of_maps_initializers_shared.bpf.o",
        "kprobe:mom_seed_counter",
        None,
        false,
    );
    assert!(String::from_utf8_lossy(&failure.stderr).contains("create shared map"));
    c.no_artifacts();

    let output = load(
        "map_of_maps_initializers.bpf.o",
        "kprobe:mom_seed_counter",
        None,
        true,
    );
    let owner = id(&serde_json::from_slice::<serde_json::Value>(&output.stdout)
        .expect("loaded JSON")["programs"][0]);
    let directory = c.layout.map_directory_path(owner);
    let b = MapData::from_pin(directory.join("mom_seed_b")).expect("second inner");
    let a_id = MapData::from_pin(directory.join("mom_seed_a"))
        .expect("first inner")
        .info()
        .expect("identity")
        .id();
    let b_id = b.info().expect("identity").id();
    let mut array: ArrayOfMaps<_, MapData> =
        Map::ArrayOfMaps(MapData::from_pin(directory.join("mom_seed_array")).expect("array"))
            .try_into()
            .expect("outer");
    let mut hash: HashOfMaps<_, u32, MapData> =
        Map::HashOfMaps(MapData::from_pin(directory.join("mom_seed_hash")).expect("hash"))
            .try_into()
            .expect("outer");
    array.set(1, &b, 0).expect("operator update");
    hash.insert(1, &b, 0).expect("operator update");
    load(
        "map_of_maps_initializers.bpf.o",
        "kprobe:mom_seed_bad",
        Some(owner),
        false,
    );
    assert_eq!(
        array
            .get(&1, 0)
            .expect("entry")
            .info()
            .expect("identity")
            .id(),
        b_id
    );
    assert_eq!(
        hash.get(&1, 0)
            .expect("entry")
            .info()
            .expect("identity")
            .id(),
        b_id
    );

    // Successful collection loading replays Contents even on replacements.
    let output = load(
        "map_of_maps_initializers.bpf.o",
        "kprobe:mom_seed_counter",
        Some(owner),
        true,
    );
    let dependent = id(&serde_json::from_slice::<serde_json::Value>(&output.stdout)
        .expect("loaded JSON")["programs"][0]);
    assert_eq!(
        array
            .get(&1, 0)
            .expect("entry")
            .info()
            .expect("identity")
            .id(),
        a_id
    );
    assert_eq!(
        hash.get(&1, 0)
            .expect("entry")
            .info()
            .expect("identity")
            .id(),
        a_id
    );
    unload(dependent);

    // Existing named pins bypass standalone creation. Fresh private inners then
    // replace the previous live entries in both shared outer maps.
    fs::create_dir_all(c.layout.shared_maps_path()).expect("shared directory");
    MapData::from_pin(directory.join("mom_seed_array"))
        .expect("array")
        .pin(c.layout.shared_maps_path().join("mom_seed_array"))
        .expect("external array pin");
    MapData::from_pin(directory.join("mom_seed_hash"))
        .expect("hash")
        .pin(c.layout.shared_maps_path().join("mom_seed_hash"))
        .expect("external hash pin");
    let output = load(
        "map_of_maps_initializers_shared.bpf.o",
        "kprobe:mom_seed_counter",
        None,
        true,
    );
    let named = id(&serde_json::from_slice::<serde_json::Value>(&output.stdout)
        .expect("loaded JSON")["programs"][0]);
    let new_a = MapData::from_pin(c.layout.map_directory_path(named).join("mom_seed_a"))
        .expect("new private inner")
        .info()
        .expect("identity")
        .id();
    assert_ne!(new_a, a_id);
    assert_eq!(
        array
            .get(&1, 0)
            .expect("entry")
            .info()
            .expect("identity")
            .id(),
        new_a
    );
    assert_eq!(
        hash.get(&1, 0)
            .expect("entry")
            .info()
            .expect("identity")
            .id(),
        new_a
    );
    drop((array, hash, b));
    unload(named);
    unload(owner);
    c.no_artifacts();
}

pub(super) fn initializers<S>(backend: S)
where
    S: OpenStore + CommitLoad + UnloadStore + LinkStore + TcStore + XdpReplacementStore + Clone,
    S::Reader: LinkReader,
{
    initializer_recovery(
        backend,
        "map_of_maps_initializers.bpf.o",
        "map_of_maps_initializers_invalid.bpf.o",
    );
}

pub(super) fn shared_initializers<S>(backend: S)
where
    S: OpenStore + CommitLoad + UnloadStore + LinkStore + TcStore + XdpReplacementStore + Clone,
    S::Reader: LinkReader,
{
    initializer_recovery(
        backend.clone(),
        "map_of_maps_initializers_named.bpf.o",
        "map_of_maps_initializers_named_invalid.bpf.o",
    );
    named_initializer_reuse(backend);
    initializer_source_authority();
}

fn initializer_recovery<S>(backend: S, object: &str, invalid: &str)
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
    let request = || prepared(object, "mom_seed_counter");
    let warm = app.load(request()).expect("warm initialization");
    assert_eq!(
        app.unload(warm.record.id)
            .expect("warm unload")
            .unresolved(),
        0
    );
    c.no_artifacts();
    let baseline = descriptors();

    for (object, selection) in [(invalid, "mom_seed_counter"), (object, "mom_seed_bad")] {
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

    let owner = app.load(request()).expect("initialized maps");
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

        // Matching replay is observation-only and shares the entire map set.
        let dependent = app
            .load(request().with_map_owner(owner.record.id))
            .expect("idempotent replay");
        assert_eq!(
            app.unload(dependent.record.id)
                .expect("dependent unload")
                .unresolved(),
            0
        );

        // A conflicting replay preserves the operator's replacement and all
        // original identities. No cleanup receipt belongs to a reused pin.
        array.set(1, &b, 0).expect("operator update");
        let failure = app
            .load(request().with_map_owner(owner.record.id))
            .expect_err("initializer replay refusal");
        assert_eq!(failure.kind(), bpfman_runtime::LoadErrorKind::InvalidInput);
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

fn named_initializer_reuse<S>(backend: S)
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
    let request = || prepared("map_of_maps_initializers_named.bpf.o", "mom_seed_counter");
    let first = app.load(request()).expect("fresh named initializer graph");
    let a =
        MapData::from_pin(c.layout.shared_maps_path().join("mom_seed_a")).expect("shared inner A");
    let b =
        MapData::from_pin(c.layout.shared_maps_path().join("mom_seed_b")).expect("shared inner B");
    let a_id = a.info().expect("identity").id();
    let b_id = b.info().expect("identity").id();
    let mut array: ArrayOfMaps<_, MapData> = Map::ArrayOfMaps(
        MapData::from_pin(c.layout.shared_maps_path().join("mom_seed_array"))
            .expect("shared array"),
    )
    .try_into()
    .expect("outer");
    let mut hash: HashOfMaps<_, u32, MapData> = Map::HashOfMaps(
        MapData::from_pin(c.layout.shared_maps_path().join("mom_seed_hash")).expect("shared hash"),
    )
    .try_into()
    .expect("outer");
    let array_id = MapData::from_pin(c.layout.shared_maps_path().join("mom_seed_array"))
        .expect("array identity")
        .info()
        .expect("identity")
        .id();
    let hash_id = MapData::from_pin(c.layout.shared_maps_path().join("mom_seed_hash"))
        .expect("hash identity")
        .info()
        .expect("identity")
        .id();
    let assert_entries =
        |array: &ArrayOfMaps<_, MapData>, hash: &HashOfMaps<_, u32, MapData>, expected| {
            assert_eq!(
                array
                    .get(&1, 0)
                    .expect("array entry")
                    .info()
                    .expect("identity")
                    .id(),
                expected
            );
            assert_eq!(
                hash.get(&1, 0)
                    .expect("hash entry")
                    .info()
                    .expect("identity")
                    .id(),
                a_id
            );
        };
    // Non-initializer slots are operator-owned and survive idempotent reuse.
    array.set(0, &b, 0).expect("extra operator entry");
    let second = app.load(request()).expect("named idempotent reuse");
    assert_entries(&array, &hash, a_id);
    assert_eq!(
        array
            .get(&0, 0)
            .expect("extra entry")
            .info()
            .expect("identity")
            .id(),
        b_id
    );
    let second_directory = c.layout.map_directory_path(second.record.id);
    for (name, expected) in [
        ("mom_seed_a", a_id),
        ("mom_seed_b", b_id),
        ("mom_seed_array", array_id),
        ("mom_seed_hash", hash_id),
    ] {
        assert_eq!(
            MapData::from_pin(second_directory.join(name))
                .expect("alias")
                .info()
                .expect("identity")
                .id(),
            expected
        );
    }

    // Warm private-map unload while shared users remain before measuring
    // descriptors, including resources retained by the real store adapter.
    let warm = app
        .load(prepared(
            "map_of_maps_initializers.bpf.o",
            "mom_seed_counter",
        ))
        .expect("warm private control");
    assert_eq!(
        app.unload(warm.record.id)
            .expect("warm private unload")
            .unresolved(),
        0
    );

    let baseline = descriptors();
    let before_fds = descriptor_targets();
    for selection in ["mom_seed_bad", "mom_seed_counter"] {
        store.set((selection == "mom_seed_counter").then_some(Point::Commit));
        let failure = app
            .load(prepared("map_of_maps_initializers_named.bpf.o", selection))
            .expect_err("failed reuse");
        assert_eq!(failure.unresolved(), 0);
        drop(failure);
        assert_entries(&array, &hash, a_id);
        assert_eq!(descriptors(), baseline);
    }
    store.set(None);

    array.set(1, &b, 0).expect("conflicting operator entry");
    let failure = app.load(request()).expect_err("conflicting array reuse");
    assert_eq!(failure.kind(), bpfman_runtime::LoadErrorKind::InvalidInput);
    assert_eq!(failure.unresolved(), 0);
    drop(failure);
    assert_entries(&array, &hash, b_id);
    array.set(1, &a, 0).expect("restore array");
    hash.remove(&1).expect("missing initialized hash entry");
    let failure = app.load(request()).expect_err("missing hash seed");
    assert_eq!(failure.kind(), bpfman_runtime::LoadErrorKind::InvalidInput);
    assert_eq!(failure.unresolved(), 0);
    drop(failure);
    assert!(hash.get(&1, 0).is_err());
    hash.insert(1, &a, 0).expect("restore hash");

    // Existing durable users forbid recreating a removed canonical name. The
    // failed load must leave it missing, with the original aliased maps intact.
    let array_pin = c.layout.shared_maps_path().join("mom_seed_array");
    let saved_array = c.layout.shared_maps_path().join("saved_array");
    fs::rename(&array_pin, &saved_array).expect("external unpin from canonical name");
    hash.insert(1, &b, 0).expect("conflicting borrowed hash");
    let failure = app.load(request()).expect_err("missing durable shared pin");
    assert_eq!(failure.kind(), bpfman_runtime::LoadErrorKind::InvalidInput);
    assert_eq!(failure.unresolved(), 0);
    drop(failure);
    assert!(!array_pin.exists());
    assert_eq!(
        array
            .get(&1, 0)
            .expect("original array")
            .info()
            .expect("identity")
            .id(),
        a_id
    );
    assert_eq!(
        hash.get(&1, 0)
            .expect("preserved hash")
            .info()
            .expect("identity")
            .id(),
        b_id
    );
    fs::rename(&saved_array, &array_pin).expect("restore canonical identity");
    hash.insert(1, &a, 0).expect("restore hash initializer");

    // An outer of the correct shape but a different kernel identity also fails
    // before acquisition. Do not confuse structural compatibility with ownership.
    let independent = app
        .load(prepared(
            "map_of_maps_initializers.bpf.o",
            "mom_seed_counter",
        ))
        .expect("independent private maps");
    let other_array = c
        .layout
        .map_directory_path(independent.record.id)
        .join("mom_seed_array");
    fs::rename(&array_pin, &saved_array).expect("save original pin");
    fs::rename(&other_array, &array_pin).expect("replace with compatible foreign map");
    let failure = app
        .load(request())
        .expect_err("changed durable shared identity");
    assert_eq!(failure.kind(), bpfman_runtime::LoadErrorKind::InvalidInput);
    assert_eq!(failure.unresolved(), 0);
    drop(failure);
    assert_ne!(
        MapData::from_pin(&array_pin)
            .expect("foreign pin preserved")
            .info()
            .expect("identity")
            .id(),
        array_id
    );
    fs::rename(&array_pin, &other_array).expect("restore private alias");
    fs::rename(&saved_array, &array_pin).expect("restore original shared pin");
    assert_eq!(
        app.unload(independent.record.id)
            .expect("independent unload")
            .unresolved(),
        0
    );
    assert_eq!(
        descriptors(),
        baseline,
        "before: {before_fds:?}; after: {:?}",
        descriptor_targets()
    );

    assert_eq!(
        app.unload(first.record.id)
            .expect("owner-first unload")
            .unresolved(),
        0
    );
    assert_entries(&array, &hash, a_id);
    drop((array, hash, a, b));
    assert_eq!(
        app.unload(second.record.id)
            .expect("last user unload")
            .unresolved(),
        0
    );
    c.no_artifacts();

    // An operator supplies only three named pins. A conflict in the borrowed
    // hash follows acquisition of a fresh array; cleanup owns only that array.
    let control = app
        .load(prepared(
            "map_of_maps_initializers.bpf.o",
            "mom_seed_counter",
        ))
        .expect("private control");
    let control_directory = c.layout.map_directory_path(control.record.id);
    for name in ["mom_seed_a", "mom_seed_b", "mom_seed_hash"] {
        MapData::from_pin(control_directory.join(name))
            .expect("control map")
            .pin(c.layout.shared_maps_path().join(name))
            .expect("external shared pin");
    }
    let mut control_hash: HashOfMaps<_, u32, MapData> = Map::HashOfMaps(
        MapData::from_pin(control_directory.join("mom_seed_hash")).expect("control hash"),
    )
    .try_into()
    .expect("outer");
    let control_b = MapData::from_pin(control_directory.join("mom_seed_b")).expect("control B");
    let control_b_id = control_b.info().expect("identity").id();
    control_hash
        .insert(1, &control_b, 0)
        .expect("conflicting hash entry");
    let baseline = descriptors();
    let failure = app
        .load(request())
        .expect_err("mixed fresh/reused conflict");
    assert_eq!(failure.kind(), bpfman_runtime::LoadErrorKind::InvalidInput);
    assert_eq!(failure.unresolved(), 0);
    drop(failure);
    assert!(!c.layout.shared_maps_path().join("mom_seed_array").exists());
    assert_eq!(
        control_hash
            .get(&1, 0)
            .expect("preserved hash")
            .info()
            .expect("identity")
            .id(),
        control_b_id
    );
    for name in ["mom_seed_a", "mom_seed_b", "mom_seed_hash"] {
        let shared = MapData::from_pin(c.layout.shared_maps_path().join(name))
            .expect("preserved external pin");
        let original = MapData::from_pin(control_directory.join(name)).expect("original map");
        assert_eq!(
            shared.info().expect("identity").id(),
            original.info().expect("identity").id()
        );
        // Remove only the external pin this fixture created above. Managed
        // compensation is exercised separately through its consuming receipts.
        #[allow(clippy::disallowed_methods)]
        {
            fs::remove_file(c.layout.shared_maps_path().join(name))
                .expect("operator removes external pin");
        }
    }
    assert_eq!(descriptors(), baseline);
    drop((control_hash, control_b));
    assert_eq!(
        app.unload(control.record.id)
            .expect("control unload")
            .unresolved(),
        0
    );
    c.no_artifacts();

    // Shared outers referring to private inners initialize once. A later named
    // load would substitute different inner identities and is refused intact.
    let private_inners = || prepared("map_of_maps_initializers_shared.bpf.o", "mom_seed_counter");
    let owner = app
        .load(private_inners())
        .expect("fresh named outers with private inners");
    let failure = app
        .load(private_inners())
        .expect_err("different private inner identity");
    assert_eq!(failure.kind(), bpfman_runtime::LoadErrorKind::InvalidInput);
    assert_eq!(failure.unresolved(), 0);
    drop(failure);
    let dependent = app
        .load(private_inners().with_map_owner(owner.record.id))
        .expect("explicit owner retains inner identity");
    assert_eq!(
        app.unload(owner.record.id)
            .expect("owner unload")
            .unresolved(),
        0
    );
    assert_eq!(
        app.unload(dependent.record.id)
            .expect("dependent unload")
            .unresolved(),
        0
    );
    c.no_artifacts();
}

fn initializer_source_authority() {
    use bpfman_kernel::{ObjectLoader, ProgramLoad, ProgramResources};
    let c = Context::new();
    let kernel = bpfman_kernel_aya::Kernel::default();
    let bytes = fs::read(fixture("map_of_maps_initializers_named.bpf.o")).expect("ELF");
    let spec = bpfman_model::ProgramSpec::Kprobe("mom_seed_counter".try_into().expect("symbol"));
    let info = kernel.validate_object(&bytes, &spec).expect("admission");
    let runtime = bpfman_fs::RuntimeDirectory::open_or_create(c.layout.clone()).expect("runtime");
    runtime
        .with_writer(
            bpfman_lock::AcquireOptions {
                timeout: TIMEOUT,
                cancelled: None,
            },
            |writer| {
                let w = &writer;
                let prepared = kernel.prepare_load(w).expect("prepare");
                let pins = kernel
                    .prepare_maps(w, &prepared, &bytes, &info.shared_maps, None)
                    .map_err(|failure| failure.cause)
                    .expect("new receipts");
                assert_eq!(pins.len(), 4);
                let load = |receipts: &[bpfman_fs::MapPin]| {
                    kernel.load_program_with_maps(
                        w,
                        &bytes,
                        &info.maps,
                        &Default::default(),
                        &spec,
                        None,
                        &info.shared_maps,
                        receipts,
                    )
                };
                let failure = load(&[])
                    .err()
                    .expect("empty borrowed outers cannot initialize");
                assert_eq!(failure.kind(), bpfman_kernel::ErrorKind::InvalidInput);
                drop(failure);

                let pin = c.layout.shared_maps_path().join("mom_seed_array");
                let saved = c.layout.shared_maps_path().join("saved_array");
                fs::rename(&pin, &saved).expect("operator replaces owned pin");
                let original = MapData::from_pin(&saved).expect("original map");
                original
                    .pin(&pin)
                    .expect("same map under a different pin inode");
                let failure = load(&pins)
                    .err()
                    .expect("replacement inode is not new ownership");
                assert_eq!(failure.kind(), bpfman_kernel::ErrorKind::InvalidData);
                drop(failure);
                let original_array: ArrayOfMaps<_, MapData> =
                    Map::ArrayOfMaps(original).try_into().expect("outer");
                assert!(original_array.get(&1, 0).is_err());
                drop(original_array);
                // Repair only the test's external replacement, preserving the
                // original owned inode for production receipt cleanup below.
                #[allow(clippy::disallowed_methods)]
                {
                    fs::remove_file(&pin).expect("operator removes replacement");
                }
                fs::rename(&saved, &pin).expect("restore owned inode");

                let loaded = load(&pins).expect("matching receipts authorize initialization");
                drop(loaded);
                for pin in pins {
                    kernel
                        .remove_map(w, pin)
                        .map_err(|failure| failure.cause)
                        .expect("remove owned pin");
                }
            },
        )
        .expect("writer");
    c.no_artifacts();
}

fn descriptor_targets() -> Vec<(String, std::path::PathBuf)> {
    fs::read_dir("/proc/self/fd")
        .expect("descriptors")
        .filter_map(|entry| {
            let entry = entry.expect("descriptor entry");
            fs::read_link(entry.path())
                .ok()
                .map(|target| (entry.file_name().to_string_lossy().into_owned(), target))
        })
        .collect()
}
