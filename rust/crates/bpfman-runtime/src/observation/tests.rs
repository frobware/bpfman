#![allow(clippy::expect_used)]

use super::*;
use crate::sample;
#[derive(Clone, Copy)]
enum Fault {
    None,
    Missing,
    Denied,
    Map,
}

struct Fake {
    fault: Fault,
    calls: Vec<String>,
}

impl KernelObservations for Fake {
    fn program(
        &mut self,
        id: NonZeroU32,
    ) -> Result<(KernelProgram, Option<ProgramStats>), Failure> {
        self.calls.push(format!("program:{id}"));

        match self.fault {
            Fault::Missing => Err(Failure::Reconciliation {
                id,
                cause: std::io::Error::from(std::io::ErrorKind::NotFound).into(),
            }),
            Fault::Denied => Err(Failure::Kernel(
                std::io::Error::from(std::io::ErrorKind::PermissionDenied).into(),
            )),
            _ => Ok((
                sample::kernel(),
                Some(ProgramStats {
                    runtime_ns: 0,
                    run_count: 0,
                    recursion_misses: 0,
                }),
            )),
        }
    }

    fn map(&mut self, id: u32) -> Result<KernelMap, Failure> {
        self.calls.push(format!("map:{id}"));

        if matches!(self.fault, Fault::Map) {
            Err(Failure::Kernel(
                std::io::Error::from(std::io::ErrorKind::PermissionDenied).into(),
            ))
        } else {
            Ok(sample::map())
        }
    }
}

fn fake(fault: Fault) -> Fake {
    Fake {
        fault,
        calls: Vec::new(),
    }
}

#[test]
fn load_and_get_keep_distinct_stats_and_pin_observations() {
    for view in [View::Load, View::Get] {
        let mut effects = fake(Fault::None);
        let layout =
            RuntimeLayout::try_from(std::path::PathBuf::from("/tmp/runtime")).expect("layout");

        // Two pin names share a truncated prefix; correlation must use identity.
        let pins = vec![
            bpfman_fs::ObservedMapPin {
                name: "tracepoint_stats_wrong".into(),
                id: 99,
            },
            bpfman_fs::ObservedMapPin {
                name: "tracepoint_stats_map".into(),
                id: 100,
            },
        ];
        let result = build(
            &mut effects,
            &layout,
            sample::record(),
            vec![NonZeroU32::new(42).expect("id")],
            pins,
            view,
        )
        .expect("observation");

        assert_eq!(effects.calls, ["program:42", "map:100"]);

        match view {
            View::Load => {
                assert!(result.stats.is_none());
                assert!(result.maps[0].pin_path.is_none());
                assert!(!result.maps[0].present);
            }
            View::Get => {
                assert_eq!(result.stats.expect("stats").runtime_ns, 0);
                assert_eq!(
                    result.maps[0].pin_path.as_deref(),
                    Some("/tmp/runtime/fs/maps/42/tracepoint_stats_map")
                );
                assert!(result.maps[0].present);
            }
        }
    }
}

#[test]
fn disappearance_and_permission_failure_are_not_interchangeable() {
    for fault in [Fault::Missing, Fault::Denied] {
        let mut effects = fake(fault);
        let layout =
            RuntimeLayout::try_from(std::path::PathBuf::from("/tmp/runtime")).expect("layout");
        let err = build(
            &mut effects,
            &layout,
            sample::record(),
            Vec::new(),
            Vec::new(),
            View::Get,
        )
        .expect_err("failure");
        let expected = if matches!(fault, Fault::Missing) {
            ObservationErrorKind::RequiresReconciliation
        } else {
            ObservationErrorKind::Unavailable
        };

        assert_eq!(ObservationError::from(err).kind(), expected);
        assert_eq!(effects.calls, ["program:42"]);

        let list = entries(&mut fake(fault), vec![sample::record()]);

        if matches!(fault, Fault::Missing) {
            assert!(list.expect("missing is null")[0].kernel.is_none());
        } else {
            assert!(list.is_err());
        }
    }
}

#[test]
fn failed_map_is_omitted_without_inventing_attributes() {
    let mut effects = fake(Fault::Map);
    let layout = RuntimeLayout::try_from(std::path::PathBuf::from("/tmp/runtime")).expect("layout");
    let result = build(
        &mut effects,
        &layout,
        sample::record(),
        Vec::new(),
        Vec::new(),
        View::Get,
    )
    .expect("best effort map");

    assert_eq!(result.kernel.map_ids, Some(vec![100]));
    assert!(result.maps.is_empty());
}
