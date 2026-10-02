use crate::StoreSetup;

/// Decide setup from an observed schema version; `None` means no store exists.
///
/// The adapter supplies its supported version. Unreadable/corrupt state is an
/// observation failure, never absence. The interpreter must gather and apply
/// these observations under the same writer lock.
pub fn plan_store_setup(observed: Option<i64>, supported: i64) -> StoreSetup {
    match observed {
        None => StoreSetup::Initialise,
        Some(found) if found == supported => StoreSetup::UseExisting,
        Some(found) => StoreSetup::RejectIncompatible {
            found,
            expected: supported,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_state_is_initialised() {
        assert_eq!(plan_store_setup(None, 2), StoreSetup::Initialise);
    }

    #[test]
    fn compatible_state_is_used_without_mutation() {
        for supported in [1, 2, 42] {
            assert_eq!(
                plan_store_setup(Some(supported), supported),
                StoreSetup::UseExisting
            );
        }
    }

    #[test]
    fn incompatible_state_is_rejected_not_repaired() {
        for found in [i64::MIN, -1, 0, 1, 3, i64::MAX] {
            assert_eq!(
                plan_store_setup(Some(found), 2),
                StoreSetup::RejectIncompatible { found, expected: 2 }
            );
        }
    }
}
