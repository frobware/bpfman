//! Pure membership, replacement ownership, and dependency-ordered retry contracts.

#![allow(clippy::expect_used, clippy::panic)]

use bpfman_core::{
    EffectFailure, XdpCleanup, XdpCleanupKind, XdpCleanupReport, XdpCleanupStep, XdpMemberIdentity,
    XdpMemberOrder, XdpMembershipPlan, XdpPlanError, XdpReplacement, XdpResource, XdpRevisionPlan,
    plan_xdp_membership,
};
use bpfman_model::XdpProceedOn;
use std::num::{NonZeroU32, NonZeroU64};

fn member(id: Option<u64>, name: &str, priority: u32, action: u32) -> XdpMemberOrder {
    XdpMemberOrder {
        identity: id.map_or(XdpMemberIdentity::New, |id| {
            XdpMemberIdentity::Existing(NonZeroU64::new(id).expect("nonzero link"))
        }),
        name: name.try_into().expect("symbol"),
        priority: priority.try_into().expect("priority"),
        proceed_on: action.try_into().expect("mask"),
    }
}

fn revision(current: Option<u32>, members: &[XdpMemberOrder]) -> XdpRevisionPlan {
    let plan = plan_xdp_membership(current.and_then(NonZeroU32::new), members).expect("plan");
    let XdpMembershipPlan::Revision(plan) = plan else {
        panic!("expected revision");
    };
    plan
}

fn sources(plan: &XdpRevisionPlan) -> Vec<usize> {
    plan.placements().iter().map(|p| p.source()).collect()
}

#[test]
fn one_two_one_zero_planning_preserves_go_order_and_reclaims_slots() {
    let first = member(None, "old", 50, 1 << 2);
    let one = revision(None, std::slice::from_ref(&first));
    assert_eq!(one.revision().get(), 1);
    assert_eq!(sources(&one), [0]);

    let existing = member(Some(42), "old", 50, 1 << 2);
    let new = member(None, "z_new", 50, 1 << 31);
    let two = revision(Some(1), &[existing.clone(), new.clone()]);
    // Unattached precedes attached, even when its name sorts later.
    assert_eq!(sources(&two), [1, 0]);
    assert_eq!(two.revision().get(), 2);
    assert_eq!(&two.config().bytes()[..4], &[236, 2, 2, 0]);
    assert_eq!(&two.config().bytes()[4..8], &(1u32 << 31).to_ne_bytes());
    assert_eq!(&two.config().bytes()[8..12], &(1u32 << 2).to_ne_bytes());

    // Removing either member reconstructs a one-slot configuration, clearing
    // the previous slot's proceed-on bits rather than retaining stale config.
    for survivor in [existing, member(Some(43), "z_new", 50, 1 << 31)] {
        let one = revision(Some(2), std::slice::from_ref(&survivor));
        assert_eq!(one.revision().get(), 3);
        assert_eq!(one.placements()[0].slot().index(), 0);
        assert_eq!(
            &one.config().bytes()[4..8],
            &survivor.proceed_on.mask().to_ne_bytes()
        );
        assert!(one.config().bytes()[8..44].iter().all(|&b| b == 0));
    }
    assert_eq!(
        plan_xdp_membership(NonZeroU32::new(3), &[]),
        Ok(XdpMembershipPlan::Remove)
    );
}

#[test]
fn ordering_matches_go_for_every_permutation_and_zero_priority() {
    let members = [
        member(Some(1), "b", 20, 1),
        member(Some(2), "a", 20, 2),
        member(None, "z", 20, 4),
        member(Some(3), "zero", 0, 8),
    ];
    for a in 0..4 {
        for b in 0..4 {
            for c in 0..4 {
                for d in 0..4 {
                    let indices = [a, b, c, d];
                    if (0..4).any(|i| indices[i + 1..].contains(&indices[i])) {
                        continue;
                    }
                    let input: Vec<_> = indices.iter().map(|&i| members[i].clone()).collect();
                    let plan = revision(Some(7), &input);
                    let names: Vec<_> = plan
                        .placements()
                        .iter()
                        .map(|p| input[p.source()].name.as_str())
                        .collect();
                    assert_eq!(names, ["zero", "z", "a", "b"]);
                    assert_eq!(plan.revision().get(), 8);
                    for (slot, placement) in plan.placements().iter().enumerate() {
                        assert_eq!(placement.slot().index(), slot);
                    }
                }
            }
        }
    }
}

#[test]
fn exact_sort_ties_preserve_stored_order_not_link_id_order() {
    let input = [
        member(Some(99), "same", 0, 1),
        member(Some(1), "same", 0, 2),
    ];
    let plan = revision(Some(1), &input);
    assert_eq!(sources(&plan), [0, 1]);
}

#[test]
fn invalid_membership_and_revision_exhaustion_fail_before_effects() {
    let current = NonZeroU32::new(1);
    let mut members: Vec<_> = (1..=10).map(|id| member(Some(id), "p", 0, 0)).collect();
    let full = revision(Some(1), &members);
    assert_eq!(full.placements().len(), 10);
    assert_eq!(full.placements()[9].slot().index(), 9);
    members.push(member(None, "new", 0, 0));
    assert_eq!(
        plan_xdp_membership(current, &members),
        Err(XdpPlanError::Capacity)
    );
    assert_eq!(
        plan_xdp_membership(None, &[]),
        Err(XdpPlanError::Membership)
    );
    assert_eq!(
        plan_xdp_membership(None, &members[..1]),
        Err(XdpPlanError::Membership)
    );
    assert_eq!(
        plan_xdp_membership(current, &[members[0].clone(), members[0].clone()]),
        Err(XdpPlanError::DuplicateLink)
    );
    assert_eq!(
        plan_xdp_membership(current, &[member(None, "a", 0, 0), member(None, "b", 0, 0)]),
        Err(XdpPlanError::Membership)
    );
    assert_eq!(
        plan_xdp_membership(NonZeroU32::new(u32::MAX), &members[..1]),
        Err(XdpPlanError::RevisionExhausted)
    );
    // Last detach needs no new revision, including at exhaustion.
    assert_eq!(
        plan_xdp_membership(NonZeroU32::new(u32::MAX), &[]),
        Ok(XdpMembershipPlan::Remove)
    );
}

// Deliberately not Clone: policy must move opaque ownership through every path.
struct Revision(&'static str);
struct Switch(u32);

#[test]
fn rejected_switch_and_pre_switch_cancellation_only_discard_staged_revision() {
    for cause in ["switch rejected", "cancelled"] {
        let operation = XdpReplacement::new(Revision("old"), Revision("new"));
        assert_eq!(operation.old().0, "old");
        assert_eq!(operation.staged().0, "new");
        let rollback = operation.rejected(cause);
        assert_eq!(rollback.retained.0, "old");
        assert_eq!(rollback.cleanup.0, "new");
        assert_eq!(rollback.primary, cause);
        assert!(rollback.restoration_attempts.is_empty());
    }
}

#[test]
fn committed_publication_only_retires_old_revision() {
    let publication = XdpReplacement::new(Revision("old"), Revision("new")).switched(Switch(17));
    assert_eq!(publication.staged().0, "new");
    let retirement = publication.committed("stored snapshot");
    assert_eq!(retirement.retained.0, "new");
    assert_eq!(retirement.cleanup.0, "old");
    assert_eq!(retirement.switch.0, 17);
    assert_eq!(retirement.committed, "stored snapshot");
}

#[test]
fn every_post_switch_failure_blocks_cleanup_until_restored_and_keeps_history() {
    for cause in [
        "switch changed target then failed",
        "publication failed",
        "cancelled",
    ] {
        let replacement = XdpReplacement::new(Revision("old"), Revision("new"));
        let restoration = if cause.starts_with("switch") {
            replacement.switch_failed(EffectFailure {
                remaining: Switch(17),
                cause,
            })
        } else {
            replacement.switched(Switch(17)).failed(cause)
        };
        let step = restoration.restore();
        assert_eq!(step.receipt.0, 17);
        let Err(failure) = step.next.completed(Err(EffectFailure {
            remaining: step.receipt,
            cause: "restore failed",
        })) else {
            panic!("restoration failure must retain both revisions");
        };
        assert_eq!(failure.primary(), &cause);
        assert_eq!(failure.attempts(), [Err("restore failed")]);

        let step = failure.retry().restore();
        let Err(failure) = step.next.completed(Err(EffectFailure {
            remaining: step.receipt,
            cause: "foreign runtime rejected",
        })) else {
            panic!("foreign retry must preserve ownership");
        };
        assert_eq!(failure.primary(), &cause);
        assert_eq!(
            failure.attempts(),
            [Err("restore failed"), Err("foreign runtime rejected")]
        );
        let step = failure.retry().restore();
        assert_eq!(step.receipt.0, 17);
        let Ok(rollback) = step.next.completed(Ok(())) else {
            panic!("restoration should unlock staged cleanup");
        };
        assert_eq!(rollback.retained.0, "old");
        assert_eq!(rollback.cleanup.0, "new");
        assert_eq!(rollback.primary, cause);
        assert_eq!(
            rollback.restoration_attempts,
            [
                Err("restore failed"),
                Err("foreign runtime rejected"),
                Ok(())
            ]
        );
    }
}

struct Receipt {
    id: usize,
    kind: XdpCleanupKind,
}

impl XdpResource for Receipt {
    fn kind(&self) -> XdpCleanupKind {
        self.kind
    }
}

fn pass(
    mut operation: XdpCleanup<Receipt, usize>,
    failures: usize,
) -> XdpCleanupReport<Receipt, usize> {
    loop {
        match operation.next() {
            XdpCleanupStep::Complete(report) => return report,
            XdpCleanupStep::Effect { receipt, next } => {
                operation = next.completed(if failures & (1 << receipt.id) != 0 {
                    Err(EffectFailure {
                        cause: receipt.id,
                        remaining: receipt,
                    })
                } else {
                    Ok(())
                });
            }
        }
    }
}

#[test]
fn multiple_extensions_cleanup_independently_with_stable_retry_identity() {
    use XdpCleanupKind::*;
    // Every combination of two extension failures and a dispatcher-pin failure.
    for failures in 0..8 {
        let resources = [Extension, Extension, Program, Directory]
            .into_iter()
            .enumerate()
            .map(|(id, kind)| Receipt { id, kind })
            .collect();
        let report = pass(XdpCleanup::new(resources), failures);
        let attempted: Vec<_> = report.attempts().iter().map(|a| a.id).collect();
        assert_eq!(&attempted[..3], &[0, 1, 2]);
        if failures == 0 {
            assert_eq!(attempted, [0, 1, 2, 3]);
            assert_eq!(report.unresolved(), 0);
        } else {
            assert_eq!(attempted, [0, 1, 2]);
            assert_eq!(report.unresolved(), failures.count_ones() as usize + 1);
            let report = pass(report.retry(), 0);
            assert_eq!(report.unresolved(), 0);
            let retried: Vec<_> = report.attempts()[3..].iter().map(|a| a.id).collect();
            let expected: Vec<_> = (0..3)
                .filter(|id| failures & (1 << id) != 0)
                .chain([3])
                .collect();
            assert_eq!(retried, expected);
            for attempt in &report.attempts()[..3] {
                assert_eq!(attempt.outcome.is_err(), failures & (1 << attempt.id) != 0);
            }
        }
    }
}

#[test]
fn default_proceed_on_is_preserved_in_planning() {
    assert_eq!(XdpProceedOn::default().mask(), (1 << 2) | (1 << 31));
}
