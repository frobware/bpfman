//! Compatibility cases taken from Go's program-type boundary vocabulary.

use bpfman_model::ProgramType;

#[test]
fn boundary_spellings_are_stable_and_round_trip() {
    let names = [
        "xdp",
        "tc",
        "tcx",
        "tracepoint",
        "kprobe",
        "kretprobe",
        "uprobe",
        "uretprobe",
        "fentry",
        "fexit",
        "lsm",
    ];

    assert_eq!(ProgramType::ALL.map(ProgramType::as_str), names);

    for (kind, name) in ProgramType::ALL.into_iter().zip(names) {
        assert_eq!(name.parse(), Ok(kind));
        assert_eq!(kind.to_string(), name);
    }
}

#[test]
fn boundary_parser_rejects_unknown_and_noncanonical_names() {
    for name in ["", "XDP", " xdp", "xdp ", "schedcls", "tracing", "unknown"] {
        assert!(name.parse::<ProgramType>().is_err(), "accepted {name:?}");
    }
}
