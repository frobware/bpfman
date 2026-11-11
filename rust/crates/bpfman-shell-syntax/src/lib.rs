//! Argv-first syntax for bpfman-shell.
//!
//! Lexing preserves CLI-shaped words and byte locations. Later modules own
//! expression parsing and AST construction.

mod token;

pub use token::{
    Interpolation, LexError, Segment, Token, TokenKind, VariableRef, tokenise, tokenise_at,
};
