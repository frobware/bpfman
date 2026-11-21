//! Ownership and rollback of a pair of named veth endpoints.

use std::{
    fs,
    net::Ipv4Addr,
    num::{NonZeroU32, NonZeroU64},
    os::unix::fs::MetadataExt,
    path::Path,
    process::{Command, Output},
    sync::atomic::{AtomicU64, Ordering},
};

use bpfman_shell_runtime::{HostNetworkPair, NetworkEndpoint};
use serde::Deserialize;

use crate::driver::{
    process::{self, Interrupt},
    slot_pool::Lease,
};

use super::config::Prefix;

static NAME_SEQUENCE: AtomicU64 = AtomicU64::new(0);

pub(super) struct EndpointNames {
    pub namespace: String,
    pub link: String,
}

pub(super) struct PairNames {
    pub a: EndpointNames,
    pub b: EndpointNames,
}

impl PairNames {
    pub(super) fn isolated() -> Self {
        let base = unique_base();
        let first = format!("{base}a");
        let second = format!("{base}b");
        Self {
            a: EndpointNames {
                namespace: first.clone(),
                link: first,
            },
            b: EndpointNames {
                namespace: second.clone(),
                link: second,
            },
        }
    }
}

pub(super) fn unique_base() -> String {
    let sequence = NAME_SEQUENCE
        .fetch_add(1, Ordering::Relaxed)
        .wrapping_add(1);
    let label = format!("{}:{sequence}", std::process::id());
    let hash = label.bytes().fold(0xcbf2_9ce4_8422_2325_u64, |hash, byte| {
        (hash ^ u64::from(byte)).wrapping_mul(0x0100_0000_01b3)
    });
    format!("B{:012x}N", hash & 0xffff_ffff_ffff)
}

pub(super) struct HostNames {
    pub namespace: String,
    pub host_link: String,
    pub peer_link: String,
}

pub(super) struct HostPair {
    pub names: HostNames,
    resources: Resources,
}

impl HostPair {
    pub(super) fn create(
        names: HostNames,
        lease: Option<Lease>,
        [host, peer]: [Prefix; 2],
        interrupt: &Interrupt,
    ) -> Result<(Self, HostNetworkPair), String> {
        let mut resources = Resources {
            created: Vec::new(),
            lease,
        };
        let observed = resources.setup(|resources| {
            let namespace = names.namespace.as_str();
            let host_link = names.host_link.as_str();
            let peer_link = names.peer_link.as_str();
            resources.create(
                &["netns", "add", namespace],
                Resource::Namespace(namespace.to_owned()),
                interrupt,
            )?;
            resources.create(
                &[
                    "link", "add", host_link, "type", "veth", "peer", "name", peer_link,
                ],
                Resource::HostLink(host_link.to_owned()),
                interrupt,
            )?;
            let host_prefix = host.to_string();
            let peer_prefix = peer.to_string();
            for step in [
                ["link", "set", peer_link, "netns", namespace].as_slice(),
                &["link", "set", host_link, "up"],
                &["addr", "add", &host_prefix, "dev", host_link],
                &["-n", namespace, "link", "set", peer_link, "up"],
                &[
                    "-n",
                    namespace,
                    "addr",
                    "add",
                    &peer_prefix,
                    "dev",
                    peer_link,
                ],
            ] {
                require_ip(step, interrupt)?;
            }
            let index_path = Path::new("/sys/class/net").join(host_link).join("ifindex");
            let index = fs::read_to_string(&index_path)
                .map_err(|error| format!("read {}: {error}", index_path.display()))?;
            let index = index.trim().parse::<NonZeroU32>().map_err(|error| {
                format!(
                    "invalid interface index in {}: {error}",
                    index_path.display()
                )
            })?;
            let namespace_id = namespace_inode(Path::new("/proc/self/ns/net"))?;
            interrupt.check()?;
            let observed = HostNetworkPair {
                namespace: namespace.to_owned(),
                host_link: host_link.to_owned(),
                peer_link: peer_link.to_owned(),
                host_address: host.address,
                peer_address: peer.address,
                host_interface_index: index.get(),
                host_namespace_id: namespace_id.get(),
            };
            Ok(observed)
        })?;
        Ok((Self { names, resources }, observed))
    }

    pub(super) fn release(&mut self) -> Result<(), String> {
        self.resources.release()
    }
}

pub(super) struct IsolatedPair {
    pub names: PairNames,
    resources: Resources,
}

impl IsolatedPair {
    pub(super) fn create(
        names: PairNames,
        lease: Lease,
        [a, b]: [Ipv4Addr; 2],
        interrupt: &Interrupt,
    ) -> Result<(Self, NetworkEndpoint, NetworkEndpoint), String> {
        let mut resources = Resources {
            created: Vec::new(),
            lease: Some(lease),
        };
        let (a, b) = resources.setup(|resources| {
            let ns_a = names.a.namespace.as_str();
            let ns_b = names.b.namespace.as_str();
            let link_a = names.a.link.as_str();
            let link_b = names.b.link.as_str();

            resources.create(
                &["netns", "add", ns_a],
                Resource::Namespace(ns_a.to_owned()),
                interrupt,
            )?;
            resources.create(
                &["netns", "add", ns_b],
                Resource::Namespace(ns_b.to_owned()),
                interrupt,
            )?;
            resources.create(
                &[
                    "link", "add", link_a, "type", "veth", "peer", "name", link_b,
                ],
                Resource::HostLink(link_a.to_owned()),
                interrupt,
            )?;
            let prefix_a = format!("{a}/30");
            let prefix_b = format!("{b}/30");
            for step in [
                ["link", "set", link_a, "netns", ns_a].as_slice(),
                &["link", "set", link_b, "netns", ns_b],
                &["-n", ns_a, "link", "set", link_a, "up"],
                &["-n", ns_a, "addr", "add", &prefix_a, "dev", link_a],
                &["-n", ns_b, "link", "set", link_b, "up"],
                &["-n", ns_b, "addr", "add", &prefix_b, "dev", link_b],
            ] {
                require_ip(step, interrupt)?;
            }
            let a = observe_endpoint(&names.a, a, interrupt)?;
            let b = observe_endpoint(&names.b, b, interrupt)?;
            interrupt.check()?;
            Ok((a, b))
        })?;
        Ok((Self { names, resources }, a, b))
    }

    pub(super) fn release(&mut self) -> Result<(), String> {
        self.resources.release()
    }
}

fn observe_endpoint(
    names: &EndpointNames,
    address: Ipv4Addr,
    interrupt: &Interrupt,
) -> Result<NetworkEndpoint, String> {
    let args = [
        "-j",
        "-n",
        &names.namespace,
        "link",
        "show",
        "dev",
        &names.link,
    ];
    let output = checked_ip(&args, interrupt)?;
    let index = decode_interface_index(&output.stdout)
        .map_err(|error| format!("link {} in {}: {error}", names.link, names.namespace))?;
    let namespace_id = namespace_inode(&Path::new("/run/netns").join(&names.namespace))?;
    Ok(NetworkEndpoint {
        namespace: names.namespace.clone(),
        interface: names.link.clone(),
        address,
        interface_index: index.get(),
        namespace_id: namespace_id.get(),
    })
}

fn decode_interface_index(stdout: &[u8]) -> Result<NonZeroU32, String> {
    #[derive(Deserialize)]
    struct Link {
        ifindex: NonZeroU32,
    }

    let [link]: [Link; 1] = serde_json::from_slice(stdout)
        .map_err(|error| format!("decode interface index: {error}"))?;
    Ok(link.ifindex)
}

fn namespace_inode(path: &Path) -> Result<NonZeroU64, String> {
    let metadata =
        fs::metadata(path).map_err(|error| format!("stat {}: {error}", path.display()))?;
    NonZeroU64::new(metadata.ino()).ok_or_else(|| format!("{} has zero inode", path.display()))
}

struct Resources {
    created: Vec<Resource>,
    lease: Option<Lease>,
}

impl Resources {
    fn setup<T>(
        &mut self,
        create: impl FnOnce(&mut Self) -> Result<T, String>,
    ) -> Result<T, String> {
        create(self).map_err(|primary| {
            // Failed setup does not certify that the slot's names are clean.
            // Hold the lock through cleanup, then leave acquisition provenance.
            let _lease = self.lease.take();
            match self.release() {
                Ok(()) => primary,
                Err(cleanup) => {
                    // The next claimant can inspect the recorded residue.
                    self.created.clear();
                    format!("{primary}\nnetwork setup rollback: {cleanup}")
                }
            }
        })
    }

    fn create(
        &mut self,
        args: &[&str],
        resource: Resource,
        interrupt: &Interrupt,
    ) -> Result<(), String> {
        // Check before starting a mutation. Once launched, wait for the result
        // so the rollback guard knows which resources were created.
        interrupt.check()?;
        require_ip(args, &Interrupt::unregistered())?;
        self.created.push(resource);
        Ok(())
    }

    fn release(&mut self) -> Result<(), String> {
        let interrupt = Interrupt::unregistered();
        let mut failures = Vec::new();
        let mut retained = Vec::new();
        for resource in self.created.drain(..).rev() {
            if let Err(error) = resource.remove(&interrupt) {
                retained.push(resource);
                failures.push(error);
            }
        }
        self.created.extend(retained.into_iter().rev());
        if !failures.is_empty() {
            return Err(failures.join("\n"));
        }
        if let Some(lease) = self.lease.take() {
            lease
                .release()
                .map_err(|error| format!("release network slot: {error}"))?;
        }
        Ok(())
    }
}

impl Drop for Resources {
    fn drop(&mut self) {
        super::super::report_cleanup("release network topology", self.release());
    }
}

enum Resource {
    Namespace(String),
    HostLink(String),
}

impl Resource {
    fn remove(&self, interrupt: &Interrupt) -> Result<(), String> {
        let (verb, name, parent) = match self {
            Self::Namespace(name) => ("netns", name, "/run/netns"),
            Self::HostLink(name) => ("link", name, "/sys/class/net"),
        };
        let path = Path::new(parent).join(name);
        remove_checked(
            || require_ip(&[verb, "del", name], interrupt),
            || match fs::symlink_metadata(&path) {
                Ok(_) => Ok(true),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
                Err(error) => Err(format!("verify {}: {error}", path.display())),
            },
        )
    }
}

fn remove_checked(
    remove: impl FnOnce() -> Result<(), String>,
    exists: impl FnOnce() -> Result<bool, String>,
) -> Result<(), String> {
    let Err(error) = remove() else {
        return Ok(());
    };
    match exists() {
        Ok(false) => Ok(()),
        Ok(true) => Err(error),
        Err(observation) => Err(format!("{error}\n{observation}")),
    }
}

pub(super) fn ip(args: &[&str], interrupt: &Interrupt) -> Result<Output, String> {
    process::capture(Command::new("ip").args(args), interrupt)
        .map_err(|error| format!("ip {}: {error}", args.join(" ")))
}

fn require_ip(args: &[&str], interrupt: &Interrupt) -> Result<(), String> {
    checked_ip(args, interrupt).map(|_| ())
}

fn checked_ip(args: &[&str], interrupt: &Interrupt) -> Result<Output, String> {
    let output = ip(args, interrupt)?;
    if output.status.success() {
        return Ok(output);
    }
    Err(format!(
        "ip {}: {}",
        args.join(" "),
        String::from_utf8_lossy(&output.stderr).trim_end()
    ))
}

#[cfg(test)]
mod tests {
    use super::{decode_interface_index, namespace_inode, remove_checked};

    #[test]
    fn endpoint_requires_exactly_one_link_with_a_nonzero_index() -> Result<(), String> {
        assert_eq!(
            decode_interface_index(br#"[{"ifindex":7,"ifname":"test"}]"#)?.get(),
            7
        );
        for bad in [
            "[]",
            "[{}]",
            r#"[{"ifindex":0}]"#,
            r#"[{"ifindex":4294967296}]"#,
            r#"[{"ifindex":7},{"ifindex":8}]"#,
            "not json",
        ] {
            assert!(decode_interface_index(bad.as_bytes()).is_err(), "{bad}");
        }
        Ok(())
    }

    #[test]
    fn namespace_lookup_reports_the_path_on_failure() -> std::io::Result<()> {
        let temp = tempfile::tempdir()?;
        let missing = temp.path().join("missing");
        let error = namespace_inode(&missing).expect_err("missing namespace");
        assert!(error.contains(&missing.display().to_string()));
        Ok(())
    }

    #[test]
    fn failed_delete_requires_proof_of_absence() {
        let deletion = || Err("delete failed".to_owned());
        assert_eq!(remove_checked(deletion, || Ok(false)), Ok(()));
        assert_eq!(
            remove_checked(deletion, || Ok(true)),
            Err("delete failed".to_owned())
        );
        assert_eq!(
            remove_checked(deletion, || Err("stat failed".to_owned())),
            Err("delete failed\nstat failed".to_owned())
        );
    }
}
