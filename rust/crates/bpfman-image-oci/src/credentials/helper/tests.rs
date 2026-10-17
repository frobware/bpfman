#![allow(clippy::expect_used)]
use super::*;
use base64::{Engine, engine::general_purpose::STANDARD};
use std::sync::atomic::Ordering;

use super::super::test_registry as registry;

type Result = std::result::Result<(), Box<dyn std::error::Error>>;

fn shell(script: &str) -> Command {
    let mut command = Command::new("/bin/sh");
    command.args(["-c", script]);
    command
}

#[test]
fn protocol_preserves_colons_and_identity_tokens_and_redacts_malformed_responses() -> Result {
    let credentials = decode(
        br#"{"ServerURL":"ignored","Username":"helper-user","Secret":"secret:with:colons"}"#,
    )?
    .ok_or("credentials")?;
    assert_eq!(
        credentials.basic()?,
        format!(
            "Basic {}",
            STANDARD.encode("helper-user:secret:with:colons")
        )
    );
    assert!(!format!("{credentials:?}").contains("secret"));
    for response in [
        br#"{"Username":"<token>","Secret":""}"#.as_slice(),
        br#"{"Username":{"secret":true},"Secret":"password-secret"}"#,
        br#"{"Username":"user","Secret":42}"#,
        br#"{"Username":"","Secret":"password-secret"}"#,
        br#"{"Username":"user","Secret":""}"#,
        b"[]",
        b"null",
        b"malformed-secret",
        br#"{} trailing-secret"#,
    ] {
        let error = decode(response).expect_err("invalid/unsupported");
        assert!(!format!("{error:?} {error}").contains("secret"));
        assert!(std::error::Error::source(&error).is_none());
    }
    assert!(decode(b"{}")?.is_none());
    assert!(decode(br#"{"Username":"","Secret":""}"#)?.is_none());
    let token = decode(br#"{"Username":"<token>","Secret":"refresh-secret"}"#)?
        .ok_or("identity credentials")?;
    assert_eq!(token.refresh(), Some("refresh-secret"));
    assert!(token.bearer().is_none());
    assert!(token.basic().is_err());
    assert!(!format!("{token:?}").contains("refresh-secret"));
    Ok(())
}

#[test]
fn synchronous_protocol_drains_both_streams_and_closes_stdin() -> Result {
    let script = r#"
        IFS= read -r registry || [ -n "$registry" ] || exit 9
        [ "$registry" = "quay.io:5000" ] || exit 10
        n=0
        while [ "$n" -lt 3000 ]; do
            printf 'discarded-stderr\n' >&2
            n=$((n + 1))
        done
        printf '{"Username":"helper-user","Secret":"secret:with:colons"}'
    "#;
    let output = execute(
        shell(script),
        "quay.io:5000",
        &AtomicBool::new(false),
        Duration::from_secs(5),
    )?;
    assert!(decode(&output)?.is_some());
    Ok(())
}

#[test]
fn failures_missing_helpers_suffix_validation_and_not_found_are_bounded_and_redacted() -> Result {
    let flag = AtomicBool::new(false);
    for suffix in [
        "",
        "../secret",
        "helper/secret",
        "helper secret",
        "$(secret)",
        "bad\0secret",
    ] {
        let error = get(suffix, "quay.io", &flag).expect_err("invalid suffix");
        assert_eq!(
            error.to_string(),
            "invalid registry credential helper suffix"
        );
    }
    let error = execute(
        Command::new("/nonexistent/credential-secret"),
        "quay.io",
        &flag,
        TIMEOUT,
    )
    .expect_err("missing executable");
    assert!(!format!("{error:?} {error}").contains("secret"));
    for script in [
        "printf secret >&2; printf secret; exit 1",
        "kill -TERM $$",
        "while :; do printf 'secret-secret-secret-secret'; done",
        "while :; do printf 'secret-secret-secret-secret' >&2; done",
    ] {
        let error = execute(shell(script), "quay.io", &flag, Duration::from_secs(2))
            .expect_err("failed/oversized");
        assert!(!format!("{error:?} {error}").contains("secret"));
        assert!(!error.to_string().contains("timed out"));
    }
    let output = execute(
        shell("printf '  credentials not found in native keychain\n'; printf secret >&2; exit 1"),
        "quay.io",
        &flag,
        TIMEOUT,
    )?;
    assert!(decode(&output)?.is_none());
    assert!(
        execute(
            shell("printf 'credentials not found in native keychain extra-secret'; exit 1"),
            "quay.io",
            &flag,
            TIMEOUT
        )
        .is_err()
    );
    Ok(())
}

#[test]
fn cancellation_and_deadline_kill_and_reap_children() -> Result {
    for cancel in [false, true] {
        let temp = tempfile::tempdir()?;
        let record = temp.path().join("pid");
        let mut command = shell(r#"printf '%s' "$$" > "$RECORD"; while :; do :; done"#);
        command.env("RECORD", &record);
        let flag = AtomicBool::new(false);
        let error = std::thread::scope(|scope| {
            if cancel {
                let record = &record;
                let flag = &flag;
                scope.spawn(move || {
                    let started = Instant::now();
                    while !std::fs::read_to_string(record)
                        .is_ok_and(|pid| pid.parse::<u32>().is_ok())
                        && started.elapsed() < Duration::from_secs(5)
                    {
                        std::thread::sleep(Duration::from_millis(5));
                    }
                    flag.store(true, Ordering::Relaxed);
                });
            }
            execute(
                command,
                "quay.io",
                &flag,
                if cancel {
                    TIMEOUT
                } else {
                    Duration::from_millis(100)
                },
            )
            .expect_err("cancelled/timed out")
        });
        assert_eq!(error.is_cancelled(), cancel);
        if !cancel {
            assert!(error.to_string().contains("timed out"));
        }
        let pid: u32 = std::fs::read_to_string(&record)?.parse()?;
        assert!(
            !std::path::Path::new(&format!("/proc/{pid}")).exists(),
            "child must be reaped before returning"
        );
    }
    let temp = tempfile::tempdir()?;
    let record = temp.path().join("never-spawned");
    let mut command = shell(r#"printf '%s' "$$" > "$RECORD""#);
    command.env("RECORD", &record);
    assert!(
        execute(command, "quay.io", &AtomicBool::new(true), TIMEOUT)
            .expect_err("already cancelled")
            .is_cancelled()
    );
    assert!(!record.exists());
    Ok(())
}

#[test]
fn inherited_output_cannot_extend_deadline_or_leave_reader_threads() -> Result {
    let temp = tempfile::tempdir()?;
    let record = temp.path().join("pid");
    // The descendant briefly inherits stdout/stderr after the direct child exits.
    // A blocking reader-thread design would defeat the caller's deadline here.
    let mut command = shell(r#"printf '%s' "$$" > "$RECORD"; sleep 1 & exit 0"#);
    command.env("RECORD", &record);
    let started = Instant::now();
    let error = execute(
        command,
        "quay.io",
        &AtomicBool::new(false),
        Duration::from_millis(100),
    )
    .expect_err("inherited descriptors");
    assert!(error.to_string().contains("timed out"));
    assert!(started.elapsed() < Duration::from_secs(1));
    let pid: u32 = std::fs::read_to_string(record)?.parse()?;
    assert!(!std::path::Path::new(&format!("/proc/{pid}")).exists());
    Ok(())
}

#[test]
fn repeated_helper_operations_close_every_parent_descriptor() -> Result {
    const MARKER: &str = "BPFMAN_TEST_HELPER_DESCRIPTOR_CHILD";
    if std::env::var_os(MARKER).is_none() {
        // Isolate descriptor accounting from the other parallel tests. This
        // re-exec runs only this contract, without process-wide env mutation.
        let output = Command::new(std::env::current_exe()?)
            .args(["--exact", "credentials::helper::tests::repeated_helper_operations_close_every_parent_descriptor", "--test-threads=1"])
            .env(MARKER, "1")
            .output()?;
        assert!(
            output.status.success(),
            "{} {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        return Ok(());
    }
    let count = || std::fs::read_dir("/proc/self/fd").map(|entries| entries.count());
    let before = count()?;
    for _ in 0..8 {
        for script in [
            "printf '{}'",
            "printf secret; exit 1",
            "while :; do :; done",
        ] {
            let _ = execute(
                shell(script),
                "quay.io",
                &AtomicBool::new(false),
                Duration::from_millis(50),
            );
            assert_eq!(count()?, before, "helper descriptor leak");
        }
    }
    Ok(())
}

#[test]
fn resolved_helper_credentials_survive_failed_abort_and_explicit_retry() -> Result {
    use crate::{Bytecode, Publication, Reference, publish};
    use std::collections::BTreeMap;

    for (bearer, identity) in [(false, false), (true, false), (true, true)] {
        let server = registry::Registry::new();
        let reference = Reference::parse(&server.reference)?;
        let mut command = shell(if identity {
            r#"IFS= read -r registry || [ -n "$registry" ] || exit 9
            printf '{"Username":"<token>","Secret":"retry-secret"}'"#
        } else {
            r#"IFS= read -r registry || [ -n "$registry" ] || exit 9
            printf '{"Username":"helper-user","Secret":"retry-secret"}'"#
        });
        command.env_clear();
        let credentials = decode(&execute(
            command,
            &reference.registry,
            &server.cancelled,
            TIMEOUT,
        )?)?
        .ok_or("credentials")?;
        let mut state = server.state.lock().expect("state");
        if identity {
            state.refresh_form = Some("grant_type=refresh_token&refresh_token=retry-secret&service=fixture&client_id=bpfman&scope=repository%3Atest%3Apull%2Cpush".into());
        } else {
            state.required_auth = Some(format!(
                "Basic {}",
                STANDARD.encode("helper-user:retry-secret")
            ));
        }
        state.bearer = bearer;
        state.cancel_on = Some("POST /v2/test/blobs/uploads/".into());
        state
            .faults
            .insert("DELETE /v2/test/blobs/uploads/*".into(), 503);
        drop(state);
        let mut elf = vec![0; 64];
        elf[..7].copy_from_slice(b"\x7fELF\x02\x01\x01");
        elf[16..18].copy_from_slice(&1_u16.to_le_bytes());
        elf[18..20].copy_from_slice(&247_u16.to_le_bytes());
        let publication = Publication::Image(Bytecode::new("amd64", "bytecode.o", elf)?);
        let mut error = publish(
            &reference,
            Some(&credentials),
            publication,
            &BTreeMap::new(),
            0,
            &server.cancelled,
        )
        .expect_err("cancelled with failed abort");
        assert!(error.is_cancelled());
        assert!(error.has_pending_cleanup());
        assert!(!format!("{error:?} {error}").contains("retry-secret"));
        assert!(error.retry_upload_cleanup().is_err());
        assert!(error.has_pending_cleanup());
        server.state.lock().expect("state").faults.clear();
        error.retry_upload_cleanup()?;
        assert!(!error.has_pending_cleanup());
        assert!(server.state.lock().expect("state").uploads.is_empty());
    }
    Ok(())
}
