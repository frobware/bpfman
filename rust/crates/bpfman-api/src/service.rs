use crate::convert::{self, internal, invalid};
use bpfman_proto::v1 as pb;
use bpfman_runtime::{Bpfman, Cancellation, PreparedProgram};
use grpcuds::Status;
use prost::Message;
use std::{
    num::{NonZeroU32, NonZeroU64},
    path::Path,
    sync::Mutex,
};

/// Kernel capabilities needed by the existing seven RPCs.
/// The frontend never depends on a concrete kernel implementation.
pub trait Kernel:
    bpfman_kernel::ProgramLoad
    + bpfman_kernel::ProgramObservations
    + bpfman_kernel::LinkObservations
    + bpfman_kernel::KprobeLinks
    + bpfman_kernel::UprobeLinks
    + bpfman_kernel::TracingLinks
    + bpfman_kernel::TcxLinks
    + bpfman_kernel::XdpReplacement
    + bpfman_kernel::TcLifecycle
{
}

impl<T> Kernel for T where
    T: bpfman_kernel::ProgramLoad
        + bpfman_kernel::ProgramObservations
        + bpfman_kernel::LinkObservations
        + bpfman_kernel::KprobeLinks
        + bpfman_kernel::UprobeLinks
        + bpfman_kernel::TracingLinks
        + bpfman_kernel::TcxLinks
        + bpfman_kernel::XdpReplacement
        + bpfman_kernel::TcLifecycle
{
}

/// Image preparation supplied by the composition root, including signing policy.
/// The frontend normalizes incomplete credentials to anonymous access.
/// Implementations validate captured bytes before cache publication. Credentials
/// remain at this frontend boundary.
pub trait Images {
    /// Fetch or reuse bytecode with caller-owned cancellation. The validator
    /// checks all selections and globals before the image enters the cache.
    fn pull(
        &self,
        image: &pb::BytecodeImage,
        cancellation: &Cancellation,
        validate: &dyn Fn(&[u8]) -> Result<(), bpfman_kernel::Error>,
    ) -> Result<(Vec<u8>, bpfman_model::ProgramSource), Status>;
}

/// Retained cleanup ownership returned by an RPC that could not finish cleanup.
/// A caller may take these receipts and explicitly retry through its runtime.
pub enum Recovery<S, K>
where
    S: bpfman_store::UnloadStore
        + bpfman_store::LinkStore
        + bpfman_store::XdpStore
        + bpfman_store::TcStore,
    K: Kernel,
{
    /// Failed load and its remaining acquisitions.
    Load(bpfman_runtime::LoadError<K>),
    /// Standalone attachment compensation.
    Link(bpfman_runtime::LinkError<S, K>),
    /// Dispatcher replacement or cleanup.
    Xdp(bpfman_runtime::XdpError<S, K>),
    /// Legacy TC replacement or cleanup.
    Tc(bpfman_runtime::TcError<S, K>),
    /// Attached-program unload cleanup.
    Unload(bpfman_runtime::UnloadError<S, K>),
    /// Warning-only cleanup after successful unload.
    UnloadReport(bpfman_runtime::UnloadReport<S, K>),
}

type XdpModes =
    dyn Fn(&bpfman_model::InterfaceName) -> Result<bpfman_model::XdpMode, Status> + Send + Sync;

/// Generic synchronous implementation of the bpfman.v1 service.
/// Request workers borrow one active application; no operation-wide mutex
/// serializes readers. The inspector is used only for pre-load ELF validation.
pub struct Service<S, K, I>
where
    S: bpfman_store::UnloadStore
        + bpfman_store::LinkStore
        + bpfman_store::XdpStore
        + bpfman_store::TcStore,
    K: Kernel,
{
    app: std::sync::Arc<Bpfman<S, K>>,
    inspector: K,
    images: I,
    mode: Box<XdpModes>,
    recovery: Mutex<Vec<Recovery<S, K>>>,
}

impl<S, K, I> Service<S, K, I>
where
    S: bpfman_store::UnloadStore
        + bpfman_store::CommitLoad
        + bpfman_store::LinkStore
        + bpfman_store::XdpReplacementStore
        + bpfman_store::TcStore
        + 'static,
    S::Reader: bpfman_store::LinkReader,
    K: Kernel + 'static,
    I: Images,
{
    /// Bind one application and its stateless ELF inspector and image policy.
    pub fn new(app: Bpfman<S, K>, inspector: K, images: I, mode: bpfman_model::XdpMode) -> Self {
        Self::from_shared(std::sync::Arc::new(app), inspector, images, mode)
    }

    /// Share one active application with another daemon frontend such as CSI.
    pub fn from_shared(
        app: std::sync::Arc<Bpfman<S, K>>,
        inspector: K,
        images: I,
        mode: bpfman_model::XdpMode,
    ) -> Self {
        Self {
            app,
            inspector,
            images,
            mode: Box::new(move |_| Ok(mode)),
            recovery: Mutex::new(Vec::new()),
        }
    }

    /// Resolve configured XDP mode independently for each requested interface.
    pub fn with_xdp_modes(
        mut self,
        resolve: impl Fn(&bpfman_model::InterfaceName) -> Result<bpfman_model::XdpMode, Status>
        + Send
        + Sync
        + 'static,
    ) -> Self {
        self.mode = Box::new(resolve);
        self
    }

    /// Take retained cleanup receipts for explicit recovery. Poisoned ownership
    /// is recovered rather than discarded; this method performs no effects.
    pub fn take_recovery(&self) -> Vec<Recovery<S, K>> {
        let mut pending = self
            .recovery
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        std::mem::take(&mut *pending)
    }

    /// Attempt each retained cleanup once. Cancellation prevents admission;
    /// an admitted compensation pass completes under the runtime writer lock.
    /// Returns the number of recovery items still owning unresolved resources.
    pub fn retry_cleanup(&self, cancellation: &Cancellation) -> usize {
        for item in self.take_recovery() {
            match item {
                Recovery::Load(error) => {
                    let error = self
                        .app
                        .retry_load_cleanup_with_cancellation(error, cancellation);
                    if error.unresolved() > 0 {
                        self.retain(Recovery::Load(error));
                    }
                }
                Recovery::Link(error) => {
                    if let Err(error) = self
                        .app
                        .retry_link_cleanup_with_cancellation(error, cancellation)
                    {
                        if error.report().is_some_and(|r| r.unresolved() > 0) {
                            self.retain(Recovery::Link(error));
                        }
                    }
                }
                Recovery::Xdp(error) => {
                    if let Err(error) = self
                        .app
                        .retry_xdp_cleanup_with_cancellation(error, cancellation)
                    {
                        if error.unresolved() > 0 {
                            self.retain(Recovery::Xdp(error));
                        }
                    }
                }
                Recovery::Tc(error) => {
                    if let Err(error) = self
                        .app
                        .retry_tc_cleanup_with_cancellation(error, cancellation)
                    {
                        if error.unresolved() > 0 {
                            self.retain(Recovery::Tc(error));
                        }
                    }
                }
                Recovery::Unload(error) => {
                    self.keep_unload(self.app.retry_unload_with_cancellation(error, cancellation));
                }
                Recovery::UnloadReport(report) => {
                    self.keep_unload(
                        self.app
                            .retry_unload_cleanup_with_cancellation(report, cancellation),
                    );
                }
            }
        }
        self.recovery
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .len()
    }

    fn keep_unload(
        &self,
        outcome: Result<bpfman_runtime::UnloadReport<S, K>, bpfman_runtime::UnloadError<S, K>>,
    ) {
        match outcome {
            Ok(report) if report.unresolved() > 0 => self.retain(Recovery::UnloadReport(report)),
            Err(error) if error.report().is_some_and(|r| r.unresolved() > 0) => {
                self.retain(Recovery::Unload(error))
            }
            _ => {}
        }
    }

    fn retain(&self, failure: Recovery<S, K>) {
        self.recovery
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(failure);
    }

    /// Dispatch an existing fully qualified unary method with protobuf bytes.
    /// Cancellation remains active during preparation, lock waits and effects.
    pub fn handle(
        &self,
        method: &str,
        bytes: &[u8],
        cancellation: &Cancellation,
    ) -> Result<Vec<u8>, Status> {
        if cancellation.is_cancelled() {
            return Err(Status::cancelled("request cancelled"));
        }
        match method {
            "/bpfman.v1.Bpfman/Load" => self
                .load(
                    pb::LoadRequest::decode(bytes).map_err(invalid)?,
                    cancellation,
                )
                .map(|v| v.encode_to_vec()),
            "/bpfman.v1.Bpfman/Unload" => {
                let request = pb::UnloadRequest::decode(bytes).map_err(invalid)?;
                self.unload(request.id, cancellation)?;
                Ok(pb::UnloadResponse {}.encode_to_vec())
            }
            "/bpfman.v1.Bpfman/Attach" => self
                .attach(
                    pb::AttachRequest::decode(bytes).map_err(invalid)?,
                    cancellation,
                )
                .map(|v| v.encode_to_vec()),
            "/bpfman.v1.Bpfman/Detach" => {
                let request = pb::DetachRequest::decode(bytes).map_err(invalid)?;
                self.detach(request.link_id, cancellation)?;
                Ok(pb::DetachResponse {}.encode_to_vec())
            }
            "/bpfman.v1.Bpfman/List" => self
                .list(
                    pb::ListRequest::decode(bytes).map_err(invalid)?,
                    cancellation,
                )
                .map(|v| v.encode_to_vec()),
            "/bpfman.v1.Bpfman/Get" => self
                .get(
                    pb::GetRequest::decode(bytes).map_err(invalid)?,
                    cancellation,
                )
                .map(|v| v.encode_to_vec()),
            "/bpfman.v1.Bpfman/PullBytecode" => {
                let request = pb::PullBytecodeRequest::decode(bytes).map_err(invalid)?;
                let image = request
                    .image
                    .ok_or_else(|| Status::invalid_argument("image is required"))?;
                convert::policy(&image)?;
                self.images
                    .pull(&convert::image(&image), cancellation, &|_| Ok(()))?;
                Ok(pb::PullBytecodeResponse {}.encode_to_vec())
            }
            _ => Err(Status::unimplemented("unknown RPC method")),
        }
    }

    fn load(
        &self,
        request: pb::LoadRequest,
        cancellation: &Cancellation,
    ) -> Result<pb::LoadResponse, Status> {
        let bytecode = request
            .bytecode
            .as_ref()
            .ok_or_else(|| Status::invalid_argument("bytecode location is required"))?;
        let specs = request
            .info
            .iter()
            .map(|info| convert::spec(info, &request.metadata))
            .collect::<Result<Vec<_>, _>>()?;
        let mut specs = specs.into_iter();
        let first = specs
            .next()
            .ok_or_else(|| Status::invalid_argument("at least one program info is required"))?;
        let remaining: Vec<_> = specs.collect();
        let prepared = match bytecode.location.as_ref() {
            Some(pb::bytecode_location::Location::File(path)) => {
                PreparedProgram::new_with_cancellation(
                    &self.inspector,
                    Path::new(path),
                    first,
                    request.metadata.clone(),
                    cancellation,
                )
            }
            Some(pb::bytecode_location::Location::Image(image)) => {
                convert::policy(image)?;
                let (bytes, source) =
                    self.images
                        .pull(&convert::image(image), cancellation, &|bytes| {
                            for spec in std::iter::once(&first).chain(&remaining) {
                                self.inspector.validate_object(bytes, spec)?;
                            }
                            self.inspector.validate_globals(bytes, &request.global_data)
                        })?;
                PreparedProgram::from_image(
                    &self.inspector,
                    bytes,
                    source,
                    first,
                    request.metadata.clone(),
                    cancellation,
                )
            }
            None => return Err(Status::invalid_argument("invalid bytecode location")),
        }
        .map_err(|e| self.load_error(e))?
        .with_globals(&self.inspector, request.global_data.clone())
        .map_err(|e| self.load_error(e))?;
        let prepared = match request.map_owner_id.and_then(NonZeroU32::new) {
            Some(id) => prepared.with_map_owner(id),
            None => prepared,
        };
        let prepared = prepared
            .with_additional_programs(&self.inspector, remaining)
            .map_err(|e| self.load_error(e))?;
        let loaded = self
            .app
            .load_batch_with_cancellation(prepared, cancellation)
            .map_err(|e| self.load_error(e))?;
        Ok(pb::LoadResponse {
            programs: loaded
                .into_iter()
                .map(|program| {
                    let mut info = convert::info(&program.record, &program.map_used_by, false);
                    // Preserve Go's load response, including the original bytecode oneof.
                    // Registry credentials are never persisted or logged.
                    info.bytecode = request.bytecode.clone();
                    // Go's Load projection reads Load.MapOwnerID, which stays
                    // unset for manager-resolved map joins. Get/List read the
                    // durable handle instead. Preserve that wire distinction.
                    info.map_owner_id = None;
                    pb::LoadResponseInfo {
                        info: Some(info),
                        kernel_info: Some(convert::kernel(
                            &program.record,
                            &program.kernel,
                            convert::View::Load,
                        )),
                    }
                })
                .collect(),
        })
    }

    fn load_error(&self, error: bpfman_runtime::LoadError<K>) -> Status {
        let status = if error.is_map_owner_missing() {
            invalid(&error)
        } else if error.kind() == bpfman_runtime::LoadErrorKind::Cancelled {
            Status::cancelled("request cancelled")
        } else {
            internal(&error)
        };
        if error.unresolved() > 0 {
            self.retain(Recovery::Load(error));
        }
        status
    }

    fn list(
        &self,
        request: pb::ListRequest,
        cancellation: &Cancellation,
    ) -> Result<pb::ListResponse, Status> {
        let types = request
            .program_type
            .map(|kind| {
                i32::try_from(kind)
                    .map_err(invalid)
                    .and_then(convert::program_type)
            })
            .transpose()?
            .into_iter()
            .collect();
        let filter = bpfman_core::ProgramFilter {
            types,
            metadata: request.match_metadata,
            ..Default::default()
        };
        // Go ignores bpfman_programs_only and only emits live managed records.
        let all = self
            .app
            .list_entries_with_cancellation(&Default::default(), cancellation)
            .map_err(convert::observation)?;
        let selected: std::collections::BTreeSet<_> = bpfman_core::select_records(
            all.iter().map(|entry| entry.record.clone()).collect(),
            &filter,
        )
        .into_iter()
        .map(|record| record.id)
        .collect();
        Ok(pb::ListResponse {
            results: all
                .iter()
                .filter(|entry| selected.contains(&entry.record.id))
                .cloned()
                .filter_map(|entry| {
                    let kernel = entry.kernel?;
                    let users: Vec<_> = all
                        .iter()
                        .filter(|p| p.record.map_set == entry.record.map_set)
                        .map(|p| p.record.id)
                        .collect();
                    Some(pb::list_response::ListResult {
                        info: Some(convert::info(&entry.record, &users, false)),
                        kernel_info: Some(convert::kernel(
                            &entry.record,
                            &kernel,
                            convert::View::List,
                        )),
                    })
                })
                .collect(),
        })
    }

    fn get(
        &self,
        request: pb::GetRequest,
        cancellation: &Cancellation,
    ) -> Result<pb::GetResponse, Status> {
        let id =
            NonZeroU32::new(request.id).ok_or_else(|| Status::not_found("program 0 not found"))?;
        let program = self
            .app
            .get_with_cancellation(id, cancellation)
            .map_err(convert::observation)?;
        Ok(pb::GetResponse {
            info: Some(convert::info(&program.record, &program.map_used_by, true)),
            kernel_info: Some(convert::kernel(
                &program.record,
                &program.kernel,
                convert::View::Get,
            )),
        })
    }

    fn unload(&self, id: u32, cancellation: &Cancellation) -> Result<(), Status> {
        let id = NonZeroU32::new(id).ok_or_else(|| Status::not_found("program 0 not found"))?;
        match self.app.unload_with_cancellation(id, cancellation) {
            Ok(report) => {
                if report.unresolved() > 0 {
                    self.retain(Recovery::UnloadReport(report));
                }
                Ok(())
            }
            Err(error) => {
                let status = if error.is_program_missing() {
                    Status::not_found(error.to_string())
                } else if error.kind() == bpfman_runtime::UnloadErrorKind::Cancelled {
                    Status::cancelled("request cancelled")
                } else {
                    internal(&error)
                };
                if error.report().is_some_and(|r| r.unresolved() > 0) {
                    self.retain(Recovery::Unload(error));
                }
                Err(status)
            }
        }
    }

    fn detach(&self, id: u32, cancellation: &Cancellation) -> Result<(), Status> {
        let id =
            NonZeroU64::new(u64::from(id)).ok_or_else(|| Status::not_found("link 0 not found"))?;
        let outcomes = self
            .app
            .detach_many_with_cancellation(&[id], cancellation)
            .map_err(convert::application)?;
        for outcome in outcomes {
            if let Err(error) = outcome.outcome {
                let status = if error.is_link_missing() {
                    Status::not_found(error.to_string())
                } else if matches!(
                    error.kind(),
                    bpfman_runtime::LinkErrorKind::Cancelled
                        | bpfman_runtime::LinkErrorKind::TimedOut
                ) {
                    convert::link(error.kind(), &error)
                } else {
                    internal(&error)
                };
                match error {
                    bpfman_runtime::DetachError::Standalone(e)
                        if e.report().is_some_and(|r| r.unresolved() > 0) =>
                    {
                        self.retain(Recovery::Link(e))
                    }
                    bpfman_runtime::DetachError::Xdp(e) if e.unresolved() > 0 => {
                        self.retain(Recovery::Xdp(e))
                    }
                    bpfman_runtime::DetachError::Tc(e) if e.unresolved() > 0 => {
                        self.retain(Recovery::Tc(e))
                    }
                    _ => {}
                }
                return Err(status);
            }
        }
        Ok(())
    }
}

mod attach;
