//! Verified block-structured IR for the shell's lowering and execution phases.

mod dump;
mod model;

pub use model::{
    Block, BlockId, Command, Expression, ExpressionKind, Instruction, Located, Program, Temp,
    Terminator, VerifyError,
};
