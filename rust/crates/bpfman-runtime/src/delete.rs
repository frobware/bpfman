//! Cascading deletion composes existing teardown interpreters under one writer.
use crate::{Bpfman, Cancellation, DeleteError, DeleteOutcome, DeleteProgress, ProgramDelete};
use bpfman_fs::RuntimeWriter;
use bpfman_kernel::{ProgramResources, TcLifecycle, TracepointLinks, XdpReplacement};
use bpfman_lock::AcquireOptions;
use bpfman_store::{
    LinkReader, LinkStore, OpenStore, ProgramReader, TcStore, UnloadStore, XdpReplacementStore,
};
use std::{
    collections::{BTreeMap, BTreeSet},
    num::{NonZeroU32, NonZeroU64},
};

#[derive(Default)]
struct Batch {
    dependants: BTreeMap<NonZeroU32, Vec<NonZeroU32>>,
    programs: BTreeSet<NonZeroU32>,
    links: BTreeSet<NonZeroU64>,
    attempted_programs: BTreeSet<NonZeroU32>,
    attempted_links: BTreeSet<NonZeroU64>,
}

impl<S, K> Bpfman<S, K>
where
    S: UnloadStore + LinkStore + XdpReplacementStore + TcStore,
    S::Reader: LinkReader,
    K: ProgramResources + TracepointLinks + XdpReplacement + TcLifecycle,
{
    /// Delete selected programs, processing selected shared-map dependants first.
    pub fn delete_programs(
        &self,
        selection: ProgramDelete<'_>,
    ) -> Result<Vec<DeleteOutcome<NonZeroU32, S, K>>, crate::Error> {
        self.delete_programs_with_cancellation(selection, &Cancellation::new())
    }

    /// Select and order targets under one cancellable writer scope. Individual
    /// failures do not stop independent targets. Original receipts remain in the
    /// outcomes; overlapping failed resources are never retried within the batch.
    #[tracing::instrument(name = "program.delete", level = "debug", skip_all, err)]
    pub fn delete_programs_with_cancellation(
        &self,
        selection: ProgramDelete<'_>,
        cancellation: &Cancellation,
    ) -> Result<Vec<DeleteOutcome<NonZeroU32, S, K>>, crate::Error> {
        self.store
            .runtime()
            .with_writer(
                AcquireOptions {
                    timeout: self.lock_timeout,
                    cancelled: Some(cancellation.flag()),
                },
                |writer| {
                    cancellation.check()?;
                    let (ids, recursive) = match selection {
                        ProgramDelete::Ids { ids, recursive } => (ids.to_vec(), recursive),
                        ProgramDelete::All => (
                            self.store
                                .open(&writer)
                                .map_err(crate::error::store_error)?
                                .read_records()
                                .map_err(crate::error::store_error)?
                                .into_iter()
                                .map(|r| r.id)
                                .collect(),
                            true,
                        ),
                    };
                    let mut batch = self.delete_batch(&writer, recursive || ids.len() > 1)?;
                    cancellation.check()?;
                    let ordered = bpfman_core::order_program_deletes(&ids, &batch.dependants);
                    let mut results = Vec::with_capacity(ordered.len());
                    for id in ordered {
                        let mut progress = Vec::new();
                        let outcome = self.delete_program(
                            &writer,
                            id,
                            recursive,
                            &mut batch,
                            &mut progress,
                            cancellation,
                        );
                        results.push(DeleteOutcome {
                            id,
                            progress,
                            outcome,
                        });
                    }
                    Ok(results)
                },
            )
            .map_err(crate::error::filesystem_error)?
    }

    /// Detach selected links, unloading an owner only after its last link is gone.
    pub fn delete_links(
        &self,
        ids: &[NonZeroU64],
        recursive: bool,
    ) -> Result<Vec<DeleteOutcome<NonZeroU64, S, K>>, crate::Error> {
        self.delete_links_with_cancellation(ids, recursive, &Cancellation::new())
    }

    /// Keep selection, detach, orphan detection and optional dependant deletion
    /// under one writer. Cancellation prevents the next underlying teardown from
    /// starting; an admitted teardown finishes and retains its report.
    #[tracing::instrument(name = "link.delete", level = "debug", skip_all, fields(count = ids.len()), err)]
    pub fn delete_links_with_cancellation(
        &self,
        ids: &[NonZeroU64],
        recursive: bool,
        cancellation: &Cancellation,
    ) -> Result<Vec<DeleteOutcome<NonZeroU64, S, K>>, crate::Error> {
        self.store
            .runtime()
            .with_writer(
                AcquireOptions {
                    timeout: self.lock_timeout,
                    cancelled: Some(cancellation.flag()),
                },
                |writer| {
                    cancellation.check()?;
                    let mut batch = self.delete_batch(&writer, recursive)?;
                    cancellation.check()?;
                    let mut results = Vec::with_capacity(ids.len());
                    for &id in ids {
                        let mut progress = Vec::new();
                        let outcome = (|| {
                            if batch.links.contains(&id) {
                                return Ok(());
                            }
                            cancellation.check().map_err(DeleteError::Observation)?;
                            let links = self.delete_read_links(&writer)?;
                            let owner = links.iter().find(|r| r.id == id).map(|r| r.program_id);
                            self.delete_detach(
                                &writer,
                                id,
                                &mut batch,
                                &mut progress,
                                cancellation,
                            )?;
                            if let Some(owner) = owner
                                && !self
                                    .delete_read_links(&writer)?
                                    .iter()
                                    .any(|r| r.program_id == owner)
                            {
                                self.delete_program(
                                    &writer,
                                    owner,
                                    recursive,
                                    &mut batch,
                                    &mut progress,
                                    cancellation,
                                )?;
                            }
                            Ok(())
                        })();
                        results.push(DeleteOutcome {
                            id,
                            progress,
                            outcome,
                        });
                    }
                    Ok(results)
                },
            )
            .map_err(crate::error::filesystem_error)?
    }

    fn delete_batch(
        &self,
        writer: &RuntimeWriter<'_>,
        relationships: bool,
    ) -> Result<Batch, crate::Error> {
        let mut batch = Batch::default();
        if relationships {
            for record in self
                .store
                .open(writer)
                .map_err(crate::error::store_error)?
                .read_records()
                .map_err(crate::error::store_error)?
            {
                if record.id != record.map_set {
                    batch
                        .dependants
                        .entry(record.map_set)
                        .or_default()
                        .push(record.id);
                }
            }
            for ids in batch.dependants.values_mut() {
                ids.sort_unstable();
            }
        }
        Ok(batch)
    }

    fn delete_read_links(
        &self,
        writer: &RuntimeWriter<'_>,
    ) -> Result<Vec<bpfman_model::StoredLink>, DeleteError<S, K>> {
        self.store
            .open(writer)
            .and_then(|mut reader| reader.read_links())
            .map_err(|e| DeleteError::Observation(crate::error::store_error(e)))
    }

    fn delete_detach(
        &self,
        writer: &RuntimeWriter<'_>,
        id: NonZeroU64,
        batch: &mut Batch,
        progress: &mut Vec<DeleteProgress<S, K>>,
        cancellation: &Cancellation,
    ) -> Result<(), DeleteError<S, K>> {
        if batch.links.contains(&id) {
            return Ok(());
        }
        cancellation.check().map_err(DeleteError::Observation)?;
        if !batch.attempted_links.insert(id) {
            return Err(DeleteError::BlockedLink(id));
        }
        let report = self
            .detach_under_writer(writer, id, cancellation)
            .map_err(|error| {
                if error.is_link_missing() {
                    batch.attempted_links.remove(&id);
                }
                DeleteError::Link {
                    id,
                    error: Box::new(error),
                }
            })?;
        batch.links.insert(id);
        progress.push(DeleteProgress::Link { id, report });
        Ok(())
    }

    fn delete_program(
        &self,
        writer: &RuntimeWriter<'_>,
        id: NonZeroU32,
        recursive: bool,
        batch: &mut Batch,
        progress: &mut Vec<DeleteProgress<S, K>>,
        cancellation: &Cancellation,
    ) -> Result<(), DeleteError<S, K>> {
        if batch.programs.contains(&id) {
            return Ok(());
        }
        cancellation.check().map_err(DeleteError::Observation)?;
        if !batch.attempted_programs.insert(id) {
            return Err(DeleteError::BlockedProgram(id));
        }
        if recursive {
            let dependants = batch.dependants.get(&id).cloned().unwrap_or_default();
            for dependant in dependants {
                self.delete_program(writer, dependant, true, batch, progress, cancellation)?;
            }
        }
        for link in self
            .delete_read_links(writer)?
            .into_iter()
            .filter(|r| r.program_id == id)
        {
            self.delete_detach(writer, link.id, batch, progress, cancellation)?;
        }
        let report = self
            .unload_under_writer(writer, id, cancellation)
            .map_err(|error| {
                if error.is_program_missing() {
                    batch.attempted_programs.remove(&id);
                }
                DeleteError::Program { id, error }
            })?;
        batch.programs.insert(id);
        progress.push(DeleteProgress::Program { id, report });
        Ok(())
    }
}
