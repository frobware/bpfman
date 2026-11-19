//! Whole-program CLI contracts for the first Rust shell driver.

use std::{
    fs,
    io::{self, Write},
    os::unix::fs::PermissionsExt,
    process::{Command, Output, Stdio},
    thread,
    time::{Duration, Instant},
};

fn run(args: &[&str], source: &str) -> io::Result<Output> {
    run_command(
        Command::new(env!("CARGO_BIN_EXE_bpfman-shell")).args(args),
        source,
    )
}

fn run_command(command: &mut Command, source: &str) -> io::Result<Output> {
    let mut child = command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let mut stdin = child
        .stdin
        .take()
        .ok_or_else(|| io::Error::other("missing stdin"))?;
    stdin.write_all(source.as_bytes())?;
    drop(stdin);
    child.wait_with_output()
}

#[test]
fn interrupt_cancels_foreground_process_and_drains_defers() -> io::Result<()> {
    let dir = tempfile::tempdir()?;
    let marker = dir.path().join("started");
    let mut shell = Command::new(env!("CARGO_BIN_EXE_bpfman-shell"))
        .current_dir(dir.path())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let mut stdin = shell
        .stdin
        .take()
        .ok_or_else(|| io::Error::other("stdin"))?;
    stdin.write_all(b"defer touch cleaned\nexec sh -c 'echo $$ > started; sleep 30 & wait'\n")?;
    drop(stdin);
    let deadline = Instant::now() + Duration::from_secs(5);
    while fs::read_to_string(&marker).map_or(true, |text| text.trim().is_empty()) {
        if Instant::now() >= deadline {
            shell.kill()?;
            let output = shell.wait_with_output()?;
            return Err(io::Error::other(format!("child never started: {output:?}")));
        }
        thread::sleep(Duration::from_millis(5));
    }
    let pid = i32::try_from(shell.id())
        .ok()
        .and_then(rustix::process::Pid::from_raw)
        .ok_or_else(|| io::Error::other("invalid shell PID"))?;
    rustix::process::kill_process(pid, rustix::process::Signal::TERM)?;
    let output = shell.wait_with_output()?;
    assert!(!output.status.success(), "{output:?}");
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("interrupted by signal"),
        "{output:?}"
    );
    assert!(dir.path().join("cleaned").exists());
    let worker = fs::read_to_string(&marker)?;
    assert!(!std::path::Path::new("/proc").join(worker.trim()).exists());
    Ok(())
}

#[test]
fn jobs_keep_an_opaque_handle_and_cache_the_observed_exit() -> io::Result<()> {
    let output = run(
        &[],
        r#"
guard worker <- start sh -c 'printf output; printf diagnostic >&2; exit 7'
assert $worker.pid > 0
assert $worker.target_binary == sh
let first <- wait $worker
let again <- wait $worker
assert $first.exit_code == 7
assert $again.exit_code == 7
print $first.stdout $again.stderr
"#,
    )?;
    assert!(output.status.success(), "{output:?}");
    assert_eq!(output.stdout, b"output diagnostic\n");
    Ok(())
}

#[test]
fn script_exit_cleans_up_an_unmanaged_background_job() -> io::Result<()> {
    let output = run(
        &["--no-check"],
        "guard worker <- start sleep 30\nprint $worker.pid\n",
    )?;
    assert!(!output.status.success(), "{output:?}");
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("unmanaged job"),
        "{output:?}"
    );
    let pid = String::from_utf8_lossy(&output.stdout);
    assert!(!pid.trim().is_empty(), "{output:?}");
    assert!(!std::path::Path::new("/proc").join(pid.trim()).exists());
    Ok(())
}

#[test]
fn embedded_jq_preserves_precision_result_stream_and_argument_origin() -> io::Result<()> {
    let output = run(
        &[],
        r#"
let n = '100000000000000000000' |> jq '. * .'
let absent = (jq empty 'null')
let empty_thread = 'null' |> jq empty
let present_null = 'null' |> jq .
let present_list = 'null' |> jq 'null, true'
let source = '1]'
let typed_string = (jq . $source)
let keys = '{"z":1,"a":2}' |> jq tojson
let captured = ('"aab"' |> jq 'capture("(?<key>a+)").key')
assert missing $absent
assert $empty_thread.ok
assert null $present_null
assert present $present_list
print $n $typed_string $keys $captured
jq "." "1"
"#,
    )?;
    assert!(output.status.success(), "{output:?}");
    assert_eq!(
        output.stdout,
        b"10000000000000000000000000000000000000000 1] {\"a\":2,\"z\":1} aa\n"
    );
    Ok(())
}

#[test]
fn embedded_jq_rejects_trailing_json_in_both_decoders() -> io::Result<()> {
    for (source, message) in [
        ("let result = '1]' |> jq .", "input is not valid JSON"),
        (
            "let result = '\"1 2\"' |> jq fromjson",
            "fromjson cannot be applied",
        ),
        ("let result = 'null' |> jq '.['", "parse filter"),
    ] {
        let output = run(&[], source)?;
        assert!(!output.status.success(), "{output:?}");
        assert!(
            String::from_utf8_lossy(&output.stderr).contains(message),
            "{output:?}"
        );
    }
    Ok(())
}

#[test]
fn threaded_calls_keep_prior_observations_across_definition_frames() -> io::Result<()> {
    let output = run(
        &[],
        r#"
def decorate(prefix: string input: string) {
    defer exec true
    print visited
    let echo = $input |> exec printf '%s'
    return "${prefix}${echo.stdout}"
}
let decorated = ("a" |> decorate "first-")
let pair = [($decorated |> decorate "second-") ("b" |> decorate "third-")]
print $pair[0] $pair[1]
"#,
    )?;
    assert!(output.status.success(), "{output:?}");
    assert_eq!(
        output.stdout,
        b"visited\nvisited\nvisited\nsecond-first-a third-b\n"
    );
    Ok(())
}

#[test]
fn typed_bpfman_uses_the_selected_binary_and_round_trips_ids() -> io::Result<()> {
    let dir = tempfile::tempdir()?;
    let binary = dir.path().join("selected-bpfman");
    fs::write(
        &binary,
        concat!(
            "#!/bin/sh\n",
            "if test \"$*\" = 'program load file dummy.o -o json'; then\n",
            "  printf '%s' '{\"programs\":[{\"record\":{\"program_id\":7}}]}'\n",
            "elif test \"$*\" = 'program get 7 -o json'; then\n",
            "  printf '%s' '{\"record\":{\"program_id\":7},\"big\":18446744073709551615}'\n",
            "else exit 71; fi\n",
        ),
    )?;
    fs::set_permissions(&binary, fs::Permissions::from_mode(0o755))?;
    let mut command = Command::new(env!("CARGO_BIN_EXE_bpfman-shell"));
    command.env("BPFMAN_BIN", &binary);
    let output = run_command(
        &mut command,
        "guard loaded <- bpfman program load file dummy.o\nguard p <- bpfman program get $loaded.programs[0]\nguard q <- bpfman program get $p\nprint $q.record.program_id $q.big",
    )?;
    assert!(output.status.success(), "{output:?}");
    assert_eq!(output.stdout, b"7 18446744073709551615\n");
    Ok(())
}

#[test]
fn checked_stdin_refuses_before_output_and_unchecked_runs_prior_statements() -> io::Result<()> {
    let source = "print before\nprint $missing\n";
    let checked = run(&[], source)?;
    assert!(!checked.status.success());
    assert!(checked.stdout.is_empty());
    assert!(String::from_utf8_lossy(&checked.stderr).contains("undefined variable"));
    let unchecked = run(&["--no-check"], source)?;
    assert!(!unchecked.status.success());
    assert_eq!(unchecked.stdout, b"before\n");
    Ok(())
}

#[test]
fn inspection_never_dispatches_and_execution_drives_external_commands() -> io::Result<()> {
    let source = "print before\nexec printf '%s\\n' after\n";
    let checked = run(&["--check"], source)?;
    assert!(checked.status.success(), "{checked:?}");
    assert!(checked.stdout.is_empty());
    let lowered = run(&["--lowered"], source)?;
    assert!(lowered.status.success(), "{lowered:?}");
    assert!(String::from_utf8_lossy(&lowered.stdout).contains("DispatchCommand argv=t1"));
    let executed = run(&["-"], source)?;
    assert!(executed.status.success(), "{executed:?}");
    assert_eq!(executed.stdout, b"before\nafter\n");
    Ok(())
}

#[test]
fn assertions_use_stderr_continue_and_require_stops_after_cleanup() -> io::Result<()> {
    let source = "let r = record { nested: record { n: 2 } }\nassert $r matches exhaustive { nested: matches { n: 2 } }\nassert false\nassert fail exec false\nprint continued\nrequire false\nprint unreachable";
    let output = run(&[], source)?;
    assert!(!output.status.success());
    assert_eq!(output.stdout, b"continued\n");
    let errors = String::from_utf8_lossy(&output.stderr);
    assert_eq!(errors.matches("assertion failed").count(), 1);
    assert!(errors.contains("require failed"));
    Ok(())
}

#[test]
fn fixture_files_and_external_adapters_have_distinct_lifetimes() -> io::Result<()> {
    let dir = tempfile::tempdir()?;
    let mut command = Command::new(env!("CARGO_BIN_EXE_bpfman-shell"));
    command.env("TMPDIR", dir.path());
    let output = run_command(
        &mut command,
        concat!(
            "guard dir <- tempdir fixture\n",
            "defer rm -r $dir.path\n",
            "guard path <- file temp \"contents\"\n",
            "defer rm $path\n",
            "exec cat $path\n",
            "let payload = \"adapter\"\n",
            "let seen <- exec sh -c 'test -f \"$1\" && printf %s \"$1\"' sh file:$payload\n",
            "assert $seen.ok\n",
            "exec test ! -e $seen.stdout\n",
        ),
    )?;
    assert!(output.status.success(), "{output:?}");
    assert_eq!(output.stdout, b"contents");
    assert_eq!(dir.path().read_dir()?.count(), 0);
    Ok(())
}
