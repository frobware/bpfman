use crate::{Bpfman, Cancellation};
use bpfman_fs::{MapReopening, VolumeFailure, VolumeRequest, VolumeState};
use bpfman_store::{OpenStore, ProgramReader};
use std::{
    collections::BTreeSet,
    path::Path,
    sync::{Arc, Mutex},
};

/// CSI lifecycle error categories, independent of protocol and backend implementations.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum VolumeErrorKind {
    /// Cancellation before commit; any acquired mounts were compensated.
    Cancelled,
    /// Program or requested map is absent.
    NotFound,
    /// A request for this volume is already running.
    Busy,
    /// Existing publication has different maps or read-only state.
    AlreadyExists,
    /// An unsafe or foreign filesystem prevents publication or cleanup.
    FailedPrecondition,
    /// A backend operation failed, retaining diagnostics and cleanup ownership.
    Internal,
}

/// Classified CSI lifecycle failure. Unresolved resources remain in MapVolumes.
#[derive(Debug, thiserror::Error)]
#[error("{message}")]
pub struct VolumeError {
    kind: VolumeErrorKind,
    message: String,
    #[source]
    source: Option<Box<dyn std::error::Error + Send + Sync>>,
}

impl VolumeError {
    /// Stable classification for frontend status conversion.
    pub fn kind(&self) -> VolumeErrorKind {
        self.kind
    }
}

fn error(kind: VolumeErrorKind, message: impl Into<String>) -> VolumeError {
    VolumeError {
        kind,
        message: message.into(),
        source: None,
    }
}

fn backend(source: impl std::error::Error + Send + Sync + 'static) -> VolumeError {
    VolumeError {
        kind: VolumeErrorKind::Internal,
        message: "CSI backend operation failed".into(),
        source: Some(Box::new(source)),
    }
}

fn filesystem(source: bpfman_fs::Error) -> VolumeError {
    let kind = match source.kind() {
        bpfman_fs::ErrorKind::Cancelled => VolumeErrorKind::Cancelled,
        bpfman_fs::ErrorKind::UnsafeLayout => VolumeErrorKind::FailedPrecondition,
        _ => VolumeErrorKind::Internal,
    };
    VolumeError {
        kind,
        message: "CSI filesystem operation failed".into(),
        source: Some(Box::new(source)),
    }
}

/// CSI map-volume application sharing the daemon's active store and kernel.
/// Reads do not acquire the program writer; admitted volume mutations do.
pub struct MapVolumes<S: OpenStore, K> {
    app: Arc<Bpfman<S, K>>,
    inflight: Mutex<BTreeSet<String>>,
    recovery: Mutex<Vec<(String, VolumeFailure)>>,
}

struct Inflight<'a> {
    volume: String,
    set: &'a Mutex<BTreeSet<String>>,
}

impl Drop for Inflight<'_> {
    fn drop(&mut self) {
        self.set
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&self.volume);
    }
}

impl<S: OpenStore, K> MapVolumes<S, K> {
    /// Bind the same active application used by the bpfman RPC service.
    pub fn new(app: Arc<Bpfman<S, K>>) -> Self {
        Self {
            app,
            inflight: Mutex::new(BTreeSet::new()),
            recovery: Mutex::new(Vec::new()),
        }
    }

    fn enter(
        &self,
        volume: &str,
        cancellation: &Cancellation,
        teardown: bool,
    ) -> Result<Inflight<'_>, VolumeError> {
        check(cancellation)?;
        let mut active = self
            .inflight
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if !teardown
            && self
                .recovery
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .iter()
                .any(|(id, _)| id == volume)
        {
            return Err(error(
                VolumeErrorKind::FailedPrecondition,
                "volume has unresolved cleanup; explicit recovery is required",
            ));
        }
        if !active.insert(volume.into()) {
            return Err(error(
                VolumeErrorKind::Busy,
                "an operation is already in progress for volume",
            ));
        }
        Ok(Inflight {
            volume: volume.into(),
            set: &self.inflight,
        })
    }

    /// Take retained failures and their consuming cleanup receipts for explicit recovery.
    pub fn take_recovery(&self) -> Vec<VolumeFailure> {
        self.take_pending()
            .into_iter()
            .map(|(_, failure)| failure)
            .collect()
    }

    fn take_pending(&self) -> Vec<(String, VolumeFailure)> {
        std::mem::take(
            &mut *self
                .recovery
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
        )
    }

    fn finish(
        &self,
        volume: &str,
        result: Result<(), VolumeFailure>,
        cancellation: &Cancellation,
    ) -> Result<(), VolumeError> {
        result.map_err(|failure| {
            let mut status = if cancellation.is_cancelled() {
                error(VolumeErrorKind::Cancelled, "CSI operation cancelled")
            } else {
                let kind = if failure.cause.kind() == bpfman_fs::ErrorKind::UnsafeLayout {
                    VolumeErrorKind::FailedPrecondition
                } else {
                    VolumeErrorKind::Internal
                };
                error(kind, "CSI volume operation failed")
            };
            if failure.pending.is_some() {
                status.source = Some(Box::new(failure.cause.clone()));
                self.recovery
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .push((volume.into(), failure));
            } else {
                status.source = Some(Box::new(failure));
            }
            status
        })
    }

    /// Teardown completes its cleanup pass after admission, even if cancelled.
    pub fn unpublish(
        &self,
        volume: &str,
        target: &Path,
        cancellation: &Cancellation,
    ) -> Result<(), VolumeError> {
        bpfman_fs::VolumeRequest::validate_target(volume, target).map_err(filesystem)?;
        let _active = self.enter(volume, cancellation, true)?;
        let result = self
            .app
            .store
            .runtime()
            .with_writer(
                bpfman_lock::AcquireOptions {
                    timeout: self.app.lock_timeout,
                    cancelled: Some(cancellation.flag()),
                },
                |writer| {
                    check(cancellation)?;
                    let prior = {
                        let mut pending = self
                            .recovery
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner);
                        if let Some(index) = pending.iter().position(|(id, _)| id == volume) {
                            if !pending[index]
                                .1
                                .pending
                                .as_ref()
                                .is_some_and(|p| p.matches_target(target))
                            {
                                return Err(error(
                                    VolumeErrorKind::FailedPrecondition,
                                    "pending volume cleanup names a different target",
                                ));
                            }
                            Some(pending.remove(index).1)
                        } else {
                            None
                        }
                    };
                    if let Some(mut failure) = prior {
                        if let Some(owned) = failure.pending.take() {
                            let (pending, errors) = writer.cleanup_volume(owned);
                            failure.pending = pending;
                            failure.cleanup.extend(errors);
                        }
                        if failure.pending.is_some() {
                            return Ok(Err(failure));
                        }
                    }
                    // A repeated RPC is an explicit retry. Once its retained pass
                    // succeeds, finish the requested teardown (including a target
                    // directory deliberately preserved by publication rollback).
                    Ok(writer.unpublish_volume(volume, target))
                },
            )
            .map_err(filesystem)??;
        self.finish(volume, result, &Cancellation::new())
    }

    /// Retry only unresolved CSI acquisitions in a separately admitted pass.
    pub fn retry_cleanup(&self, cancellation: &Cancellation) -> Result<usize, VolumeError> {
        check(cancellation)?;
        self.app
            .store
            .runtime()
            .with_writer(
                bpfman_lock::AcquireOptions {
                    timeout: self.app.lock_timeout,
                    cancelled: Some(cancellation.flag()),
                },
                |writer| {
                    check(cancellation)?;
                    for (volume, mut failure) in self.take_pending() {
                        if let Some(pending) = failure.pending.take() {
                            let (pending, errors) = writer.cleanup_volume(pending);
                            failure.pending = pending;
                            failure.cleanup.extend(errors);
                        }
                        if failure.pending.is_some() {
                            self.recovery
                                .lock()
                                .unwrap_or_else(std::sync::PoisonError::into_inner)
                                .push((volume, failure));
                        }
                    }
                    Ok(self
                        .recovery
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .len())
                },
            )
            .map_err(filesystem)?
    }
}

fn check(cancellation: &Cancellation) -> Result<(), VolumeError> {
    if cancellation.is_cancelled() {
        Err(error(VolumeErrorKind::Cancelled, "CSI operation cancelled"))
    } else {
        Ok(())
    }
}

impl<S: OpenStore, K: bpfman_kernel::ProgramObservations + MapReopening> MapVolumes<S, K> {
    /// Resolve metadata against live managed programs under the writer lock,
    /// then pin the requested maps and publish a durable mount.
    pub fn publish(
        &self,
        program: &str,
        request: &VolumeRequest,
        cancellation: &Cancellation,
    ) -> Result<(), VolumeError> {
        let _active = self.enter(request.volume(), cancellation, false)?;
        self.app
            .store
            .runtime()
            .with_writer(
                bpfman_lock::AcquireOptions {
                    timeout: self.app.lock_timeout,
                    cancelled: Some(cancellation.flag()),
                },
                |writer| {
                    check(cancellation)?;
                    match writer.volume_state(request).map_err(filesystem)? {
                        VolumeState::Matches => return Ok(()),
                        VolumeState::Different => {
                            return Err(error(
                                VolumeErrorKind::AlreadyExists,
                                "volume already published with different arguments",
                            ));
                        }
                        VolumeState::Absent => (),
                    }
                    let mut reader = self.app.store.reader();
                    let mut records = reader.read_records().map_err(backend)?;
                    records.sort_by_key(|r| r.id);
                    let mut found = None;
                    for record in records.into_iter().filter(|r| {
                        r.metadata
                            .get("bpfman.io/ProgramName")
                            .is_some_and(|n| n == program)
                    }) {
                        check(cancellation)?;
                        match self.app.kernel.program(record.id) {
                            Ok((live, _)) => {
                                found = Some((record, live));
                                break;
                            }
                            Err(e) if e.kind() == bpfman_kernel::ErrorKind::Missing => (),
                            Err(e) => return Err(backend(e)),
                        }
                    }
                    let (record, live) = found.ok_or_else(|| {
                        error(
                            VolumeErrorKind::NotFound,
                            format!("program {program:?} not found"),
                        )
                    })?;
                    let pins = self
                        .app
                        .kernel
                        .map_pins(writer.directory(), record.map_set)
                        .map_err(backend)?;
                    let map_ids = live.map_ids.as_ref().ok_or_else(|| {
                        error(
                            VolumeErrorKind::Internal,
                            "kernel map membership is unavailable",
                        )
                    })?;
                    for name in request.maps() {
                        if !pins
                            .iter()
                            .any(|p| &p.name == name && map_ids.contains(&p.id))
                        {
                            return Err(error(
                                VolumeErrorKind::NotFound,
                                format!("map {name:?} is not published by program {program:?}"),
                            ));
                        }
                    }
                    let result = writer.publish_volume(
                        request,
                        record.map_set,
                        &self.app.kernel,
                        cancellation.flag(),
                    );
                    self.finish(request.volume(), result, cancellation)
                },
            )
            .map_err(filesystem)?
    }
}
