use crate::{StoreObservation, StoreOpenPlan};

/// Decide how to open the store, preserving evidence in every branch.
///
/// The adapter supplies its supported version. Unreadable/corrupt state is an
/// observation failure, never absence. The interpreter must gather and apply
/// these observations under the same writer lock.
pub fn plan_store_open<T>(observed: StoreObservation<T>, supported: i64) -> StoreOpenPlan<T> {
    match observed {
        StoreObservation::Missing => StoreOpenPlan::Create,
        StoreObservation::Existing { version, evidence } if version == supported => {
            StoreOpenPlan::UseExisting(evidence)
        }
        StoreObservation::Existing {
            version: found,
            evidence,
        } => StoreOpenPlan::RejectIncompatible {
            found,
            expected: supported,
            evidence,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_state_is_created() {
        assert_eq!(
            plan_store_open(StoreObservation::<()>::Missing, 2),
            StoreOpenPlan::Create
        );
    }

    #[test]
    fn compatible_state_is_used_without_mutation() {
        for supported in [1, 2, 42] {
            assert_eq!(
                plan_store_open(
                    StoreObservation::Existing {
                        version: supported,
                        evidence: "opened store"
                    },
                    supported
                ),
                StoreOpenPlan::UseExisting("opened store")
            );
        }
    }

    #[test]
    fn incompatible_state_is_rejected_not_repaired() {
        for found in [i64::MIN, -1, 0, 1, 3, i64::MAX] {
            assert_eq!(
                plan_store_open(
                    StoreObservation::Existing {
                        version: found,
                        evidence: "opened store"
                    },
                    2
                ),
                StoreOpenPlan::RejectIncompatible {
                    found,
                    expected: 2,
                    evidence: "opened store"
                }
            );
        }
    }

    #[test]
    fn policy_returns_non_clone_evidence_without_running_its_destructor() {
        use core::cell::Cell;

        struct Evidence<'a>(&'a Cell<u32>);
        impl Drop for Evidence<'_> {
            fn drop(&mut self) {
                self.0.set(self.0.get() + 1);
            }
        }

        for version in [1, 2, 3] {
            let drops = Cell::new(0);
            let decision = plan_store_open(
                StoreObservation::Existing {
                    version,
                    evidence: Evidence(&drops),
                },
                2,
            );
            assert_eq!(
                drops.get(),
                0,
                "policy must not release interpreter resources"
            );
            assert!(matches!(
                decision,
                StoreOpenPlan::UseExisting(_) | StoreOpenPlan::RejectIncompatible { .. }
            ));
            drop(decision);
            assert_eq!(drops.get(), 1);
        }
    }
}
