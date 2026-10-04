//! Shared owned observations for boundary tests; never an I/O interpreter.
#![allow(dead_code, clippy::expect_used)]

use bpfman_model::*;
use std::{collections::BTreeMap, num::NonZeroU32};

pub(crate) fn record() -> StoredProgram {
    StoredProgram {
        id: NonZeroU32::new(42).expect("fixture"),
        spec: ProgramSpec::Tracepoint(
            Symbol::try_from("tracepoint_kill_recorder").expect("symbol"),
        ),
        source: ProgramSource::File(Some("original.o".into())),
        object_path: "/tmp/runtime/programs/42/bytecode.o".into(),
        pin_path: "/tmp/runtime/fs/prog_42".into(),
        map_path: "/tmp/runtime/fs/maps/42".into(),
        map_set: NonZeroU32::new(42).expect("fixture"),
        globals: BTreeMap::new(),
        license: "Dual BSD/GPL".into(),
        gpl_compatible: true,
        owner: String::new(),
        description: String::new(),
        metadata: BTreeMap::new(),
        created_at: "2026-10-03T12:00:00Z".into(),
        updated_at: None,
        links: Vec::new(),
    }
}

pub(crate) fn kernel() -> KernelProgram {
    KernelProgram {
        id: NonZeroU32::new(42).expect("fixture"),
        name: "tracepoint_kill".into(),
        kind: "tracepoint".into(),
        tag: "1122334455667788".into(),
        loaded_at: Some("2026-10-03T12:00:00Z".into()),
        uid: Some(0),
        btf_id: Some(7),
        map_ids: Some(vec![100]),
        jited_size: 114,
        xlated_size: 168,
        verified_insns: 12,
        memlock: Some(4096),
        restricted: false,
    }
}

pub(crate) fn map() -> KernelMap {
    KernelMap {
        id: 100,
        name: "tracepoint_stat".into(),
        kind: "percpuarray".into(),
        key_size: 4,
        value_size: 8,
        max_entries: 1,
        flags: 0,
        btf_id: Some(7),
        map_extra: None,
        memlock: Some(448),
        frozen: false,
    }
}

pub(crate) fn program() -> ObservedProgram {
    let record = record();
    ObservedProgram {
        links: Vec::new(),
        kernel: kernel(),
        stats: Some(ProgramStats {
            runtime_ns: 0,
            run_count: 0,
            recursion_misses: 0,
        }),
        prog_pin: record.pin_path.clone(),
        map_dir: record.map_path.clone(),
        bytecode: record.object_path.clone(),
        maps: vec![ObservedMap {
            kernel: map(),
            pin_path: Some(format!("{}/tracepoint_stats_map", record.map_path)),
            present: true,
        }],
        map_used_by: vec![record.id],
        record,
    }
}
