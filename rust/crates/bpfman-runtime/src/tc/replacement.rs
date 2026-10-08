//! Complete TC revisions share the consuming switch/publication typestate protocol.
use super::*;
use bpfman_core::{
    XdpMemberIdentity, XdpMemberOrder, XdpMembershipPlan, XdpReplacement, plan_xdp_membership,
};
use bpfman_model::{TcDispatcherSnapshot, XdpKey};
use bpfman_store::{TcMemberCommit, TcReplace, XdpMemberId};

type Owned<S, K> = Vec<Resource<S, K>>;
type Restoration<S, K> =
    bpfman_core::XdpRestoration<Owned<S, K>, Owned<S, K>, <K as TcLifecycle>::Switch, LinkCause>;
pub(super) type RestoreFailure<S, K> =
    bpfman_core::XdpRestoreFailure<Owned<S, K>, Owned<S, K>, <K as TcLifecycle>::Switch, LinkCause>;

pub(super) struct Removal<S: TcStore, K: TcLifecycle> {
    snapshot: TcDispatcherSnapshot,
    receipt: S::TcReceipt,
    stage: K::Stage,
    filter: K::Filter,
    id: NonZeroU64,
}

struct Member {
    identity: XdpMemberIdentity,
    name: bpfman_model::Symbol,
    request: TcAttach,
    created_at: String,
}

fn invalid(message: &'static str) -> LinkCause {
    Cause::Invalid(message).into()
}

fn members(snapshot: &TcDispatcherSnapshot) -> Vec<Member> {
    snapshot
        .members()
        .iter()
        .map(|m| Member {
            identity: XdpMemberIdentity::Existing(m.member.id),
            name: m.program_name.clone(),
            request: TcAttach {
                program_id: m.member.program_id,
                interface: m.details.interface.clone(),
                netns: m.details.netns.clone(),
                priority: m.details.priority,
                proceed_on: m.details.proceed_on,
                metadata: m.member.metadata.clone(),
            },
            created_at: m.member.created_at.clone(),
        })
        .collect()
}

pub(super) fn attach<S: TcStore, K: TcLifecycle>(
    app: &Bpfman<S, K>,
    w: &RuntimeWriter<'_>,
    r: &TcAttach,
    c: &Cancellation,
) -> Result<StoredLink, TcError<S, K>> {
    check(c)?;
    if r.priority > i32::MAX as u32 {
        return Err(invalid("priority exceeds i32::MAX").into());
    }
    let program = app
        .store
        .open(w)
        .map_err(LinkCause::from)?
        .read_records()
        .map_err(LinkCause::from)?
        .into_iter()
        .find(|p| p.id == r.program_id)
        .ok_or_else(|| LinkCause::from(Cause::NotFound))?;
    if !matches!(program.spec, ProgramSpec::Tc(_)) {
        return Err(LinkCause::from(Cause::Unsupported).into());
    }
    if w.layout().program_pin_path(program.id).to_str() != Some(program.pin_path.as_str()) {
        return Err(invalid("noncanonical managed TC pin").into());
    }
    let (key, prepared) = app
        .kernel
        .prepare_tc(w, r.program_id, &r.interface, &r.netns)
        .map_err(LinkCause::from)?;
    let Some((snapshot, receipt)) = app
        .store
        .observe_tc_dispatcher(w, key)
        .map_err(LinkCause::from)?
    else {
        return app.attach_tc_first(w, r, c, key, prepared);
    };
    let mut desired = members(&snapshot);
    let first = snapshot
        .members()
        .first()
        .ok_or_else(|| invalid("empty TC dispatcher"))?;
    desired.push(Member {
        identity: XdpMemberIdentity::New,
        name: program.spec.name().clone(),
        request: TcAttach {
            program_id: r.program_id,
            interface: first.details.interface.clone(),
            netns: first.details.netns.clone(),
            priority: r.priority,
            proceed_on: r.proceed_on,
            metadata: r.metadata.clone(),
        },
        created_at: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Nanos, true),
    });
    // Capacity, order and revision exhaustion are validated before staging effects.
    let plan = plan(&snapshot, &desired)?;
    let (stage, filter) = app
        .kernel
        .observe_tc_dispatcher(w, &snapshot)
        .map_err(LinkCause::from)?;
    let (committed, report) = replace(app, w, snapshot, receipt, stage, filter, &desired, plan, c)?;
    let member = committed
        .members()
        .iter()
        .find(|m| {
            !desired
                .iter()
                .any(|d| d.identity == XdpMemberIdentity::Existing(m.member.id))
        })
        .map(|m| m.member.clone());
    match member {
        Some(member) => Ok(member),
        None => Err(TcError {
            cause: Some(invalid("TC publication omitted new member")),
            recovery: Some(Recovery::Cleanup(Box::new(report.cleanup))),
            admission: None,
            restorations: report.restorations,
            committed: Some(committed),
        }),
    }
}

fn plan(
    snapshot: &TcDispatcherSnapshot,
    desired: &[Member],
) -> Result<bpfman_core::XdpRevisionPlan, LinkCause> {
    let first = snapshot
        .members()
        .first()
        .ok_or_else(|| invalid("empty TC dispatcher"))?;
    let order = desired
        .iter()
        .map(|m| {
            Ok(XdpMemberOrder {
                identity: m.identity,
                name: m.name.clone(),
                priority: m
                    .request
                    .priority
                    .try_into()
                    .map_err(|_| invalid("invalid TC priority"))?,
                proceed_on: Default::default(),
            })
        })
        .collect::<Result<Vec<_>, LinkCause>>()?;
    // Only membership ordering and slots are shared; the XDP CONFIG is never used.
    match plan_xdp_membership(Some(first.details.revision), &order)
        .map_err(|_| invalid("TC membership exceeds ten slots or revision is exhausted"))?
    {
        XdpMembershipPlan::Revision(p) => Ok(p),
        XdpMembershipPlan::Remove => Err(invalid("empty TC replacement")),
    }
}

pub(super) fn observe_removal<S: TcStore, K: TcLifecycle>(
    app: &Bpfman<S, K>,
    w: &RuntimeWriter<'_>,
    id: NonZeroU64,
) -> Result<(StoredLink, Teardown<S, K>), LinkCause> {
    let (snapshot, receipt) = app
        .store
        .observe_tc_member_dispatcher(w, id)?
        .ok_or(Cause::NotFound)?;
    let record = snapshot
        .members()
        .iter()
        .find(|m| m.member.id == id)
        .ok_or_else(|| invalid("missing TC member"))?
        .member
        .clone();
    let (stage, filter) = app.kernel.observe_tc_dispatcher(w, &snapshot)?;
    if snapshot.members().len() == 1 {
        return Ok((
            record,
            Teardown(Detach::Last(vec![
                Resource::Record(receipt),
                Resource::Stage(stage),
                Resource::Filter(filter),
            ])),
        ));
    }
    Ok((
        record,
        Teardown(Detach::Replace(Removal {
            snapshot,
            receipt,
            stage,
            filter,
            id,
        })),
    ))
}

pub(super) fn remove<S: TcStore, K: TcLifecycle>(
    app: &Bpfman<S, K>,
    w: &RuntimeWriter<'_>,
    removal: Removal<S, K>,
) -> Result<TcReport<S, K>, TcError<S, K>> {
    let Removal {
        snapshot,
        receipt,
        stage,
        filter,
        id,
    } = removal;
    let desired: Vec<_> = members(&snapshot)
        .into_iter()
        .filter(|m| m.identity != XdpMemberIdentity::Existing(id))
        .collect();
    let plan = plan(&snapshot, &desired)?;
    replace(
        app,
        w,
        snapshot,
        receipt,
        stage,
        filter,
        &desired,
        plan,
        &Cancellation::new(),
    )
    .map(|(_, report)| report)
}

fn stage_ref<S: TcStore, K: TcLifecycle>(owned: &[Resource<S, K>]) -> Result<&K::Stage, LinkCause> {
    owned
        .iter()
        .find_map(|r| {
            if let Resource::Stage(s) = r {
                Some(s)
            } else {
                None
            }
        })
        .ok_or_else(|| invalid("missing TC revision ownership"))
}

fn stage<S: TcStore, K: TcLifecycle>(
    app: &Bpfman<S, K>,
    w: &RuntimeWriter<'_>,
    key: XdpKey,
    desired: &[Member],
    plan: &bpfman_core::XdpRevisionPlan,
    owned: bool,
    c: &Cancellation,
) -> Result<K::Stage, EffectFailure<Option<K::Stage>, LinkCause>> {
    let before = |cause| EffectFailure {
        cause,
        remaining: None,
    };
    let mut prepared = Vec::new();
    let mut actions = Vec::new();
    for placement in plan.placements() {
        check(c).map_err(before)?;
        let m = &desired[placement.source()];
        let (observed, p) = app
            .kernel
            .prepare_tc(
                w,
                m.request.program_id,
                &m.request.interface,
                &m.request.netns,
            )
            .map_err(LinkCause::from)
            .map_err(before)?;
        if observed != key {
            return Err(before(invalid("TC interface changed during staging")));
        }
        prepared.push(p);
        actions.push(m.request.proceed_on);
    }
    check(c).map_err(before)?;
    app.kernel
        .stage_tc_revision(w, prepared, plan.revision(), &actions, owned)
        .map_err(|f| EffectFailure {
            cause: f.cause.into(),
            remaining: f.remaining,
        })
}

#[allow(clippy::too_many_arguments)]
fn replace<S: TcStore, K: TcLifecycle>(
    app: &Bpfman<S, K>,
    w: &RuntimeWriter<'_>,
    snapshot: TcDispatcherSnapshot,
    receipt: S::TcReceipt,
    old: K::Stage,
    filter: K::Filter,
    desired: &[Member],
    plan: bpfman_core::XdpRevisionPlan,
    c: &Cancellation,
) -> Result<(TcDispatcherSnapshot, TcReport<S, K>), TcError<S, K>> {
    let first = snapshot
        .members()
        .first()
        .ok_or_else(|| invalid("empty TC dispatcher"))?;
    let (old_id, old_members) = K::tc_revision_ids(&old).map_err(LinkCause::from)?;
    if old_id != first.details.dispatcher_id
        || old_members.len() != snapshot.members().len()
        || K::tc_filter(&filter).map_err(LinkCause::from)?
            != (first.details.filter_priority, first.details.filter_handle)
    {
        return Err(invalid("incomplete active TC dispatcher").into());
    }
    let owned = app
        .kernel
        .tc_clsact_owned(w, &old)
        .map_err(LinkCause::from)?;
    let new = match stage(app, w, first.details.key, desired, &plan, owned, c) {
        Ok(stage) => stage,
        Err(f) => {
            return Err(TcError {
                cause: Some(f.cause),
                recovery: Some(Recovery::Cleanup(Box::new(cleanup(
                    app,
                    w,
                    XdpCleanup::new(f.remaining.into_iter().map(Resource::Stage).collect()),
                )))),
                admission: None,
                restorations: Vec::new(),
                committed: None,
            });
        }
    };
    let protocol = XdpReplacement::new(vec![Resource::Stage(old)], vec![Resource::Stage(new)]);
    let switch = (|| -> Result<_, EffectFailure<Option<K::Switch>, LinkCause>> {
        let before = |cause| EffectFailure {
            cause,
            remaining: None,
        };
        check(c).map_err(before)?;
        let old = stage_ref::<S, K>(protocol.old()).map_err(before)?;
        let new = stage_ref::<S, K>(protocol.staged()).map_err(before)?;
        app.kernel
            .switch_tc(w, &filter, old, new)
            .map_err(|f| EffectFailure {
                cause: f.cause.into(),
                remaining: f.remaining,
            })
    })();
    let publication = match switch {
        Ok(s) => protocol.switched(s),
        Err(EffectFailure {
            cause,
            remaining: None,
        }) => {
            let rollback = protocol.rejected(cause);
            return Err(rollback_error(app, w, rollback));
        }
        Err(EffectFailure {
            cause,
            remaining: Some(s),
        }) => {
            return Err(restore(
                app,
                w,
                protocol.switch_failed(EffectFailure {
                    cause,
                    remaining: s,
                }),
            ));
        }
    };
    let published = (|| -> Result<TcDispatcherSnapshot, LinkCause> {
        check(c)?;
        let (dispatcher_id, extensions) =
            K::tc_revision_ids(stage_ref::<S, K>(publication.staged())?)?;
        if extensions.len() != desired.len() {
            return Err(invalid("TC staged membership mismatch"));
        }
        let details: Vec<_> = plan
            .placements()
            .iter()
            .map(|p| {
                let m = &desired[p.source()];
                TcLink {
                    revision: plan.revision(),
                    slot: p.slot(),
                    netns: first.details.netns.clone(),
                    key: first.details.key,
                    interface: first.details.interface.clone(),
                    priority: m.request.priority,
                    proceed_on: m.request.proceed_on,
                    dispatcher_id,
                    filter_priority: first.details.filter_priority,
                    filter_handle: first.details.filter_handle,
                }
            })
            .collect();
        let commits: Vec<_> = plan
            .placements()
            .iter()
            .zip(&details)
            .zip(extensions)
            .map(|((p, details), extension_link_id)| {
                let m = &desired[p.source()];
                TcMemberCommit {
                    identity: match m.identity {
                        XdpMemberIdentity::New => XdpMemberId::New,
                        XdpMemberIdentity::Existing(id) => XdpMemberId::Existing(id),
                    },
                    attachment: TcCommit {
                        program_id: m.request.program_id,
                        details,
                        extension_link_id,
                        metadata: &m.request.metadata,
                        created_at: &m.created_at,
                    },
                }
            })
            .collect();
        app.store
            .replace_tc(
                w,
                receipt,
                TcReplace {
                    updated_at: &chrono::Utc::now()
                        .to_rfc3339_opts(chrono::SecondsFormat::Nanos, true),
                    members: &commits,
                },
            )
            .map_err(|f| f.cause.into())
    })();
    match published {
        Err(cause) => Err(restore(app, w, publication.failed(cause))),
        Ok(committed) => {
            let retirement = publication.committed(committed);
            let snapshot = retirement.committed;
            let report = cleaned(
                app,
                w,
                None,
                retirement.cleanup,
                Vec::new(),
                Some(snapshot.clone()),
            )?;
            Ok((snapshot, report))
        }
    }
}

fn cleaned<S: TcStore, K: TcLifecycle>(
    app: &Bpfman<S, K>,
    w: &RuntimeWriter<'_>,
    cause: Option<LinkCause>,
    resources: Owned<S, K>,
    restorations: Vec<Result<(), LinkCause>>,
    committed: Option<TcDispatcherSnapshot>,
) -> Result<TcReport<S, K>, TcError<S, K>> {
    let report = cleanup(app, w, XdpCleanup::new(resources));
    // Forward failures remain errors even if compensation succeeded.
    if cause.is_some() || report.unresolved() != 0 {
        Err(TcError {
            cause,
            recovery: Some(Recovery::Cleanup(Box::new(report))),
            admission: None,
            restorations,
            committed,
        })
    } else {
        Ok(TcReport {
            cause,
            cleanup: report,
            restorations,
            committed,
        })
    }
}

fn rollback_error<S: TcStore, K: TcLifecycle>(
    app: &Bpfman<S, K>,
    w: &RuntimeWriter<'_>,
    rollback: bpfman_core::XdpReplacementRollback<Owned<S, K>, Owned<S, K>, LinkCause>,
) -> TcError<S, K> {
    let report = cleanup(app, w, XdpCleanup::new(rollback.cleanup));
    TcError {
        cause: Some(rollback.primary),
        recovery: Some(Recovery::Cleanup(Box::new(report))),
        admission: None,
        restorations: rollback.restoration_attempts,
        committed: None,
    }
}

pub(super) fn restore<S: TcStore, K: TcLifecycle>(
    app: &Bpfman<S, K>,
    w: &RuntimeWriter<'_>,
    restoration: Restoration<S, K>,
) -> TcError<S, K> {
    let step = restoration.restore();
    let outcome = app
        .kernel
        .restore_tc(w, step.receipt)
        .map_err(|f| EffectFailure {
            cause: f.cause.into(),
            remaining: f.remaining,
        });
    match step.next.completed(outcome) {
        Ok(rollback) => rollback_error(app, w, rollback),
        Err(failure) => TcError {
            cause: None,
            admission: None,
            recovery: Some(Recovery::Restore(Box::new(failure))),
            restorations: Vec::new(),
            committed: None,
        },
    }
}
