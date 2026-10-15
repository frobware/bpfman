//! Map-set membership and PinByName lifetime through both concrete stores.
#![allow(clippy::expect_used)]
use bpfman_fs::{RuntimeDirectory, RuntimeLayout, RuntimeWriter};
use bpfman_lock::AcquireOptions;
use bpfman_model::ProgramSpec;
use bpfman_store::{CommitLoad, LoadRecord, OpenStore, ProgramReader, UnloadStore};
use std::{collections::BTreeMap, num::NonZeroU32, time::Duration};

fn id(value: u32) -> NonZeroU32 {
    NonZeroU32::new(value).expect("id")
}
fn writer<T>(runtime: &RuntimeDirectory, action: impl FnOnce(&RuntimeWriter<'_>) -> T) -> T {
    runtime
        .with_writer(
            AcquireOptions {
                timeout: Duration::from_secs(1),
                cancelled: None,
            },
            |w| action(&w),
        )
        .expect("writer")
}

fn exercise<S: OpenStore + CommitLoad + UnloadStore + Copy>(backend: S) {
    let temp = tempfile::tempdir().expect("fixture");
    let layout = RuntimeLayout::try_from(temp.path().join("runtime")).expect("layout");
    let runtime = RuntimeDirectory::open_or_create(layout.clone()).expect("runtime");
    let foreign = RuntimeDirectory::open_or_create(
        RuntimeLayout::try_from(temp.path().join("foreign")).expect("layout"),
    )
    .expect("runtime");
    let spec = ProgramSpec::Kprobe("entry".try_into().expect("symbol"));
    let metadata = BTreeMap::new();
    let globals = BTreeMap::new();
    let names = vec!["counter".to_owned()];
    let record = |raw, owner: Option<u32>| LoadRecord {
        id: id(raw),
        spec: &spec,
        map_owner: owner.map(id),
        shared_maps: &names,
        source: "source.o",
        license: "GPL",
        created_at: "2026-10-15T12:00:00Z",
        metadata: &metadata,
        globals: &globals,
    };

    let (owner_maps, independent_maps) = writer(&runtime, |w| {
        let mut reader = backend.open(w).expect("open");
        reader.validate_map_sharing().expect("sharing supported");
        backend.commit_program(w, record(42, None)).expect("owner");
        let (owner, maps) = backend
            .observe_unload(w, id(42))
            .expect("observe")
            .expect("owner");
        backend
            .commit_program(w, record(43, Some(42)))
            .expect("dependent");
        backend
            .commit_program(w, record(44, None))
            .expect("independent named user");
        assert_eq!(reader.read_map_sets().expect("sets"), [id(42), id(44)]);
        assert_eq!(
            reader.read_shared_maps().expect("users")["counter"],
            [id(42), id(43), id(44)]
        );
        let before = reader.read_records().expect("snapshot");
        for owner in [43, 999, 45] {
            assert!(backend.commit_program(w, record(45, Some(owner))).is_err());
            assert_eq!(reader.read_records().expect("unchanged"), before);
        }
        assert!(
            backend
                .commit_programs(w, &[record(45, Some(42)), record(46, Some(999))])
                .is_err()
        );
        assert_eq!(reader.read_records().expect("atomic refusal"), before);
        let mut invalid = record(45, Some(42));
        let bad_names = vec!["../escape".to_owned()];
        invalid.shared_maps = &bad_names;
        assert!(backend.commit_program(w, invalid).is_err());
        assert_eq!(reader.read_records().expect("invalid name refused"), before);

        // Adding users changes the reference count, not this program's identity.
        backend
            .delete_program(w, owner)
            .map_err(|e| e.cause)
            .expect("owner can unload first");
        let maps = backend
            .delete_map_set(w, maps)
            .expect_err("dependent protects set")
            .remaining;
        assert!(
            reader
                .read_records()
                .expect("owner gone")
                .iter()
                .all(|p| p.id != id(42))
        );
        assert_eq!(
            reader.read_map_sets().expect("independent lifetime"),
            [id(42), id(44)]
        );
        let (independent, independent_maps) = backend
            .observe_unload(w, id(44))
            .expect("observe")
            .expect("independent");
        backend
            .delete_program(w, independent)
            .map_err(|e| e.cause)
            .expect("remove independent user");
        (maps, independent_maps)
    });

    let owner_maps = writer(&foreign, |w| {
        backend
            .delete_map_set(w, owner_maps)
            .expect_err("foreign authority refused")
            .remaining
    });
    let reopened = backend
        .open_reader(&runtime)
        .expect("reopen")
        .expect("store");
    writer(&runtime, |w| {
        let mut reader = reopened.clone();
        backend
            .commit_program(w, record(45, Some(42)))
            .expect("join surviving set after original program is gone");
        assert_eq!(
            reader.read_shared_maps().expect("users")["counter"],
            [id(43), id(45)]
        );
        let (one, _) = backend
            .observe_unload(w, id(43))
            .expect("observe")
            .expect("member");
        let (two, _) = backend
            .observe_unload(w, id(45))
            .expect("observe")
            .expect("member");
        backend
            .delete_program(w, one)
            .map_err(|e| e.cause)
            .expect("first member");
        backend
            .delete_program(w, two)
            .map_err(|e| e.cause)
            .expect("last member");
        assert!(reader.read_shared_maps().expect("no users").is_empty());
        backend
            .delete_map_set(w, owner_maps)
            .map_err(|e| e.cause)
            .expect("last-user set deletion");
        backend
            .delete_map_set(w, independent_maps)
            .map_err(|e| e.cause)
            .expect("independent set deletion");
        assert!(reader.read_records().expect("empty").is_empty());
        assert!(reader.read_map_sets().expect("no residue").is_empty());
    });
}

#[test]
fn sqlite_map_sharing() {
    exercise(bpfman_store_sqlite::Backend);
}
#[test]
fn json_map_sharing() {
    exercise(bpfman_store_json::Backend);
}
