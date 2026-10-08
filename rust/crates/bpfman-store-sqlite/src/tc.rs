use crate::{Backend, TcReceipt, error::Failure, open, queries::tc as queries};
use bpfman_core::EffectFailure;
use bpfman_fs::{RuntimeLayout, RuntimeWriter};
use bpfman_model::{
    LinkDetails, LinkState, StoredLink, TcDispatcherSnapshot, TcLink, TcSnapshot, XdpKey,
};
use bpfman_store::{Error, TcCommit, TcReplace, TcStore};
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
        || !(0..10).contains(&row.position)
        || row.revision <= 0
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
        revision: id32(row.revision)?,
        slot: usize::try_from(row.position)
            .map_err(|_| invalid())?
            .try_into()
            .map_err(|_| invalid())?,
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

pub(super) fn snapshots(c: &Connection) -> Result<Vec<TcDispatcherSnapshot>, Failure> {
    let rows = queries::rows(c, None, None)?;
    let mut result = Vec::new();
    let mut total = 0;
    for (kind, nsid, ifindex) in crate::queries::xdp::dispatcher_keys(c)? {
        if kind != "tc-ingress" {
            continue;
        }
        let selected = rows
            .iter()
            .filter(|r| r.nsid == nsid && r.ifindex == ifindex)
            .map(decode)
            .collect::<Result<Vec<_>, _>>()?;
        total += selected.len();
        result.push(TcDispatcherSnapshot::new(selected).map_err(|_| invalid())?);
    }
    if total != rows.len() {
        return Err(invalid());
    }
    Ok(result)
}

fn canonical(row: &queries::Row, layout: &RuntimeLayout) -> Result<TcSnapshot, Failure> {
    let s = decode(row)?;
    if layout
        .tc_slot_path(s.details.key, s.details.revision, s.details.slot)
        .to_str()
        != Some(s.member.pin_path.as_str())
    {
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
        return Err(Failure::Unsupported("TC ingress attach point is occupied"));
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
            let pin = w.layout().tc_slot_path(
                request.details.key,
                request.details.revision,
                request.details.slot,
            );
            let pin = pin.to_str().ok_or_else(invalid)?;
            if request.details.revision != NonZeroU32::MIN
                || request.details.slot != bpfman_model::XdpSlot::FIRST
            {
                return Err(invalid());
            }
            let id = queries::insert(&tx, &request, pin, &metadata, &actions, None, true)?;
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
                rows: vec![row],
            };
            Ok(Some((snapshot, receipt)))
        })()
        .map_err(crate::Error::from)
        .map_err(Into::into)
    }

    fn observe_tc_member_dispatcher(
        &self,
        w: &RuntimeWriter<'_>,
        id: NonZeroU64,
    ) -> Result<Option<(TcDispatcherSnapshot, TcReceipt)>, Error> {
        let key = (|| -> Result<_, Failure> {
            let c = connection(w)?;
            let Ok(id) = i64::try_from(id.get()) else {
                return Ok(None);
            };
            queries::rows(&c, None, Some(id))?
                .first()
                .map(|r| canonical(r, w.layout()).map(|s| s.details.key))
                .transpose()
        })()
        .map_err(crate::Error::from)?;
        match key {
            Some(key) => self.observe_tc_dispatcher(w, key),
            None => Ok(None),
        }
    }

    fn observe_tc_dispatcher(
        &self,
        w: &RuntimeWriter<'_>,
        key: XdpKey,
    ) -> Result<Option<(TcDispatcherSnapshot, TcReceipt)>, Error> {
        (|| -> Result<_, Failure> {
            let mut c = connection(w)?;
            let tx = c.transaction()?;
            let rows = queries::rows(&tx, Some(key), None)?;
            if rows.is_empty() {
                return Ok(None);
            }
            let snapshot = TcDispatcherSnapshot::new(
                rows.iter()
                    .map(|r| canonical(r, w.layout()))
                    .collect::<Result<_, _>>()?,
            )
            .map_err(|_| invalid())?;
            Ok(Some((
                snapshot,
                TcReceipt {
                    root: w.identity()?,
                    database: identity(w)?,
                    rows,
                },
            )))
        })()
        .map_err(crate::Error::from)
        .map_err(Into::into)
    }

    fn replace_tc(
        &self,
        w: &RuntimeWriter<'_>,
        receipt: TcReceipt,
        request: TcReplace<'_>,
    ) -> Result<TcDispatcherSnapshot, EffectFailure<TcReceipt, Error>> {
        let result = (|| -> Result<_, Failure> {
            if w.identity()? != receipt.root || identity(w)? != receipt.database {
                return Err(invalid());
            }
            let mut c = connection(w)?;
            let tx = c.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let old = TcDispatcherSnapshot::new(
                receipt
                    .rows
                    .iter()
                    .map(|r| canonical(r, w.layout()))
                    .collect::<Result<_, _>>()?,
            )
            .map_err(|_| invalid())?;
            let first = receipt.rows.first().ok_or_else(invalid)?;
            let key = old.members()[0].details.key;
            if queries::rows(&tx, Some(key), None)? != receipt.rows {
                return Err(invalid());
            }
            request.validate(&old).map_err(|_| invalid())?;
            for r in &receipt.rows {
                queries::remove_member(&tx, r.id)?;
            }
            queries::remove_dispatcher(&tx, first)?;
            for (index, m) in request.members.iter().enumerate() {
                let a = &m.attachment;
                if !queries::is_tc(&tx, a.program_id.get())? {
                    return Err(invalid());
                }
                let pin = w
                    .layout()
                    .tc_slot_path(key, a.details.revision, a.details.slot);
                let metadata = serde_json::to_string(a.metadata).map_err(Failure::LinkMetadata)?;
                let codes: Vec<i32> = (-1..=30)
                    .filter(|code| a.details.proceed_on.mask() & (1 << (code + 1)) != 0)
                    .collect();
                let actions = serde_json::to_string(&codes).map_err(Failure::LinkMetadata)?;
                let id = match m.identity {
                    bpfman_store::XdpMemberId::New => None,
                    bpfman_store::XdpMemberId::Existing(id) => {
                        Some(i64::try_from(id.get()).map_err(|_| invalid())?)
                    }
                };
                queries::insert(
                    &tx,
                    a,
                    pin.to_str().ok_or_else(invalid)?,
                    &metadata,
                    &actions,
                    id,
                    index == 0,
                )?;
            }
            queries::updated(&tx, key, request.updated_at)?;
            let result = TcDispatcherSnapshot::new(
                queries::rows(&tx, Some(key), None)?
                    .iter()
                    .map(|r| canonical(r, w.layout()))
                    .collect::<Result<_, _>>()?,
            )
            .map_err(|_| invalid())?;
            tx.commit()?;
            Ok(result)
        })();
        result.map_err(|cause| EffectFailure {
            cause: crate::Error::from(cause).into(),
            remaining: receipt,
        })
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
            let first = receipt.rows.first().ok_or_else(invalid)?;
            let snapshot = canonical(first, w.layout())?;
            let rows = queries::rows(&tx, Some(snapshot.details.key), None)?;
            if rows != receipt.rows || rows.len() != 1 {
                return Err(Failure::InvalidLink(
                    "TC snapshot changed since observation",
                ));
            }
            queries::remove_member(&tx, first.id)?;
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
