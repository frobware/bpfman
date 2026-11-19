//! Verified block-structured IR for the shell's lowering and execution phases.

mod dump;
mod expr;
mod loops;
mod model;

pub use expr::{
    BinaryOperator, Expression, ExpressionKind, InterpolatedPart, LogicalOperator, MatchBlock,
    MatchEntry, MatchPattern, MatchPredicate, PureBuiltin, RecordField, UnaryOperator, VariableRef,
};
pub use loops::{LoopBindings, LoopId, LoopTrace};
pub use model::{
    BindPolicy, Block, BlockId, BranchTrace, Command, Instruction, Located, Program, Temp,
    Terminator, VerifyError,
};
