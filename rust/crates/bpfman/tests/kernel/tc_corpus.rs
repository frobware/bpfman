//! Unchanged Go TC chain, ordering, encoding and member-unload scripts on both stores.
macro_rules! backend_tests {
    ($module:ident,$store:literal) => {
        mod $module {
            #[test]
            fn tc_ingress_all_proceed() {
                crate::cli::dsl($store, "TestMultiProgTC_AllProceed_CustomProceedOn");
            }

            #[test]
            fn tc_ingress_stop_ok() {
                crate::cli::dsl($store, "TestMultiProgTC_ChainStopsAtOK_DefaultProceedOn");
            }

            #[test]
            fn tc_ingress_stop_pipe() {
                crate::cli::dsl($store, "TestMultiProgTC_ChainStopsAtPipe_CustomProceedOn");
            }

            #[test]
            fn tc_ingress_priority() {
                crate::cli::dsl($store, "TestDispatcher_PriorityOrderingTC");
            }

            #[test]
            fn tc_ingress_zero_priority() {
                crate::cli::dsl($store, "TestDispatcher_ZeroPriorityDefaultOrderingTC");
            }

            #[test]
            fn tc_ingress_name_tie() {
                crate::cli::dsl($store, "TestTC_DispatcherPriorityTieBreakByName");
            }

            #[test]
            fn tc_ingress_fill_drain_refill() {
                crate::cli::dsl($store, "TestTC_DispatcherFillDrainRefill");
            }

            #[test]
            fn tc_ingress_ten_slot_execution() {
                crate::cli::dsl($store, "TestTC_DispatcherChainExecution");
            }

            #[test]
            fn tc_ingress_encoding_matrix() {
                crate::cli::dsl($store, "TestTC_ProceedOnEncodingMatrix");
            }

            #[test]
            fn tc_ingress_netns_rebuild() {
                crate::cli::dsl($store, "TestTC_NetnsVethPairDispatcherRebuild");
            }

            #[test]
            fn tc_ingress_unload_survivor() {
                crate::cli::dsl($store, "TestTC_UnloadDispatcherMemberRebuildsSurvivor");
            }

            #[test]
            fn tc_ingress_clsact_reclaimed() {
                crate::cli::dsl($store, "TestTC_ClsactReclaimedOnLastDetach");
            }
        }
    };
}

backend_tests!(sqlite, "sqlite");
backend_tests!(json, "json");
