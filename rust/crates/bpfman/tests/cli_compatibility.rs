//! Differential command/flag discovery and process-level compatibility contracts.

use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
    process::{Command, Output},
};

type Result<T = ()> = std::result::Result<T, Box<dyn std::error::Error>>;

fn go() -> PathBuf {
    std::env::var_os("BPFMAN_GO_BIN")
        .map(PathBuf::from)
        .unwrap_or_else(|| Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../bin/bpfman"))
}

fn run(binary: &Path, args: &[&str]) -> Result<Output> {
    Ok(Command::new(binary)
        .env(
            "BPFMAN_CONFIG",
            concat!(env!("CARGO_MANIFEST_DIR"), "/../../tests/fixtures/cli.toml"),
        )
        .env_remove("BPFMAN_LOG")
        .env_remove("BPFMAN_RUNTIME_DIR")
        .env_remove("BPFMAN_LOCK_TIMEOUT")
        .env_remove("RUST_LOG")
        .args(args)
        .output()?)
}

fn help(binary: &Path, path: &[&str]) -> Result<String> {
    let mut args = path.to_vec();
    args.push("--help");
    let output = run(binary, &args)?;
    assert!(
        output.status.success(),
        "{} {path:?}: {}",
        binary.display(),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stderr.is_empty());
    Ok(String::from_utf8(output.stdout)?)
}

fn flags(help: &str) -> BTreeSet<String> {
    let mut flags: BTreeSet<_> = help
        .lines()
        .filter(|line| line.trim_start().starts_with('-'))
        .flat_map(|line| {
            line.split_whitespace()
                .take_while(|word| word.starts_with('-'))
        })
        .map(|word| {
            word.split(['=', ',', '['])
                .next()
                .unwrap_or_default()
                .to_owned()
        })
        .collect();
    for line in help.lines() {
        if let Some((_, aliases)) = line
            .split_once("[alias: ")
            .or_else(|| line.split_once("[aliases: "))
        {
            for alias in aliases.split(']').next().unwrap_or_default().split(',') {
                flags.insert(alias.trim().to_owned());
            }
        }
    }
    flags.retain(|flag| {
        flag.trim_start_matches('-')
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
    });
    flags
}

#[test]
fn every_go_tool_command_and_flag_has_a_rust_entry_point() -> Result {
    let go = go();
    let rust = Path::new(env!("CARGO_BIN_EXE_bpfman"));
    let root = help(&go, &[])?;
    let mut commands = vec![Vec::new()];
    for line in root
        .lines()
        .filter(|line| line.starts_with("  ") && !line.starts_with("   "))
    {
        let line = line.trim_start();
        if line.starts_with(|c: char| c.is_ascii_lowercase()) {
            if let Some((command, _)) = line.split_once("  ") {
                commands.push(command.split_whitespace().collect());
            }
        }
    }
    // Kong hides these commands from root discovery. Daemon/CSI entry points
    // are deliberately deferred until the gRPC service is implemented.
    commands.extend([
        vec!["program", "delete"],
        vec!["link", "delete"],
        vec!["dispatcher", "list"],
        vec!["dispatcher", "get"],
        vec!["dispatcher", "delete"],
        vec!["get", "program"],
        vec!["get", "link"],
    ]);
    assert!(
        commands.len() >= 30,
        "empty or truncated Go command discovery: {commands:?}"
    );
    for path in commands {
        let expected = flags(&help(&go, &path)?);
        let actual = flags(&help(rust, &path)?);
        let missing: Vec<_> = expected.difference(&actual).collect();
        assert!(missing.is_empty(), "{path:?}: missing Go flags {missing:?}");
    }
    Ok(())
}

#[test]
fn version_is_truthful_stamped_and_independent_of_runtime_and_caches() -> Result {
    let temp = tempfile::tempdir()?;
    let output = run(
        Path::new(env!("CARGO_BIN_EXE_bpfman")),
        &[
            "--runtime-dir",
            temp.path().join("runtime").to_str().ok_or("path")?,
            "--image-cache-dir",
            temp.path().join("cache").to_str().ok_or("path")?,
            "version",
        ],
    )?;
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stderr.is_empty());
    let text = String::from_utf8(output.stdout)?;
    for field in [
        "Version:",
        "Git commit:",
        "Git branch:",
        "Git state:",
        "Build date:",
        "Rust version:",
        "Platform:",
    ] {
        assert!(text.lines().any(|line| line.starts_with(field)), "{text}");
    }
    assert!(text.contains(env!("BPFMAN_BUILD_GIT_COMMIT")));
    assert!(text.contains(env!("BPFMAN_BUILD_PLATFORM")));
    assert!(!text.contains("Go version:"));
    assert_eq!(std::fs::read_dir(temp.path())?.count(), 0);
    Ok(())
}

#[test]
fn global_logging_and_config_contracts_match_go_without_state_creation() -> Result {
    let temp = tempfile::tempdir()?;
    let config = temp.path().join("config.toml");
    for binary in [go(), PathBuf::from(env!("CARGO_BIN_EXE_bpfman"))] {
        for spec in [
            "",
            "warn",
            " WARNING, manager=DEBUG,store=err ",
            "store=trace",
            "info,,manager=debug",
            "warn,odd.component=debug",
        ] {
            let output = run(&binary, &["version", "--log", spec])?;
            assert!(
                output.status.success(),
                "{} {spec}: {}",
                binary.display(),
                String::from_utf8_lossy(&output.stderr)
            );
        }
        for duration in [
            "0",
            "+1µs",
            "1μs",
            ".5s",
            "1m.5s",
            "1.5ns",
            "0.1ns",
            "-1s",
            "-9223372036854775808ns",
        ] {
            let flag = format!("--lock-timeout={duration}");
            let output = run(&binary, &["version", &flag])?;
            assert!(
                output.status.success(),
                "{} {flag}: {}",
                binary.display(),
                String::from_utf8_lossy(&output.stderr)
            );
        }
        for duration in [
            "1day",
            "1_0s",
            "+",
            "9223372036854775808ns",
            "-9223372036854775809ns",
        ] {
            let flag = format!("--lock-timeout={duration}");
            assert!(
                !run(&binary, &["version", &flag])?.status.success(),
                "{} accepted {flag}",
                binary.display()
            );
        }
        for spec in ["off", "info,debug", "info,=debug", "warn,store=bogus"] {
            let output = run(&binary, &["version", "--log", spec])?;
            assert!(
                !output.status.success(),
                "{} accepted {spec}",
                binary.display()
            );
            assert!(output.stdout.is_empty());
        }
        let output = Command::new(&binary)
            .env(
                "BPFMAN_CONFIG",
                concat!(env!("CARGO_MANIFEST_DIR"), "/../../tests/fixtures/cli.toml"),
            )
            .env("BPFMAN_LOG", "bad-level")
            .args(["version", "--log", "warn"])
            .output()?;
        assert!(
            output.status.success(),
            "explicit log flag must override environment"
        );
        let output = Command::new(&binary)
            .env(
                "BPFMAN_CONFIG",
                concat!(env!("CARGO_MANIFEST_DIR"), "/../../tests/fixtures/cli.toml"),
            )
            .env("BPFMAN_LOG", "bad-level")
            .arg("version")
            .output()?;
        assert!(!output.status.success());
        for contents in [
            "[logging]\nformat='bad'",
            "[logging]\nlevel='bad'",
            "[logging.components]\nstore='bad'",
            "[signing]\n[[signing.trusted_identities]]\ncertificate_identity='only-half'",
        ] {
            std::fs::write(&config, contents)?;
            let output = run(
                &binary,
                &["version", "--config", config.to_str().ok_or("path")?],
            )?;
            assert!(
                !output.status.success(),
                "{} accepted {contents}",
                binary.display()
            );
            assert!(output.stdout.is_empty());
        }
        let output = run(
            &binary,
            &[
                "version",
                "--config",
                temp.path().join("missing.toml").to_str().ok_or("path")?,
            ],
        )?;
        assert!(!output.status.success());
    }
    Ok(())
}

#[test]
fn global_options_work_between_command_groups_and_their_children() -> Result {
    let temp = tempfile::tempdir()?;
    let config = concat!(env!("CARGO_MANIFEST_DIR"), "/../../tests/fixtures/cli.toml");
    for (args, depth) in [
        (vec!["program", "get", "42"], 2),
        (vec!["link", "get", "42"], 2),
        (vec!["dispatcher", "get", "xdp", "1", "7"], 2),
        (vec!["get", "program", "42"], 2),
        (vec!["get", "link", "42"], 2),
        (
            vec![
                "program",
                "load",
                "file",
                "missing.o",
                "--programs",
                "xdp:p",
            ],
            3,
        ),
        (
            vec![
                "program",
                "load",
                "image",
                "example.invalid/test",
                "--programs",
                "xdp:p",
                "--pull-policy",
                "Never",
            ],
            3,
        ),
        (vec!["image", "generate-build-args", "missing.o"], 2),
    ] {
        for position in 0..=depth {
            let mut arguments = args.clone();
            arguments.splice(position..position, ["--config", config, "--log", "warn"]);
            let output = Command::new(env!("CARGO_BIN_EXE_bpfman"))
                .env_remove("BPFMAN_CONFIG")
                .env_remove("BPFMAN_REGISTRY_AUTH")
                .arg("--runtime-dir")
                .arg(temp.path().join("runtime"))
                .arg("--image-cache-dir")
                .arg(temp.path().join("cache"))
                .args(&arguments)
                .output()?;
            assert_eq!(
                output.status.code(),
                Some(1),
                "{arguments:?}: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            assert!(output.stdout.is_empty());
            assert!(!String::from_utf8_lossy(&output.stderr).contains("read bpfman config"));
        }
    }
    Ok(())
}
