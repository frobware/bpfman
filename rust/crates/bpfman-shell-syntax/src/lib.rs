//! Argv-first syntax for bpfman-shell.
//!
//! The caller supplies a source name and text; the crate manages tokenisation,
//! parsing, and source spans. The parser currently covers only simple command
//! and binding forms and reports other constructs as incomplete.
//!
//! ```
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let program = bpfman_shell_syntax::parse("main.bpfman", "print hello")?;
//! assert_eq!(program.statements().len(), 1);
//! # Ok(())
//! # }
//! ```

mod ast;
mod parse;
mod token;

pub use ast::{
    BindMode, Command, CommandHead, Expression, ExpressionKind, Identifier, ParsedProgram,
    Statement, StatementKind,
};
pub use parse::{SyntaxError, SyntaxErrorKind, parse};

pub use token::{
    Interpolation, LexError, Segment, Token, TokenKind, VariableRef, tokenise, tokenise_at,
};
