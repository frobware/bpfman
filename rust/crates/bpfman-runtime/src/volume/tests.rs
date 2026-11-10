#![allow(clippy::expect_used)]
use super::*;

fn pins() -> Vec<ObservedMapPin> {
    vec![
        ObservedMapPin {
            name: "first".into(),
            id: 7,
        },
        ObservedMapPin {
            name: "second".into(),
            id: 9,
        },
    ]
}

fn inventory() -> BTreeMap<String, u32> {
    [("first".into(), 7), ("second".into(), 9)].into()
}

#[test]
fn published_maps_do_not_need_to_be_used_by_one_function() {
    for used in [Some([7].as_slice()), None] {
        admit_maps(
            "app",
            &["first".into(), "second".into()],
            &pins(),
            Some(&inventory()),
            used,
        )
        .expect("complete published inventory supplies map identity evidence");
    }
}

#[test]
fn replaced_identity_is_rejected_even_if_the_function_uses_it() {
    let mut observed = pins();
    observed[1].id = 11;
    assert_eq!(
        admit_maps(
            "app",
            &["second".into()],
            &observed,
            Some(&inventory()),
            Some(&[7, 11])
        )
        .expect_err("a live but different map is not the published map")
        .kind(),
        VolumeErrorKind::FailedPrecondition,
    );
}

#[test]
fn missing_or_unrecorded_names_are_not_published() {
    let mut observed = pins();
    observed.push(ObservedMapPin {
        name: "injected".into(),
        id: 7,
    });
    for name in ["absent", "injected"] {
        assert_eq!(
            admit_maps(
                "app",
                &[name.into()],
                &observed,
                Some(&inventory()),
                Some(&[7, 9])
            )
            .expect_err("names require both a published entry and a matching pin")
            .kind(),
            VolumeErrorKind::NotFound
        );
    }
    assert_eq!(
        admit_maps(
            "app",
            &["second".into()],
            &observed[..1],
            Some(&inventory()),
            Some(&[7, 9])
        )
        .expect_err("inventory cannot replace a missing pin")
        .kind(),
        VolumeErrorKind::NotFound
    );
}

#[test]
fn empty_inventory_does_not_fall_back_to_kernel_membership() {
    assert_eq!(
        admit_maps(
            "app",
            &["first".into()],
            &pins(),
            Some(&BTreeMap::new()),
            Some(&[7])
        )
        .expect_err("present inventory is authoritative")
        .kind(),
        VolumeErrorKind::NotFound
    );
}

#[test]
fn older_artifacts_require_live_kernel_membership() {
    admit_maps("app", &["first".into()], &pins(), None, Some(&[7])).expect("legacy used map");
    assert_eq!(
        admit_maps("app", &["second".into()], &pins(), None, Some(&[7]))
            .expect_err("unused map without published evidence")
            .kind(),
        VolumeErrorKind::NotFound
    );
    assert_eq!(
        admit_maps("app", &["first".into()], &pins(), None, None)
            .expect_err("no identity evidence")
            .kind(),
        VolumeErrorKind::Internal
    );
}
