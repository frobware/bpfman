//! Deterministic file-gated stimulus workers.

use std::{
    fs::{self, OpenOptions},
    num::NonZeroUsize,
    os::unix::fs::OpenOptionsExt,
    path::{Path, PathBuf},
    sync::{Arc, atomic::AtomicBool},
    thread,
    time::Duration,
};

use rustix::{
    fs::{AtFlags, CWD, unlinkat},
    process::{Signal, getpid, kill_process},
};

#[derive(Clone, Copy)]
pub(crate) enum Kind {
    Kill,
    UnlinkAt,
    Uprobe,
}

impl Kind {
    pub(crate) const fn mode(self) -> &'static str {
        match self {
            Self::Kill => "kill-fire-worker",
            Self::UnlinkAt => "unlinkat-fire-worker",
            Self::Uprobe => "uprobe-fire-worker",
        }
    }

    pub(crate) const fn publishes_binary(self) -> bool {
        matches!(self, Self::Uprobe)
    }

    pub(super) fn from_mode(mode: &str) -> Option<Self> {
        match mode {
            "kill-fire-worker" => Some(Self::Kill),
            "unlinkat-fire-worker" => Some(Self::UnlinkAt),
            "uprobe-fire-worker" => Some(Self::Uprobe),
            _ => None,
        }
    }

    fn from_name(name: &str) -> Option<Self> {
        match name {
            "kill" => Some(Self::Kill),
            "unlinkat" => Some(Self::UnlinkAt),
            "uprobe" => Some(Self::Uprobe),
            _ => None,
        }
    }

    fn prepare(self, sentinel: &Path) -> Result<(), String> {
        match self {
            Self::Kill => {
                signal_hook::flag::register(
                    signal_hook::consts::SIGUSR1,
                    Arc::new(AtomicBool::new(false)),
                )
                .map_err(|error| error.to_string())?;
            }
            Self::UnlinkAt => {
                let directory = sentinel.parent().ok_or("sentinel path has no parent")?;
                std::env::set_current_dir(directory)
                    .map_err(|error| format!("chdir {}: {error}", directory.display()))?;
            }
            Self::Uprobe => {}
        }
        Ok(())
    }

    fn event(self) -> Result<(), String> {
        match self {
            Self::Kill => kill_process(getpid(), Signal::USR1).map_err(|error| error.to_string()),
            Self::UnlinkAt => {
                drop(
                    OpenOptions::new()
                        .create(true)
                        .truncate(true)
                        .write(true)
                        .mode(0o644)
                        .open("unlinkat-target")
                        .map_err(|error| error.to_string())?,
                );
                unlink_fixture_target()
            }
            Self::Uprobe => {
                super::uprobe::fire_once();
                Ok(())
            }
        }
    }
}

#[expect(
    clippy::disallowed_methods,
    reason = "the fixture must trigger unlinkat on its own constant file"
)]
fn unlink_fixture_target() -> Result<(), String> {
    unlinkat(CWD, "unlinkat-target", AtFlags::empty()).map_err(|error| error.to_string())
}

pub(crate) struct Waves {
    sentinel: String,
    acknowledgement: String,
    count: usize,
    waves: NonZeroUsize,
}

impl Waves {
    pub(crate) fn builtin(arguments: &[String]) -> Result<(Kind, Self), String> {
        let mut positional = Vec::new();
        let mut count = None;
        let mut waves = "1";
        let mut words = arguments.iter().map(String::as_str);
        while let Some(word) = words.next() {
            let (flag, inline) = word
                .split_once('=')
                .map_or((word, None), |(a, b)| (a, Some(b)));
            match flag {
                "--count" => {
                    count = Some(
                        inline
                            .or_else(|| words.next())
                            .ok_or("--count requires a value")?,
                    )
                }
                "--waves" => {
                    waves = inline
                        .or_else(|| words.next())
                        .ok_or("--waves requires a value")?
                }
                flag if flag.starts_with("--") => {
                    return Err(format!("fire: unknown flag {flag:?}"));
                }
                _ => positional.push(word),
            }
        }
        let [kind, sentinel, acknowledgement] = positional.as_slice() else {
            return Err("fire expects KIND SENTINEL ACK --count=N [--waves=N]".to_owned());
        };
        let kind = Kind::from_name(kind).ok_or_else(|| format!("fire: unknown kind {kind:?}"))?;
        let count = count.ok_or("fire: --count is required")?;
        Ok((kind, Self::new(sentinel, acknowledgement, count, waves)?))
    }

    fn new(
        sentinel: &str,
        acknowledgement: &str,
        count: &str,
        waves: &str,
    ) -> Result<Self, String> {
        let count = count
            .parse()
            .map_err(|error| format!("invalid event count {count:?}: {error}"))?;
        let waves = waves
            .parse()
            .map_err(|error| format!("invalid wave count {waves:?}: {error}"))?;
        Ok(Self {
            sentinel: sentinel.to_owned(),
            acknowledgement: acknowledgement.to_owned(),
            count,
            waves,
        })
    }

    pub(crate) fn argv(self) -> [String; 4] {
        [
            self.sentinel,
            self.acknowledgement,
            self.count.to_string(),
            self.waves.to_string(),
        ]
    }

    pub(super) fn worker(kind: Kind, arguments: &[String]) -> Result<(), String> {
        let [sentinel, acknowledgement, count, waves] = arguments else {
            return Err(format!(
                "{}: expected SENTINEL ACK COUNT WAVES",
                kind.mode()
            ));
        };
        let script = Self::new(sentinel, acknowledgement, count, waves)?;
        let sentinel = std::path::absolute(&script.sentinel).map_err(|error| error.to_string())?;
        let acknowledgement =
            std::path::absolute(&script.acknowledgement).map_err(|error| error.to_string())?;
        kind.prepare(&sentinel)?;
        for wave in 1..=script.waves.get() {
            let ready = wave_path(&sentinel, wave);
            loop {
                match fs::metadata(&ready) {
                    Ok(_) => break,
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                        thread::sleep(Duration::from_millis(10))
                    }
                    Err(error) => return Err(format!("{}: {error}", ready.display())),
                }
            }
            for index in 0..script.count {
                kind.event().map_err(|error| {
                    format!("{}: wave {wave}, event {}: {error}", kind.mode(), index + 1)
                })?;
            }
            let complete = wave_path(&acknowledgement, wave);
            OpenOptions::new()
                .create(true)
                .truncate(true)
                .write(true)
                .mode(0o644)
                .open(&complete)
                .map_err(|error| {
                    format!("create acknowledgement {}: {error}", complete.display())
                })?;
        }
        Ok(())
    }
}

fn wave_path(prefix: &Path, wave: usize) -> PathBuf {
    let mut name = prefix.as_os_str().to_owned();
    name.push(format!(".{wave}"));
    name.into()
}
