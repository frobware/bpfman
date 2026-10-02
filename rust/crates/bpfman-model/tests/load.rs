//! Refined load vocabulary shared by the CLI and lifecycle policy.

use bpfman_model::{InvalidSymbol, ProgramSpec, ProgramType, Symbol};

#[test]
fn symbols_are_refined_without_becoming_path_authority() -> Result<(), InvalidSymbol> {
    for invalid in ["", " ", "\t", " name", "name\n", "name:target", "name\0"] {
        assert!(Symbol::try_from(invalid).is_err(), "{invalid:?}");
    }
    // ELF/target spellings are not automatically filesystem-safe names.
    // The filesystem adapter must use typed identities, never these as paths.
    for valid in [
        "tracepoint_kill_recorder",
        ".rodata",
        "../not-a-path-capability",
    ] {
        assert_eq!(Symbol::try_from(valid)?.as_str(), valid);
    }
    Ok(())
}

#[test]
fn every_kind_is_derived_from_its_payload() -> Result<(), InvalidSymbol> {
    let name = Symbol::try_from("entry")?;
    let target = Symbol::try_from("do_open")?;
    let specs = [
        ProgramSpec::Xdp(name.clone()),
        ProgramSpec::Tc(name.clone()),
        ProgramSpec::Tcx(name.clone()),
        ProgramSpec::Tracepoint(name.clone()),
        ProgramSpec::Kprobe(name.clone()),
        ProgramSpec::Kretprobe(name.clone()),
        ProgramSpec::Uprobe(name.clone()),
        ProgramSpec::Uretprobe(name.clone()),
        ProgramSpec::Fentry {
            name: name.clone(),
            target: target.clone(),
        },
        ProgramSpec::Fexit {
            name: name.clone(),
            target,
        },
        ProgramSpec::Lsm {
            name,
            hook: Symbol::try_from("file_open")?,
        },
    ];
    for (spec, kind) in specs.iter().zip(ProgramType::ALL) {
        assert_eq!(spec.kind(), kind);
        assert_eq!(spec.name().as_str(), "entry");
    }
    Ok(())
}
