//! Faults exercise the production forward interpreter, not a second unload plan.
#![allow(clippy::expect_used)]

use super::*;
use bpfman_core::UnloadKind;
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
    type Pin = Pin;
    type Record = Record;
    type Map = Map;
    type Directory = Directory;
    type MapSet = MapSet;
    type Bytecode = Bytecode;
    type Error = Fault;

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
            let report = run(writer, &mut fake, NonZeroU32::MIN).expect("preflight");
            let attempted = expected(&faults);

            assert_eq!(
                fake.calls,
                [
                    vec!["observe-store", "observe-artifacts"],
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
        for failure in ["observe-store", "observe-artifacts"] {
            let mut fake = Fake::new([failure].into_iter().collect());
            let before = fake.state.clone();
            let result = run(writer, &mut fake, NonZeroU32::MIN);

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

        let report = run(writer, &mut fake, NonZeroU32::MIN).expect("preflight");

        assert_eq!(
            fake.calls,
            ["observe-store", "observe-artifacts", "record", "map-set"]
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
        let report = run(writer, &mut fake, NonZeroU32::MIN).expect("preflight");
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
            let report = run(writer, &mut fake, NonZeroU32::MIN).expect("preflight");
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
