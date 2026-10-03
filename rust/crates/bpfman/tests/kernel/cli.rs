//! Public CLI checks, using returned observations rather than persistence queries.

use super::support::*;
use std::{
    fs,
    process::{Command, Stdio},
};

pub(super) fn behaviour(store: &'static str) {
    let c = Context::with_store(store);
    let root = c.layout.root();
    let fifo = root.parent().expect("temporary parent").join("source.fifo");
    assert!(
        Command::new("mkfifo")
            .arg(&fifo)
            .status()
            .expect("mkfifo")
            .success()
    );
    for (source, selection, diagnostic) in [
        (fifo, SELECTION, "regular ELF file"),
        (
            fixture("tracepoint_counter_pinned.bpf.o"),
            SELECTION,
            "PinByName",
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
    assert_eq!(
        fs::read(c.layout.bytecode_path(pid)).expect("bytecode"),
        fs::read(&source).expect("source")
    );
    assert_eq!(
        loaded["record"]["load"]["source_path"],
        source.to_str().expect("path")
    );
    assert_eq!(loaded["record"]["load"]["program_name"], NAME);
    assert_eq!(loaded["record"]["license"], "Dual BSD/GPL");
    assert!(loaded["record"]["updated_at"].is_null());
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
    let observation = c.json(&rust(), &["program", "get", &pid_text, "-o", "json"]);
    assert_eq!(loaded["record"], observation["record"]);
    assert_eq!(loaded["status"]["kernel"], observation["status"]["kernel"]);
    assert!(loaded["status"]["stats"].is_null());
    assert!(
        loaded["status"]["maps"]
            .as_array()
            .expect("maps")
            .iter()
            .all(|m| m["pin_path"] == "" && m["present"] == false)
    );
    let list = c.json(&rust(), &["program", "list", "-o", "json"]);
    let programs = list["programs"].as_array().expect("programs");
    assert_eq!(programs.len(), 1);
    assert_eq!(programs[0]["record"], observation["record"]);
    assert_eq!(
        String::from_utf8(c.run(&rust(), &["program", "list", "-q"], true).stdout)
            .expect("IDs")
            .trim(),
        pid_text
    );

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
    for id in [pid, unrelated] {
        c.run(&rust(), &["program", "unload", &id.to_string()], true);
        c.absent(id);
    }
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

pub(super) fn dsl(store: &'static str) {
    let c = Context::with_store(store);
    let runner = std::env::var_os("BPFMAN_DSL_TEST_BIN")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| repository().join("bin/e2e-scripts.test"));
    let shell_dir = std::env::var_os("BPFMAN_SHELL_BIN_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| repository().join("bin"));
    let output = Command::new("timeout")
        .arg("180s")
        .arg("make")
        .arg("run-e2e-scripts")
        .arg(format!("BPFMAN_UNDER_TEST={}", rust().display()))
        .arg(format!("E2E_SCRIPTS_TEST_BIN={}", runner.display()))
        .arg(format!("BIN_DIR={}", shell_dir.display()))
        .arg("TEST=TestBPFManScripts/scripts/TestTracepoint_LoadAndGet[.]bpfman$")
        .env("BPFMAN_RUNTIME_DIR", c.layout.root())
        .env("BPFMAN_STORE", store)
        .env("BPFMAN_E2E_BYTECODE_SOURCE", "file")
        .current_dir(repository())
        .output()
        .expect("DSL runner");
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        String::from_utf8_lossy(&output.stdout)
            .contains("--- PASS: TestBPFManScripts/scripts/TestTracepoint_LoadAndGet.bpfman"),
        "must execute the selected script"
    );
    assert_eq!(
        c.json(&rust(), &["program", "list", "-o", "json"])["programs"],
        serde_json::json!([])
    );
    c.no_artifacts();
}
