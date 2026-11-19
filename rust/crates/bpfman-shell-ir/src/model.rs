use std::fmt;

use bpfman_shell_source::Span;

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
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
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
    /// A jump leaves the program's block table.
    InvalidSuccessor(BlockId),
    /// A destination is out of emission order or already allocated.
    InvalidDestination(Temp),
    /// A use has no prior definition of the required kind in its block.
    InvalidOperand(Temp),
}

impl fmt::Display for VerifyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingEntry => f.write_str("missing entry block"),
            Self::InvalidSuccessor(BlockId(id)) => write!(f, "invalid successor bb{id}"),
            Self::InvalidDestination(Temp(id)) => write!(f, "invalid destination t{id}"),
            Self::InvalidOperand(Temp(id)) => write!(f, "invalid operand t{id}"),
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
        let mut num_temps = 0;
        for block in &blocks {
            if let Terminator::Jump(id) = block.terminator.node {
                if id.0 >= blocks.len() {
                    return Err(VerifyError::InvalidSuccessor(id));
                }
            }
            let base = num_temps;
            let mut kinds = Vec::new();
            for instruction in &block.instructions {
                match &instruction.node {
                    Instruction::Eval { dst, .. } | Instruction::BuildArgs { dst, .. } => {
                        if dst.0 != num_temps {
                            return Err(VerifyError::InvalidDestination(*dst));
                        }
                        kinds.push(match instruction.node {
                            Instruction::Eval { .. } => TempKind::Value,
                            _ => TempKind::Args,
                        });
                        num_temps += 1;
                    }
                    Instruction::BindName { src, .. } | Instruction::EmitResult { src } => {
                        verify_operand(*src, TempKind::Value, base, &kinds)?;
                    }
                    Instruction::DispatchCommand { argv } | Instruction::RegisterDefer { argv } => {
                        verify_operand(*argv, TempKind::Args, base, &kinds)?;
                    }
                    Instruction::EnterProgramDefers | Instruction::RunProgramDefers => {}
                }
            }
        }
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
    base: usize,
    kinds: &[TempKind],
) -> Result<(), VerifyError> {
    if temp.0.checked_sub(base).and_then(|index| kinds.get(index)) == Some(&kind) {
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
}
