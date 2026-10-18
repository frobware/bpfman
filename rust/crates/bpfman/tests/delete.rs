//! Hidden delete parsing and ordered missing-target diagnostics.
use std::process::Command;

#[test]
fn invalid_delete_requests_fail_before_runtime_creation() -> Result<(), Box<dyn std::error::Error>>
{
    let temp = tempfile::tempdir()?;
    let root = temp.path().join("not-created");
    for family in ["program", "link"] {
        for operands in [
            &[][..],
            &["0"][..],
            &["-1"][..],
            &["42", "bad"][..],
            &["42", "0"][..],
            &["42", "--all"][..],
            &["42", "--unknown"][..],
            &["--recursive"][..],
            &["--ignore-missing", "42"][..],
        ] {
            let result = Command::new(env!("CARGO_BIN_EXE_bpfman"))
                .env(
                    "BPFMAN_CONFIG",
                    concat!(env!("CARGO_MANIFEST_DIR"), "/../../tests/fixtures/cli.toml"),
                )
                .arg("--runtime-dir")
                .arg(&root)
                .args([family, "delete"])
                .args(operands)
                .output()?;
            assert_eq!(result.status.code(), Some(2), "{family} {operands:?}");
            assert!(result.stdout.is_empty());
            assert!(!root.exists());
        }
        let help = Command::new(env!("CARGO_BIN_EXE_bpfman"))
            .env(
                "BPFMAN_CONFIG",
                concat!(env!("CARGO_MANIFEST_DIR"), "/../../tests/fixtures/cli.toml"),
            )
            .args([family, "--help"])
            .output()?;
        assert!(help.status.success());
        assert!(!String::from_utf8(help.stdout)?.contains("  delete"));
    }
    Ok(())
}

#[test]
fn missing_delete_targets_report_per_operand_and_empty_all_is_silent()
-> Result<(), Box<dyn std::error::Error>> {
    let temp = tempfile::tempdir()?;
    for family in ["program", "link"] {
        let result = Command::new(env!("CARGO_BIN_EXE_bpfman"))
            .env(
                "BPFMAN_CONFIG",
                concat!(env!("CARGO_MANIFEST_DIR"), "/../../tests/fixtures/cli.toml"),
            )
            .arg("--runtime-dir")
            .arg(temp.path())
            .args([family, "delete", "43", "42", "43"])
            .output()?;
        assert_eq!(result.status.code(), Some(1));
        assert!(result.stdout.is_empty());
        let stderr = String::from_utf8(result.stderr)?;
        let lines: Vec<_> = stderr.lines().collect();
        assert_eq!(lines.len(), 4, "{stderr}");
        for (line, id) in lines[..3].iter().zip([43, 42, 43]) {
            assert!(line.starts_with(&format!("{family} {id}: ")), "{stderr}");
            assert!(line.contains("does not exist"), "{stderr}");
        }
        assert_eq!(
            lines[3],
            format!("Error: 3 of 3 {family}(s) failed to delete")
        );
    }
    for operands in [&["--all"][..], &["--all", "-r"][..]] {
        let result = Command::new(env!("CARGO_BIN_EXE_bpfman"))
            .env(
                "BPFMAN_CONFIG",
                concat!(env!("CARGO_MANIFEST_DIR"), "/../../tests/fixtures/cli.toml"),
            )
            .arg("--runtime-dir")
            .arg(temp.path())
            .args(["program", "delete"])
            .args(operands)
            .output()?;
        assert!(result.status.success());
        assert!(result.stdout.is_empty());
        assert!(result.stderr.is_empty());
    }
    assert!(!temp.path().join("fs").exists());
    Ok(())
}

#[test]
fn dispatcher_delete_validates_keys_and_reports_absence() -> Result<(), Box<dyn std::error::Error>>
{
    let temp = tempfile::tempdir()?;
    let root = temp.path().join("runtime");
    for operands in [
        &[][..],
        &["xdp", "1"][..],
        &["unknown", "1", "7"][..],
        &["xdp", "0", "7"][..],
        &["xdp", "1", "0"][..],
        &["xdp", "-1", "7"][..],
        &["xdp", "1", "4294967296"][..],
        &["xdp", "1", "7", "--recursive"][..],
        &["xdp", "1", "7", "-o", "json"][..],
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_bpfman"))
            .env(
                "BPFMAN_CONFIG",
                concat!(env!("CARGO_MANIFEST_DIR"), "/../../tests/fixtures/cli.toml"),
            )
            .arg("--runtime-dir")
            .arg(&root)
            .args(["dispatcher", "delete"])
            .args(operands)
            .output()?;
        assert_eq!(output.status.code(), Some(2), "{operands:?}");
        assert!(output.stdout.is_empty());
        assert!(!root.exists());
    }
    for kind in ["xdp", "tc-ingress", "tc-egress"] {
        let output = Command::new(env!("CARGO_BIN_EXE_bpfman"))
            .env(
                "BPFMAN_CONFIG",
                concat!(env!("CARGO_MANIFEST_DIR"), "/../../tests/fixtures/cli.toml"),
            )
            .arg("--runtime-dir")
            .arg(&root)
            .args(["dispatcher", "delete", kind, "1", "7"])
            .output()?;
        assert_eq!(output.status.code(), Some(1));
        assert!(output.stdout.is_empty());
        assert_eq!(
            String::from_utf8(output.stderr)?,
            format!("Error: dispatcher ({kind}, 1, 7): record not found\n")
        );
    }
    let help = Command::new(env!("CARGO_BIN_EXE_bpfman"))
        .env(
            "BPFMAN_CONFIG",
            concat!(env!("CARGO_MANIFEST_DIR"), "/../../tests/fixtures/cli.toml"),
        )
        .args(["dispatcher", "--help"])
        .output()?;
    assert!(help.status.success());
    assert!(!String::from_utf8(help.stdout)?.contains("  delete"));
    assert!(!root.join("fs").exists());
    Ok(())
}
