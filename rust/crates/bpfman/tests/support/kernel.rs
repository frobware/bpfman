//! Stateful kernel substitute. Bytecode and persistence still use real adapters.
use bpfman_core::EffectFailure;
use bpfman_fs::{RuntimeDirectory, RuntimeIdentity, RuntimeWriter};
use bpfman_kernel::*;
use bpfman_model::*;
use std::{
    collections::BTreeMap,
    num::{NonZeroU32, NonZeroU64},
    sync::{Arc, Mutex},
};

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub(super) enum Point {
    Prepare,
    Load,
    PinProgram,
    Directory,
    PinMap,
    ObserveProgram,
    ObserveUnload,
    RemoveProgram,
    RemoveMap,
    RemoveDirectory,
    PrepareLink,
    Attach,
    PinLink,
    Release,
    RemoveLink,
    PrepareXdp,
    LoadDispatcher,
    Revision,
    PinDispatcher,
    Extension,
    Outer,
    ObserveXdp,
    RemoveOuter,
    RemoveExtension,
    RemoveDispatcher,
    RemoveRevision,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum Phase {
    Before,
    After,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd)]
enum Resource {
    Program(NonZeroU32),
    Map(u32),
    Directory(NonZeroU32),
    Link(NonZeroU32),
    Revision(XdpKey),
    Dispatcher(NonZeroU32),
    Extension(NonZeroU32),
    Outer(NonZeroU32),
    LiveOuter(NonZeroU32),
}

struct Program {
    observation: KernelProgram,
    handles: usize,
}

struct Link {
    observation: KernelLink,
    handles: usize,
    attached: bool,
}

struct State {
    next: u32,
    generation: u64,
    programs: BTreeMap<NonZeroU32, Program>,
    maps: BTreeMap<u32, KernelMap>,
    links: BTreeMap<NonZeroU32, Link>,
    resources: BTreeMap<Resource, u64>,
    map_names: BTreeMap<(NonZeroU32, String), u32>,
    tracepoint_pins: BTreeMap<NonZeroU64, NonZeroU32>,
    extensions: BTreeMap<XdpKey, NonZeroU32>,
    outers: BTreeMap<XdpKey, NonZeroU32>,
    fault: BTreeMap<Point, Phase>,
    cancellation: Option<(Point, bpfman_runtime::Cancellation)>,
    events: Vec<Point>,
}

#[derive(Clone)]
pub(super) struct FakeKernel {
    root: RuntimeIdentity,
    state: Arc<Mutex<State>>,
}

pub(super) struct Receipt {
    root: RuntimeIdentity,
    state: Arc<Mutex<State>>,
    resource: Resource,
    generation: u64,
}

pub(super) struct Loaded {
    root: RuntimeIdentity,
    state: Arc<Mutex<State>>,
    id: NonZeroU32,
    maps: Vec<String>,
}

pub(super) struct Live {
    receipt: Receipt,
}

pub(super) struct PreparedXdp {
    extension: Loaded,
    key: XdpKey,
}

fn error(kind: ErrorKind, message: &'static str) -> Error {
    Error::new(kind, message, std::io::Error::other(message))
}

impl State {
    fn id(&mut self) -> NonZeroU32 {
        self.next += 1;
        NonZeroU32::new(self.next).expect("fake ID")
    }

    fn collect(&mut self) {
        self.links.retain(|id, link| link.handles > 0 || self.resources.keys().any(|r| matches!(r, Resource::Link(n) | Resource::Extension(n) | Resource::Outer(n) if n == id)));
        self.programs.retain(|id, program| program.handles > 0
            || self.resources.contains_key(&Resource::Program(*id)) || self.resources.contains_key(&Resource::Dispatcher(*id))
            || self.links.values().any(|l| l.observation.program_id == *id || matches!(l.observation.details, KernelLinkDetails::Tracing { target_obj_id, .. } if target_obj_id == id.get())));
        self.maps.retain(|id, _| {
            self.resources.contains_key(&Resource::Map(*id))
                || self.programs.values().any(|p| {
                    p.observation
                        .map_ids
                        .as_ref()
                        .is_some_and(|ids| ids.contains(id))
                })
        });
        self.map_names.retain(|(program, _), map| {
            self.programs.contains_key(program) || self.maps.contains_key(map)
        });
        self.tracepoint_pins
            .retain(|_, id| self.resources.contains_key(&Resource::Link(*id)));
        self.extensions
            .retain(|_, id| self.resources.contains_key(&Resource::Extension(*id)));
        self.outers
            .retain(|_, id| self.links.get(id).is_some_and(|l| l.attached));
    }
}

// A partial outer acquisition owns a live handle, not a persistent pin.
impl Drop for Receipt {
    fn drop(&mut self) {
        if let Resource::LiveOuter(id) = self.resource {
            let mut s = self.state.lock().expect("state");
            s.resources.remove(&self.resource);
            if let Some(link) = s.links.get_mut(&id) {
                link.handles -= 1;
            }
            s.collect();
        }
    }
}

pub(super) struct HeldOuter {
    state: Arc<Mutex<State>>,
    id: NonZeroU32,
}

impl HeldOuter {
    pub(super) fn attached(&self) -> bool {
        self.state.lock().expect("state").links[&self.id].attached
    }
}

impl Drop for HeldOuter {
    fn drop(&mut self) {
        let mut s = self.state.lock().expect("state");
        s.links.get_mut(&self.id).expect("held link").handles -= 1;
        s.collect();
    }
}

impl Drop for Loaded {
    fn drop(&mut self) {
        let mut s = self.state.lock().expect("state");
        if let Some(p) = s.programs.get_mut(&self.id) {
            p.handles -= 1;
        }
        s.collect();
    }
}

impl Drop for Live {
    fn drop(&mut self) {
        let mut s = self.receipt.state.lock().expect("state");
        let Resource::Link(id) = self.receipt.resource else {
            unreachable!("live link")
        };
        if let Some(link) = s.links.get_mut(&id) {
            link.handles -= 1;
        }
        s.collect();
    }
}

impl FakeKernel {
    pub(super) fn new(runtime: &RuntimeDirectory) -> Self {
        Self {
            root: runtime.identity().expect("root"),
            state: Arc::new(Mutex::new(State {
                next: 1000,
                generation: 0,
                programs: BTreeMap::new(),
                maps: BTreeMap::new(),
                links: BTreeMap::new(),
                resources: BTreeMap::new(),
                map_names: BTreeMap::new(),
                tracepoint_pins: BTreeMap::new(),
                extensions: BTreeMap::new(),
                outers: BTreeMap::new(),
                fault: BTreeMap::new(),
                cancellation: None,
                events: Vec::new(),
            })),
        }
    }

    pub(super) fn fail(&self, point: Point, phase: Phase) {
        self.state.lock().expect("state").fault.insert(point, phase);
    }

    pub(super) fn clear(&self) {
        let mut s = self.state.lock().expect("state");
        s.fault.clear();
        s.cancellation = None;
    }

    pub(super) fn cancel_at(&self, point: Point, token: &bpfman_runtime::Cancellation) {
        self.state.lock().expect("state").cancellation = Some((point, token.clone()));
    }

    pub(super) fn events(&self) -> Vec<Point> {
        self.state.lock().expect("state").events.clone()
    }

    pub(super) fn assert_empty(&self) {
        let s = self.state.lock().expect("state");
        assert!(s.resources.is_empty(), "residue: {:?}", s.resources);
        assert!(s.links.is_empty(), "live links");
        assert!(s.programs.is_empty(), "live programs");
        assert!(s.maps.is_empty(), "live maps");
    }

    pub(super) fn hold_outer(&self) -> HeldOuter {
        let mut s = self.state.lock().expect("state");
        let id = *s.outers.values().next().expect("attached outer");
        s.links.get_mut(&id).expect("outer").handles += 1;
        HeldOuter {
            state: self.state.clone(),
            id,
        }
    }

    fn check_root(&self, root: RuntimeIdentity) -> Result<(), Error> {
        if root != self.root {
            return Err(error(ErrorKind::InvalidData, "foreign runtime"));
        }
        Ok(())
    }

    fn writer(&self, w: &RuntimeWriter<'_>) -> Result<(), Error> {
        self.check_root(w.identity().expect("root"))
    }

    fn enter(&self, p: Point) -> Result<(), Error> {
        let mut s = self.state.lock().expect("state");
        s.events.push(p);
        if let Some((at, token)) = &s.cancellation {
            if *at == p {
                token.cancel();
            }
        }
        if s.fault.get(&p) == Some(&Phase::Before) {
            return Err(error(
                ErrorKind::Unavailable,
                "injected before kernel effect",
            ));
        }
        Ok(())
    }

    fn after<T>(&self, p: Point, receipt: T) -> Acquisition<T> {
        if self.state.lock().expect("state").fault.get(&p) == Some(&Phase::After) {
            Err(EffectFailure {
                cause: error(ErrorKind::Unavailable, "injected after kernel acquisition"),
                remaining: Some(receipt),
            })
        } else {
            Ok(receipt)
        }
    }

    fn acquire(&self, w: &RuntimeWriter<'_>, p: Point, resource: Resource) -> Acquisition<Receipt> {
        let fail = |cause| EffectFailure {
            cause,
            remaining: None,
        };
        self.writer(w).map_err(fail)?;
        self.enter(p).map_err(fail)?;
        let receipt = self.insert(resource).map_err(fail)?;
        self.after(p, receipt)
    }

    fn insert(&self, resource: Resource) -> Result<Receipt, Error> {
        let mut s = self.state.lock().expect("state");
        if s.resources.contains_key(&resource) {
            return Err(error(ErrorKind::InvalidData, "resource already pinned"));
        }
        s.generation += 1;
        let generation = s.generation;
        s.resources.insert(resource, generation);
        Ok(Receipt {
            root: self.root,
            state: self.state.clone(),
            resource,
            generation,
        })
    }

    fn observed(&self, resource: Resource) -> Option<Receipt> {
        self.state
            .lock()
            .expect("state")
            .resources
            .get(&resource)
            .map(|generation| Receipt {
                root: self.root,
                state: self.state.clone(),
                resource,
                generation: *generation,
            })
    }

    fn validate(&self, w: &RuntimeWriter<'_>, r: &Receipt) -> Result<(), Error> {
        self.writer(w)?;
        if r.root != self.root
            || !Arc::ptr_eq(&r.state, &self.state)
            || self.state.lock().expect("state").resources.get(&r.resource) != Some(&r.generation)
        {
            return Err(error(
                ErrorKind::InvalidData,
                "foreign or stale kernel receipt",
            ));
        }
        Ok(())
    }

    fn loaded(&self, w: &RuntimeWriter<'_>, l: &Loaded) -> Result<(), Error> {
        self.writer(w)?;
        if l.root != self.root || !Arc::ptr_eq(&l.state, &self.state) {
            return Err(error(ErrorKind::InvalidData, "foreign live program"));
        }
        Ok(())
    }

    fn remove(&self, w: &RuntimeWriter<'_>, p: Point, r: Receipt) -> Removal<Receipt> {
        let result = (|| {
            self.validate(w, &r)?;
            self.enter(p)?;
            let mut s = self.state.lock().expect("state");
            match r.resource {
                Resource::Directory(id)
                    if s.map_names.iter().any(|((owner, _), map)| {
                        *owner == id && s.resources.contains_key(&Resource::Map(*map))
                    }) =>
                {
                    return Err(error(ErrorKind::InvalidData, "map directory is not empty"));
                }
                Resource::Revision(key)
                    if s.extensions.contains_key(&key)
                        || s.resources
                            .keys()
                            .any(|r| matches!(r, Resource::Dispatcher(_))) =>
                {
                    return Err(error(ErrorKind::InvalidData, "revision is not empty"));
                }
                Resource::Outer(id) | Resource::LiveOuter(id) => {
                    if let Some(l) = s.links.get_mut(&id) {
                        l.attached = false;
                    }
                }
                _ => {}
            }
            s.resources.remove(&r.resource);
            s.collect();
            Ok(())
        })();
        result.map_err(|cause| EffectFailure {
            cause,
            remaining: r,
        })
    }

    fn new_program(&self, name: &str, kind: &str, maps: &[String]) -> Loaded {
        let mut s = self.state.lock().expect("state");
        let id = s.id();
        let mut ids = Vec::new();
        for name in maps {
            let map = s.id().get();
            s.maps.insert(
                map,
                KernelMap {
                    id: map,
                    name: name.clone(),
                    kind: "array".into(),
                    key_size: 4,
                    value_size: 8,
                    max_entries: 1,
                    flags: 0,
                    btf_id: None,
                    map_extra: None,
                    memlock: None,
                    frozen: false,
                },
            );
            s.map_names.insert((id, name.clone()), map);
            ids.push(map);
        }
        s.programs.insert(
            id,
            Program {
                handles: 1,
                observation: KernelProgram {
                    id,
                    name: name.into(),
                    kind: kind.into(),
                    tag: "0000000000000000".into(),
                    loaded_at: None,
                    uid: None,
                    btf_id: None,
                    map_ids: Some(ids),
                    jited_size: 0,
                    xlated_size: 0,
                    verified_insns: 0,
                    memlock: None,
                    restricted: false,
                },
            },
        );
        Loaded {
            root: self.root,
            state: self.state.clone(),
            id,
            maps: maps.to_vec(),
        }
    }

    fn adopt(&self, w: &RuntimeWriter<'_>, id: NonZeroU32, kind: &str) -> Result<Loaded, Error> {
        self.writer(w)?;
        let mut s = self.state.lock().expect("state");
        if !s.resources.contains_key(&Resource::Program(id)) {
            return Err(error(ErrorKind::Missing, "program pin missing"));
        }
        let p = s
            .programs
            .get_mut(&id)
            .ok_or_else(|| error(ErrorKind::Missing, "program missing"))?;
        if p.observation.kind != kind {
            return Err(error(ErrorKind::InvalidData, "wrong program type"));
        }
        p.handles += 1;
        Ok(Loaded {
            root: self.root,
            state: self.state.clone(),
            id,
            maps: Vec::new(),
        })
    }
}

impl ObjectLoader for FakeKernel {
    fn validate_object(&self, bytes: &[u8], _spec: &ProgramSpec) -> Result<ObjectInfo, Error> {
        if bytes != b"fake object" {
            return Err(error(ErrorKind::InvalidInput, "invalid fake object"));
        }
        Ok(ObjectInfo {
            license: "GPL".into(),
            maps: vec!["counter".into()],
        })
    }

    fn validate_globals(
        &self,
        _bytes: &[u8],
        _globals: &BTreeMap<String, Vec<u8>>,
    ) -> Result<(), Error> {
        Ok(())
    }
}

impl ProgramLoad for FakeKernel {
    type Prepared = RuntimeIdentity;
    type Loaded = Loaded;

    fn prepare_load(&self, w: &RuntimeWriter<'_>) -> Result<RuntimeIdentity, Error> {
        self.writer(w)?;
        self.enter(Point::Prepare)?;
        Ok(self.root)
    }

    fn load_program(
        &self,
        w: &RuntimeWriter<'_>,
        _bytes: &[u8],
        maps: &[String],
        _globals: &BTreeMap<String, Vec<u8>>,
        spec: &ProgramSpec,
    ) -> Result<Loaded, Error> {
        self.writer(w)?;
        self.enter(Point::Load)?;
        let kind = match spec {
            ProgramSpec::Tracepoint(_) => "tracepoint",
            ProgramSpec::Xdp(_) => "extension",
            _ => return Err(error(ErrorKind::Unsupported, "program type")),
        };
        Ok(self.new_program(spec.name().as_str(), kind, maps))
    }

    fn map_names(l: &Loaded) -> &[String] {
        &l.maps
    }

    fn pin_program(
        &self,
        w: &RuntimeWriter<'_>,
        p: &RuntimeIdentity,
        l: &mut Loaded,
        _name: &Symbol,
    ) -> Acquisition<Receipt> {
        self.check_root(*p)
            .and_then(|()| self.loaded(w, l))
            .map_err(|cause| EffectFailure {
                cause,
                remaining: None,
            })?;
        self.acquire(w, Point::PinProgram, Resource::Program(l.id))
    }

    fn create_map_directory(
        &self,
        w: &RuntimeWriter<'_>,
        p: &RuntimeIdentity,
        id: NonZeroU32,
    ) -> Acquisition<Receipt> {
        self.check_root(*p).map_err(|cause| EffectFailure {
            cause,
            remaining: None,
        })?;
        self.acquire(w, Point::Directory, Resource::Directory(id))
    }

    fn pin_map(
        &self,
        w: &RuntimeWriter<'_>,
        l: &Loaded,
        d: &Receipt,
        name: &str,
    ) -> Acquisition<Receipt> {
        self.loaded(w, l)
            .and_then(|()| self.validate(w, d))
            .map_err(|cause| EffectFailure {
                cause,
                remaining: None,
            })?;
        assert_eq!(d.resource, Resource::Directory(l.id));
        let id = self.state.lock().expect("state").map_names[&(l.id, name.into())];
        self.acquire(w, Point::PinMap, Resource::Map(id))
    }
}

impl ProgramResources for FakeKernel {
    type ProgramPin = Receipt;
    type MapPin = Receipt;
    type MapDirectory = Receipt;
    fn program_id(p: &Receipt) -> NonZeroU32 {
        let Resource::Program(id) = p.resource else {
            unreachable!("program receipt")
        };
        id
    }

    fn remove_program(&self, w: &RuntimeWriter<'_>, r: Receipt) -> Removal<Receipt> {
        self.remove(w, Point::RemoveProgram, r)
    }

    fn remove_map(&self, w: &RuntimeWriter<'_>, r: Receipt) -> Removal<Receipt> {
        self.remove(w, Point::RemoveMap, r)
    }

    fn remove_map_directory(&self, w: &RuntimeWriter<'_>, r: Receipt) -> Removal<Receipt> {
        self.remove(w, Point::RemoveDirectory, r)
    }

    fn observe_unload(
        &self,
        w: &RuntimeWriter<'_>,
        id: NonZeroU32,
    ) -> Result<UnloadArtifacts<Self>, Error> {
        self.writer(w)?;
        self.enter(Point::ObserveUnload)?;
        let maps: Vec<_> = self
            .state
            .lock()
            .expect("state")
            .map_names
            .iter()
            .filter(|((p, _), _)| *p == id)
            .map(|(_, m)| *m)
            .collect();
        Ok(UnloadArtifacts {
            program: self.observed(Resource::Program(id)),
            maps: maps
                .into_iter()
                .filter_map(|id| self.observed(Resource::Map(id)))
                .collect(),
            directory: self.observed(Resource::Directory(id)),
            bytecode: w
                .observe_bytecode(id)
                .map_err(|e| Error::new(ErrorKind::Unavailable, "observe bytecode", e))?,
        })
    }
}

impl TracepointLinks for FakeKernel {
    type PreparedTracepoint = Loaded;
    type LiveTracepoint = Live;
    type LinkPin = Receipt;

    fn prepare_tracepoint(&self, w: &RuntimeWriter<'_>, id: NonZeroU32) -> Result<Loaded, Error> {
        self.enter(Point::PrepareLink)?;
        self.adopt(w, id, "tracepoint")
    }

    fn attach_tracepoint(
        &self,
        w: &RuntimeWriter<'_>,
        p: Loaded,
        _target: &Tracepoint,
    ) -> Result<Live, Error> {
        self.loaded(w, &p)?;
        self.enter(Point::Attach)?;
        let mut s = self.state.lock().expect("state");
        let id = s.id();
        s.links.insert(
            id,
            Link {
                observation: KernelLink {
                    id,
                    program_id: p.id,
                    details: KernelLinkDetails::PerfEvent,
                },
                handles: 1,
                attached: true,
            },
        );
        drop(s);
        Ok(Live {
            receipt: Receipt {
                root: self.root,
                state: self.state.clone(),
                resource: Resource::Link(id),
                generation: 0,
            },
        })
    }

    fn pin_tracepoint(
        &self,
        w: &RuntimeWriter<'_>,
        live: Live,
        id: NonZeroU64,
    ) -> Acquisition<Receipt> {
        if live.receipt.root != self.root || !Arc::ptr_eq(&live.receipt.state, &self.state) {
            return Err(EffectFailure {
                cause: error(ErrorKind::InvalidData, "foreign live link"),
                remaining: None,
            });
        }
        let Resource::Link(kernel) = live.receipt.resource else {
            unreachable!("link")
        };
        let result = self.acquire(w, Point::PinLink, Resource::Link(kernel));
        if result.is_ok() || result.as_ref().err().is_some_and(|e| e.remaining.is_some()) {
            self.state
                .lock()
                .expect("state")
                .tracepoint_pins
                .insert(id, kernel);
        }
        result
    }

    fn link_id(p: &Receipt) -> NonZeroU32 {
        let Resource::Link(id) = p.resource else {
            unreachable!("link receipt")
        };
        id
    }

    fn observe_link_pin(
        &self,
        w: &RuntimeWriter<'_>,
        id: NonZeroU64,
        program: NonZeroU32,
        kernel: Option<NonZeroU32>,
    ) -> Result<Option<Receipt>, Error> {
        self.writer(w)?;
        let s = self.state.lock().expect("state");
        let Some(found) = s.tracepoint_pins.get(&id).copied() else {
            return Ok(None);
        };
        if s.links[&found].observation.program_id != program || kernel.is_some_and(|k| k != found) {
            return Err(error(ErrorKind::InvalidData, "link mismatch"));
        }
        drop(s);
        Ok(self.observed(Resource::Link(found)))
    }

    fn release_tracepoint(&self, w: &RuntimeWriter<'_>, live: Live) -> Removal<Live> {
        let result = self.writer(w).and_then(|()| {
            if !Arc::ptr_eq(&self.state, &live.receipt.state) {
                return Err(error(ErrorKind::InvalidData, "foreign live link"));
            }
            self.enter(Point::Release)
        });
        result.map_err(|cause| EffectFailure {
            cause,
            remaining: live,
        })
    }

    fn remove_link(&self, w: &RuntimeWriter<'_>, r: Receipt) -> Removal<Receipt> {
        self.remove(w, Point::RemoveLink, r)
    }
}

impl ProgramObservations for FakeKernel {
    fn program(&self, id: NonZeroU32) -> Result<(KernelProgram, Option<ProgramStats>), Error> {
        self.enter(Point::ObserveProgram)?;
        self.state
            .lock()
            .expect("state")
            .programs
            .get(&id)
            .map(|p| (p.observation.clone(), None))
            .ok_or_else(|| error(ErrorKind::Missing, "missing program"))
    }

    fn map(&self, id: u32) -> Result<KernelMap, Error> {
        self.state
            .lock()
            .expect("state")
            .maps
            .get(&id)
            .cloned()
            .ok_or_else(|| error(ErrorKind::Missing, "missing map"))
    }

    fn map_pins(
        &self,
        runtime: &RuntimeDirectory,
        id: NonZeroU32,
    ) -> Result<Vec<bpfman_fs::ObservedMapPin>, Error> {
        self.check_root(runtime.identity().expect("root"))?;
        let s = self.state.lock().expect("state");
        Ok(s.map_names
            .iter()
            .filter(|((p, _), m)| *p == id && s.resources.contains_key(&Resource::Map(**m)))
            .map(|((_, name), id)| bpfman_fs::ObservedMapPin {
                name: name.clone(),
                id: *id,
            })
            .collect())
    }
}

impl LinkObservations for FakeKernel {
    fn tracepoint_link(&self, id: NonZeroU32) -> Result<KernelLink, Error> {
        self.state
            .lock()
            .expect("state")
            .links
            .get(&id)
            .map(|l| l.observation.clone())
            .ok_or_else(|| error(ErrorKind::Missing, "missing link"))
    }

    fn extension_link(&self, id: NonZeroU32) -> Result<KernelLink, Error> {
        self.tracepoint_link(id)
    }

    fn tracepoint_pin(
        &self,
        runtime: &RuntimeDirectory,
        id: NonZeroU64,
    ) -> Result<Option<KernelLink>, Error> {
        self.check_root(runtime.identity().expect("root"))?;
        let s = self.state.lock().expect("state");
        Ok(s.tracepoint_pins
            .get(&id)
            .map(|id| s.links[id].observation.clone()))
    }

    fn extension_pin(
        &self,
        runtime: &RuntimeDirectory,
        link: &XdpLink,
    ) -> Result<Option<KernelLink>, Error> {
        self.check_root(runtime.identity().expect("root"))?;
        let s = self.state.lock().expect("state");
        Ok(s.extensions
            .get(&link.key)
            .map(|id| s.links[id].observation.clone()))
    }
}

impl XdpLifecycle for FakeKernel {
    type PreparedXdp = PreparedXdp;
    type Dispatcher = Loaded;
    type Outer = Receipt;
    type Extension = Receipt;
    type DispatcherPin = Receipt;
    type Revision = Receipt;

    fn prepare_xdp(
        &self,
        w: &RuntimeWriter<'_>,
        program: NonZeroU32,
        interface: &InterfaceName,
    ) -> Result<(XdpKey, PreparedXdp), Error> {
        self.enter(Point::PrepareXdp)?;
        if interface.as_str() != "fake0" {
            return Err(error(ErrorKind::Missing, "missing interface"));
        }
        let extension = self.adopt(w, program, "extension")?;
        let key = XdpKey {
            nsid: NonZeroU64::MIN,
            ifindex: NonZeroU32::MIN,
        };
        Ok((key, PreparedXdp { extension, key }))
    }

    fn load_dispatcher(&self, _proceed_on: XdpProceedOn) -> Result<Loaded, Error> {
        self.enter(Point::LoadDispatcher)?;
        Ok(self.new_program("xdp_dispatcher", "xdp", &[]))
    }

    fn create_revision(&self, w: &RuntimeWriter<'_>, p: &PreparedXdp) -> Acquisition<Receipt> {
        self.loaded(w, &p.extension)
            .map_err(|cause| EffectFailure {
                cause,
                remaining: None,
            })?;
        self.acquire(w, Point::Revision, Resource::Revision(p.key))
    }

    fn pin_dispatcher(
        &self,
        w: &RuntimeWriter<'_>,
        r: &Receipt,
        d: &mut Loaded,
    ) -> Acquisition<Receipt> {
        self.loaded(w, d)
            .and_then(|()| self.validate(w, r))
            .map_err(|cause| EffectFailure {
                cause,
                remaining: None,
            })?;
        self.acquire(w, Point::PinDispatcher, Resource::Dispatcher(d.id))
    }

    fn pin_extension(
        &self,
        w: &RuntimeWriter<'_>,
        p: &mut PreparedXdp,
        r: &Receipt,
        d: &Loaded,
    ) -> Acquisition<Receipt> {
        let fail = |cause| EffectFailure {
            cause,
            remaining: None,
        };
        self.loaded(w, &p.extension)
            .and_then(|()| self.loaded(w, d))
            .and_then(|()| self.validate(w, r))
            .map_err(fail)?;
        self.enter(Point::Extension).map_err(fail)?;
        let mut s = self.state.lock().expect("state");
        let id = s.id();
        s.links.insert(
            id,
            Link {
                observation: KernelLink {
                    id,
                    program_id: p.extension.id,
                    details: KernelLinkDetails::Tracing {
                        attach_type: 0,
                        target_obj_id: d.id.get(),
                        target_btf_id: 1,
                    },
                },
                handles: 0,
                attached: true,
            },
        );
        s.extensions.insert(p.key, id);
        drop(s);
        let receipt = self.insert(Resource::Extension(id)).map_err(fail)?;
        self.after(Point::Extension, receipt)
    }

    fn pin_outer(
        &self,
        w: &RuntimeWriter<'_>,
        p: &PreparedXdp,
        d: &Loaded,
    ) -> Acquisition<Receipt> {
        let fail = |cause| EffectFailure {
            cause,
            remaining: None,
        };
        self.loaded(w, &p.extension)
            .and_then(|()| self.loaded(w, d))
            .map_err(fail)?;
        self.enter(Point::Outer).map_err(fail)?;
        let mut s = self.state.lock().expect("state");
        if s.outers.contains_key(&p.key) {
            return Err(fail(error(ErrorKind::InvalidData, "occupied interface")));
        }
        let id = s.id();
        // The outer's membership and synchronous detach are modeled separately
        // from its diagnostic link kind (public views expose member links only).
        s.links.insert(
            id,
            Link {
                observation: KernelLink {
                    id,
                    program_id: d.id,
                    details: KernelLinkDetails::PerfEvent,
                },
                handles: 0,
                attached: true,
            },
        );
        s.outers.insert(p.key, id);
        drop(s);
        if self.state.lock().expect("state").fault.get(&Point::Outer) == Some(&Phase::After) {
            self.state
                .lock()
                .expect("state")
                .links
                .get_mut(&id)
                .expect("outer")
                .handles = 1;
            let receipt = self.insert(Resource::LiveOuter(id)).map_err(fail)?;
            self.after(Point::Outer, receipt)
        } else {
            self.insert(Resource::Outer(id)).map_err(fail)
        }
    }

    fn dispatcher_id(r: &Receipt) -> NonZeroU32 {
        let Resource::Dispatcher(id) = r.resource else {
            unreachable!("dispatcher")
        };
        id
    }

    fn extension_id(r: &Receipt) -> NonZeroU32 {
        let Resource::Extension(id) = r.resource else {
            unreachable!("extension")
        };
        id
    }

    fn outer_id(r: &Receipt) -> Result<NonZeroU32, Error> {
        let Resource::Outer(id) = r.resource else {
            return Err(error(ErrorKind::InvalidData, "outer"));
        };
        Ok(id)
    }

    fn observe_xdp(
        &self,
        w: &RuntimeWriter<'_>,
        snapshot: &XdpSnapshot,
    ) -> Result<XdpArtifacts<Self>, Error> {
        self.writer(w)?;
        self.enter(Point::ObserveXdp)?;
        let LinkState::Attached { kernel_id } = snapshot.member.state else {
            return Err(error(ErrorKind::InvalidData, "uncommitted XDP link"));
        };
        let s = self.state.lock().expect("state");
        if let Some(link) = s.links.get(&kernel_id) {
            if link.observation.program_id != snapshot.member.program_id
                || !matches!(link.observation.details,KernelLinkDetails::Tracing { target_obj_id,.. } if target_obj_id==snapshot.details.dispatcher_id.get())
            {
                return Err(error(ErrorKind::InvalidData, "extension target changed"));
            }
        }
        if let Some(link) = s.links.get(&snapshot.outer_link_id) {
            if link.observation.program_id != snapshot.details.dispatcher_id {
                return Err(error(ErrorKind::InvalidData, "outer target changed"));
            }
        }
        drop(s);
        Ok(XdpArtifacts {
            outer: self.observed(Resource::Outer(snapshot.outer_link_id)),
            extension: self.observed(Resource::Extension(kernel_id)),
            program: self.observed(Resource::Dispatcher(snapshot.details.dispatcher_id)),
            directory: self.observed(Resource::Revision(snapshot.details.key)),
        })
    }

    fn remove_outer(&self, w: &RuntimeWriter<'_>, r: Receipt) -> Removal<Receipt> {
        self.remove(w, Point::RemoveOuter, r)
    }

    fn remove_extension(&self, w: &RuntimeWriter<'_>, r: Receipt) -> Removal<Receipt> {
        self.remove(w, Point::RemoveExtension, r)
    }

    fn remove_dispatcher(&self, w: &RuntimeWriter<'_>, r: Receipt) -> Removal<Receipt> {
        self.remove(w, Point::RemoveDispatcher, r)
    }

    fn remove_revision(&self, w: &RuntimeWriter<'_>, r: Receipt) -> Removal<Receipt> {
        self.remove(w, Point::RemoveRevision, r)
    }
}
