//! Unchanged Go XDP acceptance: traffic, repeated membership changes, and ordering.
//! Every scenario runs independently against both concrete stores.

macro_rules! backend_tests {
    ($module:ident, $store:literal) => {
        mod $module {
            #[test]
            fn fill_drain_refill() {
                crate::cli::dsl($store, "TestXDP_DispatcherFillDrainRefill");
            }

            #[test]
            fn ten_slot_chain() {
                crate::cli::dsl($store, "TestXDP_DispatcherChainExecution");
            }

            #[test]
            fn all_proceed_default() {
                crate::cli::dsl($store, "TestMultiProgXDP_AllProceed_DefaultProceedOn");
            }

            #[test]
            fn all_proceed_custom() {
                crate::cli::dsl($store, "TestMultiProgXDP_AllProceed_CustomProceedOn");
            }

            #[test]
            fn stops_at_drop() {
                crate::cli::dsl($store, "TestMultiProgXDP_ChainStopsAtDrop_DefaultProceedOn");
            }

            #[test]
            fn stops_at_pass() {
                crate::cli::dsl($store, "TestMultiProgXDP_ChainStopsAtPass_CustomProceedOn");
            }

            #[test]
            fn proceed_on_pass_encoding() {
                crate::cli::dsl($store, "TestXDP_ProceedOnPassEncoding");
            }

            #[test]
            fn proceed_on_encoding_matrix() {
                crate::cli::dsl($store, "TestXDP_ProceedOnEncodingMatrix");
            }

            #[test]
            fn priority_name_tie_break() {
                crate::cli::dsl($store, "TestXDP_DispatcherPriorityTieBreakByName");
            }

            #[test]
            fn priority_zero() {
                crate::cli::dsl($store, "TestDispatcher_ZeroPriorityDefaultOrderingXDP");
            }

            #[test]
            fn independent_interfaces() {
                crate::cli::dsl($store, "TestDispatcher_MultipleInterfacesIndependentXDP");
            }
        }
    };
}

backend_tests!(sqlite, "sqlite");
backend_tests!(json, "json");
