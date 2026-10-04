//! Real CLI signals, synchronized by lock-wait telemetry rather than sleeps.
#![allow(clippy::expect_used)]

use bpfman_fs::{RuntimeDirectory, RuntimeLayout};
use bpfman_lock::AcquireOptions;
use std::{
    io::{BufRead, BufReader},
    process::{Child, Command, Stdio},
    sync::mpsc,
    time::{Duration, Instant},
};

struct Process(Child);

impl Drop for Process {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[test]
fn signals_cancel_startup_and_mutation_lock_waits_for_both_stores() {
    for backend in ["sqlite", "json"] {
        for signal in ["INT", "TERM"] {
            for existing in [false, true] {
                let temp = tempfile::tempdir().expect("runtime");
                let layout = RuntimeLayout::try_from(temp.path().join("runtime")).expect("layout");
                let trace = temp.path().join("trace.json");
                if existing {
                    let output = Command::new(env!("CARGO_BIN_EXE_bpfman"))
                        .args(["--store", backend, "--runtime-dir"])
                        .arg(layout.root())
                        .args(["program", "list", "-q"])
                        .env("RUST_LOG", "off")
                        .output()
                        .expect("initialize");
                    assert!(
                        output.status.success(),
                        "{}",
                        String::from_utf8_lossy(&output.stderr)
                    );
                }
                let runtime = RuntimeDirectory::open_or_create(layout.clone()).expect("runtime");

                runtime
                    .with_writer(
                        AcquireOptions {
                            timeout: Duration::from_secs(3),
                            cancelled: None,
                        },
                        |_| {
                            let mut command = Command::new(env!("CARGO_BIN_EXE_bpfman"));
                            command
                                .args(["--store", backend, "--runtime-dir"])
                                .arg(layout.root())
                                .arg("--trace-file")
                                .arg(&trace)
                                .args(["--lock-timeout", "0"])
                                .env("RUST_LOG", "bpfman_lock=trace,bpfman_runtime=debug")
                                .stdout(Stdio::piped())
                                .stderr(Stdio::piped());
                            if existing {
                                command.args(["program", "unload", "42"]);
                            } else {
                                command.args(["program", "list", "-q"]);
                            }
                            let mut process = Process(command.spawn().expect("CLI"));
                            let stderr = process.0.stderr.take().expect("stderr");
                            let (send, receive) = mpsc::channel();
                            let reader = std::thread::spawn(move || {
                                let mut output = String::new();
                                for line in BufReader::new(stderr).lines() {
                                    let line = line.expect("stderr line");
                                    if line.contains("\"name\":\"lock.wait\"")
                                        && line.contains("\"message\":\"new\"")
                                    {
                                        let _ = send.send(());
                                    }
                                    output.push_str(&line);
                                    output.push('\n');
                                }
                                output
                            });
                            receive
                                .recv_timeout(Duration::from_secs(5))
                                .expect("CLI waiting for lock with handlers installed");

                            assert!(
                                Command::new("kill")
                                    .arg(format!("-{signal}"))
                                    .arg(process.0.id().to_string())
                                    .status()
                                    .expect("signal")
                                    .success()
                            );
                            let deadline = Instant::now() + Duration::from_secs(3);
                            let status = loop {
                                if let Some(status) = process.0.try_wait().expect("wait") {
                                    break status;
                                }
                                assert!(
                                    Instant::now() < deadline,
                                    "cancellation did not stop lock wait"
                                );
                                std::thread::sleep(Duration::from_millis(5));
                            };
                            let stderr = reader.join().expect("reader");

                            assert_eq!(
                                status.code(),
                                Some(if signal == "INT" { 130 } else { 143 }),
                                "{stderr}"
                            );
                            assert!(stderr.contains("cancelled"), "{stderr}");
                            let events: serde_json::Value =
                                serde_json::from_slice(&std::fs::read(&trace).expect("trace"))
                                    .expect("flushed trace");
                            assert!(
                                !events
                                    .as_array()
                                    .expect("events")
                                    .iter()
                                    .any(|event| event["name"] == "lock.held")
                            );
                        },
                    )
                    .expect("writer");

                assert_eq!(
                    layout.database_path().exists(),
                    existing,
                    "cancelled startup must not create a store"
                );
            }
        }
    }
}
