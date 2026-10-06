use crate::{
    Backend, Reader, XdpReceipt,
    backend::{publish, read},
    error::Failure,
    state::Kind,
};
use bpfman_core::EffectFailure;
use bpfman_fs::{RuntimeLayout, RuntimeWriter};
use bpfman_model::{
    LinkDetails, LinkState, StoredLink, XdpDispatcherSnapshot, XdpKey, XdpLink, XdpSnapshot,
};
use bpfman_store::{Error, XdpCommit, XdpReader, XdpStore};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    num::{NonZeroU32, NonZeroU64},
};

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Row {
    #[serde(default, skip_serializing_if = "is_zero")]
    pub(super) position: usize,
    pub(super) link_id: NonZeroU64,
    pub(super) program_id: NonZeroU32,
    pub(super) extension_link_id: NonZeroU32,
    pub(super) outer_link_id: NonZeroU32,
    pub(super) nsid: NonZeroU64,
    pub(super) ifindex: NonZeroU32,
    interface: String,
    priority: u32,
    proceed_on: u32,
    dispatcher_id: NonZeroU32,
    revision: NonZeroU32,
    metadata: BTreeMap<String, String>,
    created_at: String,
}

fn is_zero(value: &usize) -> bool {
    *value == 0
}

impl Row {
    pub(super) fn key(&self) -> XdpKey {
        XdpKey {
            nsid: self.nsid,
            ifindex: self.ifindex,
        }
    }

    pub(super) fn snapshot(
        &self,
        layout: &RuntimeLayout,
        program_name: &str,
    ) -> Result<XdpSnapshot, Failure> {
        let details = self.details()?;
        Ok(XdpSnapshot {
            program_name: program_name
                .try_into()
                .map_err(|_| Failure::Invalid("invalid XDP program name"))?,
            program_pin_path: layout
                .program_pin_path(self.program_id)
                .into_os_string()
                .into_string()
                .map_err(|_| Failure::Invalid("runtime path is not UTF-8"))?,
            details: details.clone(),
            outer_link_id: self.outer_link_id,
            member: StoredLink {
                id: self.link_id,
                program_id: self.program_id,
                details: LinkDetails::Xdp(details),
                state: LinkState::Attached {
                    kernel_id: self.extension_link_id,
                },
                pin_path: layout
                    .xdp_slot_path(
                        self.key(),
                        self.revision,
                        self.position
                            .try_into()
                            .map_err(|_| Failure::Invalid("invalid XDP slot"))?,
                    )
                    .into_os_string()
                    .into_string()
                    .map_err(|_| Failure::Invalid("runtime path is not UTF-8"))?,
                metadata: self.metadata.clone(),
                created_at: crate::state::timestamp(&self.created_at)?,
            },
        })
    }

    pub(super) fn details(&self) -> Result<XdpLink, Failure> {
        if self.priority > i32::MAX as u32 {
            return Err(Failure::Invalid("invalid XDP priority"));
        }
        let details = XdpLink {
            slot: self
                .position
                .try_into()
                .map_err(|_| Failure::Invalid("invalid XDP slot"))?,
            key: self.key(),
            interface: self
                .interface
                .parse()
                .map_err(|_| Failure::Invalid("invalid XDP interface"))?,
            priority: self.priority,
            proceed_on: self
                .proceed_on
                .try_into()
                .map_err(|_| Failure::Invalid("invalid proceed-on mask"))?,
            dispatcher_id: self.dispatcher_id,
            revision: self.revision,
        };
        crate::state::timestamp(&self.created_at)?;
        Ok(details)
    }
}

pub(super) fn preflight(
    state: &crate::state::State,
    key: XdpKey,
    program: NonZeroU32,
) -> Result<(), Failure> {
    if state.version < 4 {
        return Err(Failure::DispatcherVersion);
    }
    if state.xdp.iter().any(|row| row.key() == key) {
        return Err(Failure::Unsupported(
            "XDP dispatcher replacement is not implemented; attach point is occupied",
        ));
    }
    if !state
        .programs
        .iter()
        .any(|row| row.id == program && row.kind == Kind::Xdp)
    {
        return Err(Failure::Invalid("missing managed XDP program"));
    }
    Ok(())
}

impl XdpReader for Reader {
    fn read_xdp(&mut self, key: XdpKey) -> Result<Option<XdpSnapshot>, Error> {
        let (_, state) = read(&self.file)?;
        if state.xdp.iter().filter(|r| r.key() == key).count() > 1 {
            return Err(
                Failure::Unsupported("multi-member XDP lifecycle is not implemented").into(),
            );
        }
        state
            .xdp
            .iter()
            .find(|row| row.key() == key)
            .map(|row| state.xdp_snapshot(row, &self.layout).map_err(Into::into))
            .transpose()
    }
}

impl XdpStore for Backend {
    type XdpReceipt = XdpReceipt;

    fn preflight_xdp(
        &self,
        writer: &RuntimeWriter<'_>,
        key: XdpKey,
        program: NonZeroU32,
    ) -> Result<(), Error> {
        let file = writer.open_store_snapshot().map_err(Failure::from)?;
        let (_, state) = read(&file)?;
        preflight(&state, key, program).map_err(Into::into)
    }

    fn commit_xdp(
        &self,
        writer: &RuntimeWriter<'_>,
        request: XdpCommit<'_>,
    ) -> Result<StoredLink, Error> {
        request.validate()?;
        let file = writer.open_store_snapshot().map_err(Failure::from)?;
        let (previous, mut state) = read(&file)?;
        preflight(&state, request.details.key, request.program_id)?;
        let id = NonZeroU64::new(state.next_link_id)
            .filter(|id| id.get() <= i64::MAX as u64)
            .ok_or(Failure::Invalid("link ID exhausted"))?;
        state.next_link_id += 1;
        let row = Row {
            position: 0,
            link_id: id,
            program_id: request.program_id,
            extension_link_id: request.extension_link_id,
            outer_link_id: request.outer_link_id,
            nsid: request.details.key.nsid,
            ifindex: request.details.key.ifindex,
            interface: request.details.interface.as_str().into(),
            priority: request.details.priority,
            proceed_on: request.details.proceed_on.mask(),
            dispatcher_id: request.details.dispatcher_id,
            revision: request.details.revision,
            metadata: request.metadata.clone(),
            created_at: request.created_at.into(),
        };
        let record = state.xdp_snapshot(&row, writer.layout())?.member;
        state.xdp.push(row);
        state.validate()?;
        publish(writer, &file, Some(&previous), &state)?;
        Ok(record)
    }

    fn observe_xdp(
        &self,
        writer: &RuntimeWriter<'_>,
        link: NonZeroU64,
    ) -> Result<Option<(XdpSnapshot, XdpReceipt)>, Error> {
        let file = writer.open_store_snapshot().map_err(Failure::from)?;
        let (_, state) = read(&file)?;
        let Some(row) = state.xdp.iter().find(|r| r.link_id == link) else {
            return Ok(None);
        };
        if state.xdp.iter().filter(|r| r.key() == row.key()).count() != 1 {
            return Err(Failure::Unsupported("multi-member XDP detach is not implemented").into());
        }
        Ok(Some((
            state.xdp_snapshot(row, writer.layout())?,
            XdpReceipt {
                root: writer.identity().map_err(Failure::from)?,
                store: state.identity.clone(),
                rows: vec![row.clone()],
                programs: member_programs(&state, row.key()),
            },
        )))
    }

    fn delete_xdp(
        &self,
        writer: &RuntimeWriter<'_>,
        receipt: XdpReceipt,
    ) -> Result<(), EffectFailure<XdpReceipt, Error>> {
        let result = (|| -> Result<(), Failure> {
            let file = writer.open_store_snapshot()?;
            let (previous, mut state) = read(&file)?;
            if writer.identity()? != receipt.root || state.identity != receipt.store {
                return Err(Failure::Invalid(
                    "XDP receipt belongs to another runtime or store",
                ));
            }
            let key = receipt
                .rows
                .first()
                .ok_or(Failure::Invalid("empty XDP receipt"))?
                .key();
            unchanged(&state, &receipt, key)?;
            state.xdp.retain(|row| row.key() != key);
            publish(writer, &file, Some(&previous), &state)
        })();
        result.map_err(|cause| EffectFailure {
            cause: cause.into(),
            remaining: receipt,
        })
    }
}

fn member_programs(state: &crate::state::State, key: XdpKey) -> Vec<crate::state::Program> {
    state
        .programs
        .iter()
        .filter(|p| {
            state
                .xdp
                .iter()
                .any(|r| r.key() == key && r.program_id == p.id)
        })
        .cloned()
        .collect()
}

fn unchanged(
    state: &crate::state::State,
    receipt: &XdpReceipt,
    key: XdpKey,
) -> Result<(), Failure> {
    let rows: Vec<_> = state
        .xdp
        .iter()
        .filter(|r| r.key() == key)
        .cloned()
        .collect();
    if rows != receipt.rows || member_programs(state, key) != receipt.programs {
        return Err(Failure::Invalid("dispatcher changed since observation"));
    }
    Ok(())
}

pub(super) fn dispatcher(
    state: &crate::state::State,
    layout: &RuntimeLayout,
    key: XdpKey,
) -> Result<Option<XdpDispatcherSnapshot>, Failure> {
    let mut members = state
        .xdp
        .iter()
        .filter(|r| r.key() == key)
        .map(|r| state.xdp_snapshot(r, layout))
        .collect::<Result<Vec<_>, _>>()?;
    if members.is_empty() {
        return Ok(None);
    }
    members.sort_by_key(|s| s.details.slot);
    XdpDispatcherSnapshot::new(members)
        .map(Some)
        .map_err(|_| Failure::Invalid("invalid XDP dispatcher membership"))
}

impl bpfman_store::XdpDispatcherReader for Reader {
    fn read_xdp_dispatchers(&mut self) -> Result<Vec<XdpDispatcherSnapshot>, Error> {
        let (_, state) = read(&self.file)?;
        let keys: std::collections::BTreeSet<_> = state.xdp.iter().map(Row::key).collect();
        keys.into_iter()
            .map(|key| {
                dispatcher(&state, &self.layout, key)?.ok_or(Failure::Invalid("missing dispatcher"))
            })
            .collect::<Result<_, _>>()
            .map_err(Into::into)
    }

    fn read_xdp_dispatcher(&mut self, key: XdpKey) -> Result<Option<XdpDispatcherSnapshot>, Error> {
        let (_, state) = read(&self.file)?;
        dispatcher(&state, &self.layout, key).map_err(Into::into)
    }
}

impl bpfman_store::XdpReplacementStore for Backend {
    fn observe_xdp_dispatcher(
        &self,
        writer: &RuntimeWriter<'_>,
        key: XdpKey,
    ) -> Result<Option<(XdpDispatcherSnapshot, XdpReceipt)>, Error> {
        let file = writer.open_store_snapshot().map_err(Failure::from)?;
        let (_, state) = read(&file)?;
        let Some(snapshot) = dispatcher(&state, writer.layout(), key)? else {
            return Ok(None);
        };
        Ok(Some((
            snapshot,
            XdpReceipt {
                root: writer.identity().map_err(Failure::from)?,
                store: state.identity.clone(),
                rows: state
                    .xdp
                    .iter()
                    .filter(|r| r.key() == key)
                    .cloned()
                    .collect(),
                programs: member_programs(&state, key),
            },
        )))
    }

    fn replace_xdp(
        &self,
        writer: &RuntimeWriter<'_>,
        receipt: XdpReceipt,
        request: bpfman_store::XdpReplace<'_>,
    ) -> Result<XdpDispatcherSnapshot, EffectFailure<XdpReceipt, Error>> {
        let result = (|| -> Result<_, Error> {
            let file = writer.open_store_snapshot().map_err(Failure::from)?;
            let (previous, mut state) = read(&file)?;
            if writer.identity().map_err(Failure::from)? != receipt.root
                || state.identity != receipt.store
            {
                return Err(
                    Failure::Invalid("XDP receipt belongs to another runtime or store").into(),
                );
            }
            if state.version < 5 {
                return Err(Failure::Unsupported(
                    "XDP replacement requires JSON format 5; no implicit upgrade",
                )
                .into());
            }
            let key = receipt
                .rows
                .first()
                .ok_or(Failure::Invalid("empty XDP receipt"))?
                .key();
            unchanged(&state, &receipt, key)?;
            let current = dispatcher(&state, writer.layout(), key)?
                .ok_or(Failure::Invalid("missing dispatcher"))?;
            request.validate(&current)?;
            crate::state::timestamp(request.updated_at)?;
            let mut desired = Vec::new();
            for (position, member) in request.members.iter().enumerate() {
                let a = &member.attachment;
                let link_id = match member.identity {
                    bpfman_store::XdpMemberId::Existing(id) => id,
                    bpfman_store::XdpMemberId::New => {
                        let id = NonZeroU64::new(state.next_link_id)
                            .filter(|id| id.get() <= i64::MAX as u64)
                            .ok_or(Failure::Invalid("link ID exhausted"))?;
                        state.next_link_id += 1;
                        id
                    }
                };
                desired.push(Row {
                    position,
                    link_id,
                    program_id: a.program_id,
                    extension_link_id: a.extension_link_id,
                    outer_link_id: a.outer_link_id,
                    nsid: key.nsid,
                    ifindex: key.ifindex,
                    interface: a.details.interface.as_str().into(),
                    priority: a.details.priority,
                    proceed_on: a.details.proceed_on.mask(),
                    dispatcher_id: a.details.dispatcher_id,
                    revision: a.details.revision,
                    metadata: a.metadata.clone(),
                    created_at: a.created_at.into(),
                });
            }
            state.xdp.retain(|row| row.key() != key);
            state.xdp.extend(desired);
            state.validate()?;
            let snapshot = dispatcher(&state, writer.layout(), key)?
                .ok_or(Failure::Invalid("empty replacement"))?;
            publish(writer, &file, Some(&previous), &state)?;
            Ok(snapshot)
        })();
        result.map_err(|cause| EffectFailure {
            cause,
            remaining: receipt,
        })
    }
}
