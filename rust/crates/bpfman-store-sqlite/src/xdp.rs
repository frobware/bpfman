use crate::{Backend, Store, XdpReceipt, error::Failure, open, queries::xdp as queries};
use bpfman_core::EffectFailure;
use bpfman_fs::{RuntimeLayout, RuntimeWriter};
use bpfman_model::{LinkDetails, LinkState, StoredLink, XdpKey, XdpLink, XdpSnapshot};
use bpfman_store::{Error, XdpCommit, XdpReader, XdpStore};
use rusqlite::{Connection, OpenFlags, TransactionBehavior};
use std::{
    num::{NonZeroU32, NonZeroU64},
    os::unix::fs::MetadataExt,
};

fn invalid() -> Failure {
    Failure::InvalidLink("invalid single-member XDP snapshot")
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

pub(super) fn decode(row: &queries::Row) -> Result<XdpSnapshot, Failure> {
    if row.program_kind != "xdp"
        || row.position != 0
        || !row.netns.is_empty()
        || !row.dispatcher_netns.is_empty()
        || row.kernel == row.outer
        || row.priority < 0
        || row.priority > i32::MAX as i64
    {
        return Err(Failure::Unsupported(
            "only single-member XDP dispatchers in the current namespace are implemented",
        ));
    }
    let actions: Vec<u32> = serde_json::from_str(&row.proceed_on).map_err(Failure::LinkMetadata)?;
    let mut mask = 0;
    for code in actions {
        if !matches!(code, 0..=4 | 31) {
            return Err(invalid());
        }
        mask |= 1 << code;
    }
    let details = XdpLink {
        key: XdpKey {
            nsid: id64(row.nsid)?,
            ifindex: id32(row.ifindex)?,
        },
        interface: row.interface.parse().map_err(|_| invalid())?,
        priority: row.priority as u32,
        proceed_on: mask.try_into().map_err(|_| invalid())?,
        dispatcher_id: id32(row.dispatcher)?,
        revision: id32(row.revision)?,
    };
    crate::records::timestamp(row.dispatcher_created.clone(), row.program)?;
    crate::records::timestamp(row.dispatcher_updated.clone(), row.program)?;
    Ok(XdpSnapshot {
        program_name: row
            .program_name
            .as_str()
            .try_into()
            .map_err(|_| invalid())?,
        program_pin_path: row.program_pin.clone(),
        details: details.clone(),
        outer_link_id: id32(row.outer)?,
        member: StoredLink {
            id: id64(row.id)?,
            program_id: id32(row.program)?,
            details: LinkDetails::Xdp(details),
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

fn canonical(row: &queries::Row, layout: &RuntimeLayout) -> Result<XdpSnapshot, Failure> {
    let s = decode(row)?;
    if layout
        .xdp_extension_path(s.details.key, s.details.revision)
        .to_str()
        != Some(s.member.pin_path.as_str())
    {
        return Err(Failure::InvalidLink("noncanonical XDP extension path"));
    }
    if layout.program_pin_path(s.member.program_id).to_str() != Some(s.program_pin_path.as_str()) {
        return Err(Failure::InvalidLink("noncanonical XDP program path"));
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
            "XDP dispatcher replacement is not implemented; attach point is occupied",
        ));
    }
    if !queries::is_xdp(c, program.get())? {
        return Err(Failure::InvalidLink("missing managed XDP program"));
    }
    Ok(())
}

impl XdpReader for Store {
    fn read_xdp(&mut self, key: XdpKey) -> Result<Option<XdpSnapshot>, Error> {
        self.reader
            .read(|c| {
                let tx = c.transaction()?;
                open::require_supported(open::schema_version(&tx)?)?;
                let rows = queries::rows(&tx, Some(key), None)?;
                if rows.len() > 1 {
                    return Err(Failure::Unsupported("multi-member XDP snapshot"));
                }
                let row = rows.first();
                if row.is_none() && !queries::vacant(&tx, key)? {
                    return Err(invalid());
                }
                row.map(decode).transpose()
            })
            .map_err(crate::Error::from)
            .map_err(Into::into)
    }
}

impl XdpStore for Backend {
    type XdpReceipt = XdpReceipt;

    fn preflight_xdp(
        &self,
        w: &RuntimeWriter<'_>,
        key: XdpKey,
        program: NonZeroU32,
    ) -> Result<(), Error> {
        (|| -> Result<(), Failure> { preflight(&connection(w)?, key, program) })()
            .map_err(crate::Error::from)
            .map_err(Into::into)
    }

    fn commit_xdp(
        &self,
        w: &RuntimeWriter<'_>,
        request: XdpCommit<'_>,
    ) -> Result<StoredLink, Error> {
        request.validate()?;
        (|| -> Result<_, Failure> {
            let mut c = connection(w)?;
            let tx = c.transaction_with_behavior(TransactionBehavior::Immediate)?;
            preflight(&tx, request.details.key, request.program_id)?;
            let metadata =
                serde_json::to_string(request.metadata).map_err(Failure::LinkMetadata)?;
            let codes: Vec<_> = (0..32)
                .filter(|code| request.details.proceed_on.mask() & (1 << code) != 0)
                .collect();
            let actions = serde_json::to_string(&codes).map_err(Failure::LinkMetadata)?;
            let pin = w
                .layout()
                .xdp_extension_path(request.details.key, request.details.revision);
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

    fn observe_xdp(
        &self,
        w: &RuntimeWriter<'_>,
        link: NonZeroU64,
    ) -> Result<Option<(XdpSnapshot, XdpReceipt)>, Error> {
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
                return Err(Failure::Unsupported("multi-member XDP detach"));
            }
            let receipt = XdpReceipt {
                root: w.identity()?,
                database: identity(w)?,
                row,
            };
            Ok(Some((snapshot, receipt)))
        })()
        .map_err(crate::Error::from)
        .map_err(Into::into)
    }

    fn delete_xdp(
        &self,
        w: &RuntimeWriter<'_>,
        receipt: XdpReceipt,
    ) -> Result<(), EffectFailure<XdpReceipt, Error>> {
        let result = (|| -> Result<(), Failure> {
            if w.identity()? != receipt.root || identity(w)? != receipt.database {
                return Err(Failure::InvalidLink(
                    "XDP receipt belongs to another runtime or store",
                ));
            }
            let mut c = connection(w)?;
            let tx = c.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let snapshot = canonical(&receipt.row, w.layout())?;
            let rows = queries::rows(&tx, Some(snapshot.details.key), None)?;
            if rows != [receipt.row.clone()] {
                return Err(Failure::InvalidLink(
                    "XDP snapshot changed since observation",
                ));
            }
            queries::remove(&tx, &receipt.row)?;
            tx.commit()?;
            Ok(())
        })();
        result.map_err(|cause| EffectFailure {
            cause: crate::Error::from(cause).into(),
            remaining: receipt,
        })
    }
}
