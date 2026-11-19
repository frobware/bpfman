//! Verified block-structured IR for the shell's lowering and execution phases.

mod dump;
mod loops;
mod model;

pub use loops::{LoopBindings, LoopId, LoopTrace};
pub use model::{
    BinaryOperator, Block, BlockId, BranchTrace, Command, Expression, ExpressionKind, Instruction,
    Located, Program, Temp, Terminator, VerifyError,
};
