//! Detach parsing and ordered, precisely idempotent batch errors.
use std::process::Command;

#[test]
fn malformed_detach_operands_fail_before_runtime_setup() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempfile::tempdir()?;
    let root = temp.path().join("not-created");
    for operands in [
        &[][..],
        &["-1"][..],
        &["+1"][..],
        &["18446744073709551616"][..],
        &["42", "bad"][..],
        &["--ignore-missing"][..],
        &["42", "--unknown"][..],
    ] {
        let result = Command::new(env!("CARGO_BIN_EXE_bpfman"))
            .env(
                "BPFMAN_CONFIG",
                concat!(env!("CARGO_MANIFEST_DIR"), "/../../tests/fixtures/cli.toml"),
            )
            .arg("--runtime-dir")
            .arg(&root)
            .args(["link", "detach"])
            .args(operands)
            .output()?;
        assert_eq!(result.status.code(), Some(2));
        assert!(result.stdout.is_empty());
        assert!(!root.exists());
    }
    Ok(())
}

#[test]
fn missing_batch_reports_every_operand_in_order() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempfile::tempdir()?;
    for ignore in [false, true] {
        let mut command = Command::new(env!("CARGO_BIN_EXE_bpfman"));
        command.env(
            "BPFMAN_CONFIG",
            concat!(env!("CARGO_MANIFEST_DIR"), "/../../tests/fixtures/cli.toml"),
        );
        command
            .arg("--runtime-dir")
            .arg(temp.path())
            .args(["link", "detach", "43", "0", "42", "43"]);
        if ignore {
            command.arg("--ignore-missing");
        }
        let output = command.output()?;
        assert!(output.stdout.is_empty());
        if ignore {
            assert!(output.status.success());
            assert!(output.stderr.is_empty());
        } else {
            assert_eq!(output.status.code(), Some(1));
            let stderr = String::from_utf8(output.stderr)?;
            let lines: Vec<_> = stderr.lines().collect();
            assert_eq!(lines.len(), 5);
            for (line, id) in lines[..4].iter().zip([43, 0, 42, 43]) {
                assert!(line.starts_with(&format!("link {id}: ")));
                assert!(line.contains(&format!("managed link {id} does not exist")));
            }
            assert_eq!(lines[4], "Error: 4 of 4 link(s) failed to detach");
        }
    }
    assert!(!temp.path().join("fs").exists());
    Ok(())
}
