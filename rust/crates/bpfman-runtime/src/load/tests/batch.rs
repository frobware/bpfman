//! Multiple independent fake kernels exercise the production batch interpreter.
use super::*;

struct Batch {
    members: Vec<Fake>,
    next: usize,
    current: usize,
    commits: usize,
}

impl Batch {
    fn new(writer: &RuntimeWriter<'_>) -> Self {
        let members = (42..45)
            .map(|id| {
                let mut fake = Fake::new(writer, []);
                fake.program_id = NonZeroU32::new(id).expect("id");
                fake
            })
            .collect();
        Self {
            members,
            next: 0,
            current: 0,
            commits: 0,
        }
    }

    fn owner(&mut self, receipt: &Receipt) -> &mut Fake {
        self.members
            .iter_mut()
            .find(|f| Rc::ptr_eq(&f.owner, &receipt.owner))
            .expect("owned receipt")
    }

    fn assert_clean(&self) {
        for member in &self.members {
            member.assert_clean();
        }
    }
}

impl LoadCleanup for Batch {
    type ProgramPin = Program;
    type MapPin = Map;
    type Bytecode = BytecodeReceipt;
    type Error = TestError;

    fn remove_bytecode(
        &mut self,
        w: &RuntimeWriter<'_>,
        r: BytecodeReceipt,
    ) -> Result<(), EffectFailure<BytecodeReceipt, TestError>> {
        self.owner(&r.0).remove_bytecode(w, r)
    }

    fn remove_program_pin(
        &mut self,
        w: &RuntimeWriter<'_>,
        r: Program,
    ) -> Result<(), EffectFailure<Program, TestError>> {
        self.owner(&r.0).remove_program_pin(w, r)
    }

    fn remove_map_pin(
        &mut self,
        w: &RuntimeWriter<'_>,
        r: Map,
    ) -> Result<(), EffectFailure<Map, TestError>> {
        self.owner(&r.0).remove_map_pin(w, r)
    }
}

impl CleanupEffects for Batch {
    type MapDirectory = Directory;

    fn remove_map_directory(
        &mut self,
        w: &RuntimeWriter<'_>,
        r: Directory,
    ) -> Result<(), EffectFailure<Directory, TestError>> {
        self.owner(&r.0).remove_map_directory(w, r)
    }
}

impl LoadEffects for Batch {
    type Store = ();
    type Prepared = ();
    type Kernel = Kernel;

    fn cancelled(&self) -> TestError {
        self.members[self.current].cancelled()
    }

    fn batch_aborted(&self) -> TestError {
        self.members[self.current].batch_aborted()
    }

    fn open_store(&mut self, w: &RuntimeWriter<'_>) -> Result<(), TestError> {
        self.members[0].open_store(w)
    }

    fn prepare(&mut self, w: &RuntimeWriter<'_>) -> Result<(), TestError> {
        self.members[0].prepare(w)
    }

    fn load_kernel(&mut self, w: &RuntimeWriter<'_>, i: &Inputs<'_>) -> Result<Kernel, TestError> {
        self.current = self.next;
        self.next += 1;
        self.members[self.current].load_kernel(w, i)
    }

    fn pin_program(
        &mut self,
        w: &RuntimeWriter<'_>,
        p: &(),
        k: &mut Kernel,
        n: &Symbol,
    ) -> Result<Program, EffectFailure<Option<Program>, TestError>> {
        self.members[self.current].pin_program(w, p, k, n)
    }

    fn program_id(p: &Program) -> NonZeroU32 {
        Fake::program_id(p)
    }

    fn map_names(kernel: &Kernel) -> &[String] {
        Fake::map_names(kernel)
    }

    fn create_map_directory(
        &mut self,
        w: &RuntimeWriter<'_>,
        p: &(),
        id: NonZeroU32,
    ) -> Result<Directory, EffectFailure<Option<Directory>, TestError>> {
        self.members[self.current].create_map_directory(w, p, id)
    }

    fn pin_map(
        &mut self,
        w: &RuntimeWriter<'_>,
        k: &Kernel,
        d: &Directory,
        n: &str,
    ) -> Result<Map, EffectFailure<Option<Map>, TestError>> {
        self.members[self.current].pin_map(w, k, d, n)
    }

    fn publish(
        &mut self,
        w: &RuntimeWriter<'_>,
        id: NonZeroU32,
        i: &Inputs<'_>,
    ) -> Result<BytecodeReceipt, EffectFailure<Vec<BytecodeReceipt>, TestError>> {
        self.members[self.current].publish(w, id, i)
    }

    fn persist(
        &mut self,
        w: &RuntimeWriter<'_>,
        records: &[(NonZeroU32, &Inputs<'_>)],
    ) -> Result<(), TestError> {
        self.commits += 1;
        assert_eq!(records.len(), self.members.len());
        // Validate all rows before publishing any, like a backend transaction.
        for (member, (id, _)) in self.members.iter_mut().zip(records) {
            assert_eq!(*id, member.program_id);
            assert!(member.resources.contains(&Resource::Bytecode));
            member.enter(w, Event::Persist)?;
            member.check(Fault::BeforeCommit)?;
        }
        for member in &mut self.members {
            member.records.insert(member.program_id.get());
            member.map_sets.insert(member.program_id.get());
        }
        Ok(())
    }
}

fn invoke_batch(
    w: &RuntimeWriter<'_>,
    effects: &mut Batch,
) -> Result<(StoredProgramSummary, Vec<StoredProgramSummary>), FailureFor<Batch>> {
    let cancellation = effects.members[0].cancellation.clone();
    for member in &mut effects.members {
        member.cancellation = cancellation.clone();
    }
    let object = LocalObject {
        globals: BTreeMap::new(),
        bytes: b"ELF".to_vec(),
        license: "GPL".into(),
        maps: vec!["1".into(), "2".into(), "3".into()],
    };
    let name = Symbol::try_from("trace").expect("symbol");
    let metadata = BTreeMap::new();
    let spec = bpfman_model::ProgramSpec::Tracepoint(name.clone());
    let inputs: Vec<_> = (0..3)
        .map(|_| Inputs {
            cancellation: &cancellation,
            object: &object,
            source: "source.o",
            spec: &spec,
            metadata: &metadata,
            created_at: "2026-10-05T00:00:00Z",
        })
        .collect();
    run_batch(w, effects, &inputs[0], &inputs[1..])
}

#[test]
fn later_member_failures_cross_earlier_cleanup_failures_and_retry() {
    for case in cases()
        .into_iter()
        .filter(|c| !matches!(c.last, Event::OpenStore | Event::Prepare))
    {
        for cleanup in CLEANUP {
            with_writer(|writer| {
                let mut batch = Batch::new(writer);
                batch.members[1].faults.insert(case.fault);
                batch.members[0].faults.insert(Fault::Before(*cleanup));
                let failure = invoke_batch(writer, &mut batch).expect_err("injected failure");

                assert_eq!(primary(&failure).fault, case.fault);
                assert!(
                    !unresolved(&failure).is_empty(),
                    "earlier member cleanup failure retained"
                );
                assert_eq!(batch.members[0].records, BTreeSet::from([7]));
                assert_eq!(batch.members[1].records, BTreeSet::from([7]));
                if case.last != Event::Persist {
                    assert_eq!(batch.commits, 0);
                    assert!(batch.members[2].events.is_empty(), "no later acquisition");
                }
                // Independent cleanup still runs after a previous cleanup error.
                assert!(batch.members[0].events.contains(&Event::RemoveProgram));
                assert!(batch.members[0].events.contains(&Event::RemoveBytecode));
                for member in &mut batch.members {
                    member.faults.clear();
                }
                let commits = batch.commits;
                let failure = retry(writer, &mut batch, failure);

                assert!(unresolved(&failure).is_empty());
                assert_eq!(primary(&failure).fault, case.fault);
                assert_eq!(batch.commits, commits);
                batch.assert_clean();
                drop(failure);
                for member in &batch.members {
                    assert_dropped_once(&member.counts);
                }
            });
        }
    }
}

#[test]
fn cancellation_in_later_member_compensates_all_preceding_members() {
    for event in [
        Event::LoadKernel,
        Event::PinProgram,
        Event::CreateDirectory,
        Event::PinMap(2),
        Event::Publish,
    ] {
        with_writer(|writer| {
            let mut batch = Batch::new(writer);
            batch.members[1].cancel_on = Some(event);
            let failure = invoke_batch(writer, &mut batch).expect_err("cancelled");

            assert_eq!(primary(&failure).fault, Fault::Cancelled);
            assert!(unresolved(&failure).is_empty());
            assert_eq!(batch.commits, 0);
            batch.assert_clean();
        });
    }
}

#[test]
fn one_batch_commit_preserves_order_and_ends_cancellation_authority() {
    with_writer(|writer| {
        let mut batch = Batch::new(writer);
        batch.members[2].cancel_on = Some(Event::Persist);
        let (first, rest) = invoke_batch(writer, &mut batch).ok().expect("commit");

        assert_eq!(
            std::iter::once(first)
                .chain(rest)
                .map(|p| p.id().get())
                .collect::<Vec<_>>(),
            [42, 43, 44]
        );
        assert_eq!(batch.commits, 1);
        assert!(batch.members[0].cancellation.is_cancelled());
        for member in &batch.members {
            assert!(member.records.contains(&member.program_id.get()));
            assert!(!member.events.iter().any(|e| CLEANUP.contains(e)));
            assert_eq!(member.counts.handles.get(), 0);
        }
    });
}
