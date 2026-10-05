use super::*;
use crate::ActiveStore;
use aya::programs::Xdp;
use bpfman_store::{OpenStore, ProgramReader, XdpCommit};

pub(super) struct Adapter<'a, S: OpenStore>(pub(super) &'a ActiveStore<S>);

fn map<R>(e: EffectFailure<R, impl Into<LinkCause>>) -> EffectFailure<R, LinkCause> {
    EffectFailure {
        cause: e.cause.into(),
        remaining: e.remaining,
    }
}

fn kernel(e: impl std::error::Error + Send + Sync + 'static) -> LinkCause {
    Cause::Xdp(Box::new(e)).into()
}

impl<S: XdpStore> Effects for Adapter<'_, S> {
    type Prepared = bpfman_fs::PreparedXdp;
    type Kernel = aya::Ebpf;
    type Outer = bpfman_fs::XdpOuter;
    type Extension = bpfman_fs::XdpExtensionPin;
    type Program = bpfman_fs::XdpProgramPin;
    type Directory = bpfman_fs::XdpRevision;
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
        let prepared = w.prepare_xdp(r.program_id, &r.interface)?;
        self.0.preflight_xdp(w, prepared.key(), r.program_id)?;
        Ok((prepared.key(), prepared))
    }

    fn load(&mut self, r: &XdpAttach) -> Result<aya::Ebpf, LinkCause> {
        let config = bpfman_model::xdp_config(r.proceed_on);
        let mut bpf = aya::EbpfLoader::new()
            .override_global("conf", config.as_slice(), true)
            .load(aya::include_bytes_aligned!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/../../../dispatcher/xdp_dispatcher_v2.bpf.o"
            )))
            .map_err(kernel)?;
        let program: &mut Xdp = bpf
            .program_mut("xdp_dispatcher")
            .ok_or(Cause::Invalid("embedded XDP dispatcher missing"))?
            .try_into()
            .map_err(kernel)?;
        program.load().map_err(kernel)?;
        Ok(bpf)
    }

    fn directory(
        &mut self,
        w: &RuntimeWriter<'_>,
        p: &Self::Prepared,
    ) -> Result<Self::Directory, EffectFailure<Option<Self::Directory>, LinkCause>> {
        p.create_revision(w, NonZeroU32::MIN).map_err(map)
    }

    fn program(
        &mut self,
        w: &RuntimeWriter<'_>,
        d: &Self::Directory,
        k: &mut aya::Ebpf,
    ) -> Result<Self::Program, EffectFailure<Option<Self::Program>, LinkCause>> {
        let program: &mut Xdp = k
            .program_mut("xdp_dispatcher")
            .ok_or_else(|| EffectFailure {
                cause: Cause::Invalid("dispatcher missing").into(),
                remaining: None,
            })?
            .try_into()
            .map_err(|e| EffectFailure {
                cause: kernel(e),
                remaining: None,
            })?;
        d.pin_program(w, program).map_err(map)
    }

    fn extension(
        &mut self,
        w: &RuntimeWriter<'_>,
        p: &mut Self::Prepared,
        d: &Self::Directory,
        k: &aya::Ebpf,
    ) -> Result<Self::Extension, EffectFailure<Option<Self::Extension>, LinkCause>> {
        p.pin_extension(
            w,
            d,
            target(k).map_err(|cause| EffectFailure {
                cause,
                remaining: None,
            })?,
        )
        .map_err(map)
    }

    fn outer(
        &mut self,
        w: &RuntimeWriter<'_>,
        p: &Self::Prepared,
        k: &aya::Ebpf,
    ) -> Result<Self::Outer, EffectFailure<Option<Self::Outer>, LinkCause>> {
        p.pin_outer(
            w,
            target(k).map_err(|cause| EffectFailure {
                cause,
                remaining: None,
            })?,
        )
        .map_err(map)
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
            dispatcher_id: p.id(),
            revision: NonZeroU32::MIN,
        };
        self.0
            .commit_xdp(
                w,
                XdpCommit {
                    program_id: r.program_id,
                    details: &details,
                    extension_link_id: e.id(),
                    outer_link_id: o.id()?,
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
        let artifacts = w.observe_xdp(&snapshot)?;
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
        w.remove_xdp_outer(r).map_err(map)
    }

    fn remove_extension(
        &mut self,
        w: &RuntimeWriter<'_>,
        r: Self::Extension,
    ) -> Result<(), EffectFailure<Self::Extension, LinkCause>> {
        w.remove_xdp_extension(r).map_err(map)
    }

    fn remove_program(
        &mut self,
        w: &RuntimeWriter<'_>,
        r: Self::Program,
    ) -> Result<(), EffectFailure<Self::Program, LinkCause>> {
        w.remove_xdp_program(r).map_err(map)
    }

    fn remove_directory(
        &mut self,
        w: &RuntimeWriter<'_>,
        r: Self::Directory,
    ) -> Result<(), EffectFailure<Self::Directory, LinkCause>> {
        w.remove_xdp_revision(r).map_err(map)
    }

    fn remove_record(
        &mut self,
        w: &RuntimeWriter<'_>,
        r: Self::Record,
    ) -> Result<(), EffectFailure<Self::Record, LinkCause>> {
        self.0.delete_xdp(w, r).map_err(map)
    }
}

fn target(bpf: &aya::Ebpf) -> Result<&Xdp, LinkCause> {
    bpf.program("xdp_dispatcher")
        .ok_or(Cause::Invalid("dispatcher missing"))?
        .try_into()
        .map_err(kernel)
}
