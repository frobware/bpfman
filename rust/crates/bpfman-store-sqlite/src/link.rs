//! Atomic link intent/finalisation. No kernel lifetime decisions belong here.

use crate::{Backend, LinkReceipt, Store, error::Failure, open, queries};
use bpfman_core::EffectFailure;
use bpfman_fs::{RuntimeIdentity, RuntimeWriter};
use bpfman_model::{LinkDetails, LinkState, StoredLink};
use bpfman_store::{Error, LinkObservation, LinkReader, LinkStore, PendingTracepoint};
use rusqlite::{Connection, OpenFlags, TransactionBehavior};
use std::{
    num::{NonZeroU32, NonZeroU64},
    os::unix::fs::MetadataExt,
};

pub(super) struct Evidence {
    root: RuntimeIdentity,
    database: (u64, u64),
    row: queries::StoredLinkRow,
}

fn database_identity(writer: &RuntimeWriter<'_>) -> Result<(u64, u64), Failure> {
    let path = writer.database_path();
    let meta = std::fs::metadata(&path).map_err(|source| Failure::Filesystem { path, source })?;

    Ok((meta.dev(), meta.ino()))
}

fn connection(writer: &RuntimeWriter<'_>) -> Result<Connection, Failure> {
    let connection =
        Connection::open_with_flags(writer.database_path(), OpenFlags::SQLITE_OPEN_READ_WRITE)?;
    connection.busy_timeout(std::time::Duration::from_secs(5))?;
    connection.pragma_update(None, "foreign_keys", true)?;
    open::require_supported(open::schema_version(&connection)?)?;
    connection.pragma_update(None, "journal_mode", "WAL")?;

    Ok(connection)
}

fn decode(row: &queries::StoredLinkRow) -> Result<StoredLink, Failure> {
    if row.kind != "tracepoint" {
        return Err(Failure::UnsupportedLink);
    }

    if row.program_kind.as_deref() != Some("tracepoint") {
        return Err(Failure::InvalidLink("missing or non-tracepoint program"));
    }

    let id = u64::try_from(row.id)
        .ok()
        .and_then(NonZeroU64::new)
        .ok_or(Failure::InvalidLink("invalid managed ID"))?;
    let program_id = u32::try_from(row.program_id)
        .ok()
        .and_then(NonZeroU32::new)
        .ok_or(Failure::InvalidLink("invalid program ID"))?;
    let state = match row.kernel_id {
        None => LinkState::Pending,
        Some(raw) => LinkState::Attached {
            kernel_id: u32::try_from(raw)
                .ok()
                .and_then(NonZeroU32::new)
                .ok_or(Failure::InvalidLink("invalid kernel ID"))?,
        },
    };
    let group = row
        .group
        .as_deref()
        .ok_or(Failure::InvalidLink("missing tracepoint details"))?;
    let name = row
        .name
        .as_deref()
        .ok_or(Failure::InvalidLink("missing tracepoint details"))?;
    let target = format!("{group}/{name}")
        .parse()
        .map_err(|_| Failure::InvalidLink("invalid tracepoint target"))?;
    let metadata = serde_json::from_str::<Option<_>>(&row.metadata)
        .map_err(Failure::LinkMetadata)?
        .unwrap_or_default();

    Ok(StoredLink {
        id,
        program_id,
        state,
        details: LinkDetails::Tracepoint(target),
        pin_path: row
            .pin_path
            .clone()
            .filter(|p| !p.is_empty())
            .ok_or(Failure::InvalidLink("missing pin path"))?,
        metadata,
        created_at: crate::records::timestamp(row.created_at.clone(), row.program_id)?,
    })
}

fn one(connection: &Connection, id: i64) -> Result<queries::StoredLinkRow, Failure> {
    queries::stored_links(connection, Some(id))?
        .pop()
        .ok_or(Failure::InvalidLink("link record disappeared"))
}

fn canonical(
    writer: &RuntimeWriter<'_>,
    row: &queries::StoredLinkRow,
) -> Result<StoredLink, Failure> {
    let record = decode(row)?;

    if writer.layout().link_pin_path(record.id).to_str() != Some(record.pin_path.as_str()) {
        return Err(Failure::InvalidLink(
            "pin path differs from canonical runtime layout",
        ));
    }

    Ok(record)
}

fn evidence(
    writer: &RuntimeWriter<'_>,
    row: queries::StoredLinkRow,
) -> Result<LinkReceipt, Failure> {
    Ok(LinkReceipt {
        evidence: Box::new(Evidence {
            root: writer.identity()?,
            database: database_identity(writer)?,
            row,
        }),
    })
}

fn check_identity(writer: &RuntimeWriter<'_>, receipt: &LinkReceipt) -> Result<(), Failure> {
    if writer.identity()? != receipt.evidence.root
        || database_identity(writer)? != receipt.evidence.database
    {
        return Err(Failure::InvalidLink(
            "receipt belongs to another runtime or store",
        ));
    }

    Ok(())
}

fn check(
    writer: &RuntimeWriter<'_>,
    connection: &Connection,
    receipt: &LinkReceipt,
) -> Result<(), Failure> {
    open::require_supported(open::schema_version(connection)?)?;

    let row = one(connection, receipt.evidence.row.id)?;
    canonical(writer, &row)?;

    if row != receipt.evidence.row {
        return Err(Failure::InvalidLink("link changed since observation"));
    }

    Ok(())
}

impl LinkReader for Store {
    #[tracing::instrument(name = "store.read_links", level = "debug", skip_all, err)]
    fn read_links(&mut self) -> Result<Vec<StoredLink>, Error> {
        self.reader
            .read(|connection| {
                let tx = connection.transaction()?;
                open::require_supported(open::schema_version(&tx)?)?;
                let mut links = queries::stored_links(&tx, None)?
                    .iter()
                    .filter(|row| row.kind != "xdp")
                    .map(decode)
                    .collect::<Result<Vec<_>, _>>()?;
                links.extend(
                    queries::xdp::rows(&tx, None, None)?
                        .iter()
                        .map(|row| crate::xdp::decode(row).map(|s| s.member))
                        .collect::<Result<Vec<_>, _>>()?,
                );
                links.sort_by_key(|l| l.id);
                Ok(links)
            })
            .map_err(crate::Error::from)
            .map_err(Into::into)
    }
}

impl LinkStore for Backend {
    type LinkReceipt = LinkReceipt;

    #[tracing::instrument(name = "store.create_pending_link", level = "debug", skip_all, err)]
    fn create_pending_tracepoint(
        &self,
        writer: &RuntimeWriter<'_>,
        request: PendingTracepoint<'_>,
    ) -> Result<(StoredLink, LinkReceipt), Error> {
        (|| -> Result<_, Failure> {
            let mut connection = connection(writer)?;
            let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
            open::require_supported(open::schema_version(&tx)?)?;

            if !queries::is_tracepoint(&tx, request.program_id)? {
                return Err(Failure::InvalidLink("missing or non-tracepoint program"));
            }

            let metadata =
                serde_json::to_string(request.metadata).map_err(Failure::LinkMetadata)?;
            let id = queries::insert_pending_link(
                &tx,
                queries::PendingLinkInsert {
                    program_id: request.program_id,
                    metadata: &metadata,
                    created_at: request.created_at,
                    group: request.target.group(),
                    name: request.target.name(),
                },
            )?;
            let managed = u64::try_from(id)
                .ok()
                .and_then(NonZeroU64::new)
                .ok_or(Failure::InvalidLink("invalid allocated ID"))?;
            let path = writer.layout().link_pin_path(managed);
            let path = path
                .to_str()
                .ok_or(Failure::InvalidLink("runtime path is not UTF-8"))?;

            if queries::set_link_pin(&tx, id, path)? != 1 {
                return Err(Failure::InvalidLink("pending link disappeared"));
            }

            let row = one(&tx, id)?;
            let record = canonical(writer, &row)?;
            let receipt = evidence(writer, row)?;
            tx.commit()?;

            Ok((record, receipt))
        })()
        .map_err(crate::Error::from)
        .map_err(Into::into)
    }

    #[tracing::instrument(name = "store.finalise_link", level = "debug", skip_all)]
    fn finalise_link(
        &self,
        writer: &RuntimeWriter<'_>,
        receipt: LinkReceipt,
        kernel_id: NonZeroU32,
    ) -> Result<StoredLink, EffectFailure<LinkReceipt, Error>> {
        let result = (|| -> Result<_, Failure> {
            check_identity(writer, &receipt)?;
            let mut connection = connection(writer)?;
            let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
            check(writer, &tx, &receipt)?;

            if queries::kernel_link_exists(&tx, kernel_id)? {
                return Err(Failure::InvalidLink("kernel link ID already recorded"));
            }

            if queries::finalise_link(&tx, receipt.evidence.row.id, kernel_id)? != 1 {
                return Err(Failure::InvalidLink("link is not pending"));
            }

            let record = canonical(writer, &one(&tx, receipt.evidence.row.id)?)?;
            tx.commit()?;

            Ok(record)
        })();

        result.map_err(|cause| EffectFailure {
            remaining: receipt,
            cause: crate::Error::from(cause).into(),
        })
    }

    #[tracing::instrument(name = "store.observe_link", level = "debug", skip_all, err)]
    fn observe_link(
        &self,
        writer: &RuntimeWriter<'_>,
        id: NonZeroU64,
    ) -> Result<LinkObservation<Self>, Error> {
        (|| -> Result<_, Failure> {
            let mut connection = connection(writer)?;
            let tx = connection.transaction()?;
            open::require_supported(open::schema_version(&tx)?)?;
            let Ok(id) = i64::try_from(id.get()) else {
                return Ok(None);
            };
            let Some(row) = queries::stored_links(&tx, Some(id))?.pop() else {
                return Ok(None);
            };
            let record = canonical(writer, &row)?;

            Ok(Some((record, evidence(writer, row)?)))
        })()
        .map_err(crate::Error::from)
        .map_err(Into::into)
    }

    #[tracing::instrument(name = "store.delete_link", level = "debug", skip_all)]
    fn delete_link(
        &self,
        writer: &RuntimeWriter<'_>,
        receipt: LinkReceipt,
    ) -> Result<(), EffectFailure<LinkReceipt, Error>> {
        let result = (|| -> Result<(), Failure> {
            check_identity(writer, &receipt)?;
            let mut connection = connection(writer)?;
            let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
            check(writer, &tx, &receipt)?;

            if queries::delete_link(&tx, receipt.evidence.row.id)? != 1 {
                return Err(Failure::InvalidLink("link deletion did not remove its row"));
            }

            tx.commit()?;

            Ok(())
        })();

        result.map_err(|cause| EffectFailure {
            remaining: receipt,
            cause: crate::Error::from(cause).into(),
        })
    }
}
