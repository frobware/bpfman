//! Argv-first syntax for bpfman-shell.
//!
//! The caller supplies a source name and text; the crate manages tokenisation,
//! parsing, and source spans. The parser covers simple statements and core
//! expressions; remaining language forms report incomplete coverage.
//!
//! ```
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let program = bpfman_shell_syntax::parse("main.bpfman", "print hello")?;
//! assert_eq!(program.statements().len(), 1);
//! # Ok(())
//! # }
//! ```

mod ast;
mod expr;
mod parse;
mod token;

pub use ast::{
    BinaryOperator, BindMode, Command, Expression, ExpressionKind, Identifier, InterpolatedPart,
    LogicalOperator, ParsedProgram, PureBuiltin, Statement, StatementKind, ThreadCall,
};
pub use parse::{SyntaxError, SyntaxErrorKind, parse};

pub use token::{
    Interpolation, LexError, Segment, Token, TokenKind, VariableRef, tokenise, tokenise_at,
};
