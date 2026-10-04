//! Subprocess isolation keeps process-wide signal policy out of other tests.
#![allow(clippy::expect_used)]

use std::{
    io::{BufRead, BufReader, Write},
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
#[ignore = "subprocess fixture; invoked by second_signal_forces_exit"]
fn signal_child() {
    if std::env::var_os("BPFMAN_SIGNAL_TEST_CHILD").is_none() {
        return;
    }
    let shutdown = super::Shutdown::install().expect("handlers");
    println!("handlers ready");
    std::io::stdout().flush().expect("ready");
    let deadline = Instant::now() + Duration::from_secs(5);
    while !shutdown.cancellation().is_cancelled() {
        assert!(Instant::now() < deadline, "no first signal");
        std::thread::sleep(Duration::from_millis(1));
    }
    println!("cancellation received; simulating an in-flight effect");
    std::io::stdout().flush().expect("cancelled");
    // The second signal must exit even while the operation makes no progress.
    std::thread::sleep(Duration::from_secs(10));
}

#[test]
fn second_signal_forces_exit() {
    for (first, second, code) in [
        ("INT", "INT", 130),
        ("TERM", "TERM", 143),
        ("INT", "TERM", 143),
    ] {
        let mut process = Process(
            Command::new(std::env::current_exe().expect("test binary"))
                .args([
                    "--ignored",
                    "--exact",
                    "signals::tests::signal_child",
                    "--nocapture",
                ])
                .env("BPFMAN_SIGNAL_TEST_CHILD", "1")
                .stdout(Stdio::piped())
                .spawn()
                .expect("child"),
        );
        let stdout = process.0.stdout.take().expect("stdout");
        let (send, receive) = mpsc::channel();
        let reader = std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                if send.send(line.expect("line")).is_err() {
                    break;
                }
            }
        });
        let await_line = |text: &str| {
            loop {
                if receive
                    .recv_timeout(Duration::from_secs(5))
                    .expect("child readiness")
                    .contains(text)
                {
                    break;
                }
            }
        };
        let signal = |name: &str| {
            assert!(
                Command::new("kill")
                    .arg(format!("-{name}"))
                    .arg(process.0.id().to_string())
                    .status()
                    .expect("signal")
                    .success()
            );
        };
        await_line("handlers ready");
        signal(first);
        await_line("cancellation received");
        signal(second);

        let deadline = Instant::now() + Duration::from_secs(3);
        let status = loop {
            if let Some(status) = process.0.try_wait().expect("wait") {
                break status;
            }
            assert!(
                Instant::now() < deadline,
                "second signal did not force exit"
            );
            std::thread::sleep(Duration::from_millis(5));
        };
        assert_eq!(status.code(), Some(code));
        reader.join().expect("reader");
    }
}
