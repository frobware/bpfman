use std::{
    collections::{HashMap, HashSet},
    fmt,
    num::NonZeroU64,
};

use bpfman_shell_source::Span;

use crate::{Expression, LoopBindings, LoopId, LoopTrace, loops};

/// A node together with its original source extent.
#[derive(Debug, Eq, PartialEq)]
pub struct Located<T> {
    /// Lowered node.
    pub node: T,
    /// Original source extent.
    pub span: Span,
}

/// A block index in its owning program, numbered in emission order.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BlockId(pub usize);

/// A temporary index in its owning program, numbered in emission order.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct Temp(pub usize);

/// Trace event emitted when control selects an if-chain path.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BranchTrace {
    /// The initial condition matched.
    Then,
    /// An elif condition matched.
    Elif,
    /// The else body was selected.
    Else,
    /// All conditions failed and there was no else.
    Skip,
}

/// A nonempty argv description.
#[derive(Debug, Eq, PartialEq)]
pub struct Command {
    /// Expression that produces the command head.
    pub head: Expression,
    /// Remaining arguments in order.
    pub arguments: Vec<Expression>,
}

/// How the shell exposes a command result at a bind site.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BindPolicy {
    /// Bind the entire command outcome.
    Capture,
    /// Bind the primary value on success, or transfer control to cleanup.
    Guard {
        /// Cleanup block entered with the guard failure.
        on_failure: BlockId,
    },
}

/// How a collecting loop publishes its accumulated results.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CollectMode {
    /// Bind the aggregate outcome.
    Capture,
    /// Bind successful primary values and fail on the first failed producer.
    Guard,
}

/// Where a retry is handled.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RetryTarget {
    /// Poll header in the current unit.
    Local(BlockId),
    /// Dynamically enclosing poll of a calling unit.
    Caller,
}

/// A positive timeout and retry interval in nanoseconds.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PollSchedule {
    /// Total time budget.
    pub timeout: NonZeroU64,
    /// Delay between attempts.
    pub every: NonZeroU64,
}

/// Trace events for time-based control flow.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PollTrace {
    /// Enter an attempt.
    Attempt,
    /// The deadline expired.
    Timeout,
}

/// Expected command status.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CommandStatus {
    /// The command should succeed.
    Ok,
    /// The command should fail.
    Fail,
}

/// What an assertion evaluates.
#[derive(Debug, Eq, PartialEq)]
pub enum Assertion {
    /// Evaluate the truth of an expression.
    Expression(Expression),
    /// Dispatch a command and inspect its status.
    Command {
        /// Expected success or failure.
        expected: CommandStatus,
        /// Location of the status keyword.
        status_span: Span,
        /// Command arguments in source order.
        command: Command,
        /// Whether to invert the expectation.
        negated: bool,
    },
}

/// An operation with fallthrough to the next instruction.
#[derive(Debug, Eq, PartialEq)]
pub enum Instruction {
    /// Open the program's defer scope.
    EnterProgramDefers,
    /// Enter the callable definition frame.
    EnterDefinitionFrame,
    /// Open a definition-local defer scope.
    EnterDefinitionDefers,
    /// Drain only the current definition’s deferred commands.
    RunDefinitionDefers,
    /// Enter a fresh poll attempt frame.
    EnterAttemptFrame,
    /// Open a poll attempt defer scope.
    EnterAttemptDefers,
    /// Drain the current scope according to attempt-fatal policy.
    RunAttemptDefers,
    /// Trace the attempt or timeout.
    TracePoll(PollTrace),
    /// Enter the lexical frame for a selected if branch.
    EnterIfFrame,
    /// Leave the current lexical frame.
    ExitFrame,
    /// Record a trace event omitted by the canonical dump.
    TraceBranch(BranchTrace),
    /// Record an explicit loop exit or continue.
    TraceLoop(LoopTrace),
    /// Evaluate a value.
    Eval {
        /// Destination value temporary.
        dst: Temp,
        /// Expression to evaluate.
        expression: Expression,
    },
    /// Associate a name with an evaluated value.
    BindName {
        /// Lexical name to bind.
        name: String,
        /// Source value temporary.
        src: Temp,
    },
    /// Bind positional list elements to named slots.
    BindDestructure {
        /// Ordered names, including discard slots.
        bindings: LoopBindings,
        /// Source list value.
        src: Temp,
    },
    /// Evaluate an assertion or requirement.
    Assert {
        /// Stop execution on failure.
        required: bool,
        /// Condition to evaluate.
        clause: Box<Assertion>,
    },
    /// Print the value of an expression statement.
    EmitResult {
        /// Source value temporary.
        src: Temp,
    },
    /// Evaluate argv before dispatch.
    BuildArgs {
        /// Destination argv temporary.
        dst: Temp,
        /// Head and remaining argument expressions.
        command: Command,
    },
    /// Dispatch a command with the definition-then-exec command policy.
    DispatchCommand {
        /// Source argv temporary.
        argv: Temp,
    },
    /// Dispatch in bind position using the definition-then-exec bind policy.
    DispatchBind {
        /// Destination command-result temporary.
        dst: Temp,
        /// Source argv temporary.
        argv: Temp,
        /// Source extent of the producer invocation.
        call_span: Span,
        /// Trace header rendered before dispatch.
        trace_header: String,
    },
    /// Consume a command result and apply the requested binding policy.
    ApplyBind {
        /// Command-result temporary.
        src: Temp,
        /// The argv that produced this result.
        argv: Temp,
        /// Binding name, or `_` for a discarded result.
        target: String,
        /// Capture or guarded binding.
        policy: BindPolicy,
    },
    /// Register an evaluated argv for deferred bind-policy dispatch.
    RegisterDefer {
        /// Source argv temporary.
        argv: Temp,
    },
    /// Drain the program's deferred cleanup scope.
    RunProgramDefers,
}

/// The only operation allowed at the end of a block.
#[derive(Debug, Eq, PartialEq)]
pub enum Terminator {
    /// Transfer control within the current program.
    Jump(BlockId),
    /// Select a successor using an evaluated condition.
    Branch {
        /// Condition value temporary.
        condition: Temp,
        /// Successor when the condition is true.
        on_true: BlockId,
        /// Successor when the condition is false.
        on_false: BlockId,
    },
    /// Initialize an iterator and select the first body or exit successor.
    ForEach {
        /// Evaluated list temporary.
        list: Temp,
        /// Slots bound in each iteration frame.
        bindings: LoopBindings,
        /// Body entered with a fresh iteration frame.
        body: BlockId,
        /// Successor when the iterator has no remaining elements.
        exit: BlockId,
    },
    /// Unwind through the current iteration frame and advance its iterator.
    ForEachContinue(LoopId),
    /// Initialize a foreach that accumulates producer results.
    ForEachCollect {
        /// Evaluated list.
        list: Temp,
        /// Iteration binding slots.
        bindings: LoopBindings,
        /// Name that receives the aggregate.
        target: String,
        /// Aggregation and failure policy.
        mode: CollectMode,
        /// Iteration body.
        body: BlockId,
        /// Continuation after the aggregate is published.
        exit: BlockId,
    },
    /// Submit a command outcome, unwind the iteration, and advance.
    CollectProduce {
        /// Collecting iterator owner.
        owner: LoopId,
        /// Command-result temporary.
        result: Temp,
        /// Full bind span used when framing a failed guard.
        bind_span: Span,
    },
    /// Unwind through the iteration frame and leave the loop.
    ExitLoop(LoopId),
    /// Report the active error after cleanup and unwind the program.
    PropagateError,
    /// Publish the value and unwind to the definition epilogue.
    ReturnValue {
        /// Result temporary.
        src: Temp,
        /// Definition cleanup successor.
        epilogue: BlockId,
    },
    /// Finish the callable definition and publish its bind result.
    FinishDefinition,
    /// A block retained for dump compatibility with no executable predecessors.
    Unreachable,
    /// Read the clock, establish a deadline, and begin an attempt.
    BeginPoll {
        /// Timeout and interval.
        schedule: PollSchedule,
        /// First block of each attempt.
        attempt: BlockId,
        /// Failure continuation on deadline expiry.
        timeout: BlockId,
        /// Continuation when an attempt completes.
        success: BlockId,
    },
    /// Retry after deferred cleanup, subject to the active deadline.
    RetryPoll {
        /// Poll that receives the retry signal.
        target: RetryTarget,
        /// Optional message value kept for timeout diagnostics.
        message: Option<Temp>,
    },
    /// Finish the program.
    Stop,
}

/// A sequence with exactly one terminator.
#[derive(Debug, Eq, PartialEq)]
pub struct Block {
    /// Source extent of the block.
    pub span: Span,
    /// Fallthrough instructions in execution order.
    pub instructions: Vec<Located<Instruction>>,
    /// Final operation.
    pub terminator: Located<Terminator>,
}

/// An immutable verified program body.
#[derive(Debug, Eq, PartialEq)]
pub struct Program {
    body: Unit,
    definitions: Vec<Definition>,
}

#[derive(Debug, Eq, PartialEq)]
struct Unit {
    span: Span,
    blocks: Vec<Block>,
    num_temps: usize,
}

/// An invalid block or temporary reference during IR construction.
#[derive(Debug, Eq, PartialEq)]
pub enum VerifyError {
    /// The program has no entry block.
    MissingEntry,
    /// A successor leaves the program's block table.
    InvalidSuccessor(BlockId),
    /// A destination is outside the dense temporary range or already allocated.
    InvalidDestination(Temp),
    /// A use has no prior definition of the required kind in its block.
    InvalidOperand(Temp),
    /// A loop action refers to a block without a foreach terminator.
    InvalidLoopOwner(LoopId),
    /// A reachable block cannot run with the incoming iterator stack.
    InvalidLoopContext(BlockId),
    /// A guarded bind's failure target is not a program cleanup block.
    InvalidGuardCleanup(BlockId),
    /// A callable and a program use incompatible return terminators.
    InvalidReturn(BlockId),
    /// A poll control target or retry location is invalid.
    InvalidPoll(BlockId),
    /// A synthetic dead block has a reachable predecessor.
    ReachableDeadEnd(BlockId),
    /// The definition table contains the same name twice.
    DuplicateDefinition(String),
}

impl fmt::Display for VerifyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingEntry => f.write_str("missing entry block"),
            Self::InvalidSuccessor(BlockId(id)) => write!(f, "invalid successor bb{id}"),
            Self::InvalidDestination(Temp(id)) => write!(f, "invalid destination t{id}"),
            Self::InvalidOperand(Temp(id)) => write!(f, "invalid operand t{id}"),
            Self::InvalidLoopOwner(LoopId(BlockId(id))) => write!(f, "invalid loop owner bb{id}"),
            Self::InvalidLoopContext(BlockId(id)) => write!(f, "invalid loop context at bb{id}"),
            Self::InvalidGuardCleanup(BlockId(id)) => write!(f, "invalid guard cleanup at bb{id}"),
            Self::InvalidReturn(BlockId(id)) => write!(f, "invalid return at bb{id}"),
            Self::InvalidPoll(BlockId(id)) => write!(f, "invalid poll at bb{id}"),
            Self::ReachableDeadEnd(BlockId(id)) => write!(f, "reachable dead block bb{id}"),
            Self::DuplicateDefinition(name) => write!(f, "duplicate definition {name:?}"),
        }
    }
}

impl std::error::Error for VerifyError {}

#[derive(Clone, Copy, Eq, PartialEq)]
enum TempKind {
    Value,
    Args,
    BindResult(Temp),
}

impl Program {
    /// Verify a program without definitions.
    ///
    /// # Errors
    ///
    /// Returns a verification error if the body is invalid.
    pub fn new(span: Span, blocks: Vec<Block>) -> Result<Self, VerifyError> {
        Self::with_definitions(span, blocks, Vec::new())
    }

    /// Verify a program body and attach its hoisted definitions in source order.
    ///
    /// # Errors
    ///
    /// Returns a verification error if the body or definition table is invalid.
    pub fn with_definitions(
        span: Span,
        blocks: Vec<Block>,
        definitions: Vec<Definition>,
    ) -> Result<Self, VerifyError> {
        let mut names = HashSet::new();
        for definition in &definitions {
            if !names.insert(definition.name()) {
                return Err(VerifyError::DuplicateDefinition(
                    definition.name().to_owned(),
                ));
            }
        }
        Ok(Self {
            body: Unit::new(span, blocks, None)?,
            definitions,
        })
    }

    /// Hoisted callable definitions in source order.
    #[must_use]
    pub fn definitions(&self) -> &[Definition] {
        &self.definitions
    }

    /// Source extent of the lowered program.
    #[must_use]
    pub fn span(&self) -> &Span {
        &self.body.span
    }

    /// Entry block ID.
    #[must_use]
    pub fn entry(&self) -> BlockId {
        BlockId(0)
    }

    /// Immutable blocks in deterministic emission order.
    #[must_use]
    pub fn blocks(&self) -> &[Block] {
        &self.body.blocks
    }

    /// Number of temporary slots in the program.
    #[must_use]
    pub fn num_temps(&self) -> usize {
        self.body.num_temps
    }
}

/// Type annotation checked when a definition is invoked.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ParameterType {
    /// JSON number.
    Number,
    /// Text string.
    String,
    /// Boolean.
    Bool,
}

/// One definition parameter.
#[derive(Debug, Eq, PartialEq)]
pub struct Parameter {
    /// Lexical name.
    pub name: String,
    /// Optional call-time type requirement.
    pub annotation: Option<ParameterType>,
}

/// A verified callable with its own block and temporary namespace.
#[derive(Debug, Eq, PartialEq)]
pub struct Definition {
    name: String,
    parameters: Vec<Parameter>,
    unit: Unit,
}

impl Definition {
    /// Verify a callable body. The leading temporary slots are its parameters.
    ///
    /// # Errors
    ///
    /// Returns a structural verification error for an invalid definition body.
    pub fn new(
        name: String,
        parameters: Vec<Parameter>,
        span: Span,
        blocks: Vec<Block>,
    ) -> Result<Self, VerifyError> {
        let unit = Unit::new(span, blocks, Some(parameters.len()))?;
        Ok(Self {
            name,
            parameters,
            unit,
        })
    }

    /// Invocation name.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Parameters in declaration order.
    #[must_use]
    pub fn parameters(&self) -> &[Parameter] {
        &self.parameters
    }

    /// Definition source extent.
    #[must_use]
    pub fn span(&self) -> &Span {
        &self.unit.span
    }

    /// Blocks in deterministic emission order, starting at the entry.
    #[must_use]
    pub fn blocks(&self) -> &[Block] {
        &self.unit.blocks
    }

    /// Number of temporary slots including the parameters.
    #[must_use]
    pub fn num_temps(&self) -> usize {
        self.unit.num_temps
    }
}

impl Unit {
    fn new(span: Span, blocks: Vec<Block>, parameters: Option<usize>) -> Result<Self, VerifyError> {
        if blocks.is_empty() {
            return Err(VerifyError::MissingEntry);
        }
        let mut destinations: HashSet<_> = (0..parameters.unwrap_or(0)).map(Temp).collect();
        for (index, block) in blocks.iter().enumerate() {
            let mut kinds = HashMap::new();
            if index == 0 {
                kinds
                    .extend((0..parameters.unwrap_or(0)).map(|slot| (Temp(slot), TempKind::Value)));
            }
            for instruction in &block.instructions {
                let definition = match &instruction.node {
                    Instruction::Eval { dst, .. } => Some((*dst, TempKind::Value)),
                    Instruction::BuildArgs { dst, .. } => Some((*dst, TempKind::Args)),
                    Instruction::DispatchBind { dst, argv, .. } => {
                        verify_operand(*argv, TempKind::Args, &kinds)?;
                        Some((*dst, TempKind::BindResult(*argv)))
                    }
                    Instruction::BindName { src, .. }
                    | Instruction::BindDestructure { src, .. }
                    | Instruction::EmitResult { src } => {
                        verify_operand(*src, TempKind::Value, &kinds)?;
                        None
                    }
                    Instruction::DispatchCommand { argv } | Instruction::RegisterDefer { argv } => {
                        verify_operand(*argv, TempKind::Args, &kinds)?;
                        None
                    }
                    Instruction::ApplyBind {
                        src, argv, policy, ..
                    } => {
                        verify_operand(*src, TempKind::BindResult(*argv), &kinds)?;
                        if let BindPolicy::Guard { on_failure } = policy {
                            verify_guard_cleanup(&blocks, *on_failure, parameters.is_some())?;
                        }
                        None
                    }
                    Instruction::EnterProgramDefers
                    | Instruction::EnterDefinitionFrame
                    | Instruction::EnterDefinitionDefers
                    | Instruction::RunDefinitionDefers
                    | Instruction::EnterAttemptFrame
                    | Instruction::EnterAttemptDefers
                    | Instruction::RunAttemptDefers
                    | Instruction::TracePoll(_)
                    | Instruction::RunProgramDefers
                    | Instruction::EnterIfFrame
                    | Instruction::ExitFrame
                    | Instruction::TraceBranch(_)
                    | Instruction::TraceLoop(_)
                    | Instruction::Assert { .. } => None,
                };
                if let Some((dst, kind)) = definition {
                    if !destinations.insert(dst) {
                        return Err(VerifyError::InvalidDestination(dst));
                    }
                    kinds.insert(dst, kind);
                }
            }
            let verify_successor = |id: BlockId| {
                if id.0 < blocks.len() {
                    Ok(())
                } else {
                    Err(VerifyError::InvalidSuccessor(id))
                }
            };
            match block.terminator.node {
                Terminator::Jump(id) => verify_successor(id)?,
                Terminator::Branch {
                    condition,
                    on_true,
                    on_false,
                } => {
                    verify_operand(condition, TempKind::Value, &kinds)?;
                    verify_successor(on_true)?;
                    verify_successor(on_false)?;
                }
                Terminator::ReturnValue { src, epilogue } => {
                    verify_operand(src, TempKind::Value, &kinds)?;
                    verify_successor(epilogue)?;
                    if parameters.is_none()
                        || !matches!(
                            blocks[epilogue.0].terminator.node,
                            Terminator::FinishDefinition
                        )
                    {
                        return Err(VerifyError::InvalidReturn(BlockId(index)));
                    }
                }
                Terminator::FinishDefinition
                    if parameters.is_none()
                        || !matches!(
                            block.instructions.as_slice(),
                            [
                                Located {
                                    node: Instruction::RunDefinitionDefers,
                                    ..
                                },
                                Located {
                                    node: Instruction::ExitFrame,
                                    ..
                                },
                            ]
                        ) =>
                {
                    return Err(VerifyError::InvalidReturn(BlockId(index)));
                }
                Terminator::Stop if parameters.is_some() => {
                    return Err(VerifyError::InvalidReturn(BlockId(index)));
                }
                Terminator::Stop
                | Terminator::PropagateError
                | Terminator::FinishDefinition
                | Terminator::Unreachable => {}
                Terminator::BeginPoll {
                    attempt,
                    timeout,
                    success,
                    ..
                } => {
                    verify_successor(attempt)?;
                    verify_successor(timeout)?;
                    verify_successor(success)?;
                    if !matches!(
                        blocks[timeout.0].terminator.node,
                        Terminator::PropagateError
                    ) {
                        return Err(VerifyError::InvalidPoll(timeout));
                    }
                }
                Terminator::RetryPoll { target, message } => {
                    if let Some(message) = message {
                        verify_operand(message, TempKind::Value, &kinds)?;
                    }
                    match target {
                        RetryTarget::Local(owner) => {
                            verify_successor(owner)?;
                            if !matches!(
                                blocks[owner.0].terminator.node,
                                Terminator::BeginPoll { .. }
                            ) {
                                return Err(VerifyError::InvalidPoll(owner));
                            }
                        }
                        RetryTarget::Caller if parameters.is_none() => {
                            return Err(VerifyError::InvalidPoll(BlockId(index)));
                        }
                        RetryTarget::Caller => {}
                    }
                }
                Terminator::ForEach {
                    list, body, exit, ..
                }
                | Terminator::ForEachCollect {
                    list, body, exit, ..
                } => {
                    verify_operand(list, TempKind::Value, &kinds)?;
                    verify_successor(body)?;
                    verify_successor(exit)?;
                }
                Terminator::ForEachContinue(owner) | Terminator::ExitLoop(owner) => {
                    loops::targets(&blocks, owner)?;
                }
                Terminator::CollectProduce { owner, result, .. } => {
                    if !matches!(kinds.get(&result), Some(TempKind::BindResult(_))) {
                        return Err(VerifyError::InvalidOperand(result));
                    }
                    if !matches!(
                        blocks.get(owner.0.0).map(|block| &block.terminator.node),
                        Some(Terminator::ForEachCollect { .. })
                    ) {
                        return Err(VerifyError::InvalidLoopOwner(owner));
                    }
                }
            }
        }
        let num_temps = destinations.len();
        if let Some(temp) = destinations
            .into_iter()
            .filter(|temp| temp.0 >= num_temps)
            .min_by_key(|temp| temp.0)
        {
            return Err(VerifyError::InvalidDestination(temp));
        }
        loops::verify_context(&blocks)?;
        Ok(Self {
            span,
            blocks,
            num_temps,
        })
    }
}

fn verify_guard_cleanup(
    blocks: &[Block],
    target: BlockId,
    definition: bool,
) -> Result<(), VerifyError> {
    let block = blocks
        .get(target.0)
        .ok_or(VerifyError::InvalidSuccessor(target))?;
    let valid = match block.instructions.as_slice() {
        [
            Located {
                node: Instruction::RunProgramDefers,
                ..
            },
        ] => !definition,
        [
            Located {
                node: Instruction::RunDefinitionDefers,
                ..
            },
            Located {
                node: Instruction::ExitFrame,
                ..
            },
        ] => definition,
        [
            Located {
                node: Instruction::RunAttemptDefers,
                ..
            },
            Located {
                node: Instruction::ExitFrame,
                ..
            },
        ] => true,
        _ => false,
    };
    if matches!(block.terminator.node, Terminator::PropagateError) && valid {
        Ok(())
    } else {
        Err(VerifyError::InvalidGuardCleanup(target))
    }
}

fn verify_operand(
    temp: Temp,
    kind: TempKind,
    kinds: &HashMap<Temp, TempKind>,
) -> Result<(), VerifyError> {
    if kinds.get(&temp) == Some(&kind) {
        Ok(())
    } else {
        Err(VerifyError::InvalidOperand(temp))
    }
}

#[cfg(test)]
mod tests {
    use crate::ExpressionKind;
    use bpfman_shell_source::{Position, SourceName, Span};

    use super::{Block, BlockId, Instruction, Located, Program, Temp, Terminator, VerifyError};

    fn located<T>(node: T) -> Located<T> {
        let pos = Position::new(SourceName::new("ir.bpfman"), 1, 1)
            .unwrap_or_else(|| unreachable!("valid position"));
        Located {
            node,
            span: Span::point(pos),
        }
    }

    fn block(instructions: Vec<Located<Instruction>>, terminator: Terminator) -> Block {
        let terminator = located(terminator);
        Block {
            span: terminator.span.clone(),
            instructions,
            terminator,
        }
    }

    #[test]
    fn entry_and_successors_belong_to_the_program() {
        let span = located(()).span;
        assert_eq!(
            Program::new(span.clone(), vec![]),
            Err(VerifyError::MissingEntry)
        );
        let jump = block(vec![], Terminator::Jump(BlockId(1)));
        assert_eq!(
            Program::new(span.clone(), vec![jump]),
            Err(VerifyError::InvalidSuccessor(BlockId(1)))
        );
        let jump = block(vec![], Terminator::Jump(BlockId(1)));
        let stop = block(vec![], Terminator::Stop);
        let program =
            Program::new(span, vec![jump, stop]).unwrap_or_else(|error| unreachable!("{error}"));
        assert_eq!(program.entry(), BlockId(0));
        assert!(program.to_string().contains("  Jump bb1\n\nbb1:\n  Stop\n"));
    }

    #[test]
    fn operands_have_the_required_definition_and_kind() {
        let span = located(()).span;
        let eval = |dst| {
            located(Instruction::Eval {
                dst: Temp(dst),
                expression: located(ExpressionKind::Word("1".to_owned())),
            })
        };
        let dispatch = located(Instruction::DispatchCommand { argv: Temp(0) });
        let wrong_kind = block(vec![eval(0), dispatch], Terminator::Stop);
        assert_eq!(
            Program::new(span.clone(), vec![wrong_kind]),
            Err(VerifyError::InvalidOperand(Temp(0)))
        );
        let unknown = located(Instruction::EmitResult { src: Temp(0) });
        let forward = block(vec![unknown, eval(0)], Terminator::Stop);
        assert_eq!(
            Program::new(span.clone(), vec![forward]),
            Err(VerifyError::InvalidOperand(Temp(0)))
        );
        let duplicate = block(vec![eval(0), eval(0)], Terminator::Stop);
        assert_eq!(
            Program::new(span, vec![duplicate]),
            Err(VerifyError::InvalidDestination(Temp(0)))
        );
    }

    #[test]
    fn branch_checks_both_successors_and_its_condition() {
        let span = located(()).span;
        let condition = located(Instruction::Eval {
            dst: Temp(0),
            expression: located(ExpressionKind::Word("true".to_owned())),
        });
        let branch = |condition, on_false| Terminator::Branch {
            condition: Temp(condition),
            on_true: BlockId(0),
            on_false: BlockId(on_false),
        };
        assert_eq!(
            Program::new(span.clone(), vec![block(vec![condition], branch(0, 1))]),
            Err(VerifyError::InvalidSuccessor(BlockId(1)))
        );
        assert_eq!(
            Program::new(span, vec![block(vec![], branch(0, 0))]),
            Err(VerifyError::InvalidOperand(Temp(0)))
        );
    }

    #[test]
    fn foreach_controls_require_a_real_and_active_owner() {
        use crate::{LoopBindings, LoopId};

        let span = located(()).span;
        let owner = LoopId(BlockId(1));
        let entry = block(vec![], Terminator::ExitLoop(owner));
        assert_eq!(
            Program::new(span.clone(), vec![entry]),
            Err(VerifyError::InvalidLoopOwner(owner))
        );
        let entry = block(vec![], Terminator::Jump(BlockId(2)));
        let bindings = LoopBindings::new(vec!["item".to_owned()])
            .unwrap_or_else(|| unreachable!("valid bindings"));
        let header = block(
            vec![located(Instruction::Eval {
                dst: Temp(0),
                expression: located(ExpressionKind::List(vec![])),
            })],
            Terminator::ForEach {
                list: Temp(0),
                bindings,
                body: BlockId(2),
                exit: BlockId(3),
            },
        );
        let body = block(vec![], Terminator::ForEachContinue(owner));
        let stop = block(vec![], Terminator::Stop);
        assert_eq!(
            Program::new(span, vec![entry, header, body, stop]),
            Err(VerifyError::InvalidLoopContext(BlockId(2)))
        );
    }

    #[test]
    fn bind_results_keep_their_argv_and_guards_require_failure_cleanup() {
        use crate::{BindPolicy, Command};

        let span = located(()).span;
        let entry = |argv, policy| {
            let build = |dst, word: &str| {
                located(Instruction::BuildArgs {
                    dst: Temp(dst),
                    command: Command {
                        head: located(ExpressionKind::Word(word.to_owned())),
                        arguments: vec![],
                    },
                })
            };
            block(
                vec![
                    build(0, "false"),
                    build(1, "true"),
                    located(Instruction::DispatchBind {
                        dst: Temp(2),
                        argv: Temp(0),
                        call_span: span.clone(),
                        trace_header: "guard item".to_owned(),
                    }),
                    located(Instruction::ApplyBind {
                        src: Temp(2),
                        argv: Temp(argv),
                        target: "item".to_owned(),
                        policy,
                    }),
                ],
                Terminator::Stop,
            )
        };
        assert_eq!(
            Program::new(span.clone(), vec![entry(1, BindPolicy::Capture)]),
            Err(VerifyError::InvalidOperand(Temp(2)))
        );
        let policy = BindPolicy::Guard {
            on_failure: BlockId(1),
        };
        assert_eq!(
            Program::new(span.clone(), vec![entry(0, policy)]),
            Err(VerifyError::InvalidSuccessor(BlockId(1)))
        );
        assert_eq!(
            Program::new(
                span.clone(),
                vec![entry(0, policy), block(vec![], Terminator::PropagateError)]
            ),
            Err(VerifyError::InvalidGuardCleanup(BlockId(1)))
        );
        let cleanup = block(
            vec![located(Instruction::RunProgramDefers)],
            Terminator::PropagateError,
        );
        assert!(Program::new(span.clone(), vec![entry(0, policy), cleanup]).is_ok());
    }
}
