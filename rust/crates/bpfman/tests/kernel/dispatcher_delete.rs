//! Administrative record deletion preserves exact live traffic and native identities.
use super::{
    support::*,
    tc,
    xdp_delivery::{self, Network},
};
use bpfman_model::*;
use bpfman_runtime::{ActiveStore, Bpfman, PreparedProgram, XdpAttach};
use bpfman_store::*;

pub(super) fn exercise<S>(backend: S)
where
    S: DispatcherStore
        + CommitLoad
        + UnloadStore
        + LinkStore
        + XdpReplacementStore
        + TcStore
        + Copy,
    S::Reader: LinkReader + XdpDispatcherReader,
{
    for hook in [None, Some(TcDirection::Ingress), Some(TcDirection::Egress)] {
        let c = Context::new();
        let network = Network::new();
        let app = Bpfman::new(
            ActiveStore::open(backend, &c.layout, TIMEOUT).expect("store"),
            bpfman_kernel_aya::Kernel::default(),
            TIMEOUT,
        );
        let (object, spec) = match hook {
            None => (
                "xdp_delivery.bpf.o",
                ProgramSpec::Xdp("delivery_tail".try_into().expect("symbol")),
            ),
            Some(_) => (
                "tc_ingress.bpf.o",
                ProgramSpec::Tc("tc_ingress".try_into().expect("symbol")),
            ),
        };
        let loaded = app
            .load(
                PreparedProgram::new(
                    &bpfman_kernel_aya::Kernel::default(),
                    &fixture(object),
                    spec,
                    Default::default(),
                )
                .expect("prepare"),
            )
            .expect("load");
        let id = loaded.record.id;
        let link = match hook {
            None => app
                .attach_xdp(XdpAttach {
                    netns: network.namespace(),
                    mode: Default::default(),
                    program_id: id,
                    interface: "in0".parse().expect("interface"),
                    priority: 50,
                    proceed_on: Default::default(),
                    metadata: Default::default(),
                })
                .expect("XDP attach"),
            Some(hook) => app
                .attach_tc(tc::request(id, &network, hook))
                .expect("TC attach"),
        };
        let xdp = app.list_xdp_dispatchers().expect("XDP snapshots");
        let tc = app.list_tc_dispatchers().expect("TC snapshots");
        let (kind, nsid, ifindex) = match &link.details {
            LinkDetails::Xdp(d) => ("xdp", d.key.nsid, d.key.ifindex),
            LinkDetails::Tc(d) => (d.key.direction.dispatcher_type(), d.key.nsid, d.key.ifindex),
            _ => unreachable!(),
        };
        let before_filter = hook.map(|hook| {
            tc::tc(
                &network,
                &["filter", "show", "dev", tc::interface(hook), hook.as_str()],
            )
        });
        let before_files = artifacts(c.layout.root());
        let count = || match hook {
            None => xdp_delivery::count(&c, id, 1),
            Some(_) => tc::count(&c, id),
        };
        let traffic = || {
            if let Some(hook) = hook {
                tc::traffic(&network, 0, hook);
            } else {
                let captures: Vec<u32> =
                    serde_json::from_slice(&network.probe(&["packets", "64"])).expect("captures");
                assert_eq!(captures, [0, 0, 0]);
            }
        };
        let before = count();
        traffic();
        assert_eq!(count(), before + 3);
        let result = c.run(
            &rust(),
            &[
                "dispatcher",
                "delete",
                kind,
                &nsid.to_string(),
                &ifindex.to_string(),
            ],
            true,
        );
        assert!(result.stdout.is_empty());
        assert!(result.stderr.is_empty());
        assert_eq!(artifacts(c.layout.root()), before_files);
        assert!(c.layout.program_pin_path(id).exists());
        assert!(c.layout.bytecode_path(id).exists());
        assert!(
            c.layout
                .map_directory_path(id)
                .join(if hook.is_none() {
                    "delivery_stats"
                } else {
                    "tc_stats"
                })
                .exists()
        );
        let program = app.get(id).expect("program");
        assert!(program.links.is_empty());
        assert!(app.list_link_records().expect("links").is_empty());
        assert!(app.list_xdp_dispatchers().expect("XDP records").is_empty());
        assert!(app.list_tc_dispatchers().expect("TC records").is_empty());
        assert!(std::path::Path::new(&link.pin_path).exists());
        let kernel_id = match link.state {
            LinkState::Attached { kernel_id } => kernel_id,
            _ => unreachable!(),
        };
        assert_eq!(
            bpfman_kernel::LinkObservations::extension_link(
                &bpfman_kernel_aya::Kernel::default(),
                kernel_id
            )
            .expect("live extension")
            .program_id,
            id
        );
        if let Some(hook) = hook {
            assert_eq!(
                Some(tc::tc(
                    &network,
                    &["filter", "show", "dev", tc::interface(hook), hook.as_str()]
                )),
                before_filter
            );
        }
        let before = count();
        traffic();
        assert_eq!(
            count(),
            before + 3,
            "exact execution survives record deletion"
        );
        c.run(
            &rust(),
            &[
                "dispatcher",
                "delete",
                kind,
                &nsid.to_string(),
                &ifindex.to_string(),
            ],
            false,
        );

        // Re-publish captured first-revision intent solely to clean the fixture
        // through ordinary production detach/unload, after proving deletion persists.
        let app = Bpfman::new(
            ActiveStore::open(backend, &c.layout, TIMEOUT).expect("reopen"),
            bpfman_kernel_aya::Kernel::default(),
            TIMEOUT,
        );
        assert!(app.list_link_records().expect("reopened links").is_empty());
        let restored = c.writer(|w| match hook {
            None => {
                let old = &xdp[0].members()[0];
                backend
                    .commit_xdp(
                        w,
                        XdpCommit {
                            program_id: id,
                            details: &old.details,
                            extension_link_id: kernel_id,
                            outer_link_id: old.outer_link_id,
                            metadata: &old.member.metadata,
                            created_at: &old.member.created_at,
                        },
                    )
                    .expect("restore XDP intent")
            }
            Some(_) => {
                let old = &tc[0].members()[0];
                backend
                    .commit_tc(
                        w,
                        TcCommit {
                            program_id: id,
                            details: &old.details,
                            extension_link_id: kernel_id,
                            metadata: &old.member.metadata,
                            created_at: &old.member.created_at,
                        },
                    )
                    .expect("restore TC intent")
            }
        });
        match hook {
            None => {
                app.detach_xdp(restored.id).expect("detach XDP");
            }
            Some(_) => {
                app.detach_tc(restored.id).expect("detach TC");
            }
        }
        let before = count();
        if let Some(hook) = hook {
            tc::traffic(&network, 3, hook);
        } else {
            let captures: Vec<u32> =
                serde_json::from_slice(&network.probe(&["packets", "64"])).expect("captures");
            assert_eq!(captures, [0, 3, 0]);
        }
        assert_eq!(count(), before, "restored cleanup stops execution");
        assert_eq!(app.unload(id).expect("unload").unresolved(), 0);
        c.absent(id);
        c.no_artifacts();
    }
}

fn artifacts(root: &std::path::Path) -> Vec<std::path::PathBuf> {
    fn walk(path: &std::path::Path, entries: &mut Vec<std::path::PathBuf>) {
        for entry in std::fs::read_dir(path).expect("owned artifacts") {
            let entry = entry.expect("entry");
            let path = entry.path();
            entries.push(path.clone());
            if entry.file_type().expect("type").is_dir() {
                walk(&path, entries);
            }
        }
    }
    let mut entries = Vec::new();
    for collection in ["fs", "tc", "bytecode"] {
        let path = root.join(collection);
        if path.exists() {
            walk(&path, &mut entries);
        }
    }
    entries.sort();
    entries
}
