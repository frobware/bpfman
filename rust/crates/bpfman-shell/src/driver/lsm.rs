//! Private files and task markers for the LSM hook fixtures.

use std::{
    collections::BTreeMap,
    fs::{File, OpenOptions},
    io::{Read, Write},
    num::NonZeroU64,
    os::unix::fs::OpenOptionsExt,
    process::Command,
};

use bpfman_shell_runtime::{Argument, CommandResponse, LsmHandle, Value};
use tempfile::TempDir;

use super::process::{self, Interrupt};

#[derive(Default)]
pub(super) struct Probes {
    last_id: u64,
    probes: BTreeMap<NonZeroU64, Probe>,
}

struct Probe {
    directory: TempDir,
    marker: String,
}

impl Probes {
    pub(super) fn invoke(
        &mut self,
        arguments: &[Argument],
        interrupt: &Interrupt,
    ) -> Result<CommandResponse, String> {
        let (mode, arguments) = arguments.split_first().ok_or("lsm: subcommand required")?;
        let mode = super::argument(mode)?;
        match (mode.as_str(), arguments) {
            ("probe", []) => self.probe(),
            ("fire", [Argument::Value(Value::Lsm(probe)), count]) => {
                self.fire(probe, count, interrupt)
            }
            ("probe", _) => Err("lsm probe takes no arguments".to_owned()),
            ("fire", _) => Err("lsm fire requires a probe and count".to_owned()),
            _ => Err(format!("lsm: unknown subcommand {mode:?}")),
        }
    }

    fn probe(&mut self) -> Result<CommandResponse, String> {
        let id = self
            .last_id
            .checked_add(1)
            .and_then(NonZeroU64::new)
            .ok_or("exhausted LSM fixture identities")?;
        let mut random = [0_u8; 8];
        File::open("/dev/urandom")
            .and_then(|mut source| source.read_exact(&mut random))
            .map_err(|error| format!("lsm probe: read marker entropy: {error}"))?;
        let marker: String = random
            .into_iter()
            .map(|byte| char::from(b'a' + byte % 26))
            .collect();
        let directory = tempfile::Builder::new()
            .prefix("lsm-probe-")
            .tempdir()
            .map_err(|error| format!("lsm probe: create directory: {error}"))?;
        let target = directory.path().join("target");
        OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o644)
            .open(&target)
            .and_then(|mut file| file.write_all(b"lsm probe target\n"))
            .map_err(|error| format!("lsm probe: create target: {error}"))?;
        let handle = LsmHandle::new(
            id,
            marker.clone(),
            target.display().to_string(),
            directory.path().display().to_string(),
        );
        self.probes.insert(id, Probe { directory, marker });
        self.last_id = id.get();
        Ok(CommandResponse::success(Value::Lsm(handle)))
    }

    fn fire(
        &self,
        handle: &LsmHandle,
        count: &Argument,
        interrupt: &Interrupt,
    ) -> Result<CommandResponse, String> {
        let probe = self.probes.get(&handle.id()).ok_or("unknown LSM probe")?;
        let count = super::argument(count)?
            .parse::<usize>()
            .map_err(|error| format!("lsm fire: invalid count: {error}"))?;
        let executable = std::env::current_exe().map_err(|error| format!("lsm fire: {error}"))?;
        let mut command = Command::new(executable);
        command
            .env("BPFMAN_SHELL_MODE", "lsm-open-worker")
            .arg(&probe.marker)
            .arg(probe.directory.path().join("target"))
            .arg(count.to_string());
        let output = process::capture(&mut command, interrupt)?;
        Ok(process::response(output))
    }
}
