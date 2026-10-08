//! Process policy stays in the CLI; the library receives only cancellation.

use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicI32, Ordering},
};
use std::thread::JoinHandle;

use bpfman_runtime::Cancellation;
use signal_hook::{
    SigId,
    consts::{SIGINT, SIGTERM},
    iterator::{Handle, Signals},
};

pub(super) struct Shutdown {
    cancellation: Cancellation,
    received: Arc<AtomicI32>,
    handle: Handle,
    thread: Option<JoinHandle<()>>,
    registrations: Vec<SigId>,
}

impl Shutdown {
    pub(super) fn install() -> std::io::Result<Self> {
        let armed = Arc::new(AtomicBool::new(false));
        let mut registrations = Vec::new();
        // Register the forced-exit check before arming it. Both signals share
        // the flag, so INT followed by TERM also forces an immediate exit.
        let setup = (|| {
            for signal in [SIGINT, SIGTERM] {
                registrations.push(signal_hook::flag::register_conditional_shutdown(
                    signal,
                    128 + signal,
                    armed.clone(),
                )?);
                registrations.push(signal_hook::flag::register(signal, armed.clone())?);
            }
            Signals::new([SIGINT, SIGTERM])
        })();
        let mut signals = match setup {
            Ok(signals) => signals,
            Err(error) => {
                for registration in registrations {
                    signal_hook::low_level::unregister(registration);
                }
                return Err(error);
            }
        };
        let handle = signals.handle();
        let cancellation = Cancellation::new();
        let token = cancellation.clone();
        let received = Arc::new(AtomicI32::new(0));
        let first = received.clone();
        let thread = std::thread::Builder::new()
            .name("bpfman-signals".into())
            .spawn(move || {
                for signal in signals.forever() {
                    first.store(signal, Ordering::Relaxed);
                    token.cancel();
                    tracing::debug!(
                        signal,
                        "shutdown requested; waiting for safe operation boundary"
                    );
                }
            });
        let thread = match thread {
            Ok(thread) => thread,
            Err(error) => {
                for registration in registrations {
                    signal_hook::low_level::unregister(registration);
                }
                return Err(error);
            }
        };

        Ok(Self {
            cancellation,
            received,
            handle,
            thread: Some(thread),
            registrations,
        })
    }

    pub(super) fn cancellation(&self) -> &Cancellation {
        &self.cancellation
    }

    pub(super) fn exit_code(&self, error: &crate::error::Error) -> std::process::ExitCode {
        match (error.is_cancelled(), self.received.load(Ordering::Relaxed)) {
            (true, SIGINT) => std::process::ExitCode::from(130),
            (true, SIGTERM) => std::process::ExitCode::from(143),
            _ => std::process::ExitCode::FAILURE,
        }
    }
}

impl Drop for Shutdown {
    fn drop(&mut self) {
        self.handle.close();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
        for registration in self.registrations.drain(..) {
            signal_hook::low_level::unregister(registration);
        }
    }
}
