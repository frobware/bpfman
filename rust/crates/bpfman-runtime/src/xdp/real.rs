use super::*;
use crate::ActiveStore;
use bpfman_store::{OpenStore, ProgramReader, XdpCommit};

pub(super) struct Adapter<'a, S: OpenStore, K>(pub(super) &'a ActiveStore<S>, pub(super) &'a K);

fn map<R>(e: EffectFailure<R, impl Into<LinkCause>>) -> EffectFailure<R, LinkCause> {
    EffectFailure {
        cause: e.cause.into(),
        remaining: e.remaining,
    }
}

impl<S: XdpStore, K: bpfman_kernel::XdpLifecycle> Effects for Adapter<'_, S, K> {
    type Prepared = K::PreparedXdp;
    type Kernel = K::Dispatcher;
    type Outer = K::Outer;
    type Extension = K::Extension;
    type Program = K::DispatcherPin;
    type Directory = K::Revision;
    type Record = S::XdpReceipt;

    fn prepare(
        &mut self,
        w: &RuntimeWriter<'_>,
        r: &XdpAttach,
    ) -> Result<(XdpKey, Self::Prepared), LinkCause> {
        let mut reader = self.0.open(w)?;
        let program = reader
            .read_records()?
            .into_iter()
            .find(|p| p.id == r.program_id)
            .ok_or(Cause::NotFound)?;
        if !matches!(program.spec, bpfman_model::ProgramSpec::Xdp(_)) {
            return Err(Cause::Unsupported.into());
        }
        if w.layout().program_pin_path(program.id).to_str() != Some(program.pin_path.as_str()) {
            return Err(Cause::Invalid("noncanonical managed program pin").into());
        }
        let (key, prepared) = self.1.prepare_xdp(w, r.program_id, &r.interface)?;
        self.0.preflight_xdp(w, key, r.program_id)?;
        Ok((key, prepared))
    }

    fn load(&mut self, r: &XdpAttach) -> Result<K::Dispatcher, LinkCause> {
        self.1.load_dispatcher(r.proceed_on).map_err(Into::into)
    }

    fn directory(
        &mut self,
        w: &RuntimeWriter<'_>,
        p: &Self::Prepared,
    ) -> Result<Self::Directory, EffectFailure<Option<Self::Directory>, LinkCause>> {
        self.1.create_revision(w, p).map_err(map)
    }

    fn program(
        &mut self,
        w: &RuntimeWriter<'_>,
        d: &Self::Directory,
        k: &mut K::Dispatcher,
    ) -> Result<Self::Program, EffectFailure<Option<Self::Program>, LinkCause>> {
        self.1.pin_dispatcher(w, d, k).map_err(map)
    }

    fn extension(
        &mut self,
        w: &RuntimeWriter<'_>,
        p: &mut Self::Prepared,
        d: &Self::Directory,
        k: &K::Dispatcher,
    ) -> Result<Self::Extension, EffectFailure<Option<Self::Extension>, LinkCause>> {
        self.1.pin_extension(w, p, d, k).map_err(map)
    }

    fn outer(
        &mut self,
        w: &RuntimeWriter<'_>,
        p: &Self::Prepared,
        k: &K::Dispatcher,
    ) -> Result<Self::Outer, EffectFailure<Option<Self::Outer>, LinkCause>> {
        self.1.pin_outer(w, p, k).map_err(map)
    }

    fn commit(
        &mut self,
        w: &RuntimeWriter<'_>,
        r: &XdpAttach,
        key: XdpKey,
        p: &Self::Program,
        e: &Self::Extension,
        o: &Self::Outer,
    ) -> Result<StoredLink, LinkCause> {
        let details = XdpLink {
            key,
            interface: r.interface.clone(),
            priority: r.priority,
            proceed_on: r.proceed_on,
            dispatcher_id: K::dispatcher_id(p),
            revision: NonZeroU32::MIN,
        };
        self.0
            .commit_xdp(
                w,
                XdpCommit {
                    program_id: r.program_id,
                    details: &details,
                    extension_link_id: K::extension_id(e),
                    outer_link_id: K::outer_id(o)?,
                    metadata: &r.metadata,
                    created_at: &chrono::Utc::now()
                        .to_rfc3339_opts(chrono::SecondsFormat::Nanos, true),
                },
            )
            .map_err(Into::into)
    }

    fn observe(
        &mut self,
        w: &RuntimeWriter<'_>,
        id: NonZeroU64,
    ) -> Result<Vec<ResourceFor<Self>>, LinkCause> {
        let (snapshot, receipt) = self.0.observe_xdp(w, id)?.ok_or(Cause::NotFound)?;
        if w.layout()
            .xdp_extension_path(snapshot.details.key, snapshot.details.revision)
            .to_str()
            != Some(snapshot.member.pin_path.as_str())
        {
            return Err(Cause::Invalid("noncanonical extension path").into());
        }
        let artifacts = self.1.observe_xdp(w, &snapshot)?;
        let mut resources = vec![Resource::Record(receipt)];
        resources.extend(artifacts.outer.map(Resource::Outer));
        resources.extend(artifacts.extension.map(Resource::Extension));
        resources.extend(artifacts.program.map(Resource::Program));
        resources.extend(artifacts.directory.map(Resource::Directory));
        Ok(resources)
    }

    fn remove_outer(
        &mut self,
        w: &RuntimeWriter<'_>,
        r: Self::Outer,
    ) -> Result<(), EffectFailure<Self::Outer, LinkCause>> {
        self.1.remove_outer(w, r).map_err(map)
    }

    fn remove_extension(
        &mut self,
        w: &RuntimeWriter<'_>,
        r: Self::Extension,
    ) -> Result<(), EffectFailure<Self::Extension, LinkCause>> {
        self.1.remove_extension(w, r).map_err(map)
    }

    fn remove_program(
        &mut self,
        w: &RuntimeWriter<'_>,
        r: Self::Program,
    ) -> Result<(), EffectFailure<Self::Program, LinkCause>> {
        self.1.remove_dispatcher(w, r).map_err(map)
    }

    fn remove_directory(
        &mut self,
        w: &RuntimeWriter<'_>,
        r: Self::Directory,
    ) -> Result<(), EffectFailure<Self::Directory, LinkCause>> {
        self.1.remove_revision(w, r).map_err(map)
    }

    fn remove_record(
        &mut self,
        w: &RuntimeWriter<'_>,
        r: Self::Record,
    ) -> Result<(), EffectFailure<Self::Record, LinkCause>> {
        self.0.delete_xdp(w, r).map_err(map)
    }
}
