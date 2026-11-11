//! Argv-first syntax for bpfman-shell.
//!
//! The caller supplies a source name and text; the crate manages tokenisation,
//! parsing, and source spans. The parser covers core expressions, simple
//! statements, blocks, records, matches, bind-collect loops, definitions, and assertions.
//! Semantic validation belongs to the later checker phase.
//!
//! ```
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let program = bpfman_shell_syntax::parse("main.bpfman", "print hello")?;
//! assert_eq!(program.statements().len(), 1);
//! # Ok(())
//! # }
//! ```

mod ast;
mod compound;
mod duration;
mod expr;
mod parse;
mod token;

pub use ast::{
    AssertionClause, AssertionMode, BinaryOperator, BindMode, Command, CommandOutcome, Definition,
    Expression, ExpressionKind, Identifier, IfBranch, InterpolatedPart, LogicalOperator,
    LoopBindings, MatchEntry, MatchPattern, MatchPredicate, MatchesBlock, Parameter, ParameterType,
    ParsedProgram, PureBuiltin, RecordField, Statement, StatementKind, ThreadCall,
};
pub use duration::{PositiveDuration, parse_go_duration};
pub use parse::{SyntaxError, SyntaxErrorKind, parse};

pub use token::{
    Interpolation, LexError, Segment, Token, TokenKind, VariableRef, tokenise, tokenise_at,
};
