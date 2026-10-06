//! Kernel/FS replacement capabilities with real pins, targets, and both stores.
//! Runtime orchestration is deliberately not exercised by this adapter contract.
use super::{support::*, xdp_attach::Interface};
use bpfman_fs::*;
use bpfman_kernel::{XdpLifecycle, XdpReplacement};
use bpfman_model::*;
use bpfman_runtime::{ActiveStore, Bpfman, PreparedProgram, XdpAttach};
use bpfman_store::*;
use std::{
    num::NonZeroU32,
    sync::{Arc, Mutex},
};

#[derive(Default)]
struct Faults {
    reject_update: bool,
    fail_after_update: bool,
    fail_info: bool,
}

#[derive(Default)]
struct Probe(Arc<Mutex<Faults>>);

struct Outer {
    fd: bpfman_kernel_aya::AyaOuter,
    faults: Arc<Mutex<Faults>>,
}

fn injected<T>() -> KernelResult<T> {
    Err(std::io::Error::other("injected outer-link boundary failure").into())
}

impl OuterLink for Outer {
    fn info(&self) -> KernelResult<OuterInfo> {
        if std::mem::take(&mut self.faults.lock().expect("faults").fail_info) {
            return injected();
        }
        self.fd.info()
    }

    fn detach(&self) -> KernelResult<()> {
        self.fd.detach()
    }

    fn pin(&self, target: PinTarget<'_>) -> KernelResult<()> {
        self.fd.pin(target)
    }
}

impl ProgramInspection for Probe {
    fn program_at(&self, source: PinSource<'_>) -> KernelResult<PinnedProgram> {
        bpfman_kernel_aya::Kernel.program_at(source)
    }

    fn map_at(&self, source: PinSource<'_>) -> KernelResult<u32> {
        bpfman_kernel_aya::Kernel.map_at(source)
    }
}

impl LinkInspection for Probe {
    fn link_at(&self, source: PinSource<'_>) -> KernelResult<KernelLink> {
        bpfman_kernel_aya::Kernel.link_at(source)
    }
}

impl XdpKernel for Probe {
    type Extension = bpfman_kernel_aya::AyaExtension;
    type Outer = Outer;

    type Namespace = bpfman_kernel_aya::XdpNamespace;

    fn interface(
        &self,
        interface: &InterfaceName,
        netns: &bpfman_model::NetworkNamespace,
    ) -> KernelResult<(XdpKey, Self::Namespace)> {
        bpfman_kernel_aya::Kernel.interface(interface, netns)
    }

    fn validate_namespace(&self, namespace: &Self::Namespace) -> KernelResult<()> {
        bpfman_kernel_aya::Kernel.validate_namespace(namespace)
    }

    fn extension_at(
        &self,
        source: PinSource<'_>,
    ) -> KernelResult<(PinnedProgram, Self::Extension)> {
        bpfman_kernel_aya::Kernel.extension_at(source)
    }

    fn outer_at(&self, source: PinSource<'_>) -> KernelResult<Outer> {
        Ok(Outer {
            fd: bpfman_kernel_aya::Kernel.outer_at(source)?,
            faults: self.0.clone(),
        })
    }

    fn outer_by_id(&self, id: NonZeroU32) -> KernelResult<Option<Outer>> {
        Ok(bpfman_kernel_aya::Kernel.outer_by_id(id)?.map(|fd| Outer {
            fd,
            faults: self.0.clone(),
        }))
    }

    fn attach_outer(
        &self,
        dispatcher: &bpfman_kernel_aya::Dispatcher,
        key: XdpKey,
        namespace: &Self::Namespace,
        mode: bpfman_model::XdpMode,
    ) -> KernelResult<Outer> {
        Ok(Outer {
            fd: bpfman_kernel_aya::Kernel.attach_outer(dispatcher, key, namespace, mode)?,
            faults: self.0.clone(),
        })
    }
}

impl XdpSwitchKernel for Probe {
    type Target = bpfman_kernel_aya::AyaXdpTarget;

    fn target_at(&self, source: PinSource<'_>) -> KernelResult<(PinnedProgram, Self::Target)> {
        bpfman_kernel_aya::Kernel.target_at(source)
    }

    fn replace_outer(
        &self,
        outer: &Outer,
        old: &Self::Target,
        new: &Self::Target,
    ) -> KernelResult<()> {
        let mut faults = self.0.lock().expect("faults");
        if faults.reject_update {
            return injected();
        }
        bpfman_kernel_aya::Kernel.replace_outer(&outer.fd, old, new)?;
        if std::mem::take(&mut faults.fail_after_update) {
            faults.fail_info = true;
        }
        Ok(())
    }
}

fn live(id: NonZeroU32) -> OuterInfo {
    bpfman_kernel_aya::Kernel
        .outer_by_id(id)
        .expect("outer lookup")
        .expect("live link")
        .info()
        .expect("outer info")
}

struct Revision {
    directory: XdpRevision,
    program: XdpProgramPin,
    extensions: Vec<XdpExtensionPin>,
}

fn stage(
    w: &RuntimeWriter<'_>,
    snapshot: &XdpDispatcherSnapshot,
    programs: &[NonZeroU32],
) -> Revision {
    stage_config(
        w,
        snapshot,
        programs,
        &vec![XdpProceedOn::default(); programs.len()],
    )
}

fn stage_config(
    w: &RuntimeWriter<'_>,
    snapshot: &XdpDispatcherSnapshot,
    programs: &[NonZeroU32],
    actions: &[XdpProceedOn],
) -> Revision {
    let kernel = bpfman_kernel_aya::Kernel;
    let first = &snapshot.members()[0];
    let revision = NonZeroU32::new(first.details.revision.get() + 1).expect("revision");
    let config = XdpConfig::new(actions).expect("config");
    let mut dispatcher = kernel.load_revision(&config).expect("load dispatcher");
    let mut prepared: Vec<_> = programs
        .iter()
        .map(|p| {
            let (key, prepared) = kernel
                .prepare_xdp(w, *p, &first.details.interface, &first.details.netns)
                .expect("prepare member");
            assert_eq!(key, first.details.key);
            prepared
        })
        .collect();
    let directory = kernel
        .create_revision_at(w, &prepared[0], revision)
        .map_err(|e| e.cause)
        .expect("revision");
    assert!(
        kernel
            .create_revision_at(w, &prepared[0], revision)
            .is_err(),
        "never adopt residue"
    );
    let program = kernel
        .pin_dispatcher(w, &directory, &mut dispatcher)
        .map_err(|e| e.cause)
        .expect("dispatcher pin");
    let extensions = prepared
        .iter_mut()
        .enumerate()
        .map(|(index, p)| {
            kernel
                .pin_extension_at(
                    w,
                    p,
                    &directory,
                    &dispatcher,
                    index.try_into().expect("slot"),
                )
                .map_err(|e| e.cause)
                .expect("extension pin")
        })
        .collect();
    Revision {
        directory,
        program,
        extensions,
    }
}

fn remove(w: &RuntimeWriter<'_>, revision: Revision) {
    let kernel = bpfman_kernel_aya::Kernel;
    for extension in revision.extensions {
        kernel
            .remove_extension(w, extension)
            .map_err(|e| e.cause)
            .expect("remove extension");
    }
    kernel
        .remove_dispatcher(w, revision.program)
        .map_err(|e| e.cause)
        .expect("remove dispatcher");
    kernel
        .remove_revision(w, revision.directory)
        .map_err(|e| e.cause)
        .expect("remove directory");
}

pub(super) fn exercise<S>(backend: S)
where
    S: OpenStore + CommitLoad + UnloadStore + LinkStore + XdpReplacementStore + Clone,
    S::Reader: LinkReader + XdpReader + XdpDispatcherReader,
{
    exercise_case(backend.clone(), false);
    exercise_case(backend, true);
}

fn exercise_case<S>(backend: S, keep_first: bool)
where
    S: OpenStore + CommitLoad + UnloadStore + LinkStore + XdpReplacementStore + Clone,
    S::Reader: LinkReader + XdpReader + XdpDispatcherReader,
{
    let c = Context::new();
    let interface = Interface::new();
    let app = Bpfman::new(
        ActiveStore::open(backend.clone(), &c.layout, TIMEOUT).expect("store"),
        bpfman_kernel_aya::Kernel,
        TIMEOUT,
    );
    let load = || {
        app.load(
            PreparedProgram::new(
                &bpfman_kernel_aya::Kernel,
                &fixture("xdp_counter.bpf.o"),
                ProgramSpec::Xdp("xdp_stats".try_into().expect("symbol")),
                Default::default(),
            )
            .expect("prepare"),
        )
        .expect("load")
        .record
        .id
    };
    let first = load();
    let second = load();
    let link = app
        .attach_xdp(XdpAttach {
            netns: Default::default(),
            mode: Default::default(),
            program_id: first,
            interface: interface.name(),
            priority: 50,
            proceed_on: Default::default(),
            metadata: Default::default(),
        })
        .expect("first attach");
    let LinkDetails::Xdp(details) = &link.details else {
        unreachable!("XDP")
    };
    let kernel = bpfman_kernel_aya::Kernel;
    traffic(&c, &[first], &[second]);
    let final_link = c.writer(|w| {
        let (old, _) = backend
            .observe_xdp_dispatcher(w, details.key)
            .expect("observe")
            .expect("snapshot");
        let old_id = old.members()[0].details.dispatcher_id;
        let outer_id = old.members()[0].outer_link_id;
        let staged = stage(w, &old, &[second, first]);
        let new_id = staged.program.id();
        let probe = Probe::default();
        let old_artifacts = w
            .observe_xdp_dispatcher(&probe, &old)
            .expect("complete adoption");
        let outer = old_artifacts.outer.expect("outer");
        let old_program = old_artifacts.program.expect("old dispatcher");

        // A syscall rejection grants no restoration evidence and leaves the target unchanged.
        probe.0.lock().expect("faults").reject_update = true;
        let failure = w
            .switch_xdp(&probe, &outer, &old_program, &staged.program)
            .err()
            .expect("rejected");
        assert!(failure.remaining.is_none());
        assert_eq!(live(outer_id).program, old_id.get());
        probe.0.lock().expect("faults").reject_update = false;

        // Failure observing a successful switch retains both live target descriptors.
        probe.0.lock().expect("faults").fail_after_update = true;
        let failure = w
            .switch_xdp(&probe, &outer, &old_program, &staged.program)
            .err()
            .expect("post-switch observation");
        let receipt = failure.remaining.expect("restoration evidence");
        assert_eq!(live(outer_id).program, new_id.get());
        assert_eq!(live(outer_id).id, outer_id.get());
        traffic(&c, &[first, second], &[]);

        // Rejected restoration cannot make the staged revision eligible for cleanup.
        probe.0.lock().expect("faults").reject_update = true;
        let failure = w
            .restore_xdp(&probe, receipt)
            .expect_err("restore rejected");
        assert_eq!(live(outer_id).program, new_id.get());
        assert!(
            c.layout
                .xdp_program_path(details.key, details.revision)
                .exists()
        );
        assert!(
            c.layout
                .xdp_program_path(details.key, NonZeroU32::new(2).expect("revision"))
                .exists()
        );
        probe.0.lock().expect("faults").reject_update = false;
        probe.0.lock().expect("faults").fail_after_update = true;
        let failure = w
            .restore_xdp(&probe, failure.remaining)
            .expect_err("post-restore observation");
        assert_eq!(live(outer_id).program, old_id.get());
        w.restore_xdp(&probe, failure.remaining)
            .map_err(|e| e.cause)
            .expect("explicit already-restored retry");
        assert_eq!(
            backend
                .observe_xdp_dispatcher(w, details.key)
                .expect("store")
                .expect("present")
                .0,
            old
        );

        traffic(&c, &[first], &[second]);

        // The public kernel capability uses the same confined mechanism.
        let adopted = kernel.observe_dispatcher(w, &old).expect("adopt");
        let staged_path = c
            .layout
            .xdp_revision_path(details.key, NonZeroU32::new(2).expect("revision"));
        let moved = staged_path.with_file_name("moved_revision");
        std::fs::rename(&staged_path, &moved).expect("move staged parent");
        let failure = kernel
            .switch_dispatcher(
                w,
                adopted.outer.as_ref().expect("outer"),
                adopted.program.as_ref().expect("program"),
                &staged.program,
            )
            .err()
            .expect("moved revision is refused");
        assert!(failure.remaining.is_none());
        assert_eq!(live(outer_id).program, old_id.get());
        std::fs::rename(&moved, &staged_path).expect("restore staged parent");
        let receipt = kernel
            .switch_dispatcher(
                w,
                adopted.outer.as_ref().expect("outer"),
                adopted.program.as_ref().expect("program"),
                &staged.program,
            )
            .map_err(|e| e.cause)
            .expect("switch");
        assert_eq!(live(outer_id).program, new_id.get());
        assert!(
            kernel
                .switch_dispatcher(
                    w,
                    adopted.outer.as_ref().expect("outer"),
                    adopted.program.as_ref().expect("program"),
                    &staged.program
                )
                .is_err(),
            "stale expected target refused"
        );
        let outer_path = c.layout.xdp_outer_path(details.key);
        let moved = outer_path.with_file_name("moved_outer");
        std::fs::rename(&outer_path, &moved).expect("move outer pin");
        let failure = kernel
            .restore_dispatcher(w, receipt)
            .expect_err("changed pin blocks restoration");
        assert_eq!(live(outer_id).program, new_id.get());
        std::fs::rename(&moved, &outer_path).expect("restore outer pin");
        let receipt = failure.remaining;
        let foreign = Context::new();
        let failure = foreign
            .writer(|other| kernel.restore_dispatcher(other, receipt))
            .expect_err("foreign writer");
        assert_eq!(live(outer_id).program, new_id.get());
        kernel
            .restore_dispatcher(w, failure.remaining)
            .map_err(|e| e.cause)
            .expect("right writer restores");
        let current = publish_replacement(w, &backend, &old, &staged, &[second, first]);
        let all = kernel
            .observe_dispatcher(w, &current)
            .expect("adopt complete two-member snapshot");
        assert_eq!(all.extensions.len(), 2);
        let slot_one = c.layout.xdp_slot_path(
            details.key,
            current.members()[0].details.revision,
            1usize.try_into().expect("slot"),
        );
        let unknown = slot_one.with_file_name("link_9");
        std::fs::rename(&slot_one, &unknown).expect("move to undeclared slot");
        assert!(
            kernel.observe_dispatcher(w, &current).is_err(),
            "unknown revision children are refused"
        );
        std::fs::rename(&unknown, &slot_one).expect("restore declared slot");

        for member in current.members() {
            assert!(
                app.get_link(member.member.id)
                    .expect("slot observation")
                    .pin_present
            );
        }
        traffic(&c, &[first, second], &[]);
        // A zero continuation mask at slot zero stops slot one's execution.
        let stopped = stage_config(
            w,
            &current,
            &[second, first],
            &[0u32.try_into().expect("mask"), XdpProceedOn::default()],
        );
        let receipt = kernel
            .switch_dispatcher(
                w,
                all.outer.as_ref().expect("outer"),
                all.program.as_ref().expect("program"),
                &stopped.program,
            )
            .map_err(|e| e.cause)
            .expect("switch stop mask");
        traffic(&c, &[second], &[first]);
        kernel
            .restore_dispatcher(w, receipt)
            .map_err(|e| e.cause)
            .expect("restore full chain");
        remove(w, stopped);
        let (keep, removed) = if keep_first {
            (first, second)
        } else {
            (second, first)
        };
        let survivor = stage(w, &current, &[keep]);
        let final_snapshot = publish_replacement(w, &backend, &current, &survivor, &[keep]);
        assert_eq!(live(outer_id).program, survivor.program.id().get());
        assert_eq!(live(outer_id).id, outer_id.get());
        traffic(&c, &[keep], &[removed]);
        final_snapshot.members()[0].member.id
    });
    app.detach_xdp(final_link)
        .expect("last detach after restoration");
    assert_eq!(app.unload(first).expect("unload first").unresolved(), 0);
    assert_eq!(app.unload(second).expect("unload second").unresolved(), 0);
    c.no_artifacts();
    assert!(names(&c.layout.root().join("fs/xdp")).is_empty());
}

pub(super) fn traffic(c: &Context, active: &[NonZeroU32], inactive: &[NonZeroU32]) {
    fn count(c: &Context, id: NonZeroU32) -> u64 {
        let map =
            aya::maps::MapData::from_pin(c.layout.map_directory_path(id).join("xdp_stats_map"))
                .expect("counter pin");
        let values: aya::maps::PerCpuArray<_, [u64; 2]> = aya::maps::Map::PerCpuArray(map)
            .try_into()
            .expect("counter map");
        values
            .get(&2, 0)
            .expect("PASS counters")
            .iter()
            .map(|v| v[0])
            .sum()
    }
    let before: Vec<_> = active
        .iter()
        .chain(inactive)
        .map(|id| count(c, *id))
        .collect();
    let peer = format!("bxb{}", std::process::id());
    for args in [
        vec!["address", "replace", "198.18.0.2/24", "dev", &peer],
        vec!["neigh", "flush", "dev", &peer],
    ] {
        assert!(
            std::process::Command::new("ip")
                .args(args)
                .output()
                .expect("configure test peer")
                .status
                .success()
        );
    }
    // The unanswered ARP request crosses only this private veth pair. Its ingress
    // executes the dispatcher even though ping cannot reach a configured peer IP.
    let output = std::process::Command::new("ping")
        .args(["-n", "-I", &peer, "-c", "1", "-W", "1", "198.18.0.3"])
        .output()
        .expect("send test traffic");
    assert!(
        matches!(output.status.code(), Some(0 | 1)),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    for (index, id) in active.iter().chain(inactive).enumerate() {
        if index < active.len() {
            assert!(
                count(c, *id) > before[index],
                "active member {id} did not execute"
            );
        } else {
            assert_eq!(
                count(c, *id),
                before[index],
                "inactive member {id} executed"
            );
        }
    }
}

fn publish_replacement<S: XdpReplacementStore>(
    w: &RuntimeWriter<'_>,
    backend: &S,
    old: &XdpDispatcherSnapshot,
    staged: &Revision,
    programs: &[NonZeroU32],
) -> XdpDispatcherSnapshot {
    let kernel = bpfman_kernel_aya::Kernel;
    let first = &old.members()[0];
    let old_artifacts = kernel
        .observe_dispatcher(w, old)
        .expect("adopt old artifacts");
    let (_, receipt) = backend
        .observe_xdp_dispatcher(w, first.details.key)
        .expect("observe store")
        .expect("snapshot");
    let details: Vec<_> = programs
        .iter()
        .enumerate()
        .map(|(index, program)| {
            let member = old
                .members()
                .iter()
                .find(|m| m.member.program_id == *program)
                .unwrap_or(first);
            let mut d = member.details.clone();
            d.slot = index.try_into().expect("slot");
            d.dispatcher_id = staged.program.id();
            d.revision = NonZeroU32::new(d.revision.get() + 1).expect("revision");
            d
        })
        .collect();
    let metadata = Default::default();
    let desired: Vec<_> = programs
        .iter()
        .zip(&details)
        .enumerate()
        .map(|(index, (program, details))| {
            let member = old
                .members()
                .iter()
                .find(|m| m.member.program_id == *program);
            XdpMemberCommit {
                identity: member.map_or(XdpMemberId::New, |m| XdpMemberId::Existing(m.member.id)),
                attachment: XdpCommit {
                    program_id: *program,
                    details,
                    extension_link_id: staged.extensions[index].id(),
                    outer_link_id: first.outer_link_id,
                    metadata: member.map_or(&metadata, |m| &m.member.metadata),
                    created_at: member
                        .map_or("2026-10-06T00:00:00Z", |m| m.member.created_at.as_str()),
                },
            }
        })
        .collect();
    let switch = kernel
        .switch_dispatcher(
            w,
            old_artifacts.outer.as_ref().expect("outer"),
            old_artifacts.program.as_ref().expect("program"),
            &staged.program,
        )
        .map_err(|e| e.cause)
        .expect("switch before publication");
    let published = backend
        .replace_xdp(
            w,
            receipt,
            XdpReplace {
                members: &desired,
                updated_at: "2026-10-06T00:00:00Z",
            },
        )
        .map_err(|e| e.cause)
        .expect("publish complete snapshot");
    drop(switch);
    remove(
        w,
        Revision {
            directory: old_artifacts.directory.expect("directory"),
            program: old_artifacts.program.expect("program"),
            extensions: old_artifacts.extensions,
        },
    );
    published
}
