use crate::{
    Backend, TcReceipt,
    backend::{publish, read},
    error::Failure,
    state::{Kind, State},
};
use bpfman_core::EffectFailure;
use bpfman_fs::{RuntimeLayout, RuntimeWriter};
use bpfman_model::{
    LinkDetails, LinkState, StoredLink, TcDispatcherSnapshot, TcLink, TcSnapshot, XdpKey,
};
use bpfman_store::{Error, TcCommit, TcStore};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    num::{NonZeroU32, NonZeroU64},
};

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Row {
    pub(super) link_id: NonZeroU64,
    pub(super) program_id: NonZeroU32,
    pub(super) extension_link_id: NonZeroU32,
    nsid: NonZeroU64,
    ifindex: NonZeroU32,
    interface: String,
    netns: String,
    #[serde(default = "first_revision", skip_serializing_if = "is_first_revision")]
    revision: NonZeroU32,
    #[serde(default, skip_serializing_if = "is_first_slot")]
    position: usize,
    priority: u32,
    proceed_on: u32,
    pub(super) dispatcher_id: NonZeroU32,
    filter_handle: NonZeroU32,
    filter_priority: u16,
    metadata: BTreeMap<String, String>,
    created_at: String,
}

fn first_revision() -> NonZeroU32 {
    NonZeroU32::MIN
}

fn is_first_revision(r: &NonZeroU32) -> bool {
    *r == NonZeroU32::MIN
}

fn is_first_slot(slot: &usize) -> bool {
    *slot == 0
}

impl Row {
    pub(super) fn key(&self) -> XdpKey {
        XdpKey {
            nsid: self.nsid,
            ifindex: self.ifindex,
        }
    }

    pub(super) fn details(&self) -> Result<TcLink, Failure> {
        if self.priority > i32::MAX as u32 || self.filter_priority != 50 {
            return Err(Failure::Invalid("invalid TC priority"));
        }
        crate::state::timestamp(&self.created_at)?;
        Ok(TcLink {
            revision: self.revision,
            slot: self
                .position
                .try_into()
                .map_err(|_| Failure::Invalid("invalid TC slot"))?,
            netns: self
                .netns
                .parse()
                .map_err(|_| Failure::Invalid("invalid TC namespace"))?,
            key: self.key(),
            interface: self
                .interface
                .parse()
                .map_err(|_| Failure::Invalid("invalid TC interface"))?,
            priority: self.priority,
            proceed_on: self
                .proceed_on
                .try_into()
                .map_err(|_| Failure::Invalid("invalid TC proceed-on"))?,
            dispatcher_id: self.dispatcher_id,
            filter_handle: self.filter_handle,
            filter_priority: self.filter_priority,
        })
    }

    pub(super) fn snapshot(
        &self,
        state: &State,
        layout: &RuntimeLayout,
    ) -> Result<TcSnapshot, Failure> {
        let program = state
            .programs
            .iter()
            .find(|p| p.id == self.program_id && p.kind == Kind::Tc)
            .ok_or(Failure::Invalid("missing TC program"))?;
        let details = self.details()?;
        let path = |p: std::path::PathBuf| {
            p.into_os_string()
                .into_string()
                .map_err(|_| Failure::Invalid("runtime path is not UTF-8"))
        };
        Ok(TcSnapshot {
            program_name: program
                .name
                .as_str()
                .try_into()
                .map_err(|_| Failure::Invalid("invalid TC program name"))?,
            program_pin_path: path(layout.program_pin_path(self.program_id))?,
            details: details.clone(),
            member: StoredLink {
                id: self.link_id,
                program_id: self.program_id,
                details: LinkDetails::Tc(details.clone()),
                state: LinkState::Attached {
                    kernel_id: self.extension_link_id,
                },
                pin_path: path(layout.tc_slot_path(self.key(), self.revision, details.slot))?,
                metadata: self.metadata.clone(),
                created_at: crate::state::timestamp(&self.created_at)?,
            },
        })
    }
}

fn preflight(state: &State, key: XdpKey, program: NonZeroU32) -> Result<(), Failure> {
    if state.version < 7 {
        return Err(Failure::Unsupported(
            "TC requires JSON format 7; no implicit upgrade",
        ));
    }
    if state.tc.iter().any(|r| r.key() == key) {
        return Err(Failure::Unsupported("TC ingress attach point is occupied"));
    }
    if !state
        .programs
        .iter()
        .any(|p| p.id == program && p.kind == Kind::Tc)
    {
        return Err(Failure::Invalid("missing managed TC program"));
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
        let file = w.open_store_snapshot().map_err(Failure::from)?;
        let (_, state) = read(&file)?;
        preflight(&state, key, program).map_err(Into::into)
    }

    fn commit_tc(&self, w: &RuntimeWriter<'_>, request: TcCommit<'_>) -> Result<StoredLink, Error> {
        let file = w.open_store_snapshot().map_err(Failure::from)?;
        let (previous, mut state) = read(&file)?;
        preflight(&state, request.details.key, request.program_id)?;
        let id = NonZeroU64::new(state.next_link_id)
            .filter(|n| n.get() <= i64::MAX as u64)
            .ok_or(Failure::Invalid("link ID exhausted"))?;
        state.next_link_id += 1;
        let d = request.details;
        if d.revision != NonZeroU32::MIN || d.slot != bpfman_model::XdpSlot::FIRST {
            return Err(Failure::Invalid("invalid first TC revision").into());
        }
        let row = Row {
            link_id: id,
            program_id: request.program_id,
            extension_link_id: request.extension_link_id,
            nsid: d.key.nsid,
            ifindex: d.key.ifindex,
            interface: d.interface.as_str().into(),
            netns: d.netns.as_str().into(),
            revision: d.revision,
            position: d.slot.index(),
            priority: d.priority,
            proceed_on: d.proceed_on.mask(),
            dispatcher_id: d.dispatcher_id,
            filter_handle: d.filter_handle,
            filter_priority: d.filter_priority,
            metadata: request.metadata.clone(),
            created_at: request.created_at.into(),
        };
        let record = row.snapshot(&state, w.layout())?.member;
        state.tc.push(row);
        state.validate()?;
        publish(w, &file, Some(&previous), &state)?;
        Ok(record)
    }

    fn observe_tc(
        &self,
        w: &RuntimeWriter<'_>,
        id: NonZeroU64,
    ) -> Result<Option<(TcSnapshot, TcReceipt)>, Error> {
        let file = w.open_store_snapshot().map_err(Failure::from)?;
        let (_, state) = read(&file)?;
        let Some(row) = state.tc.iter().find(|r| r.link_id == id) else {
            return Ok(None);
        };
        if state.tc.iter().filter(|r| r.key() == row.key()).count() != 1 {
            return Err(Failure::Unsupported("use complete TC dispatcher observation").into());
        }
        let program = state
            .programs
            .iter()
            .find(|p| p.id == row.program_id)
            .ok_or(Failure::Invalid("missing TC program"))?
            .clone();
        Ok(Some((
            row.snapshot(&state, w.layout())?,
            TcReceipt {
                root: w.identity().map_err(Failure::from)?,
                store: state.identity.clone(),
                rows: vec![row.clone()],
                programs: vec![program],
            },
        )))
    }

    fn observe_tc_member_dispatcher(
        &self,
        w: &RuntimeWriter<'_>,
        id: NonZeroU64,
    ) -> Result<Option<(TcDispatcherSnapshot, TcReceipt)>, Error> {
        let file = w.open_store_snapshot().map_err(Failure::from)?;
        let (_, state) = read(&file)?;
        match state.tc.iter().find(|r| r.link_id == id) {
            Some(row) => self.observe_tc_dispatcher(w, row.key()),
            None => Ok(None),
        }
    }

    fn observe_tc_dispatcher(
        &self,
        w: &RuntimeWriter<'_>,
        key: XdpKey,
    ) -> Result<Option<(TcDispatcherSnapshot, TcReceipt)>, Error> {
        let file = w.open_store_snapshot().map_err(Failure::from)?;
        let (_, state) = read(&file)?;
        let rows: Vec<_> = state
            .tc
            .iter()
            .filter(|r| r.key() == key)
            .cloned()
            .collect();
        if rows.is_empty() {
            return Ok(None);
        }
        let snapshot = TcDispatcherSnapshot::new(
            rows.iter()
                .map(|r| r.snapshot(&state, w.layout()))
                .collect::<Result<_, _>>()?,
        )
        .map_err(|_| Failure::Invalid("invalid TC dispatcher"))?;
        let programs = state
            .programs
            .iter()
            .filter(|p| rows.iter().any(|r| r.program_id == p.id))
            .cloned()
            .collect();
        Ok(Some((
            snapshot,
            TcReceipt {
                root: w.identity().map_err(Failure::from)?,
                store: state.identity,
                rows,
                programs,
            },
        )))
    }

    fn replace_tc(
        &self,
        w: &RuntimeWriter<'_>,
        receipt: TcReceipt,
        request: bpfman_store::TcReplace<'_>,
    ) -> Result<TcDispatcherSnapshot, EffectFailure<TcReceipt, Error>> {
        let result = (|| -> Result<_, Error> {
            let file = w.open_store_snapshot().map_err(Failure::from)?;
            let (previous, mut state) = read(&file)?;
            if state.version < 8 {
                return Err(Failure::Unsupported(
                    "TC replacement requires JSON format 8; no implicit upgrade",
                )
                .into());
            }
            let key = receipt
                .rows
                .first()
                .ok_or(Failure::Invalid("empty TC receipt"))?
                .key();
            let programs: Vec<_> = state
                .programs
                .iter()
                .filter(|p| receipt.rows.iter().any(|r| r.program_id == p.id))
                .cloned()
                .collect();
            if w.identity().map_err(Failure::from)? != receipt.root
                || state.identity != receipt.store
                || programs != receipt.programs
                || state
                    .tc
                    .iter()
                    .filter(|r| r.key() == key)
                    .cloned()
                    .collect::<Vec<_>>()
                    != receipt.rows
            {
                return Err(Failure::Invalid(
                    "TC receipt belongs to another runtime/store or changed snapshot",
                )
                .into());
            }
            let old = TcDispatcherSnapshot::new(
                receipt
                    .rows
                    .iter()
                    .map(|r| r.snapshot(&state, w.layout()))
                    .collect::<Result<_, _>>()?,
            )
            .map_err(|_| Failure::Invalid("invalid TC dispatcher"))?;
            request.validate(&old)?;
            crate::state::timestamp(request.updated_at)?;
            let mut desired = Vec::new();
            for m in request.members {
                let a = &m.attachment;
                let d = a.details;
                let link_id = match m.identity {
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
                    link_id,
                    program_id: a.program_id,
                    extension_link_id: a.extension_link_id,
                    nsid: key.nsid,
                    ifindex: key.ifindex,
                    interface: d.interface.as_str().into(),
                    netns: d.netns.as_str().into(),
                    revision: d.revision,
                    position: d.slot.index(),
                    priority: d.priority,
                    proceed_on: d.proceed_on.mask(),
                    dispatcher_id: d.dispatcher_id,
                    filter_handle: d.filter_handle,
                    filter_priority: d.filter_priority,
                    metadata: a.metadata.clone(),
                    created_at: a.created_at.into(),
                });
            }
            state.tc.retain(|r| r.key() != key);
            state.tc.extend(desired);
            state.validate()?;
            let result = TcDispatcherSnapshot::new(
                state
                    .tc
                    .iter()
                    .filter(|r| r.key() == key)
                    .map(|r| r.snapshot(&state, w.layout()))
                    .collect::<Result<_, _>>()?,
            )
            .map_err(|_| Failure::Invalid("invalid TC dispatcher"))?;
            publish(w, &file, Some(&previous), &state)?;
            Ok(result)
        })();
        result.map_err(|cause| EffectFailure {
            cause,
            remaining: receipt,
        })
    }

    fn delete_tc(
        &self,
        w: &RuntimeWriter<'_>,
        receipt: TcReceipt,
    ) -> Result<(), EffectFailure<TcReceipt, Error>> {
        let result = (|| -> Result<(), Failure> {
            let file = w.open_store_snapshot()?;
            let (previous, mut state) = read(&file)?;
            if w.identity()? != receipt.root
                || state.identity != receipt.store
                || receipt.rows.len() != 1
                || !receipt.programs.iter().all(|p| state.programs.contains(p))
            {
                return Err(Failure::Invalid(
                    "TC receipt belongs to another runtime/store or changed snapshot",
                ));
            }
            let row = receipt
                .rows
                .first()
                .ok_or(Failure::Invalid("empty TC receipt"))?;
            if state
                .tc
                .iter()
                .filter(|r| r.key() == row.key())
                .cloned()
                .collect::<Vec<_>>()
                != receipt.rows
            {
                return Err(Failure::Invalid("TC snapshot changed"));
            }
            state.tc.retain(|r| r.link_id != row.link_id);
            publish(w, &file, Some(&previous), &state)
        })();
        result.map_err(|cause| EffectFailure {
            cause: cause.into(),
            remaining: receipt,
        })
    }
}
