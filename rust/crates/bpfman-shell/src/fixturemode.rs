//! Private fixture entry points in the same executable as the shell.

use std::{ffi::OsStr, fs};

pub(crate) mod uprobe;
mod waves;
pub(crate) use waves::Waves;

pub(crate) fn run(mode: &OsStr, arguments: &[String]) -> Result<(), String> {
    if let Some(kind) = mode.to_str().and_then(waves::Kind::from_mode) {
        return Waves::worker(kind, arguments);
    }
    if mode == "lsm-open-worker" {
        return lsm_open(arguments);
    }
    Err(format!("unknown BPFMAN_SHELL_MODE {mode:?}"))
}

fn lsm_open(arguments: &[String]) -> Result<(), String> {
    let [marker, path, count] = arguments else {
        return Err("lsm-open-worker: expected MARKER FILE COUNT".to_owned());
    };
    let count = count
        .parse::<usize>()
        .map_err(|error| format!("lsm-open-worker: count: {error}"))?;
    fs::write("/proc/thread-self/comm", marker)
        .map_err(|error| format!("set thread name {marker:?}: {error}"))?;
    for index in 0..count {
        fs::File::open(path)
            .map_err(|error| format!("open {path:?} ({}/{count}): {error}", index + 1))?;
    }
    Ok(())
}
