use std::io::{self, Stdin};

use nix::sys::signal::{SigSet, SigmaskHow, Signal as MaskedSignal, pthread_sigmask};
use rustix::{
    process::{Pid, Signal, getpgrp, kill_process_group},
    termios::{tcgetpgrp, tcsetpgrp},
};

pub(super) struct Foreground {
    stdin: Stdin,
    shell_group: Pid,
}

impl Foreground {
    pub(super) fn grant(child_group: Pid) -> Option<Self> {
        let stdin = io::stdin();
        let shell_group = getpgrp();
        if tcgetpgrp(&stdin).ok()? != shell_group {
            return None;
        }
        if let Err(error) = assign(&stdin, child_group) {
            super::super::report_cleanup("grant foreground terminal", Err::<(), _>(error));
            return None;
        }
        // A fast child may have attempted a read before the handoff.
        let resumed = kill_process_group(child_group, Signal::CONT).or_else(|error| {
            if error == rustix::io::Errno::SRCH {
                Ok(())
            } else {
                Err(error)
            }
        });
        super::super::report_cleanup("resume foreground process group", resumed);
        Some(Self { stdin, shell_group })
    }
}

impl Drop for Foreground {
    fn drop(&mut self) {
        super::super::report_cleanup(
            "reclaim foreground terminal",
            assign(&self.stdin, self.shell_group),
        );
    }
}

fn assign(stdin: &Stdin, foreground: Pid) -> io::Result<()> {
    // Reclaiming the terminal would ordinarily stop a background shell.
    // Block SIGTTOU for only this ioctl, on the calling thread.
    let mut blocked = SigSet::empty();
    blocked.add(MaskedSignal::SIGTTOU);
    let mut original = SigSet::empty();
    pthread_sigmask(SigmaskHow::SIG_BLOCK, Some(&blocked), Some(&mut original))?;
    let result = tcsetpgrp(stdin, foreground);
    pthread_sigmask(SigmaskHow::SIG_SETMASK, Some(&original), None)?;
    Ok(result?)
}
