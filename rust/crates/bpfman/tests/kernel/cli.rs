//! CLI effect boundaries, pin ownership and output failures.
//! Ordinary load/get/list/unload behaviour runs through the shared script corpus.

use super::support::*;
use std::{
    fs,
    process::{Command, Stdio},
};

pub(super) fn effect_boundaries() {
    let c = Context::new();
    let root = c.layout.root();
    let fifo = root.parent().expect("temporary parent").join("source.fifo");

    assert!(
        Command::new("mkfifo")
            .arg(&fifo)
            .status()
            .expect("mkfifo")
            .success()
    );

    let internal_pin = root
        .parent()
        .expect("temporary parent")
        .join("internal-pin.o");
    let mut bytes = fs::read(fixture("tracepoint_counter_pinned.bpf.o")).expect("ELF");
    let name = b"tracepoint_stats_map";
    let positions: Vec<_> = bytes
        .windows(name.len())
        .enumerate()
        .filter_map(|(offset, value)| (value == name).then_some(offset))
        .collect();
    assert!(!positions.is_empty());
    for offset in positions {
        bytes[offset] = b'.';
    }
    fs::write(&internal_pin, bytes).expect("malformed map declaration");

    for (source, selection, diagnostic) in [
        (fifo, SELECTION, "regular ELF file"),
        (
            internal_pin,
            SELECTION,
            "internal maps cannot declare PinByName",
        ),
        (
            fixture("tracepoint_counter.bpf.o"),
            "tracepoint:missing",
            "does not exist",
        ),
        (
            fixture("xdp_pass.bpf.o"),
            "tracepoint:pass",
            "not a tracepoint",
        ),
        (
            fixture("tracepoint_counter.bpf.o"),
            "xdp:tracepoint_kill_recorder",
            "not XDP",
        ),
    ] {
        let output = c.run(
            &rust(),
            &[
                "program",
                "load",
                "file",
                source.to_str().expect("path"),
                "--programs",
                selection,
            ],
            false,
        );

        assert!(String::from_utf8_lossy(&output.stderr).contains(diagnostic));
        assert!(!root.exists());
    }

    let loaded = c.load_cli(&rust());
    let pid = id(&loaded);
    let pid_text = pid.to_string();
    c.present(pid);
    let source = fixture("tracepoint_counter.bpf.o");

    // Rust's private provenance receipt is an ownership contract. The captured
    // ELF, public record round trips and quiet listing are checked by scripts.
    let provenance: serde_json::Value = serde_json::from_slice(
        &fs::read(
            c.layout
                .bytecode_path(pid)
                .parent()
                .expect("directory")
                .join("provenance.json"),
        )
        .expect("provenance"),
    )
    .expect("JSON");

    assert_eq!(provenance["program_id"], pid.get());
    assert_eq!(provenance["source"], source.to_str().expect("path"));

    let unrelated = id(&c.load_cli(&rust()));

    // Refuse foreign live program and map identities, preserving both programs.
    let pin = c.layout.program_pin_path(pid);
    let other = c.layout.program_pin_path(unrelated);
    let saved = root.join("fs/saved_program");
    fs::rename(&pin, &saved).expect("save pin");
    fs::rename(&other, &pin).expect("replace pin");
    let output = c.run(&rust(), &["program", "unload", &pid_text], false);

    assert!(String::from_utf8_lossy(&output.stderr).contains("different kernel identity"));
    fs::rename(&pin, &other).expect("restore other pin");
    fs::rename(&saved, &pin).expect("restore pin");
    c.present(pid);
    c.present(unrelated);

    let map = c
        .layout
        .map_directory_path(pid)
        .join("tracepoint_stats_map");
    let other = c
        .layout
        .map_directory_path(unrelated)
        .join("tracepoint_stats_map");
    let saved = root.join("fs/saved_map");
    fs::rename(&map, &saved).expect("save map");
    fs::rename(&other, &map).expect("replace map");
    let output = c.run(&rust(), &["program", "unload", &pid_text], false);

    assert!(String::from_utf8_lossy(&output.stderr).contains("does not belong"));
    fs::rename(&map, &other).expect("restore other map");
    fs::rename(&saved, &map).expect("restore map");
    c.present(pid);
    c.present(unrelated);

    // Rendering happens after link finalisation. A failed output destination
    // must leave a manageable link with its original kernel identity.
    let output = c
        .command(
            &rust(),
            &[
                "link",
                "attach",
                "tracepoint",
                &pid_text,
                "syscalls/sys_enter_kill",
                "-o",
                "json",
            ],
        )
        .stdout(Stdio::from(
            fs::OpenOptions::new()
                .write(true)
                .open("/dev/full")
                .expect("full sink"),
        ))
        .output()
        .expect("attach CLI");
    assert_eq!(output.status.code(), Some(1));
    let links = c.json(&rust(), &["link", "list", "-o", "json"]);
    let links = links["links"].as_array().expect("stored links");
    assert_eq!(links.len(), 1);
    let link_id = links[0]["id"]
        .as_u64()
        .expect("managed link ID")
        .to_string();
    let observed = c.json(&rust(), &["link", "get", &link_id, "-o", "json"]);
    assert_eq!(observed["status"]["kernel_seen"], true);
    assert_eq!(observed["status"]["pin_present"], true);
    let program = c.json(&rust(), &["program", "get", &pid_text, "-o", "json"]);
    assert_eq!(program["status"]["links"], serde_json::json!([observed]));
    let text = c.run(&rust(), &["program", "get", &pid_text], true);
    let text = String::from_utf8(text.stdout).expect("text");
    assert!(text.contains("syscalls/sys_enter_kill"));
    assert!(!text.contains("Links: None"));

    for id in [pid, unrelated] {
        c.run(&rust(), &["program", "unload", &id.to_string()], true);
        c.absent(id);
    }

    assert_eq!(
        c.json(&rust(), &["link", "list", "-o", "json"])["links"],
        serde_json::json!([])
    );
    c.no_artifacts();

    // Output delivery is after commit, independently of the backend format.
    let output = c
        .command(
            &rust(),
            &[
                "program",
                "load",
                "file",
                source.to_str().expect("path"),
                "--programs",
                SELECTION,
            ],
        )
        .stdout(Stdio::from(
            fs::OpenOptions::new()
                .write(true)
                .open("/dev/full")
                .expect("full sink"),
        ))
        .output()
        .expect("CLI");

    assert_eq!(output.status.code(), Some(1));

    let list = c.json(&rust(), &["program", "list", "-o", "json"]);
    let programs = list["programs"].as_array().expect("programs");

    assert_eq!(programs.len(), 1);

    let pid = id(&programs[0]);
    c.present(pid);
    c.run(&rust(), &["program", "get", &pid.to_string()], true);
    c.run(&rust(), &["program", "unload", &pid.to_string()], true);

    assert_eq!(
        c.json(&rust(), &["program", "list", "-o", "json"])["programs"],
        serde_json::json!([])
    );
    c.no_artifacts();
}

pub(super) fn go_dsl(script: &str) {
    let binary = std::env::var_os("BPFMAN_GO_BIN")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| repository().join("bin/bpfman"));
    run_dsl(Some(&[script]), &binary, false);
}

pub(super) fn corpus() {
    // Both cohorts share one runner/pool. Four scripts additionally mutate one
    // owned writer/store while ordinary acceptance keeps per-case isolation.
    run_dsl(None, &rust(), true);
}

pub(super) fn program_inventory_scripts() {
    let scripts = [
        "TestProgramList_All",
        "TestProgramList_AllTypeFilterKernelOnly",
        "TestProgramList_AttachmentState",
        "TestProgramList_AttachmentStateNetwork",
        "TestProgramList_MetadataSelectors",
        "TestLinkMetadata_PersistAndFilter",
        "TestLinkList_Filters",
    ];
    let go = std::env::var_os("BPFMAN_GO_BIN")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| repository().join("bin/bpfman"));
    for binary in [go, rust()] {
        run_dsl(Some(&scripts), &binary, true);
    }
}

fn run_dsl(scripts: Option<&[&str]>, binary: &std::path::Path, isolated: bool) {
    run_dsl_source(scripts, binary, isolated, "file");
}

fn run_dsl_source(
    scripts: Option<&[&str]>,
    binary: &std::path::Path,
    isolated: bool,
    source: &str,
) {
    let c = Context::new();
    let runner = std::env::var_os("BPFMAN_DSL_TEST_BIN")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| repository().join("bin/e2e-scripts.test"));
    let shell_dir = std::env::var_os("BPFMAN_SHELL_BIN_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| repository().join("bin"));
    let selector = if scripts.is_some() {
        "!external"
    } else {
        "rust=ok,!external"
    };
    let expected = if let Some(scripts) = scripts {
        scripts
            .iter()
            .map(|script| format!("{script}.bpfman"))
            .collect::<Vec<_>>()
    } else {
        let listing = Command::new(shell_dir.join("bpfman-shell"))
            .args(["--list-scripts", "--selector", selector, "e2e/scripts"])
            .current_dir(repository())
            .output()
            .expect("script manifest");
        assert!(
            listing.status.success(),
            "{}",
            String::from_utf8_lossy(&listing.stderr)
        );
        String::from_utf8(listing.stdout)
            .expect("script paths")
            .lines()
            .map(|path| {
                std::path::Path::new(path)
                    .file_name()
                    .expect("script name")
                    .to_string_lossy()
                    .into_owned()
            })
            .collect()
    };
    assert!(
        !expected.is_empty(),
        "script acceptance must not select an empty corpus"
    );
    let filter = scripts.map_or_else(
        || "TestBPFManScripts".to_owned(),
        |names| format!("TestBPFManScripts/scripts/({})[.]bpfman$", names.join("|")),
    );
    let started = timings_enabled().then(std::time::Instant::now);
    // Isolated cases spend time waiting for packet observation and subprocesses.
    // The Go runner owns their pool and scheduling; keep the shared-store cohort
    // limited to four slots and honour the Make override for focused profiling.
    let parallel = std::env::var("PARALLEL").map_or_else(
        |_| if isolated { 32 } else { 4 },
        |value| {
            value
                .parse::<usize>()
                .expect("PARALLEL must be nonnegative")
        },
    );
    let shared = [
        "TestProgram_FileLifecycle.bpfman",
        "TestXDP_DispatcherFillDrainRefill.bpfman",
        "TestXDP_Lifecycle_Drv.bpfman",
        "TestTC_IngressLifecycle.bpfman",
    ];
    let mixed = isolated && scripts.is_none();
    let mut command = Command::new("timeout");
    if source == "image" {
        command.env(
            "BPFMAN_CONFIG",
            repository().join("e2e/config/no-signature-verification.toml"),
        );
    }
    let output = command
        .arg("300s")
        .arg("make")
        .arg("run-e2e-scripts")
        .arg(format!("BPFMAN_UNDER_TEST={}", binary.display()))
        .arg(format!(
            "BPFMAN_E2E_IMPLEMENTATION={}",
            if binary == rust() { "rust" } else { "go" }
        ))
        .arg(format!("E2E_SCRIPTS_TEST_BIN={}", runner.display()))
        .arg(format!("BIN_DIR={}", shell_dir.display()))
        .arg(format!("TEST={filter}"))
        .arg(format!("PARALLEL={parallel}"))
        .arg(format!("BPFMAN_E2E_SCRIPT_SELECTOR={selector}"))
        .env("BPFMAN_RUNTIME_DIR", c.layout.root())
        .env(
            "BPFMAN_E2E_ISOLATED_RUNTIME",
            if isolated { "1" } else { "0" },
        )
        .env(
            "BPFMAN_E2E_SHARED_SCRIPTS",
            if mixed {
                shared.join(",")
            } else {
                String::new()
            },
        )
        .env("BPFMAN_STORE", "sqlite")
        .env("BPFMAN_E2E_BYTECODE_SOURCE", source)
        .env(
            "BPFMAN_E2E_CLSACT_RECLAIM",
            if binary == rust() { "true" } else { "" },
        )
        .current_dir(repository())
        .output()
        .expect("DSL runner");

    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    if timings_enabled() {
        use std::io::Write;

        // Go already reports per-script elapsed time. Preserve only those
        // result rows, rather than dumping captured CLI/packet output.
        let mut stderr = std::io::stderr().lock();
        let scope = if mixed {
            "mixed"
        } else if isolated {
            "isolated"
        } else {
            "shared"
        };
        let implementation = if binary == rust() { "rust" } else { "go" };
        if let Some(started) = started {
            let _ = writeln!(
                stderr,
                "script-batch-timing\tsqlite\t{scope}\t{implementation}\t{:.3}",
                started.elapsed().as_secs_f64()
            );
        }
        for line in String::from_utf8_lossy(&output.stdout).lines() {
            let line = line.trim();
            if line.starts_with("--- PASS: TestBPFManScripts/") && line.contains("/scripts/") {
                let _ = writeln!(
                    stderr,
                    "script-timing\tsqlite\t{scope}\t{implementation}\t{line}"
                );
            }
        }
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let prefix = if mixed { "isolated/" } else { "" };
    for script in expected {
        assert!(
            stdout.contains(&format!(
                "--- PASS: TestBPFManScripts/{prefix}scripts/{script} ("
            )),
            "must execute selected script {script}: {stdout}",
        );
    }
    if mixed {
        for script in shared {
            assert!(
                stdout.contains(&format!(
                    "--- PASS: TestBPFManScripts/shared/scripts/{script} ("
                )),
                "must execute shared script {script}: {stdout}",
            );
        }
    }
    assert_eq!(
        c.json(binary, &["program", "list", "-o", "json"])["programs"],
        serde_json::json!([])
    );
    assert_eq!(
        c.json(binary, &["link", "list", "-o", "json"])["links"],
        serde_json::json!([])
    );
    assert_eq!(
        c.json(binary, &["dispatcher", "list", "-o", "json"])["dispatchers"],
        serde_json::json!([])
    );
    c.no_artifacts();
}

// Existing acceptance files remain immutable and separately selected until the
// manifest policy permits admission. Keep them in the normal kernel gate.
pub(super) fn tcx_scripts() {
    run_dsl(
        Some(&[
            "TestTCX_LoadAndGet",
            "TestTCX_LinkRoundTrip",
            "TestTCX_DuplicateAttachRejected",
            "TestTCX_PriorityOrdering",
            "TestTCX_ZeroPriorityDefaultOrdering",
            "TestTCX_NetnsVethPairLinkRoundTrip",
            "TestMultiProgTCX_AllProceed_DefaultProceedOn",
            "TestMultiProgTCX_ChainStopsAtOK_DefaultProceedOn",
        ]),
        &rust(),
        true,
    );
}

pub(super) fn kprobe_scripts() {
    run_dsl(
        Some(&[
            "TestKprobe_LoadAndGet",
            "TestKretprobe_LoadAndGet",
            "TestKprobe_LinkRoundTrip",
            "TestKretprobe_LinkRoundTrip",
            "TestMultiProgKprobe_LoadAttachDetachUnload",
            "TestMultiProgKretprobe_LoadAttachDetachUnload",
            "TestKprobe_OffsetBeyondSymbolRejected",
        ]),
        &rust(),
        true,
    );
}

pub(super) fn map_sharing_scripts() {
    run_dsl(
        Some(&[
            "TestKprobe_MapOwnerIDSharedCounter",
            "TestProgramLoad_MapOwnerIDMustNameMapSet",
            "TestTC_PinByNameMapSharing",
        ]),
        &rust(),
        true,
    );
}

pub(super) fn uprobe_scripts() {
    run_dsl(
        Some(&[
            "TestUprobe_LoadAndGet",
            "TestUretprobe_LoadAndGet",
            "TestUprobe_LinkRoundTrip",
            "TestUretprobe_LinkRoundTrip",
            "TestMultiProgUprobe_LoadAttachDetachUnload",
            "TestMultiProgUretprobe_LoadAttachDetachUnload",
            "TestUprobe_OffsetOnlyTraffic",
            "TestUprobe_PidFilterTraffic",
            "TestUprobe_GoSymbolTraffic",
            "TestUprobe_LibraryNameCacheTraffic",
            "TestContainerUprobe_LibraryNameTarget",
            "TestContainerUprobe_SyntheticLdSoCache",
            "TestContainerUprobe_PodStyleNamespaces",
            "TestContainerUprobe_HostProcDaemonView",
        ]),
        &rust(),
        true,
    );
}

pub(super) fn container_uprobe_scripts() {
    run_dsl(
        Some(&[
            "TestContainerUprobe_LinkIDAndTraffic",
            "TestContainerUprobe_ExternalCLIPersistence",
            "TestContainerUprobe_PidNamespaceTraffic",
        ]),
        &rust(),
        true,
    );
}

pub(super) fn function_tracing_scripts() {
    run_dsl(
        Some(&[
            "TestFentry_LoadAndGet",
            "TestFexit_LoadAndGet",
            "TestFentry_LinkRoundTrip",
            "TestFexit_LinkRoundTrip",
            "TestMultiProgFentry_LoadAttachDetachUnload",
            "TestMultiProgFexit_LoadAttachDetachUnload",
        ]),
        &rust(),
        true,
    );
}

pub(super) fn lsm_scripts() {
    run_dsl(
        Some(&["TestLsm_LoadAndGet", "TestLsm_LinkRoundTrip"]),
        &rust(),
        true,
    );
}

pub(super) fn image_build_scripts() {
    run_dsl_source(
        Some(&[
            "TestImageBuild_GenerateBuildArgsText",
            "TestImageBuild_GenerateBuildArgsJSONSingleArch",
            "TestImageBuild_GenerateBuildArgsJSONMultiArch",
            "TestImageBuild_GenerateBuildArgsCiliumProject",
            "TestImageBuild_GenerateBuildArgsFailures",
            "TestImageBuild_BuildLoadAttachXDP",
        ]),
        &rust(),
        true,
        "image",
    );
}
