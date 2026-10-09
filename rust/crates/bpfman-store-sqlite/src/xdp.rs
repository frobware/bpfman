use crate::{Backend, Store, XdpReceipt, error::Failure, open, queries::xdp as queries};
use bpfman_core::EffectFailure;
use bpfman_fs::{RuntimeLayout, RuntimeWriter};
use bpfman_model::{
    LinkDetails, LinkState, StoredLink, XdpDispatcherSnapshot, XdpKey, XdpLink, XdpSnapshot,
};
use bpfman_store::{Error, XdpCommit, XdpReader, XdpStore};
use rusqlite::{Connection, OpenFlags, TransactionBehavior};
use std::{
    num::{NonZeroU32, NonZeroU64},
    os::unix::fs::MetadataExt,
};

fn invalid() -> Failure {
    Failure::InvalidLink("invalid XDP snapshot")
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
        || row.netns != row.dispatcher_netns
        || row.kernel == row.outer
        || row.priority < 0
        || row.priority > i32::MAX as i64
    {
        return Err(Failure::Unsupported(
            "unsupported XDP snapshot or mismatched namespace paths",
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
        netns: row.netns.parse().map_err(|_| invalid())?,
        slot: usize::try_from(row.position)
            .ok()
            .and_then(|p| p.try_into().ok())
            .ok_or_else(invalid)?,
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
        .xdp_slot_path(s.details.key, s.details.revision, s.details.slot)
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

#[tracing::instrument(name = "store.connection.open_writer", level = "debug", skip_all, err)]
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
                let snapshot = decode_rows(&rows)?;
                Ok(snapshot.and_then(|s| s.members().first().cloned()))
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
            if snapshot.details.slot != bpfman_model::XdpSlot::FIRST {
                return Err(invalid());
            }
            if queries::rows(&tx, Some(snapshot.details.key), None)?.len() != 1 {
                return Err(Failure::Unsupported("multi-member XDP detach"));
            }
            let receipt = XdpReceipt {
                root: w.identity()?,
                database: identity(w)?,
                rows: vec![row],
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
            let first = receipt.rows.first().ok_or_else(invalid)?;
            let snapshot = canonical(first, w.layout())?;
            let rows = queries::rows(&tx, Some(snapshot.details.key), None)?;
            if rows != receipt.rows {
                return Err(Failure::InvalidLink(
                    "XDP snapshot changed since observation",
                ));
            }
            for row in &receipt.rows {
                queries::remove_member(&tx, row.id)?;
            }
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

pub(super) fn decode_rows(rows: &[queries::Row]) -> Result<Option<XdpDispatcherSnapshot>, Failure> {
    if rows.is_empty() {
        return Ok(None);
    }
    let mut members = rows.iter().map(decode).collect::<Result<Vec<_>, _>>()?;
    members.sort_by_key(|s| s.details.slot);
    XdpDispatcherSnapshot::new(members)
        .map(Some)
        .map_err(|_| invalid())
}

fn canonical_rows(
    rows: &[queries::Row],
    layout: &RuntimeLayout,
) -> Result<XdpDispatcherSnapshot, Failure> {
    for row in rows {
        canonical(row, layout)?;
    }
    decode_rows(rows)?.ok_or_else(invalid)
}

impl bpfman_store::XdpDispatcherReader for Store {
    fn read_xdp_dispatchers(&mut self) -> Result<Vec<XdpDispatcherSnapshot>, Error> {
        self.reader
            .read(|c| {
                let tx = c.transaction()?;
                open::require_supported(open::schema_version(&tx)?)?;
                let keys = queries::dispatcher_keys(&tx)?;
                let rows = queries::rows(&tx, None, None)?;
                let mut result = Vec::new();
                let mut total = 0;
                for (kind, nsid, ifindex) in keys {
                    if kind == "tc-ingress" {
                        continue;
                    }
                    if kind != "xdp" {
                        return Err(Failure::Unsupported(
                            "only XDP dispatcher listing is implemented",
                        ));
                    }
                    let key = XdpKey {
                        nsid: id64(nsid)?,
                        ifindex: id32(ifindex)?,
                    };
                    let selected: Vec<_> = rows
                        .iter()
                        .filter(|r| r.nsid == nsid && r.ifindex == ifindex)
                        .cloned()
                        .collect();
                    total += selected.len();
                    let snapshot = decode_rows(&selected)?.ok_or_else(invalid)?;
                    if snapshot.members().iter().any(|s| s.details.key != key) {
                        return Err(invalid());
                    }
                    result.push(snapshot);
                }
                if total != rows.len() {
                    return Err(invalid());
                }
                Ok(result)
            })
            .map_err(crate::Error::from)
            .map_err(Into::into)
    }

    fn read_xdp_dispatcher(&mut self, key: XdpKey) -> Result<Option<XdpDispatcherSnapshot>, Error> {
        self.reader
            .read(|c| {
                let tx = c.transaction()?;
                open::require_supported(open::schema_version(&tx)?)?;
                let rows = queries::rows(&tx, Some(key), None)?;
                if rows.is_empty() && !queries::vacant(&tx, key)? {
                    return Err(invalid());
                }
                decode_rows(&rows)
            })
            .map_err(crate::Error::from)
            .map_err(Into::into)
    }
}

impl bpfman_store::XdpReplacementStore for Backend {
    fn observe_xdp_dispatcher(
        &self,
        w: &RuntimeWriter<'_>,
        key: XdpKey,
    ) -> Result<Option<(XdpDispatcherSnapshot, XdpReceipt)>, Error> {
        (|| -> Result<_, Failure> {
            let mut c = connection(w)?;
            let tx = c.transaction()?;
            let rows = queries::rows(&tx, Some(key), None)?;
            if rows.is_empty() {
                return if queries::vacant(&tx, key)? {
                    Ok(None)
                } else {
                    Err(invalid())
                };
            }
            let snapshot = canonical_rows(&rows, w.layout())?;
            Ok(Some((
                snapshot,
                XdpReceipt {
                    root: w.identity()?,
                    database: identity(w)?,
                    rows,
                },
            )))
        })()
        .map_err(crate::Error::from)
        .map_err(Into::into)
    }

    fn replace_xdp(
        &self,
        w: &RuntimeWriter<'_>,
        receipt: XdpReceipt,
        request: bpfman_store::XdpReplace<'_>,
    ) -> Result<XdpDispatcherSnapshot, EffectFailure<XdpReceipt, Error>> {
        let result = (|| -> Result<_, Error> {
            let apply = || -> Result<_, Failure> {
                if w.identity()? != receipt.root || identity(w)? != receipt.database {
                    return Err(Failure::InvalidLink(
                        "XDP receipt belongs to another runtime or store",
                    ));
                }
                let old = canonical_rows(&receipt.rows, w.layout())?;
                let first = receipt.rows.first().ok_or_else(invalid)?;
                let mut c = connection(w)?;
                let tx = c.transaction_with_behavior(TransactionBehavior::Immediate)?;
                if queries::rows(&tx, Some(old.members()[0].details.key), None)? != receipt.rows {
                    return Err(Failure::InvalidLink(
                        "XDP snapshot changed since observation",
                    ));
                }
                // Validate before any DML; retain the portable error at the outer boundary.
                let mut desired = Vec::new();
                for (position, member) in request.members.iter().enumerate() {
                    let a = &member.attachment;
                    let (program_name, program_pin) = queries::program(&tx, a.program_id.get())?;
                    let mut row = first.clone();
                    row.id = match member.identity {
                        bpfman_store::XdpMemberId::Existing(id) => {
                            i64::try_from(id.get()).map_err(|_| invalid())?
                        }
                        bpfman_store::XdpMemberId::New => 0,
                    };
                    row.program = i64::from(a.program_id.get());
                    row.program_name = program_name;
                    row.program_pin = program_pin;
                    row.kernel = i64::from(a.extension_link_id.get());
                    row.position = position as i64;
                    let slot = position.try_into().map_err(|_| invalid())?;
                    row.pin = w
                        .layout()
                        .xdp_slot_path(a.details.key, a.details.revision, slot)
                        .into_os_string()
                        .into_string()
                        .map_err(|_| invalid())?;
                    row.metadata =
                        serde_json::to_string(a.metadata).map_err(Failure::LinkMetadata)?;
                    row.created = a.created_at.into();
                    row.priority = i64::from(a.details.priority);
                    let codes: Vec<_> = (0..32)
                        .filter(|code| a.details.proceed_on.mask() & (1 << code) != 0)
                        .collect();
                    row.proceed_on =
                        serde_json::to_string(&codes).map_err(Failure::LinkMetadata)?;
                    row.dispatcher = i64::from(a.details.dispatcher_id.get());
                    row.revision = i64::from(a.details.revision.get());
                    row.dispatcher_updated = request.updated_at.into();
                    desired.push(row);
                }
                for row in &receipt.rows {
                    queries::remove_member(&tx, row.id)?;
                }
                queries::update_dispatcher(&tx, desired.first().ok_or_else(invalid)?)?;
                for row in &mut desired {
                    row.id = queries::insert_member(&tx, row)?;
                }
                let snapshot = canonical_rows(&desired, w.layout())?;
                desired.sort_by_key(|r| r.id);
                if queries::rows(&tx, Some(old.members()[0].details.key), None)? != desired {
                    return Err(Failure::InvalidLink(
                        "XDP publication did not preserve the requested snapshot",
                    ));
                }
                tx.commit()?;
                Ok(snapshot)
            };
            let current = canonical_rows(&receipt.rows, w.layout()).map_err(crate::Error::from)?;
            request.validate(&current)?;
            apply().map_err(crate::Error::from).map_err(Into::into)
        })();
        result.map_err(|cause| EffectFailure {
            cause,
            remaining: receipt,
        })
    }
}
