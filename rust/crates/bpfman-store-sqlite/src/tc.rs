use crate::{Backend, TcReceipt, error::Failure, open, queries::tc as queries};
use bpfman_core::EffectFailure;
use bpfman_fs::{RuntimeLayout, RuntimeWriter};
use bpfman_model::{LinkDetails, LinkState, StoredLink, TcLink, TcSnapshot, XdpKey};
use bpfman_store::{Error, TcCommit, TcStore};
use rusqlite::{Connection, OpenFlags, TransactionBehavior};
use std::{
    num::{NonZeroU32, NonZeroU64},
    os::unix::fs::MetadataExt,
};

fn invalid() -> Failure {
    Failure::InvalidLink("invalid TC snapshot")
}

fn id32(raw: i64) -> Result<NonZeroU32, Failure> {
    u32::try_from(raw)
        .ok()
        .and_then(NonZeroU32::new)
        .ok_or_else(invalid)
}

fn id64(raw: i64) -> Result<NonZeroU64, Failure> {
    u64::try_from(raw)
        .ok()
        .and_then(NonZeroU64::new)
        .ok_or_else(invalid)
}

pub(super) fn decode(row: &queries::Row) -> Result<TcSnapshot, Failure> {
    if row.program_kind != "tc"
        || row.netns != row.dispatcher_netns
        || row.direction != "ingress"
        || row.position != 0
        || row.revision != 1
        || row.filter_priority != 50
        || row.priority < 0
        || row.priority > i32::MAX as i64
    {
        return Err(Failure::Unsupported(
            "unsupported TC snapshot or mismatched namespace paths",
        ));
    }
    let actions: Vec<i32> = serde_json::from_str(&row.proceed_on).map_err(Failure::LinkMetadata)?;
    let mut mask = 0;
    for code in actions {
        if !matches!(code, -1..=8 | 30) {
            return Err(invalid());
        }
        mask |= 1 << (code + 1);
    }
    let details = TcLink {
        netns: row.netns.parse().map_err(|_| invalid())?,
        key: XdpKey {
            nsid: id64(row.nsid)?,
            ifindex: id32(row.ifindex)?,
        },
        interface: row.interface.parse().map_err(|_| invalid())?,
        priority: row.priority as u32,
        proceed_on: mask.try_into().map_err(|_| invalid())?,
        dispatcher_id: id32(row.dispatcher)?,
        filter_handle: id32(row.filter_handle)?,
        filter_priority: 50,
    };
    crate::records::timestamp(row.dispatcher_created.clone(), row.program)?;
    crate::records::timestamp(row.dispatcher_updated.clone(), row.program)?;
    Ok(TcSnapshot {
        program_name: row
            .program_name
            .as_str()
            .try_into()
            .map_err(|_| invalid())?,
        program_pin_path: row.program_pin.clone(),
        details: details.clone(),
        member: StoredLink {
            id: id64(row.id)?,
            program_id: id32(row.program)?,
            details: LinkDetails::Tc(details),
            state: LinkState::Attached {
                kernel_id: id32(row.kernel)?,
            },
            pin_path: row.pin.clone(),
            metadata: serde_json::from_str::<Option<_>>(&row.metadata)
                .map_err(Failure::LinkMetadata)?
                .unwrap_or_default(),
            created_at: crate::records::timestamp(row.created.clone(), row.program)?,
        },
    })
}

fn canonical(row: &queries::Row, layout: &RuntimeLayout) -> Result<TcSnapshot, Failure> {
    let s = decode(row)?;
    if layout.tc_extension_path(s.details.key).to_str() != Some(s.member.pin_path.as_str()) {
        return Err(Failure::InvalidLink("noncanonical TC extension path"));
    }
    if layout.program_pin_path(s.member.program_id).to_str() != Some(s.program_pin_path.as_str()) {
        return Err(Failure::InvalidLink("noncanonical TC program path"));
    }
    Ok(s)
}

fn connection(w: &RuntimeWriter<'_>) -> Result<Connection, Failure> {
    let c = Connection::open_with_flags(w.database_path(), OpenFlags::SQLITE_OPEN_READ_WRITE)?;
    c.busy_timeout(std::time::Duration::from_secs(5))?;
    c.pragma_update(None, "foreign_keys", true)?;
    open::require_supported(open::schema_version(&c)?)?;
    Ok(c)
}

fn identity(w: &RuntimeWriter<'_>) -> Result<(u64, u64), Failure> {
    let path = w.database_path();
    let m = std::fs::metadata(&path).map_err(|source| Failure::Filesystem { path, source })?;
    Ok((m.dev(), m.ino()))
}

fn preflight(c: &Connection, key: XdpKey, program: NonZeroU32) -> Result<(), Failure> {
    open::require_supported(open::schema_version(c)?)?;
    if !queries::vacant(c, key)? {
        return Err(Failure::Unsupported(
            "TC dispatcher replacement is not implemented; attach point is occupied",
        ));
    }
    if !queries::is_tc(c, program.get())? {
        return Err(Failure::InvalidLink("missing managed TC program"));
    }
    Ok(())
}

impl TcStore for Backend {
    type TcReceipt = TcReceipt;

    fn preflight_tc(
        &self,
        w: &RuntimeWriter<'_>,
        key: XdpKey,
        program: NonZeroU32,
    ) -> Result<(), Error> {
        (|| -> Result<(), Failure> { preflight(&connection(w)?, key, program) })()
            .map_err(crate::Error::from)
            .map_err(Into::into)
    }

    fn commit_tc(&self, w: &RuntimeWriter<'_>, request: TcCommit<'_>) -> Result<StoredLink, Error> {
        (|| -> Result<_, Failure> {
            let mut c = connection(w)?;
            let tx = c.transaction_with_behavior(TransactionBehavior::Immediate)?;
            preflight(&tx, request.details.key, request.program_id)?;
            let metadata =
                serde_json::to_string(request.metadata).map_err(Failure::LinkMetadata)?;
            let codes: Vec<i32> = (-1..=30)
                .filter(|code| request.details.proceed_on.mask() & (1 << (code + 1)) != 0)
                .collect();
            let actions = serde_json::to_string(&codes).map_err(Failure::LinkMetadata)?;
            let pin = w.layout().tc_extension_path(request.details.key);
            let pin = pin.to_str().ok_or_else(invalid)?;
            let id = queries::insert(&tx, &request, pin, &metadata, &actions)?;
            let row = queries::rows(&tx, None, Some(id))?
                .pop()
                .ok_or_else(invalid)?;
            let record = canonical(&row, w.layout())?.member;
            tx.commit()?;
            Ok(record)
        })()
        .map_err(crate::Error::from)
        .map_err(Into::into)
    }

    fn observe_tc(
        &self,
        w: &RuntimeWriter<'_>,
        link: NonZeroU64,
    ) -> Result<Option<(TcSnapshot, TcReceipt)>, Error> {
        (|| -> Result<_, Failure> {
            let mut c = connection(w)?;
            let tx = c.transaction()?;
            let Ok(id) = i64::try_from(link.get()) else {
                return Ok(None);
            };
            let Some(row) = queries::rows(&tx, None, Some(id))?.pop() else {
                return Ok(None);
            };
            let snapshot = canonical(&row, w.layout())?;
            if queries::rows(&tx, Some(snapshot.details.key), None)?.len() != 1 {
                return Err(Failure::Unsupported("multi-member TC detach"));
            }
            let receipt = TcReceipt {
                root: w.identity()?,
                database: identity(w)?,
                row,
            };
            Ok(Some((snapshot, receipt)))
        })()
        .map_err(crate::Error::from)
        .map_err(Into::into)
    }

    fn delete_tc(
        &self,
        w: &RuntimeWriter<'_>,
        receipt: TcReceipt,
    ) -> Result<(), EffectFailure<TcReceipt, Error>> {
        let result = (|| -> Result<(), Failure> {
            if w.identity()? != receipt.root || identity(w)? != receipt.database {
                return Err(Failure::InvalidLink(
                    "TC receipt belongs to another runtime or store",
                ));
            }
            let mut c = connection(w)?;
            let tx = c.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let first = &receipt.row;
            let snapshot = canonical(first, w.layout())?;
            let rows = queries::rows(&tx, Some(snapshot.details.key), None)?;
            if rows != vec![receipt.row.clone()] {
                return Err(Failure::InvalidLink(
                    "TC snapshot changed since observation",
                ));
            }
            queries::remove_member(&tx, receipt.row.id)?;
            queries::remove_dispatcher(&tx, first)?;
            tx.commit()?;
            Ok(())
        })();
        result.map_err(|cause| EffectFailure {
            cause: crate::Error::from(cause).into(),
            remaining: receipt,
        })
    }
}
