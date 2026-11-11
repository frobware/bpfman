//! Argv-first syntax for bpfman-shell.
//!
//! The caller supplies a source name and text; the crate manages tokenisation,
//! parsing, and source spans. The parser covers core expressions, simple
//! statements, `if`, `foreach`, and definitions; remaining forms report incomplete coverage.
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
mod expr;
mod parse;
mod token;

pub use ast::{
    BinaryOperator, BindMode, Command, Definition, Expression, ExpressionKind, Identifier,
    IfBranch, InterpolatedPart, LogicalOperator, LoopBindings, Parameter, ParameterType,
    ParsedProgram, PureBuiltin, Statement, StatementKind, ThreadCall,
};
pub use parse::{SyntaxError, SyntaxErrorKind, parse};

pub use token::{
    Interpolation, LexError, Segment, Token, TokenKind, VariableRef, tokenise, tokenise_at,
};
