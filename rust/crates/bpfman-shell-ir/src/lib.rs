//! Verified block-structured IR for the shell's lowering and execution phases.

mod dump;
mod duration;
pub use duration::GoDuration;
mod expr;
mod loops;
mod model;

pub use expr::{
    BinaryOperator, Expression, ExpressionKind, InterpolatedPart, LogicalOperator, MatchBlock,
    MatchEntry, MatchPattern, MatchPredicate, PureBuiltin, RecordField, UnaryOperator, VariableRef,
};
pub use loops::{LoopBindings, LoopId, LoopTrace};
pub use model::{
    Assertion, BindPolicy, Block, BlockId, BranchTrace, CollectMode, Command, CommandStatus,
    Definition, Instruction, Located, Parameter, ParameterType, PollSchedule, PollTrace, Program,
    RetryTarget, Temp, Terminator, VerifyError,
};
