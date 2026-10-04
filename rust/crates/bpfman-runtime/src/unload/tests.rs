//! Faults exercise the production forward interpreter, not a second unload plan.
#![allow(clippy::expect_used)]

use super::*;
use bpfman_core::UnloadKind;
use bpfman_fs::{RuntimeDirectory, RuntimeLayout};
use std::time::Duration;
use std::{cell::Cell, collections::BTreeSet, path::PathBuf, rc::Rc};

const WORK: [&str; 8] = [
    "pin",
    "record",
    "map-a",
    "map-b",
    "map-c",
    "directory",
    "map-set",
    "bytecode",
];
struct Receipt {
    name: &'static str,
    root: PathBuf,
    drops: Rc<Cell<usize>>,
}

impl Drop for Receipt {
    fn drop(&mut self) {
        self.drops.set(self.drops.get() + 1);
    }
}

struct Pin(Receipt);
struct Record(Receipt);
struct Map(Receipt);
struct Directory(Receipt);
struct MapSet(Receipt);
struct Bytecode(Receipt);
#[derive(Debug)]
struct Fault(&'static str);
struct Fake {
    state: BTreeSet<&'static str>,
    faults: BTreeSet<&'static str>,
    calls: Vec<&'static str>,
    minted: usize,
    cancel_on: Option<&'static str>,
    cancellation: crate::Cancellation,
    drops: Rc<Cell<usize>>,
}

impl Fake {
    fn new(faults: BTreeSet<&'static str>) -> Self {
        Self {
            state: WORK
                .into_iter()
                .chain(["unrelated-program", "shared-map", "unrelated-bytecode"])
                .collect(),
            faults,
            calls: Vec::new(),
            minted: 0,
            cancel_on: None,
            cancellation: crate::Cancellation::new(),
            drops: Rc::new(Cell::new(0)),
        }
    }

    fn receipt(&mut self, writer: &RuntimeWriter<'_>, name: &'static str) -> Receipt {
        self.minted += 1;
        Receipt {
            name,
            root: writer.layout().root().into(),
            drops: self.drops.clone(),
        }
    }

    fn observe(&mut self, name: &'static str) -> Result<(), Fault> {
        self.calls.push(name);
        if self.cancel_on == Some(name) {
            self.cancellation.cancel();
        }

        if self.faults.contains(name) {
            Err(Fault(name))
        } else {
            Ok(())
        }
    }

    fn step(
        &mut self,
        writer: &RuntimeWriter<'_>,
        receipt: Receipt,
    ) -> Result<(), EffectFailure<Receipt, Fault>> {
        let name = receipt.name;
        self.calls.push(name);
        if self.cancel_on == Some(name) {
            self.cancellation.cancel();
        }

        assert!(
            self.state.contains(name),
            "successful work must never be repeated: {name}"
        );

        if receipt.root != writer.layout().root() {
            return Err(EffectFailure {
                remaining: receipt,
                cause: Fault("wrong writer"),
            });
        }

        match name {
            "link-a-record" => assert!(!self.state.contains("link-a-pin")),
            "link-b-pin" => assert!(!self.state.contains("link-a-record")),
            "link-b-record" => assert!(!self.state.contains("link-b-pin")),
            "pin" => assert!(
                !self.state.contains("link-a-record") && !self.state.contains("link-b-record")
            ),
            "record" | "bytecode" => assert!(!self.state.contains("pin")),
            "map-a" | "map-b" | "map-c" => assert!(!self.state.contains("record")),
            "directory" => assert!(
                !["record", "map-a", "map-b", "map-c"]
                    .iter()
                    .any(|n| self.state.contains(n))
            ),
            "map-set" => {
                assert!(!self.state.contains("record") && !self.state.contains("directory"))
            }
            _ => {}
        }

        if self.faults.contains(name) {
            return Err(EffectFailure {
                remaining: receipt,
                cause: Fault(name),
            });
        }

        assert!(self.state.remove(name));

        Ok(())
    }

    fn residue(&self) -> BTreeSet<&'static str> {
        self.state
            .intersection(&WORK.into_iter().collect())
            .copied()
            .collect()
    }

    fn unrelated_preserved(&self) {
        for name in ["unrelated-program", "shared-map", "unrelated-bytecode"] {
            assert!(self.state.contains(name));
        }
    }
}

impl UnloadEffects for Fake {
    type LinkPin = Receipt;
    type LinkRecord = Receipt;
    type Pin = Pin;
    type Record = Record;
    type Map = Map;
    type Directory = Directory;
    type MapSet = MapSet;
    type Bytecode = Bytecode;
    type Error = Fault;

    fn cancelled(&self) -> Fault {
        Fault("cancelled")
    }

    fn observe_store(
        &mut self,
        writer: &RuntimeWriter<'_>,
        _id: NonZeroU32,
    ) -> Result<(Record, MapSet), Fault> {
        self.observe("observe-store")?;

        Ok((
            Record(self.receipt(writer, "record")),
            MapSet(self.receipt(writer, "map-set")),
        ))
    }

    fn observe_artifacts(
        &mut self,
        writer: &RuntimeWriter<'_>,
        _id: NonZeroU32,
    ) -> Result<ArtifactsFor<Self>, Fault> {
        self.observe("observe-artifacts")?;

        Ok(Artifacts {
            pin: self
                .state
                .contains("pin")
                .then(|| Pin(self.receipt(writer, "pin"))),
            maps: ["map-a", "map-b", "map-c"]
                .into_iter()
                .filter(|n| self.state.contains(n))
                .collect::<Vec<_>>()
                .into_iter()
                .map(|n| Map(self.receipt(writer, n)))
                .collect(),
            directory: self
                .state
                .contains("directory")
                .then(|| Directory(self.receipt(writer, "directory"))),
            bytecode: self
                .state
                .contains("bytecode")
                .then(|| Bytecode(self.receipt(writer, "bytecode"))),
        })
    }

    fn observe_links(
        &mut self,
        writer: &RuntimeWriter<'_>,
        _id: NonZeroU32,
    ) -> Result<Vec<bpfman_core::UnloadLink<Receipt, Receipt>>, Fault> {
        self.observe("observe-links")?;
        let mut links = Vec::new();
        for (id, pin, record) in [
            (1, "link-a-pin", "link-a-record"),
            (2, "link-b-pin", "link-b-record"),
        ] {
            if self.state.contains(record) {
                links.push(bpfman_core::UnloadLink {
                    id: std::num::NonZeroU64::new(id).expect("id"),
                    pin: self.state.contains(pin).then(|| self.receipt(writer, pin)),
                    record: self.receipt(writer, record),
                });
            }
        }
        Ok(links)
    }

    fn unpin_link(
        &mut self,
        writer: &RuntimeWriter<'_>,
        receipt: Receipt,
    ) -> Result<(), EffectFailure<Receipt, Fault>> {
        self.step(writer, receipt)
    }

    fn delete_link(
        &mut self,
        writer: &RuntimeWriter<'_>,
        receipt: Receipt,
    ) -> Result<(), EffectFailure<Receipt, Fault>> {
        self.step(writer, receipt)
    }

    fn unpin(
        &mut self,
        writer: &RuntimeWriter<'_>,
        receipt: Pin,
    ) -> Result<(), EffectFailure<Pin, Fault>> {
        self.step(writer, receipt.0).map_err(|f| EffectFailure {
            remaining: Pin(f.remaining),
            cause: f.cause,
        })
    }

    fn delete_record(
        &mut self,
        writer: &RuntimeWriter<'_>,
        receipt: Record,
    ) -> Result<(), EffectFailure<Record, Fault>> {
        self.step(writer, receipt.0).map_err(|f| EffectFailure {
            remaining: Record(f.remaining),
            cause: f.cause,
        })
    }

    fn remove_map(
        &mut self,
        writer: &RuntimeWriter<'_>,
        receipt: Map,
    ) -> Result<(), EffectFailure<Map, Fault>> {
        self.step(writer, receipt.0).map_err(|f| EffectFailure {
            remaining: Map(f.remaining),
            cause: f.cause,
        })
    }

    fn remove_directory(
        &mut self,
        writer: &RuntimeWriter<'_>,
        receipt: Directory,
    ) -> Result<(), EffectFailure<Directory, Fault>> {
        self.step(writer, receipt.0).map_err(|f| EffectFailure {
            remaining: Directory(f.remaining),
            cause: f.cause,
        })
    }

    fn delete_map_set(
        &mut self,
        writer: &RuntimeWriter<'_>,
        receipt: MapSet,
    ) -> Result<(), EffectFailure<MapSet, Fault>> {
        self.step(writer, receipt.0).map_err(|f| EffectFailure {
            remaining: MapSet(f.remaining),
            cause: f.cause,
        })
    }

    fn remove_bytecode(
        &mut self,
        writer: &RuntimeWriter<'_>,
        receipt: Bytecode,
    ) -> Result<(), EffectFailure<Bytecode, Fault>> {
        self.step(writer, receipt.0).map_err(|f| EffectFailure {
            remaining: Bytecode(f.remaining),
            cause: f.cause,
        })
    }
}

fn expected(faults: &BTreeSet<&'static str>) -> Vec<&'static str> {
    let mut calls = vec!["pin"];

    if faults.contains("pin") {
        return calls;
    }

    calls.push("record");

    if !faults.contains("record") {
        calls.extend(["map-a", "map-b", "map-c"]);

        if !["map-a", "map-b", "map-c"]
            .iter()
            .any(|n| faults.contains(n))
        {
            calls.push("directory");

            if !faults.contains("directory") {
                calls.push("map-set");
            }
        }
    }

    calls.push("bytecode");
    calls
}

fn kind(name: &str) -> UnloadKind {
    match name {
        "pin" => UnloadKind::ProgramPin,
        "record" => UnloadKind::ProgramRecord,
        "map-a" | "map-b" | "map-c" => UnloadKind::MapPin,
        "directory" => UnloadKind::MapDirectory,
        "map-set" => UnloadKind::MapSet,
        "bytecode" => UnloadKind::Bytecode,
        _ => unreachable!(),
    }
}

fn scope(test: impl FnOnce(&RuntimeWriter<'_>)) {
    let temporary = tempfile::tempdir().expect("temp");
    let runtime = RuntimeDirectory::open_or_create(
        RuntimeLayout::try_from(temporary.path().to_owned()).expect("layout"),
    )
    .expect("runtime");
    runtime
        .with_writer(
            AcquireOptions {
                timeout: Duration::from_secs(1),
                cancelled: None,
            },
            |writer| test(&writer),
        )
        .expect("writer");
}

#[test]
fn every_failure_subset_preserves_order_residue_status_and_successful_history() {
    scope(|writer| {
        for mask in 0..256 {
            let faults = WORK
                .into_iter()
                .enumerate()
                .filter(|(i, _)| mask & (1 << i) != 0)
                .map(|(_, n)| n)
                .collect::<BTreeSet<_>>();
            let mut fake = Fake::new(faults.clone());
            let report = run(
                writer,
                &mut fake,
                NonZeroU32::MIN,
                &crate::Cancellation::new(),
            )
            .expect("preflight");
            let attempted = expected(&faults);

            assert_eq!(
                fake.calls,
                [
                    vec!["observe-store", "observe-links", "observe-artifacts"],
                    attempted.clone()
                ]
                .concat(),
                "mask {mask}"
            );

            let residual: BTreeSet<_> = WORK
                .into_iter()
                .filter(|n| !attempted.contains(n) || faults.contains(n))
                .collect();

            assert_eq!(fake.residue(), residual);
            assert_eq!(report.remaining().len(), residual.len());
            assert_eq!(
                report.failed(),
                faults.contains("pin") || faults.contains("record")
            );
            assert_eq!(report.attempts().len(), attempted.len());

            for (attempt, name) in report.attempts().iter().zip(&attempted) {
                assert_eq!(attempt.id, WORK.iter().position(|n| n == name).expect("id"));
                assert_eq!(attempt.kind, kind(name));
                assert_eq!(attempt.outcome.is_err(), faults.contains(name));

                if let Err(Fault(cause)) = &attempt.outcome {
                    assert_eq!(cause, name);
                }
            }

            // An unchanged fault is not healed by invoking another pass.
            let before = fake.residue();
            let count = report.attempts().len();
            let report = drain(writer, &mut fake, report.retry());

            assert_eq!(fake.residue(), before);
            assert!(
                report.attempts()[count..]
                    .iter()
                    .all(|a| a.outcome.is_err())
            );

            let prior_count = report.attempts().len();
            let prior_ids: Vec<_> = report.remaining().iter().map(|p| p.id()).collect();

            // The test deliberately changes the external condition before retry.
            fake.faults.clear();
            fake.calls.clear();
            let report = drain(writer, &mut fake, report.retry());

            assert_eq!(
                fake.calls,
                WORK.into_iter()
                    .filter(|n| residual.contains(n))
                    .collect::<Vec<_>>()
            );
            assert!(fake.residue().is_empty());
            assert!(report.remaining().is_empty());
            assert!(!report.failed());
            assert_eq!(
                report.attempts()[prior_count..]
                    .iter()
                    .map(|a| a.id)
                    .collect::<Vec<_>>(),
                prior_ids
            );
            assert!(
                report.attempts()[prior_count..]
                    .iter()
                    .all(|a| a.outcome.is_ok())
            );
            fake.unrelated_preserved();
            drop(report);
            assert_eq!(fake.drops.get(), fake.minted);
        }
    });
}

#[test]
fn preflight_failures_never_mutate_committed_resources() {
    scope(|writer| {
        for failure in ["observe-store", "observe-links", "observe-artifacts"] {
            let mut fake = Fake::new([failure].into_iter().collect());
            let before = fake.state.clone();
            let result = run(
                writer,
                &mut fake,
                NonZeroU32::MIN,
                &crate::Cancellation::new(),
            );

            assert!(matches!(result, Err(Fault(name)) if name == failure));
            assert_eq!(fake.state, before);
            assert!(fake.calls.iter().all(|name| name.starts_with("observe-")));
            assert_eq!(fake.drops.get(), fake.minted);
        }
    });
}

#[test]
fn absent_artifacts_require_only_store_teardown() {
    scope(|writer| {
        let mut fake = Fake::new(BTreeSet::new());

        for name in ["pin", "map-a", "map-b", "map-c", "directory", "bytecode"] {
            fake.state.remove(name);
        }

        let report = run(
            writer,
            &mut fake,
            NonZeroU32::MIN,
            &crate::Cancellation::new(),
        )
        .expect("preflight");

        assert_eq!(
            fake.calls,
            [
                "observe-store",
                "observe-links",
                "observe-artifacts",
                "record",
                "map-set"
            ]
        );
        assert!(report.remaining().is_empty());
        assert!(!report.failed());
        fake.unrelated_preserved();
    });
}

#[test]
fn wrong_runtime_cannot_consume_retained_receipts() {
    scope(|writer| {
        let mut fake = Fake::new(["pin"].into_iter().collect());
        let report = run(
            writer,
            &mut fake,
            NonZeroU32::MIN,
            &crate::Cancellation::new(),
        )
        .expect("preflight");
        let original = fake.state.clone();
        fake.faults.clear();
        fake.calls.clear();
        let report = scope_return(|other| drain(other, &mut fake, report.retry()));

        assert_eq!(fake.calls, ["pin"]);
        assert_eq!(fake.state, original);

        let report = drain(writer, &mut fake, report.retry());

        assert!(report.remaining().is_empty());
    });
}

fn scope_return<T>(test: impl FnOnce(&RuntimeWriter<'_>) -> T) -> T {
    let temporary = tempfile::tempdir().expect("temp");
    let runtime = RuntimeDirectory::open_or_create(
        RuntimeLayout::try_from(temporary.path().to_owned()).expect("layout"),
    )
    .expect("runtime");
    runtime
        .with_writer(
            AcquireOptions {
                timeout: Duration::from_secs(1),
                cancelled: None,
            },
            |writer| test(&writer),
        )
        .expect("writer")
}

#[test]
fn unload_trace_distinguishes_failures_from_unattempted_dependencies() {
    scope(|writer| {
        for faults in [["record", "bytecode"], ["map-b", "bytecode"]] {
            let mut fake = Fake::new(faults.into_iter().collect());
            let report = run(
                writer,
                &mut fake,
                NonZeroU32::MIN,
                &crate::Cancellation::new(),
            )
            .expect("preflight");
            println!(
                "unload with injected {faults:?}: failed={}",
                report.failed()
            );

            for attempt in report.attempts() {
                println!(
                    "  {} {:?}: {}",
                    attempt.id,
                    attempt.kind,
                    if attempt.outcome.is_ok() {
                        "succeeded"
                    } else {
                        "failed"
                    }
                );
            }

            for pending in report.remaining() {
                if !report.attempts().iter().any(|a| a.id == pending.id()) {
                    println!(
                        "  {} {:?}: not attempted (prerequisite failed)",
                        pending.id(),
                        pending.instruction().kind()
                    );
                }
            }

            assert!(
                report
                    .attempts()
                    .iter()
                    .any(|a| a.kind == UnloadKind::Bytecode)
            );
            assert_eq!(report.remaining().len(), fake.residue().len());
        }
    });
}

#[test]
fn cancellation_before_teardown_preserves_all_committed_resources() {
    scope(|writer| {
        for boundary in [
            None,
            Some("observe-store"),
            Some("observe-links"),
            Some("observe-artifacts"),
        ] {
            let mut fake = Fake::new(BTreeSet::new());
            fake.cancel_on = boundary;
            let cancellation = fake.cancellation.clone();
            if boundary.is_none() {
                cancellation.cancel();
            }

            let result = run(writer, &mut fake, NonZeroU32::MIN, &cancellation);

            assert_eq!(result.err().expect("cancelled").0, "cancelled");
            assert_eq!(fake.residue(), WORK.into_iter().collect());
            assert!(!fake.calls.iter().any(|name| WORK.contains(name)));
            assert_eq!(fake.minted, fake.drops.get());
            fake.unrelated_preserved();
        }
    });
}

#[test]
fn cancellation_during_teardown_preserves_full_pass_and_failure_history() {
    scope(|writer| {
        for boundary in WORK {
            for faults in [BTreeSet::new(), BTreeSet::from(["map-b", "bytecode"])] {
                let mut fake = Fake::new(faults.clone());
                fake.cancel_on = Some(boundary);
                let cancellation = fake.cancellation.clone();
                let report = run(writer, &mut fake, NonZeroU32::MIN, &cancellation)
                    .expect("admitted teardown");

                let expected = expected(&faults);
                assert_eq!(&fake.calls[3..], expected);
                assert_eq!(report.attempts().len(), expected.len());
                assert_eq!(report.remaining().len(), fake.residue().len());
                fake.faults.clear();
                let previous = report.attempts().len();
                let report = drain(writer, &mut fake, report.retry());
                assert!(report.remaining().is_empty());
                assert!(report.attempts().len() >= previous);
                assert!(fake.residue().is_empty());
                fake.unrelated_preserved();
            }
        }
    });
}

const LINKS: [&str; 4] = ["link-a-pin", "link-a-record", "link-b-pin", "link-b-record"];

#[test]
fn link_failures_block_dependents_and_explicit_retry_keeps_successes() {
    scope(|writer| {
        for (index, failure) in LINKS.iter().enumerate() {
            let mut fake = Fake::new([*failure].into());
            fake.state.extend(LINKS);
            let report = run(
                writer,
                &mut fake,
                NonZeroU32::MIN,
                &crate::Cancellation::new(),
            )
            .expect("preflight");

            assert!(report.failed());
            assert_eq!(&fake.calls[3..], &LINKS[..=index]);
            assert_eq!(report.attempts().len(), index + 1);
            assert_eq!(report.remaining().len(), WORK.len() + LINKS.len() - index);
            assert_eq!(fake.residue(), WORK.into_iter().collect());
            assert!(report.attempts()[..index].iter().all(|a| a.outcome.is_ok()));
            assert!(report.attempts()[index].outcome.is_err());

            fake.calls.clear();
            let report = drain(writer, &mut fake, report.retry());
            assert_eq!(fake.calls, [*failure]);
            assert_eq!(report.attempts().len(), index + 2);
            assert_eq!(report.attempts()[index + 1].id, index);

            fake.faults.clear();
            fake.calls.clear();
            let report = drain(writer, &mut fake, report.retry());
            assert_eq!(fake.calls, [&LINKS[index..], &WORK].concat());
            assert!(!report.failed());
            assert!(report.remaining().is_empty());
            assert_eq!(
                report
                    .attempts()
                    .iter()
                    .filter(|a| a.outcome.is_err())
                    .count(),
                2
            );
            assert!(fake.residue().is_empty());
            assert!(LINKS.iter().all(|n| !fake.state.contains(n)));
            fake.unrelated_preserved();
            drop(report);
            assert_eq!(fake.drops.get(), fake.minted);
        }
    });
}

#[test]
fn cancellation_at_each_link_effect_finishes_the_admitted_pass() {
    scope(|writer| {
        for boundary in LINKS {
            let mut fake = Fake::new(BTreeSet::new());
            fake.state.extend(LINKS);
            fake.cancel_on = Some(boundary);
            let cancellation = fake.cancellation.clone();
            let report = run(writer, &mut fake, NonZeroU32::MIN, &cancellation).expect("admitted");

            assert!(cancellation.is_cancelled());
            assert_eq!(&fake.calls[3..], [&LINKS[..], &WORK].concat());
            assert!(report.remaining().is_empty());
            assert!(report.attempts().iter().all(|a| a.outcome.is_ok()));
            assert!(fake.residue().is_empty());
            fake.unrelated_preserved();
        }
    });
}
