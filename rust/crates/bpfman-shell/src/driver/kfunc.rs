//! Cross-process leases for the repository's kernel-function fixtures.

use std::{
    collections::BTreeMap,
    fs::{self, OpenOptions},
    io::Write,
    num::NonZeroU64,
    path::PathBuf,
};

use bpfman_shell_runtime::{Argument, CommandResponse, KfuncHandle, Value};
use jiff::Timestamp;
use serde_json::{Map, Value as Json};

use super::{process::Interrupt, slot_pool};

const SLOTS: u8 = 128;

pub(super) struct Pool {
    root: PathBuf,
    debugfs: PathBuf,
    last_lease: u64,
    active: BTreeMap<NonZeroU64, Lease>,
}

impl Default for Pool {
    fn default() -> Self {
        Self::new(
            PathBuf::from("/run/bpfman-kfunc-pool"),
            PathBuf::from("/sys/kernel/debug/bpfman_e2e"),
        )
    }
}

impl Pool {
    fn new(root: PathBuf, debugfs: PathBuf) -> Self {
        Self {
            root,
            debugfs,
            last_lease: 0,
            active: BTreeMap::new(),
        }
    }

    pub(super) fn invoke(
        &mut self,
        arguments: &[Argument],
        origin: String,
        interrupt: &Interrupt,
    ) -> Result<CommandResponse, String> {
        let (subcommand, rest) = arguments
            .split_first()
            .ok_or("kfunc: subcommand required")?;
        let subcommand = super::argument(subcommand)?;
        match (subcommand.as_str(), rest) {
            ("acquire", []) => self.acquire(origin),
            ("release", [Argument::Value(Value::Kfunc(handle))]) => self.release(handle),
            ("fire", [Argument::Value(Value::Kfunc(handle)), count]) => {
                self.fire(handle, count, interrupt)
            }
            ("acquire", _) => Err("kfunc acquire takes no arguments".to_owned()),
            ("release", _) => Err("kfunc release requires one kernel function handle".to_owned()),
            ("fire", _) => Err("kfunc fire requires a kernel function handle and count".to_owned()),
            _ => Err(format!("kfunc: unknown subcommand {subcommand:?}")),
        }
    }

    fn acquire(&mut self, origin: String) -> Result<CommandResponse, String> {
        let id = self
            .last_lease
            .checked_add(1)
            .and_then(NonZeroU64::new)
            .ok_or("exhausted kernel function lease identities")?;
        fs::metadata(&self.debugfs)
            .map_err(|error| format!("kfunc pool: {}: {error}", self.debugfs.display()))?;
        let slots = slot_pool::candidates(&self.root, 0..=SLOTS - 1)
            .map_err(|error| format!("kfunc pool: {}: {error}", self.root.display()))?;
        for slot in slots {
            let index = slot.index;
            let Some(lock) = slot
                .try_lock()
                .map_err(|error| format!("kfunc pool: lock {index}: {error}"))?
            else {
                continue;
            };
            let name = format!("bpfman_e2e_target_{index}");
            let trigger = self.debugfs.join(format!("trigger_{index:03}"));
            let count = self.debugfs.join(format!("count_{index:03}"));
            for path in [&trigger, &count] {
                fs::metadata(path)
                    .map_err(|error| format!("kfunc pool: {}: {error}", path.display()))?;
            }

            let lock = lock
                .claim(Map::from_iter([
                    ("origin".into(), Json::String(origin)),
                    ("slot".into(), Json::from(index)),
                    ("name".into(), Json::String(name.clone())),
                    (
                        "trigger".into(),
                        Json::String(trigger.display().to_string()),
                    ),
                    ("count".into(), Json::String(count.display().to_string())),
                    (
                        "acquired_at".into(),
                        Json::String(Timestamp::now().to_string()),
                    ),
                ]))
                .map_err(|error| format!("kfunc pool: save slot {index}: {error}"))?;
            let lease = Lease {
                lock,
                trigger: trigger.clone(),
            };
            self.last_lease = id.get();
            self.active.insert(id, lease);
            return Ok(CommandResponse::success(Value::Kfunc(KfuncHandle::new(
                id,
                index,
                name,
                trigger.display().to_string(),
                count.display().to_string(),
            ))));
        }
        Err(format!("kfunc pool: more than {SLOTS} slots in flight"))
    }

    fn release(&mut self, handle: &KfuncHandle) -> Result<CommandResponse, String> {
        if let Some(lease) = self.active.remove(&handle.id()) {
            lease
                .lock
                .release()
                .map_err(|error| format!("kfunc pool: release: {error}"))?;
        } else if handle.id().get() > self.last_lease {
            return Err("unknown kernel function lease".to_owned());
        }
        Ok(CommandResponse::success(Value::Absent))
    }

    fn fire(
        &self,
        handle: &KfuncHandle,
        count: &Argument,
        interrupt: &Interrupt,
    ) -> Result<CommandResponse, String> {
        let lease = self
            .active
            .get(&handle.id())
            .ok_or("kernel function handle has been released")?;
        let count = super::argument(count)?
            .parse::<u64>()
            .map_err(|error| format!("kfunc fire: count: {error}"))?;
        let mut trigger = OpenOptions::new()
            .write(true)
            .open(&lease.trigger)
            .map_err(|error| format!("kfunc fire: open {}: {error}", lease.trigger.display()))?;
        for _ in 0..count {
            interrupt.check()?;
            trigger.write_all(&[0]).map_err(|error| {
                format!("kfunc fire: write {}: {error}", lease.trigger.display())
            })?;
        }
        Ok(CommandResponse::success(Value::Absent))
    }
}

struct Lease {
    lock: slot_pool::Lease,
    trigger: PathBuf,
}

#[cfg(test)]
mod tests {
    use std::fs::File;

    use super::*;

    #[test]
    fn independent_registries_hold_distinct_slots_and_release_is_idempotent() -> Result<(), String>
    {
        let root = tempfile::tempdir().map_err(|error| error.to_string())?;
        let debugfs = root.path().join("debugfs");
        fs::create_dir(&debugfs).map_err(|error| error.to_string())?;
        for name in ["trigger_000", "count_000", "trigger_001", "count_001"] {
            File::create(debugfs.join(name)).map_err(|error| error.to_string())?;
        }
        let lock_dir = root.path().join("locks");
        let mut first = Pool::new(lock_dir.clone(), debugfs.clone());
        let mut second = Pool::new(lock_dir.clone(), debugfs.clone());
        let Value::Kfunc(function) = first.acquire("first".to_owned())?.primary else {
            return Err("acquire did not return a kernel function".to_owned());
        };
        second.acquire("second".to_owned())?;
        let provenance = |slot: &str| -> Result<Json, String> {
            let body = fs::read(lock_dir.join(slot)).map_err(|error| error.to_string())?;
            serde_json::from_slice(&body).map_err(|error| error.to_string())
        };
        assert_eq!(provenance("00.lock")?["origin"], "first");
        assert_eq!(provenance("01.lock")?["origin"], "second");

        let interrupt = Interrupt::register().map_err(|error| error.to_string())?;
        let count = Argument::Word("2".to_owned());
        first.fire(&function, &count, &interrupt)?;
        assert_eq!(
            fs::read(debugfs.join("trigger_000")).map_err(|error| error.to_string())?,
            [0, 0]
        );
        first.release(&function)?;
        first.release(&function)?;
        assert!(provenance("00.lock")?["released_at"].is_string());
        assert!(first.fire(&function, &count, &interrupt).is_err());
        Ok(())
    }
}
