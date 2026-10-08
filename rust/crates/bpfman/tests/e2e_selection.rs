//! Exercise the real Make recipe using unprivileged process fixtures.

use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::{Command, Output},
};

type Result = std::result::Result<(), Box<dyn std::error::Error>>;
struct Fixture {
    _temp: tempfile::TempDir,
    root: PathBuf,
    go: PathBuf,
    rust: PathBuf,
    tools: PathBuf,
}

fn executable(path: &Path, body: &str) -> std::io::Result<()> {
    fs::write(path, format!("#!/bin/sh\nset -eu\n{body}"))?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o755))
}

impl Fixture {
    fn new() -> std::io::Result<Self> {
        let temp = tempfile::tempdir()?;
        let root = temp.path().to_owned();
        let go = root.join("go");
        let rust = root.join("rust");
        let tools = root.join("tools");

        for path in [&go, &rust, &tools] {
            fs::create_dir(path)?;
        }

        executable(&tools.join("sudo"), "exec \"$@\"\n")?;
        executable(&go.join("bpfman"), "echo go\n")?;
        executable(&rust.join("bpfman"), "echo rust\n")?;
        executable(&go.join("e2e-scripts.test"), "exec bpfman-shell\n")?;
        executable(
            &go.join("bpfman-shell"),
            "\"$BPFMAN_BIN\"\nbpfman\nsh -c 'exec bpfman'\n",
        )?;

        Ok(Self {
            _temp: temp,
            root,
            go,
            rust,
            tools,
        })
    }

    fn run(&self, selected: Option<&Path>) -> std::io::Result<Output> {
        self.run_implementation(selected, None)
    }

    fn run_implementation(
        &self,
        selected: Option<&Path>,
        implementation: Option<&str>,
    ) -> std::io::Result<Output> {
        let mut command = Command::new("make");
        command
            .current_dir(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.."))
            .args(["--no-print-directory", "-s", "run-e2e-scripts"])
            .arg(format!("BIN_DIR={}", self.go.display()))
            .env("BPFMAN_BIN", self.go.join("bpfman"))
            .env_remove("BPFMAN_UNDER_TEST")
            .env_remove("BPFMAN_E2E_IMPLEMENTATION")
            .env_remove("E2E_SCRIPTS_TEST_BIN");
        let path = std::env::var_os("PATH").unwrap_or_default();
        let paths = std::iter::once(self.tools.clone()).chain(std::env::split_paths(&path));
        command.env(
            "PATH",
            std::env::join_paths(paths).map_err(std::io::Error::other)?,
        );

        if let Some(binary) = selected {
            command.arg(format!("BPFMAN_UNDER_TEST={}", binary.display()));
        }

        if let Some(implementation) = implementation {
            command.arg(format!("BPFMAN_E2E_IMPLEMENTATION={implementation}"));
        }

        command.output()
    }
}

#[test]
fn default_and_override_select_the_same_binary_for_typed_raw_and_nested_commands() -> Result {
    let f = Fixture::new()?;

    for (selected, expected) in [
        (None, "go\ngo\ngo\n"),
        (Some(f.rust.join("bpfman")), "rust\nrust\nrust\n"),
    ] {
        let output = f.run(selected.as_deref())?;

        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(String::from_utf8(output.stdout)?, expected);
    }

    Ok(())
}

#[test]
fn invalid_selection_fails_before_running_any_cli() -> Result {
    let f = Fixture::new()?;
    let other = f.rust.join("bpfman-other");
    executable(&other, "echo wrong\n")?;

    for (path, message) in [
        (f.root.join("missing/bpfman"), "not executable"),
        (other, "called bpfman"),
    ] {
        let output = f.run(Some(&path))?;

        assert!(!output.status.success());
        assert!(output.stdout.is_empty());
        assert!(String::from_utf8_lossy(&output.stderr).contains(message));
    }

    Ok(())
}

#[test]
fn implementation_identity_is_explicit_and_invalid_values_fail_before_execution() -> Result {
    let f = Fixture::new()?;
    executable(
        &f.go.join("e2e-scripts.test"),
        "printf '%s' \"$BPFMAN_E2E_IMPLEMENTATION\"\n",
    )?;
    for (implementation, expected) in [(None, "go"), (Some("go"), "go"), (Some("rust"), "rust")] {
        // A Rust-looking path does not change admission unless explicitly selected.
        let output = f.run_implementation(Some(&f.rust.join("bpfman")), implementation)?;
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(String::from_utf8(output.stdout)?, expected);
    }
    for implementation in ["", "Rust", "unknown"] {
        let output = f.run_implementation(None, Some(implementation))?;
        assert!(!output.status.success());
        assert!(output.stdout.is_empty());
        assert!(String::from_utf8_lossy(&output.stderr).contains("must be go or rust"));
    }
    Ok(())
}
