use std::{
    collections::{HashMap, HashSet},
    fmt,
};

use bpfman_shell_source::Span;

use crate::{LoopBindings, LoopId, LoopTrace, loops};

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

/// A lowered expression with source coordinates.
pub type Expression = Located<ExpressionKind>;

/// The expression vocabulary of the initial IR slice.
#[derive(Debug, Eq, PartialEq)]
pub enum ExpressionKind {
    /// An unquoted word, classified by the evaluator.
    Word(String),
    /// A quoted string.
    Quoted(String),
    /// A variable lookup with optional path.
    Variable {
        /// Variable name.
        name: String,
        /// Remaining field and index path.
        path: String,
    },
    /// A list evaluated left to right.
    List(Vec<Expression>),
    /// Apply an arithmetic or comparison operator.
    Binary {
        /// Left operand.
        left: Box<Expression>,
        /// Operator.
        operator: BinaryOperator,
        /// Right operand.
        right: Box<Expression>,
    },
}

/// Arithmetic and comparison operators understood by the IR.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BinaryOperator {
    /// Addition.
    Add,
    /// Subtraction.
    Subtract,
    /// Multiplication.
    Multiply,
    /// Division.
    Divide,
    /// Remainder.
    Remainder,
    /// Equality.
    Equal,
    /// Inequality.
    NotEqual,
    /// Less than.
    Less,
    /// Less than or equal.
    LessEqual,
    /// Greater than.
    Greater,
    /// Greater than or equal.
    GreaterEqual,
}

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

/// An operation with fallthrough to the next instruction.
#[derive(Debug, Eq, PartialEq)]
pub enum Instruction {
    /// Open the program's defer scope.
    EnterProgramDefers,
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
    /// Unwind through the iteration frame and leave the loop.
    ExitLoop(LoopId),
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
        }
    }
}

impl std::error::Error for VerifyError {}

#[derive(Clone, Copy, Eq, PartialEq)]
enum TempKind {
    Value,
    Args,
}

impl Program {
    /// Verify block references and temporary definitions and uses. In this
    /// slice, temporary operands must be defined earlier in their own block.
    ///
    /// # Errors
    ///
    /// Returns a verification error for an invalid entry, successor or operand.
    pub fn new(span: Span, blocks: Vec<Block>) -> Result<Self, VerifyError> {
        if blocks.is_empty() {
            return Err(VerifyError::MissingEntry);
        }
        let mut destinations = HashSet::new();
        for block in &blocks {
            let mut kinds = HashMap::new();
            for instruction in &block.instructions {
                match &instruction.node {
                    Instruction::Eval { dst, .. } | Instruction::BuildArgs { dst, .. } => {
                        if !destinations.insert(*dst) {
                            return Err(VerifyError::InvalidDestination(*dst));
                        }
                        kinds.insert(
                            *dst,
                            match instruction.node {
                                Instruction::Eval { .. } => TempKind::Value,
                                _ => TempKind::Args,
                            },
                        );
                    }
                    Instruction::BindName { src, .. } | Instruction::EmitResult { src } => {
                        verify_operand(*src, TempKind::Value, &kinds)?;
                    }
                    Instruction::DispatchCommand { argv } | Instruction::RegisterDefer { argv } => {
                        verify_operand(*argv, TempKind::Args, &kinds)?;
                    }
                    Instruction::EnterProgramDefers
                    | Instruction::RunProgramDefers
                    | Instruction::EnterIfFrame
                    | Instruction::ExitFrame
                    | Instruction::TraceBranch(_)
                    | Instruction::TraceLoop(_) => {}
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
                Terminator::Stop => {}
                Terminator::ForEach {
                    list, body, exit, ..
                } => {
                    verify_operand(list, TempKind::Value, &kinds)?;
                    verify_successor(body)?;
                    verify_successor(exit)?;
                }
                Terminator::ForEachContinue(owner) | Terminator::ExitLoop(owner) => {
                    loops::targets(&blocks, owner)?;
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

    /// Source extent of the lowered program.
    #[must_use]
    pub fn span(&self) -> &Span {
        &self.span
    }

    /// Entry block ID.
    #[must_use]
    pub fn entry(&self) -> BlockId {
        BlockId(0)
    }

    /// Immutable blocks in deterministic emission order.
    #[must_use]
    pub fn blocks(&self) -> &[Block] {
        &self.blocks
    }

    /// Number of temporary slots in the program.
    #[must_use]
    pub fn num_temps(&self) -> usize {
        self.num_temps
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
    use bpfman_shell_source::{Position, SourceName, Span};

    use super::{
        Block, BlockId, ExpressionKind, Instruction, Located, Program, Temp, Terminator,
        VerifyError,
    };

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
}
