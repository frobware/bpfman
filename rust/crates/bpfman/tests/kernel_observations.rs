//! Public application reads with a stateful kernel and real persistent stores.
#![allow(clippy::expect_used)]

#[path = "../../../tests/observation.rs"]
mod sample;

use bpfman_fs::{ObservedMapPin, RuntimeDirectory, RuntimeIdentity, RuntimeLayout};
use bpfman_kernel::{Error, ErrorKind, LinkObservations, ProgramObservations};
use bpfman_model::{
    KernelLink, KernelLinkDetails, KernelMap, KernelProgram, ProgramStats, XdpLink,
};
use bpfman_runtime::{ActiveStore, Bpfman, Cancellation, ObservationErrorKind};
use bpfman_store::{CommitLoad, LinkReader, LinkStore, LoadRecord, OpenStore, PendingTracepoint};
use std::{
    collections::BTreeMap,
    num::{NonZeroU32, NonZeroU64},
    sync::{Arc, Mutex},
    time::Duration,
};

const BUDGET: Duration = Duration::from_millis(50);

#[derive(Clone)]
struct FakeKernel {
    root: RuntimeIdentity,
    state: Arc<Mutex<State>>,
}

#[derive(Default)]
struct State {
    programs: BTreeMap<NonZeroU32, KernelProgram>,
    maps: BTreeMap<u32, KernelMap>,
    links: BTreeMap<NonZeroU32, KernelLink>,
    pins: BTreeMap<NonZeroU64, KernelLink>,
    extension_pins: BTreeMap<(bpfman_model::XdpKey, NonZeroU32), KernelLink>,
    map_pins: BTreeMap<NonZeroU32, Vec<ObservedMapPin>>,
    calls: Vec<&'static str>,
    fault: Option<(&'static str, ErrorKind)>,
    cancel: Option<(&'static str, Cancellation)>,
    disappear: Option<NonZeroU32>,
    disappear_link: Option<NonZeroU32>,
    repeat_id: bool,
}

fn failure(kind: ErrorKind) -> Error {
    Error::new(
        kind,
        "fake kernel observation",
        std::io::Error::other("injected kernel failure"),
    )
}

impl FakeKernel {
    fn new(runtime: &RuntimeDirectory) -> Self {
        Self {
            root: runtime.identity().expect("root identity"),
            state: Arc::default(),
        }
    }

    fn enter(&self, operation: &'static str) -> Result<std::sync::MutexGuard<'_, State>, Error> {
        let mut state = self.state.lock().expect("kernel state");
        state.calls.push(operation);
        if let Some((boundary, cancellation)) = &state.cancel {
            if *boundary == operation {
                cancellation.cancel();
            }
        }
        if let Some((boundary, kind)) = state.fault {
            if boundary == operation {
                return Err(failure(kind));
            }
        }
        Ok(state)
    }

    fn root(&self, runtime: &RuntimeDirectory) -> Result<(), Error> {
        if runtime.identity().expect("runtime identity") != self.root {
            return Err(failure(ErrorKind::InvalidData));
        }
        Ok(())
    }
}

impl ProgramObservations for FakeKernel {
    fn program(&self, id: NonZeroU32) -> Result<(KernelProgram, Option<ProgramStats>), Error> {
        self.enter("program")?
            .programs
            .get(&id)
            .cloned()
            .map(|p| (p, None))
            .ok_or_else(|| failure(ErrorKind::Missing))
    }

    fn map(&self, id: u32) -> Result<KernelMap, Error> {
        self.enter("map")?
            .maps
            .get(&id)
            .cloned()
            .ok_or_else(|| failure(ErrorKind::Missing))
    }

    fn map_pins(
        &self,
        runtime: &RuntimeDirectory,
        id: NonZeroU32,
    ) -> Result<Vec<ObservedMapPin>, Error> {
        self.root(runtime)?;
        Ok(self
            .enter("map pins")?
            .map_pins
            .get(&id)
            .cloned()
            .unwrap_or_default())
    }
}

impl bpfman_kernel::ProgramInventory for FakeKernel {
    fn next_program_id(&self, after: Option<NonZeroU32>) -> Result<Option<NonZeroU32>, Error> {
        let mut state = self.enter("next program")?;
        if state.repeat_id && after.is_some() {
            return Ok(after);
        }
        let next = state
            .programs
            .keys()
            .copied()
            .find(|id| after.is_none_or(|after| *id > after));
        if let Some(id) = next.filter(|_| next == state.disappear) {
            state.programs.remove(&id);
        }
        Ok(next)
    }
}

fn inventory<S: OpenStore + CommitLoad + Copy>(backend: S) {
    use bpfman_model::ProgramListEntry;

    let temp = tempfile::tempdir().expect("runtime");
    let layout = RuntimeLayout::try_from(temp.path().join("runtime")).expect("layout");
    let active = ActiveStore::open(backend, &layout, BUDGET).expect("real store");
    let runtime = RuntimeDirectory::open_existing(layout)
        .expect("adopt")
        .expect("runtime exists");
    let fake = FakeKernel::new(&runtime);
    let app = Bpfman::new(active, fake.clone(), BUDGET);
    let record = sample::record();
    runtime
        .with_writer(
            bpfman_lock::AcquireOptions {
                timeout: BUDGET,
                cancelled: None,
            },
            |writer| {
                backend
                    .commit_program(
                        &writer,
                        LoadRecord {
                            map_owner: None,
                            shared_maps: &[],
                            id: record.id,
                            spec: &record.spec,
                            source: &record.source,
                            license: &record.license,
                            created_at: &record.created_at,
                            metadata: &record.metadata,
                            globals: &BTreeMap::new(),
                        },
                    )
                    .expect("commit");
            },
        )
        .expect("writer");
    let mut stray = sample::kernel();
    stray.id = NonZeroU32::new(43).expect("id");
    {
        let mut state = fake.state.lock().expect("kernel");
        state.programs.insert(record.id, sample::kernel());
        state.programs.insert(stray.id, stray.clone());
    }
    runtime.with_writer(bpfman_lock::AcquireOptions { timeout: BUDGET, cancelled: None }, |_| {
        let rows = app.list_all_entries(&Default::default()).expect("reader bypasses writer");
        assert_eq!(rows.len(), 2);
        assert!(matches!(&rows[0], ProgramListEntry::Managed(p) if p.record.id == record.id && p.kernel.is_some()));
        assert_eq!(rows[1], ProgramListEntry::KernelOnly(stray.clone()));
    }).expect("writer");

    // Disappearance between enumeration and lookup is ordinary churn, never
    // a fabricated kernel-only row or a reason to lose the managed record.
    {
        let mut state = fake.state.lock().expect("kernel");
        state.disappear = Some(record.id);
    }
    let rows = app
        .list_all_entries(&Default::default())
        .expect("disappeared managed program");
    assert!(matches!(&rows[0], ProgramListEntry::Managed(p) if p.kernel.is_none()));
    fake.state.lock().expect("kernel").disappear = Some(stray.id);
    assert_eq!(
        app.list_all_entries(&Default::default())
            .expect("disappeared stray")
            .len(),
        1
    );

    for operation in ["next program", "program"] {
        for kind in [ErrorKind::Unavailable, ErrorKind::InvalidData] {
            let mut state = fake.state.lock().expect("kernel");
            state.disappear = None;
            state.programs.insert(stray.id, stray.clone());
            state.fault = Some((operation, kind));
            drop(state);
            assert_eq!(
                app.list_all_entries(&Default::default())
                    .expect_err("not absence")
                    .kind(),
                ObservationErrorKind::Unavailable
            );
        }
        let cancellation = Cancellation::new();
        {
            let mut state = fake.state.lock().expect("kernel");
            state.fault = None;
            state.calls.clear();
            state.cancel = Some((operation, cancellation.clone()));
            state.disappear = Some(stray.id);
        }
        assert_eq!(
            app.list_all_entries_with_cancellation(&Default::default(), &cancellation)
                .expect_err("cancel between effects")
                .kind(),
            ObservationErrorKind::Cancelled
        );
        assert_eq!(
            fake.state.lock().expect("kernel").calls,
            if operation == "next program" {
                vec!["next program"]
            } else {
                vec!["next program", "program"]
            }
        );
    }
    {
        let mut state = fake.state.lock().expect("kernel");
        state.cancel = None;
        state.disappear = None;
        state.programs.insert(stray.id, stray);
        state.repeat_id = true;
    }
    assert_eq!(
        app.list_all_entries(&Default::default())
            .expect_err("non-increasing ID")
            .kind(),
        ObservationErrorKind::Unavailable
    );
    let cancellation = Cancellation::new();
    cancellation.cancel();
    fake.state.lock().expect("kernel").calls.clear();
    assert_eq!(
        app.list_all_entries_with_cancellation(&Default::default(), &cancellation)
            .expect_err("pre-cancelled")
            .kind(),
        ObservationErrorKind::Cancelled
    );
    assert!(fake.state.lock().expect("kernel").calls.is_empty());
    assert!(!runtime.layout().root().join("fs").exists());
}

#[test]
fn sqlite_inventory_preserves_managed_identity_and_observation_failures() {
    inventory(bpfman_store_sqlite::Backend);
}

impl LinkObservations for FakeKernel {
    fn tracepoint_link(&self, id: NonZeroU32) -> Result<KernelLink, Error> {
        let mut state = self.enter("tracepoint link")?;
        if state.disappear_link == Some(id) {
            state.links.remove(&id);
        }
        state
            .links
            .get(&id)
            .cloned()
            .ok_or_else(|| failure(ErrorKind::Missing))
    }

    fn extension_link(&self, id: NonZeroU32) -> Result<KernelLink, Error> {
        self.enter("extension link")?
            .links
            .get(&id)
            .cloned()
            .ok_or_else(|| failure(ErrorKind::Missing))
    }

    fn tracepoint_pin(
        &self,
        runtime: &RuntimeDirectory,
        id: NonZeroU64,
    ) -> Result<Option<KernelLink>, Error> {
        self.root(runtime)?;
        Ok(self.enter("tracepoint pin")?.pins.get(&id).cloned())
    }

    fn extension_pin(
        &self,
        runtime: &RuntimeDirectory,
        link: &XdpLink,
    ) -> Result<Option<KernelLink>, Error> {
        self.root(runtime)?;
        Ok(self
            .enter("extension pin")?
            .extension_pins
            .get(&(link.key, link.revision))
            .cloned())
    }
}

fn exercise<S: OpenStore + CommitLoad + LinkStore + Copy>(backend: S)
where
    S::Reader: LinkReader,
{
    let temp = tempfile::tempdir().expect("runtime");
    let layout = RuntimeLayout::try_from(temp.path().join("runtime")).expect("layout");
    let active = ActiveStore::open(backend, &layout, BUDGET).expect("real store");
    let runtime = RuntimeDirectory::open_existing(layout.clone())
        .expect("adopt")
        .expect("runtime exists");
    let fake = FakeKernel::new(&runtime);
    let app = Bpfman::new(active, fake.clone(), BUDGET);
    let attached = bpfman_core::AttachmentFilter::Attached;
    let unattached = bpfman_core::AttachmentFilter::Unattached;
    let record = sample::record();
    let metadata = BTreeMap::from([("owner".into(), "acme".into())]);
    let id = record.id;
    let link = KernelLink {
        id: NonZeroU32::new(123).expect("link id"),
        program_id: id,
        details: KernelLinkDetails::PerfEvent,
    };

    let stored_link = runtime
        .with_writer(
            bpfman_lock::AcquireOptions {
                timeout: BUDGET,
                cancelled: None,
            },
            |writer| {
                backend
                    .commit_program(
                        &writer,
                        LoadRecord {
                            map_owner: None,
                            shared_maps: &[],
                            id,
                            spec: &record.spec,
                            source: &bpfman_model::ProgramSource::File(Some("original.o".into())),
                            license: "GPL",
                            created_at: &record.created_at,
                            metadata: &metadata,
                            globals: &BTreeMap::new(),
                        },
                    )
                    .expect("real commit");
                let (_, receipt) = backend
                    .create_pending_tracepoint(
                        &writer,
                        PendingTracepoint {
                            program_id: id,
                            target: &"syscalls/sys_enter_kill"
                                .parse::<bpfman_model::Tracepoint>()
                                .expect("target"),
                            metadata: &BTreeMap::new(),
                            created_at: &record.created_at,
                        },
                    )
                    .expect("pending link");
                backend
                    .finalise_link(&writer, receipt, link.id)
                    .unwrap_or_else(|_| unreachable!("finalise real link"))
            },
        )
        .expect("writer");

    {
        let mut state = fake.state.lock().expect("kernel");
        state.programs.insert(id, sample::kernel());
        state.maps.insert(100, sample::map());
        state.map_pins.insert(
            id,
            vec![ObservedMapPin {
                name: "tracepoint_stats_map".into(),
                id: 100,
            }],
        );
        state.links.insert(link.id, link.clone());
        state.pins.insert(stored_link.id, link.clone());
    }

    // The same injected backend serves reads even while another caller owns the writer.
    runtime
        .with_writer(
            bpfman_lock::AcquireOptions {
                timeout: BUDGET,
                cancelled: None,
            },
            |_| {
                let observed = app.get(id).expect("read while writer held");
                assert_eq!(observed.kernel, sample::kernel());
                assert_eq!(observed.maps.len(), 1);
                assert!(observed.maps[0].present);
                assert!(
                    observed.maps[0]
                        .pin_path
                        .as_ref()
                        .expect("pin path")
                        .ends_with("tracepoint_stats_map")
                );
                assert_eq!(observed.links[0].kernel, Some(link.clone()));
                assert!(observed.links[0].pin_present);
                assert_eq!(
                    app.get_link(stored_link.id).expect("same backend").kernel,
                    Some(link.clone())
                );
                assert_eq!(
                    app.list_entries(&Default::default()).expect("list")[0].kernel,
                    Some(sample::kernel())
                );
                assert_eq!(
                    app.list_entries_by_attachment(&Default::default(), attached)
                        .expect("attachment read while writer held")
                        .len(),
                    1
                );
                assert!(
                    app.list_entries_by_attachment(&Default::default(), unattached)
                        .expect("active link")
                        .is_empty()
                );
            },
        )
        .expect("writer");
    assert!(
        !layout.root().join("fs").exists(),
        "fake reads must never mount or inspect real bpffs"
    );

    // Selection uses real persisted metadata, before individual kernel reads.
    // Summary selection remains independent of the kernel, even while locked.
    let matching = bpfman_core::ProgramFilter {
        metadata: metadata.clone(),
        selector: "owner in (acme,other),!debug".parse().expect("selector"),
        ..Default::default()
    };
    let excluded = bpfman_core::ProgramFilter {
        selector: "owner!=acme".parse().expect("selector"),
        ..Default::default()
    };
    {
        let mut state = fake.state.lock().expect("kernel");
        state.calls.clear();
        state.fault = Some(("program", ErrorKind::Unavailable));
    }
    runtime
        .with_writer(
            bpfman_lock::AcquireOptions {
                timeout: BUDGET,
                cancelled: None,
            },
            |_| {
                assert_eq!(app.list(&matching).expect("store-only selection").len(), 1);
                assert!(
                    app.list(&excluded)
                        .expect("store-only exclusion")
                        .is_empty()
                );
                assert!(
                    app.list_entries(&excluded)
                        .expect("exclude before kernel")
                        .is_empty()
                );
                assert!(
                    app.list_entries_by_attachment(&excluded, attached)
                        .expect("exclude before link observation")
                        .is_empty()
                );
            },
        )
        .expect("writer");
    assert!(fake.state.lock().expect("kernel").calls.is_empty());
    assert_eq!(
        app.list_entries(&matching)
            .expect_err("selected observation failure")
            .kind(),
        ObservationErrorKind::Unavailable
    );
    fake.state.lock().expect("kernel").fault = None;
    assert_eq!(
        app.list_entries_by_attachment(&matching, attached)
            .expect("combined selection")
            .len(),
        1
    );

    // Pending intent with a pin but no recorded kernel identity is inactive.
    let pending = runtime
        .with_writer(
            bpfman_lock::AcquireOptions {
                timeout: BUDGET,
                cancelled: None,
            },
            |writer| {
                backend
                    .create_pending_tracepoint(
                        &writer,
                        PendingTracepoint {
                            program_id: id,
                            target: &"syscalls/sys_enter_kill".parse().expect("target"),
                            metadata: &BTreeMap::new(),
                            created_at: &record.created_at,
                        },
                    )
                    .expect("real pending intent")
                    .0
            },
        )
        .expect("writer");
    fake.state
        .lock()
        .expect("kernel")
        .pins
        .insert(pending.id, link.clone());

    // Every selected link is inspected: permission and malformed observation
    // failures cannot be hidden by another active link or turned into absence.
    for operation in ["tracepoint link", "tracepoint pin"] {
        for kind in [ErrorKind::Unavailable, ErrorKind::InvalidData] {
            fake.state.lock().expect("kernel").fault = Some((operation, kind));
            for filter in [attached, unattached] {
                assert_eq!(
                    app.list_entries_by_attachment(&Default::default(), filter)
                        .expect_err("failed observation")
                        .kind(),
                    ObservationErrorKind::Unavailable
                );
            }
        }
        for fault in [
            None,
            Some((operation, ErrorKind::Unavailable)),
            Some((operation, ErrorKind::Missing)),
        ] {
            let cancellation = Cancellation::new();
            {
                let mut state = fake.state.lock().expect("kernel");
                state.fault = fault;
                state.cancel = Some((operation, cancellation.clone()));
                state.calls.clear();
            }
            assert_eq!(
                app.list_entries_by_attachment_with_cancellation(
                    &Default::default(),
                    attached,
                    &cancellation,
                )
                .expect_err("cancel during link observation")
                .kind(),
                ObservationErrorKind::Cancelled
            );
            assert_eq!(
                fake.state.lock().expect("kernel").calls.last(),
                Some(&operation)
            );
        }
    }
    {
        let mut state = fake.state.lock().expect("kernel");
        state.cancel = None;
        state.fault = None;
        state.disappear_link = Some(link.id);
    }
    assert!(
        app.list_entries_by_attachment(&Default::default(), attached)
            .expect("stale and pending intent")
            .is_empty()
    );
    assert_eq!(
        app.list_entries_by_attachment(&Default::default(), unattached)
            .expect("pins alone are inactive")
            .len(),
        1
    );
    {
        let mut state = fake.state.lock().expect("kernel");
        state.pins.clear();
        state.disappear_link = None;
        state.links.insert(link.id, link.clone());
    }
    assert_eq!(
        app.list_entries_by_attachment(&Default::default(), attached)
            .expect("kernel link without pin remains active")
            .len(),
        1
    );
    fake.state
        .lock()
        .expect("kernel")
        .pins
        .insert(stored_link.id, link.clone());

    let cancellation = Cancellation::new();
    cancellation.cancel();
    fake.state.lock().expect("kernel").calls.clear();
    assert_eq!(
        app.list_entries_by_attachment_with_cancellation(
            &Default::default(),
            attached,
            &cancellation,
        )
        .expect_err("pre-cancelled")
        .kind(),
        ObservationErrorKind::Cancelled
    );
    assert!(fake.state.lock().expect("kernel").calls.is_empty());

    // Changed simulated state is visible through the existing application instance.
    fake.state.lock().expect("kernel").programs.remove(&id);
    let rows = app
        .list_entries_by_attachment(&Default::default(), attached)
        .expect("independent link and program observations");
    assert!(
        matches!(rows.as_slice(), [bpfman_model::ProgramListEntry::Managed(p)] if p.kernel.is_none())
    );
    assert!(
        app.list_entries(&Default::default())
            .expect("missing kernel")[0]
            .kernel
            .is_none()
    );
    assert_eq!(
        app.get(id).expect_err("missing kernel").kind(),
        ObservationErrorKind::KernelMissing
    );
    fake.state.lock().expect("kernel").fault = Some(("program", ErrorKind::Unavailable));
    assert_eq!(
        app.list_entries(&Default::default())
            .expect_err("denied is not absence")
            .kind(),
        ObservationErrorKind::Unavailable
    );
    {
        let mut state = fake.state.lock().expect("kernel");
        state.fault = Some(("map", ErrorKind::Unavailable));
        state.programs.insert(id, sample::kernel());
    }
    assert!(
        app.get(id)
            .expect("Go omits unreadable maps")
            .maps
            .is_empty()
    );

    let cancellation = Cancellation::new();
    {
        let mut state = fake.state.lock().expect("kernel");
        state.fault = None;
        state.calls.clear();
        state.cancel = Some(("program", cancellation.clone()));
    }
    assert_eq!(
        app.get_with_cancellation(id, &cancellation)
            .expect_err("cancel during kernel read")
            .kind(),
        ObservationErrorKind::Cancelled
    );
    assert_eq!(
        fake.state.lock().expect("kernel").calls,
        ["map pins", "program"]
    );
    fake.state.lock().expect("kernel").cancel = None;

    // Link absence and identity mismatch preserve the existing public classification.
    fake.state.lock().expect("kernel").links.remove(&link.id);
    let observed = app.get_link(stored_link.id).expect("missing link");
    assert!(observed.kernel.is_none());
    assert!(observed.pin_present);
    fake.state
        .lock()
        .expect("kernel")
        .pins
        .get_mut(&stored_link.id)
        .expect("pin")
        .program_id = NonZeroU32::MIN;
    assert_eq!(
        app.get_link(stored_link.id)
            .expect_err("foreign pin")
            .kind(),
        bpfman_runtime::LinkErrorKind::InvalidState
    );
}

#[test]
fn sqlite_uses_injected_kernel_observations() {
    exercise(bpfman_store_sqlite::Backend);
}

fn xdp_observations<S: OpenStore + CommitLoad + bpfman_store::XdpReplacementStore + Copy>(
    backend: S,
) where
    S::Reader: LinkReader + bpfman_store::XdpDispatcherReader,
{
    let temp = tempfile::tempdir().expect("runtime");
    let layout = RuntimeLayout::try_from(temp.path().join("runtime")).expect("layout");
    let active = ActiveStore::open(backend, &layout, BUDGET).expect("real store");
    let runtime = RuntimeDirectory::open_existing(layout.clone())
        .expect("adopt")
        .expect("runtime exists");
    let fake = FakeKernel::new(&runtime);
    let app = Bpfman::new(active, fake.clone(), BUDGET);
    let id = NonZeroU32::new(42).expect("program");
    let details = XdpLink {
        netns: Default::default(),
        slot: bpfman_model::XdpSlot::FIRST,
        key: bpfman_model::XdpKey {
            nsid: NonZeroU64::new(99).expect("namespace"),
            ifindex: NonZeroU32::new(7).expect("interface"),
        },
        interface: "fake0".parse().expect("interface name"),
        priority: 50,
        proceed_on: Default::default(),
        dispatcher_id: NonZeroU32::new(50).expect("dispatcher"),
        revision: NonZeroU32::MIN,
    };
    let link = KernelLink {
        id: NonZeroU32::new(123).expect("link"),
        program_id: id,
        details: KernelLinkDetails::Tracing {
            attach_type: 0,
            target_obj_id: details.dispatcher_id.get(),
            target_btf_id: 9,
        },
    };
    let stored = runtime
        .with_writer(
            bpfman_lock::AcquireOptions {
                timeout: BUDGET,
                cancelled: None,
            },
            |writer| {
                backend
                    .commit_program(
                        &writer,
                        LoadRecord {
                            map_owner: None,
                            shared_maps: &[],
                            id,
                            spec: &bpfman_model::ProgramSpec::Xdp(
                                "xdp_pass".try_into().expect("symbol"),
                            ),
                            source: &bpfman_model::ProgramSource::File(Some("original.o".into())),
                            license: "GPL",
                            created_at: "2026-10-03T00:00:00Z",
                            metadata: &BTreeMap::new(),
                            globals: &BTreeMap::new(),
                        },
                    )
                    .expect("real commit");
                backend
                    .commit_xdp(
                        &writer,
                        bpfman_store::XdpCommit {
                            program_id: id,
                            details: &details,
                            extension_link_id: link.id,
                            outer_link_id: NonZeroU32::new(124).expect("outer link"),
                            metadata: &BTreeMap::new(),
                            created_at: "2026-10-03T00:00:00Z",
                        },
                    )
                    .expect("real dispatcher snapshot")
            },
        )
        .expect("writer");
    {
        let mut state = fake.state.lock().expect("kernel");
        state.links.insert(link.id, link.clone());
        state
            .extension_pins
            .insert((details.key, details.revision), link.clone());
    }
    assert_eq!(
        app.get_link(stored.id).expect("extension").kernel,
        Some(link.clone())
    );
    assert!(app.get_link(stored.id).expect("extension").pin_present);
    assert_eq!(
        app.get_xdp_dispatcher(details.key)
            .expect("snapshot")
            .members()[0]
            .details,
        details
    );
    assert!(!layout.root().join("fs").exists());
    let mut wrong_target = link;
    wrong_target.details = KernelLinkDetails::Tracing {
        attach_type: 0,
        target_obj_id: 999,
        target_btf_id: 9,
    };
    fake.state
        .lock()
        .expect("kernel")
        .links
        .insert(wrong_target.id, wrong_target);
    assert_eq!(
        app.get_link(stored.id)
            .expect_err("wrong dispatcher target")
            .kind(),
        bpfman_runtime::LinkErrorKind::InvalidState
    );
}

#[test]
fn sqlite_xdp_uses_injected_kernel_observations() {
    xdp_observations(bpfman_store_sqlite::Backend);
}
