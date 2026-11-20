//! Whole-program CLI contracts for the first Rust shell driver.

use std::{
    fs,
    io::{self, Write},
    os::unix::fs::PermissionsExt,
    path::Path,
    process::{Child, Command, Output, Stdio},
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
    stdin.write_all(b"defer touch cleaned\nexec sh -c 'trap \"sleep 0.05; touch cooperated; exit 0\" INT; echo $$ > started; sleep 30 & wait'\n")?;
    drop(stdin);
    wait_for_pid_file(&mut shell, &marker)?;
    let pid = process_id(shell.id())?;
    rustix::process::kill_process(pid, rustix::process::Signal::TERM)?;
    let output = shell.wait_with_output()?;
    assert!(!output.status.success(), "{output:?}");
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("interrupted by signal"),
        "{output:?}"
    );
    assert!(dir.path().join("cleaned").exists());
    assert!(dir.path().join("cooperated").exists());
    let worker = fs::read_to_string(&marker)?;
    assert!(!std::path::Path::new("/proc").join(worker.trim()).exists());
    Ok(())
}

#[test]
fn second_signal_exits_during_a_deferred_command() -> io::Result<()> {
    let dir = tempfile::tempdir()?;
    let mut shell = Command::new(env!("CARGO_BIN_EXE_bpfman-shell"))
        .current_dir(dir.path())
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    let mut stdin = shell
        .stdin
        .take()
        .ok_or_else(|| io::Error::other("stdin"))?;
    stdin.write_all(b"defer exec sh -c 'echo $$ > cleanup; exec sleep 5'\nexec sh -c 'echo $$ > main; exec sleep 5'\n")?;
    drop(stdin);
    let pid = process_id(shell.id())?;
    wait_for_pid_file(&mut shell, &dir.path().join("main"))?;
    rustix::process::kill_process(pid, rustix::process::Signal::INT)?;
    let cleanup = wait_for_pid_file(&mut shell, &dir.path().join("cleanup"))?;
    let start = Instant::now();
    rustix::process::kill_process(pid, rustix::process::Signal::TERM)?;
    let status = shell.wait();
    let _ = rustix::process::kill_process_group(cleanup, rustix::process::Signal::KILL);
    assert_eq!(status?.code(), Some(1));
    assert!(start.elapsed() < Duration::from_secs(4));
    Ok(())
}

fn process_id(raw: u32) -> io::Result<rustix::process::Pid> {
    i32::try_from(raw)
        .ok()
        .and_then(rustix::process::Pid::from_raw)
        .ok_or_else(|| io::Error::other("invalid PID"))
}

fn wait_for_pid_file(shell: &mut Child, marker: &Path) -> io::Result<rustix::process::Pid> {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Ok(text) = fs::read_to_string(marker)
            && let Ok(pid) = text.trim().parse()
        {
            return process_id(pid);
        }
        if Instant::now() >= deadline {
            let _ = shell.kill();
            let _ = shell.wait();
            return Err(io::Error::other(format!(
                "{} was never written",
                marker.display()
            )));
        }
        thread::sleep(Duration::from_millis(5));
    }
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
        String::from_utf8_lossy(&output.stderr).contains("[job] FAIL"),
        "{output:?}"
    );
    let pid = String::from_utf8_lossy(&output.stdout);
    assert!(!pid.trim().is_empty(), "{output:?}");
    assert!(!std::path::Path::new("/proc").join(pid.trim()).exists());
    Ok(())
}

#[test]
fn termination_and_reap_preserve_the_job_outcome() -> io::Result<()> {
    let output = run(
        &[],
        r#"
guard worker <- start sleep 30
let before <- jobs
assert contains $before.stdout running
kill --signal=sigstop $worker
let paused <- jobs
assert contains $paused.stdout killing
kill --signal=CONT $worker
kill --grace=0 $worker
let after <- jobs
assert contains $after.stdout "killed KILL"
reap
let remaining <- jobs
assert $remaining.stdout == ""
let result <- wait $worker
assert $result.killed
assert $result.signal == KILL
print done
"#,
    )?;
    assert!(output.status.success(), "{output:?}");
    assert_eq!(output.stdout, b"done\n");
    Ok(())
}

#[test]
fn deferred_kill_manages_the_job_after_an_error() -> io::Result<()> {
    let output = run(
        &[],
        "guard worker <- start sleep 30\ndefer kill $worker\nprint $worker.pid\nrequire false",
    )?;
    assert!(!output.status.success(), "{output:?}");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("[require] FAIL"), "{output:?}");
    assert!(!stderr.contains("[job] FAIL"), "{output:?}");
    let pid = String::from_utf8_lossy(&output.stdout);
    assert!(!pid.trim().is_empty(), "{output:?}");
    assert!(!std::path::Path::new("/proc").join(pid.trim()).exists());
    Ok(())
}

#[test]
fn fixture_workers_acknowledge_each_requested_wave() -> io::Result<()> {
    let output = run(
        &[],
        r#"
guard fixture <- tempdir wave
defer rm -r $fixture.path
let ready = "${fixture.path}/ready"
let signal_ack = "${fixture.path}/signal"
let unlink_ack = "${fixture.path}/unlink"
exec touch "${ready}.1" "${ready}.2"
guard signaller <- fire kill $ready $signal_ack --count=3 --waves 2
guard unlinker <- fire unlinkat $ready $unlink_ack --count 3 --waves=2
guard _ <- wait $signaller
guard _ <- wait $unlinker
assert path-exists "${signal_ack}.1"
assert path-exists "${signal_ack}.2"
assert path-exists "${unlink_ack}.1"
assert path-exists "${unlink_ack}.2"
"#,
    )?;
    assert!(output.status.success(), "{output:?}");
    Ok(())
}

#[test]
fn uprobe_driver_resolves_the_same_shell_elf_in_both_target_forms() -> io::Result<()> {
    let output = run(
        &[],
        r#"
guard complete <- uprobe target
guard attach <- uprobe-target
assert $complete.symbol_offset > 0
assert $complete.pid > 0
assert $complete.path == $attach.path
assert $complete.symbol == $attach.symbol
assert contains $complete.go_symbol fixturemode.GoUprobeTarget
uprobe fire 2
"#,
    )?;
    assert!(output.status.success(), "{output:?}");
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
fn symbols_keep_valid_imports_and_local_bindings_when_an_import_is_missing() -> io::Result<()> {
    let directory = tempfile::tempdir()?;
    let main = directory.path().join("main.bpfman");
    let library = directory.path().join("lib.bpfman");
    fs::write(
        &main,
        "import ./absent.bpfman\nimport ./lib.bpfman\nlet local = 1\n",
    )?;
    fs::write(&library, "def imported(x) {\n    return $x\n}\n")?;
    let result = Command::new(env!("CARGO_BIN_EXE_bpfman-shell"))
        .arg("--symbols")
        .arg(&main)
        .output()?;
    assert!(result.status.success(), "{result:?}");
    let document: serde_json::Value = serde_json::from_slice(&result.stdout)?;
    assert_eq!(document["version"], 1);
    assert_eq!(document["symbols"][0]["name"], "local");
    assert_eq!(document["symbols"][1]["name"], "imported");
    assert_eq!(
        document["symbols"][1]["def"]["file"],
        library.display().to_string()
    );
    assert_eq!(document["symbols"][1]["scope"]["end"]["line"], 3);
    assert_eq!(document["errors"][0]["line"], 1);
    assert_eq!(document["errors"][0]["col"], 8);
    assert!(
        document["errors"][0]["message"]
            .as_str()
            .is_some_and(|message| message.contains("absent.bpfman"))
    );
    Ok(())
}

#[test]
fn ast_mode_matches_the_go_tree_shape_and_preserves_group_extent() -> io::Result<()> {
    let result = run(&["--ast"], "let x = 1 == (2 + 3)\n")?;
    assert!(result.status.success(), "{result:?}");
    let output = String::from_utf8_lossy(&result.stdout);
    assert!(output.starts_with("Program {\n  Stmts: []syntax.Stmt (len = 1) {\n"));
    assert!(output.contains("          Span: 1:15-1:20\n"));
    assert!(output.contains("        Span: 1:9-1:21\n"));
    assert!(output.ends_with("  Span: 1:1-1:21\n}\n"));
    Ok(())
}

#[test]
fn formatter_preserves_import_and_comments_and_can_write_a_file() -> io::Result<()> {
    let source = "# intro\nimport  ./missing.bpfman\n\ndef helper(x){print  $x} # retained\n";
    let expected =
        "# intro\nimport ./missing.bpfman\n\ndef helper(x) {\n    print $x\n} # retained\n";
    let formatted = run(&["--fmt"], source)?;
    assert!(formatted.status.success(), "{formatted:?}");
    assert_eq!(formatted.stdout, expected.as_bytes());

    let directory = tempfile::tempdir()?;
    let file = directory.path().join("source.bpfman");
    fs::write(&file, source)?;
    let written = Command::new(env!("CARGO_BIN_EXE_bpfman-shell"))
        .args(["fmt", "-w"])
        .arg(&file)
        .output()?;
    assert!(written.status.success(), "{written:?}");
    assert!(written.stdout.is_empty());
    assert_eq!(fs::read_to_string(file)?, expected);
    Ok(())
}

#[test]
fn assertions_use_stderr_continue_and_require_stops_after_cleanup() -> io::Result<()> {
    let source = "let r = record { nested: record { n: 2 } }\nassert $r matches exhaustive { nested: matches { n: 2 } }\nassert false\nassert fail exec false\nprint continued\nrequire false\nprint unreachable";
    let output = run(&[], source)?;
    assert!(!output.status.success());
    assert_eq!(output.stdout, b"continued\n");
    let errors = String::from_utf8_lossy(&output.stderr);
    assert_eq!(errors.matches("[assert] FAIL").count(), 1);
    assert!(errors.contains("[require] FAIL"));
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

#[test]
fn definitions_and_direct_pure_calls_share_command_dispatch() -> io::Result<()> {
    let output = run(
        &[],
        "def z(name) { print $name }\ndef a() { print a }\nrange 2\nu64le 5\ndefs\nz hello\n",
    )?;
    assert!(output.status.success(), "{output:?}");
    assert_eq!(output.stdout, b"  a()\n  z(name)\nhello\n");
    Ok(())
}

#[test]
fn trace_toggles_apply_when_defers_fire() -> io::Result<()> {
    let output = run(
        &["-x"],
        "let name = \"cleanup\"\ntrace off\ndefer print $name\ntrace on\nprint before-exit\n",
    )?;
    assert!(output.status.success(), "{output:?}");
    assert_eq!(output.stdout, b"before-exit\ncleanup\n");
    assert_eq!(
        output.stderr,
        b"+ <stdin>:1: let name = cleanup\n+ <stdin>:2: trace off\n+ <stdin>:5: print before-exit\n+ <stdin>:3: defer fire: print cleanup\n"
    );
    Ok(())
}

#[test]
fn matches_reports_nested_failures_and_unclaimed_fields() -> io::Result<()> {
    let output = run(
        &[],
        "let doc = ('{\"left\":{\"keep\":2,\"extra\":3}}' |> jq .)\nassert $doc matches {\n  left: matches exhaustive {\n    keep: 1\n  }\n}\nprint still-running\n",
    )?;
    assert!(!output.status.success());
    assert_eq!(output.stdout, b"still-running\n");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("[assert] FAIL: matches: 2 mismatches"),
        "{stderr}"
    );
    assert!(
        stderr.contains("<stdin>:4:5: left.keep: expected \"1\", got \"2\""),
        "{stderr}"
    );
    assert!(
        stderr.contains("left.extra: present in value but unclaimed"),
        "{stderr}"
    );
    assert!(stderr.ends_with("1 assertion(s) failed\n"), "{stderr}");
    Ok(())
}

#[test]
fn command_status_assertion_observes_guard_failure_in_definition() -> io::Result<()> {
    let output = run(
        &[],
        "def rejects() {\n  defer print cleanup\n  guard _ <- exec false\n}\nassert fail rejects\nprint afterward\n",
    )?;
    assert!(output.status.success(), "{output:?}");
    assert_eq!(output.stdout, b"cleanup\nafterward\n");
    assert!(output.stderr.is_empty(), "{output:?}");
    Ok(())
}

#[test]
fn version_reports_build_information_without_opening_input() -> io::Result<()> {
    let output = Command::new(env!("CARGO_BIN_EXE_bpfman-shell"))
        .args([
            "-V",
            "-C",
            "/nonexistent/working/directory",
            "missing.bpfman",
        ])
        .output()?;
    assert!(output.status.success(), "{output:?}");
    let stdout = String::from_utf8_lossy(&output.stdout);
    for field in [
        "Version:",
        "Git commit:",
        "Rust version: rustc",
        "Platform:",
    ] {
        assert!(stdout.contains(field), "{stdout}");
    }
    assert!(output.stderr.is_empty(), "{output:?}");
    Ok(())
}
