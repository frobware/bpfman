//! TEST-NET-2 address leases shared with the Go shell.

use std::{
    net::Ipv4Addr,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use jiff::Timestamp;
use serde_json::{Map, Value as Json};

use crate::driver::{process::Interrupt, slot_pool};

const POOL: &str = "/run/bpfman-net-pool";
const SLOTS: u8 = 64;

pub(super) struct ClaimNames<'a> {
    pub namespace: &'a str,
    pub second_namespace: Option<&'a str>,
    pub root_link: &'a str,
}

pub(super) fn claim(
    names: ClaimNames<'_>,
    origin: String,
    interrupt: &Interrupt,
) -> Result<(slot_pool::Lease, [Ipv4Addr; 2]), String> {
    let start = Instant::now();
    loop {
        let candidates = slot_pool::candidates(Path::new(POOL), 1..=SLOTS)
            .map_err(|error| format!("net pool: {error}"))?;
        for slot in candidates {
            interrupt.check()?;
            let Some(mut lock) = slot
                .try_lock()
                .map_err(|error| format!("net pool: {error}"))?
            else {
                continue;
            };
            let previous = lock
                .previous()
                .map_err(|error| format!("net pool: {error}"))?;
            verify_clean(slot.index, &previous)?;
            let mut record = Map::from_iter([
                ("origin".into(), Json::String(origin)),
                ("ns_name".into(), Json::String(names.namespace.to_owned())),
                (
                    "link_a_name".into(),
                    Json::String(names.root_link.to_owned()),
                ),
                (
                    "acquired_at".into(),
                    Json::String(Timestamp::now().to_string()),
                ),
            ]);
            if let Some(namespace) = names.second_namespace {
                record.insert("ns_b_name".into(), Json::String(namespace.to_owned()));
            }
            let lease = lock
                .claim(record)
                .map_err(|error| format!("net pool: {error}"))?;
            let base = 4 * (slot.index - 1);
            return Ok((
                lease,
                [
                    Ipv4Addr::new(198, 51, 100, base + 1),
                    Ipv4Addr::new(198, 51, 100, base + 2),
                ],
            ));
        }
        if start.elapsed() >= Duration::from_secs(300) {
            return Err(format!("net pool: all {SLOTS} slots are leased"));
        }
        interrupt.pause(Duration::from_millis(25))?;
    }
}

fn verify_clean(slot: u8, previous: &Map<String, Json>) -> Result<(), String> {
    let get = |name| {
        previous
            .get(name)
            .and_then(Json::as_str)
            .unwrap_or_default()
    };
    if !get("released_at").is_empty() {
        return Ok(());
    }
    for (key, parent) in [
        ("link_a_name", "/sys/class/net"),
        ("ns_name", "/run/netns"),
        ("ns_b_name", "/run/netns"),
    ] {
        let name = get(key);
        // A provenance record is data. Only a single ordinary path name is valid.
        if name.is_empty() || name.contains('/') || matches!(name, "." | "..") {
            continue;
        }
        let path = PathBuf::from(parent).join(name);
        if path
            .try_exists()
            .map_err(|error| format!("net pool: {}: {error}", path.display()))?
        {
            return Err(format!(
                "net pool: slot {slot} still has {} from previous tenant {}",
                path.display(),
                get("origin")
            ));
        }
    }
    Ok(())
}
