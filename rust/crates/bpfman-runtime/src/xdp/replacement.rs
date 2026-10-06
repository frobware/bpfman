//! Complete revision interpreter. The pure protocol gates retirement/restoration.
use super::*;
use bpfman_core::{
    XdpMemberIdentity, XdpMemberOrder, XdpMembershipPlan, XdpReplacement, XdpRevisionPlan,
    plan_xdp_membership,
};
use bpfman_kernel::XdpReplacement as Kernel;
use bpfman_model::{LinkDetails, Symbol, XdpDispatcherSnapshot};
use bpfman_store::{
    LinkReader, OpenStore, ProgramReader, XdpCommit, XdpMemberCommit, XdpMemberId, XdpReplace,
    XdpReplacementStore,
};

struct Member {
    identity: XdpMemberIdentity,
    name: Symbol,
    request: XdpAttach,
    created_at: String,
}

fn invalid(message: &'static str) -> LinkCause {
    Cause::Invalid(message).into()
}

fn members(snapshot: &XdpDispatcherSnapshot) -> Vec<Member> {
    snapshot
        .members()
        .iter()
        .map(|s| Member {
            identity: XdpMemberIdentity::Existing(s.member.id),
            name: s.program_name.clone(),
            request: XdpAttach {
                netns: s.details.netns.clone(),
                mode: Default::default(),
                program_id: s.member.program_id,
                interface: s.details.interface.clone(),
                priority: s.details.priority,
                proceed_on: s.details.proceed_on,
                metadata: s.member.metadata.clone(),
            },
            created_at: s.member.created_at.clone(),
        })
        .collect()
}

pub(super) fn attach<S: XdpReplacementStore, K: Kernel>(
    app: &Bpfman<S, K>,
    w: &RuntimeWriter<'_>,
    request: &XdpAttach,
    c: &Cancellation,
) -> Result<StoredLink, XdpError<S, K>>
where
    S::Reader: LinkReader,
{
    check(c)?;
    if request.priority > i32::MAX as u32 {
        return Err(invalid("priority exceeds i32::MAX").into());
    }
    let program = app
        .store
        .open(w)
        .map_err(LinkCause::from)?
        .read_records()
        .map_err(LinkCause::from)?
        .into_iter()
        .find(|p| p.id == request.program_id)
        .ok_or_else(|| LinkCause::from(Cause::NotFound))?;
    if !matches!(program.spec, bpfman_model::ProgramSpec::Xdp(_)) {
        return Err(LinkCause::from(Cause::Unsupported).into());
    }
    if w.layout().program_pin_path(program.id).to_str() != Some(program.pin_path.as_str()) {
        return Err(invalid("noncanonical managed program pin").into());
    }
    let (key, prepared) = app
        .kernel
        .prepare_xdp(w, request.program_id, &request.interface, &request.netns)
        .map_err(LinkCause::from)?;
    let Some((snapshot, receipt)) = app
        .store
        .observe_xdp_dispatcher(w, key)
        .map_err(LinkCause::from)?
    else {
        app.store
            .preflight_xdp(w, key, request.program_id)
            .map_err(LinkCause::from)?;
        return super::attach(
            w,
            &mut real::Adapter(&app.store, &app.kernel),
            request,
            c,
            Some((key, prepared)),
        )
        .map_err(|(cause, report)| XdpError::cleaned(Some(cause), report, Vec::new(), None));
    };
    let existing: Vec<_> = snapshot.members().iter().map(|m| m.member.id).collect();
    let mut desired = members(&snapshot);
    // Attach-point identity is inode/index based. Preserve the established
    // namespace path when another spelling resolves to that same attach point.
    let netns = snapshot
        .members()
        .first()
        .ok_or_else(|| invalid("empty dispatcher"))?
        .details
        .netns
        .clone();
    desired.push(Member {
        identity: XdpMemberIdentity::New,
        name: program.spec.name().clone(),
        request: XdpAttach {
            netns,
            mode: request.mode,
            program_id: request.program_id,
            interface: request.interface.clone(),
            priority: request.priority,
            proceed_on: request.proceed_on,
            metadata: request.metadata.clone(),
        },
        created_at: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Nanos, true),
    });
    let (committed, report) = replace(app, w, snapshot, receipt, &desired, c)?;
    // Publication includes exactly one new member; all survivors retain their IDs.
    if let Some(member) = committed
        .members()
        .iter()
        .find(|m| !existing.contains(&m.member.id))
    {
        Ok(member.member.clone())
    } else {
        // Even a violated adapter contract must not hide successful publication.
        Err(XdpError::cleaned(
            Some(invalid("publication omitted new member")),
            report.report,
            report.restorations,
            Some(committed),
        ))
    }
}

pub(super) fn detach<S: XdpReplacementStore, K: Kernel>(
    app: &Bpfman<S, K>,
    w: &RuntimeWriter<'_>,
    id: NonZeroU64,
    c: &Cancellation,
) -> Result<XdpReport<S, K>, XdpError<S, K>>
where
    S::Reader: LinkReader,
{
    check(c)?;
    let link = app
        .store
        .open(w)
        .map_err(LinkCause::from)?
        .read_links()
        .map_err(LinkCause::from)?
        .into_iter()
        .find(|l| l.id == id)
        .ok_or_else(|| LinkCause::from(Cause::NotFound))?;
    let LinkDetails::Xdp(details) = link.details else {
        return Err(LinkCause::from(Cause::Unsupported).into());
    };
    let (snapshot, receipt) = app
        .store
        .observe_xdp_dispatcher(w, details.key)
        .map_err(LinkCause::from)?
        .ok_or_else(|| invalid("missing dispatcher"))?;
    if !snapshot.members().iter().any(|m| m.member.id == id) {
        return Err(invalid("member missing from dispatcher").into());
    }
    if snapshot.members().len() == 1 {
        let mut f = real::Adapter(&app.store, &app.kernel);
        let resources = f.observe(w, id)?;
        check(c)?;
        return finish(cleanup(w, &mut f, XdpCleanup::new(resources)));
    }
    let desired: Vec<_> = members(&snapshot)
        .into_iter()
        .filter(|m| m.identity != XdpMemberIdentity::Existing(id))
        .collect();
    replace(app, w, snapshot, receipt, &desired, c).map(|(_, report)| report)
}

fn program<S: XdpStore, K: Kernel>(
    resources: &[Owned<S, K>],
) -> Result<&K::DispatcherPin, LinkCause> {
    resources
        .iter()
        .find_map(|r| match r {
            Resource::Program(p) => Some(p),
            _ => None,
        })
        .ok_or_else(|| invalid("missing dispatcher receipt"))
}

fn stage<S: XdpReplacementStore, K: Kernel>(
    app: &Bpfman<S, K>,
    w: &RuntimeWriter<'_>,
    key: XdpKey,
    desired: &[Member],
    plan: &XdpRevisionPlan,
    c: &Cancellation,
    resources: &mut Vec<Owned<S, K>>,
) -> Result<(), LinkCause> {
    let mut prepared = Vec::new();
    let mut frags = Vec::new();
    for placement in plan.placements() {
        check(c)?;
        let member = &desired[placement.source()];
        let (observed, p) = app.kernel.prepare_xdp(
            w,
            member.request.program_id,
            &member.request.interface,
            &member.request.netns,
        )?;
        if observed != key {
            return Err(invalid("interface identity changed"));
        }
        frags.push(
            app.kernel
                .xdp_frags(w, member.request.program_id, &member.name)?,
        );
        prepared.push((placement.slot(), p));
    }
    check(c)?;
    let config = plan
        .config()
        .clone()
        .with_frags(&frags)
        .map_err(|_| invalid("invalid fragment declarations"))?;
    let mut dispatcher = app.kernel.load_revision(&config)?;
    macro_rules! acquire {
        ($call:expr,$variant:ident) => {
            match $call {
                Ok(r) => r,
                Err(f) => {
                    resources.extend(f.remaining.map(Resource::$variant));
                    return Err(f.cause.into());
                }
            }
        };
    }
    let Some((_, first)) = prepared.first() else {
        return Err(invalid("empty replacement"));
    };
    check(c)?;
    let directory = acquire!(
        app.kernel.create_revision_at(w, first, plan.revision()),
        Directory
    );
    resources.push(Resource::Directory(directory));
    let Some(Resource::Directory(directory)) = resources.first() else {
        return Err(invalid("missing revision"));
    };
    check(c)?;
    let pin = acquire!(
        app.kernel.pin_dispatcher(w, directory, &mut dispatcher),
        Program
    );
    resources.push(Resource::Program(pin));
    for (slot, mut p) in prepared {
        let Some(Resource::Directory(directory)) = resources.first() else {
            return Err(invalid("missing revision"));
        };
        check(c)?;
        let extension = acquire!(
            app.kernel
                .pin_extension_at(w, &mut p, directory, &dispatcher, slot),
            Extension
        );
        resources.push(Resource::Extension(extension));
    }
    Ok(())
}

fn replace<S: XdpReplacementStore, K: Kernel>(
    app: &Bpfman<S, K>,
    w: &RuntimeWriter<'_>,
    snapshot: XdpDispatcherSnapshot,
    receipt: S::XdpReceipt,
    desired: &[Member],
    c: &Cancellation,
) -> Result<(XdpDispatcherSnapshot, XdpReport<S, K>), XdpError<S, K>> {
    let first = snapshot
        .members()
        .first()
        .ok_or_else(|| invalid("empty dispatcher"))?;
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
                    .map_err(|_| invalid("invalid priority"))?,
                proceed_on: m.request.proceed_on,
            })
        })
        .collect::<Result<Vec<_>, LinkCause>>()?;
    let XdpMembershipPlan::Revision(plan) =
        plan_xdp_membership(Some(first.details.revision), &order)
            .map_err(|e| LinkCause::from(Cause::XdpPlan(e)))?
    else {
        return Err(invalid("empty replacement plan").into());
    };
    for m in snapshot.members() {
        if w.layout().program_pin_path(m.member.program_id).to_str()
            != Some(m.program_pin_path.as_str())
            || w.layout()
                .xdp_slot_path(m.details.key, m.details.revision, m.details.slot)
                .to_str()
                != Some(m.member.pin_path.as_str())
        {
            return Err(invalid("noncanonical XDP pin").into());
        }
    }
    check(c)?;
    let artifacts = app
        .kernel
        .observe_dispatcher(w, &snapshot)
        .map_err(LinkCause::from)?;
    let (Some(outer), Some(old_program), Some(directory)) =
        (artifacts.outer, artifacts.program, artifacts.directory)
    else {
        return Err(invalid("incomplete active dispatcher artifacts").into());
    };
    if artifacts.extensions.len() != snapshot.members().len() {
        return Err(invalid("missing active extension").into());
    }
    let mut old = vec![
        Resource::Directory(directory),
        Resource::Program(old_program),
    ];
    old.extend(artifacts.extensions.into_iter().map(Resource::Extension));
    let mut staged = Vec::new();
    if let Err(cause) = stage(app, w, first.details.key, desired, &plan, c, &mut staged) {
        return Err(XdpError::cleaned(
            Some(cause),
            cleanup(
                w,
                &mut real::Adapter(&app.store, &app.kernel),
                XdpCleanup::new(staged),
            ),
            Vec::new(),
            None,
        ));
    }
    let protocol = XdpReplacement::new(old, staged);
    let switch = (|| -> Result<_, EffectFailure<Option<K::Switch>, LinkCause>> {
        let fail = |cause| EffectFailure {
            cause,
            remaining: None,
        };
        check(c).map_err(fail)?;
        let old = program::<S, K>(protocol.old()).map_err(fail)?;
        let new = program::<S, K>(protocol.staged()).map_err(fail)?;
        app.kernel
            .switch_dispatcher(w, &outer, old, new)
            .map_err(|e| EffectFailure {
                cause: e.cause.into(),
                remaining: e.remaining,
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
            let blocked = protocol.staged().len();
            return Err(restore(
                app,
                w,
                protocol.switch_failed(EffectFailure {
                    cause,
                    remaining: s,
                }),
                blocked,
            ));
        }
    };
    let blocked = publication.staged().len();
    let published = (|| -> Result<XdpDispatcherSnapshot, LinkCause> {
        check(c)?;
        let dispatcher_id = K::dispatcher_id(program::<S, K>(publication.staged())?);
        let extensions: Vec<_> = publication
            .staged()
            .iter()
            .filter_map(|r| match r {
                Resource::Extension(e) => Some(K::extension_id(e)),
                _ => None,
            })
            .collect();
        let details: Vec<_> = plan
            .placements()
            .iter()
            .map(|p| {
                let r = &desired[p.source()].request;
                XdpLink {
                    netns: r.netns.clone(),
                    slot: p.slot(),
                    key: first.details.key,
                    interface: r.interface.clone(),
                    priority: r.priority,
                    proceed_on: r.proceed_on,
                    dispatcher_id,
                    revision: plan.revision(),
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
                XdpMemberCommit {
                    identity: match m.identity {
                        XdpMemberIdentity::New => XdpMemberId::New,
                        XdpMemberIdentity::Existing(id) => XdpMemberId::Existing(id),
                    },
                    attachment: XdpCommit {
                        program_id: m.request.program_id,
                        details,
                        extension_link_id,
                        outer_link_id: first.outer_link_id,
                        metadata: &m.request.metadata,
                        created_at: &m.created_at,
                    },
                }
            })
            .collect();
        let updated_at = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Nanos, true);
        // The atomic store result decides ownership even if cancellation arrives in flight.
        app.store
            .replace_xdp(
                w,
                receipt,
                XdpReplace {
                    updated_at: &updated_at,
                    members: &commits,
                },
            )
            .map_err(|e| e.cause.into())
    })();
    match published {
        Err(cause) => Err(restore(app, w, publication.failed(cause), blocked)),
        Ok(committed) => {
            let retirement = publication.committed(committed);
            let report = cleanup(
                w,
                &mut real::Adapter(&app.store, &app.kernel),
                XdpCleanup::new(retirement.cleanup),
            );
            if report.unresolved() != 0 {
                return Err(XdpError::cleaned(
                    None,
                    report,
                    Vec::new(),
                    Some(retirement.committed),
                ));
            }
            let snapshot = retirement.committed;
            Ok((
                snapshot.clone(),
                XdpReport {
                    report,
                    primary: None,
                    restorations: Vec::new(),
                    committed: Some(snapshot),
                },
            ))
        }
    }
}

fn rollback_error<S: XdpStore, K: Kernel>(
    app: &Bpfman<S, K>,
    w: &RuntimeWriter<'_>,
    rollback: Rollback<S, K>,
) -> XdpError<S, K> {
    let report = cleanup(
        w,
        &mut real::Adapter(&app.store, &app.kernel),
        XdpCleanup::new(rollback.cleanup),
    );
    XdpError::cleaned(
        Some(rollback.primary),
        report,
        rollback.restoration_attempts,
        None,
    )
}

pub(super) fn restore<S: XdpStore, K: Kernel>(
    app: &Bpfman<S, K>,
    w: &RuntimeWriter<'_>,
    restoration: Restoration<S, K>,
    blocked: usize,
) -> XdpError<S, K> {
    let step = restoration.restore();
    let outcome = app
        .kernel
        .restore_dispatcher(w, step.receipt)
        .map_err(|e| EffectFailure {
            cause: e.cause.into(),
            remaining: e.remaining,
        });
    match step.next.completed(outcome) {
        Ok(rollback) => rollback_error(app, w, rollback),
        Err(failure) => XdpError {
            primary: None,
            recovery: Some(Recovery::Restore {
                failure: Box::new(failure),
                blocked,
            }),
            admission: None,
            restorations: Vec::new(),
            committed: None,
        },
    }
}

type Rollback<S, K> =
    bpfman_core::XdpReplacementRollback<Vec<Owned<S, K>>, Vec<Owned<S, K>>, LinkCause>;
type Restoration<S, K> = bpfman_core::XdpRestoration<
    Vec<Owned<S, K>>,
    Vec<Owned<S, K>>,
    <K as Kernel>::Switch,
    LinkCause,
>;
