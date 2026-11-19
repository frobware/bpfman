use crate::CommandResponse;
use std::collections::BTreeMap;

use bpfman_shell_syntax::is_json_number;

/// A validated JSON number that retains its exact spelling and precision.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Number(String);

impl Number {
    /// Accept one finite JSON number, including arbitrarily large integers.
    #[must_use]
    pub fn new(text: String) -> Option<Self> {
        is_json_number(&text).then_some(Self(text))
    }

    /// The exact decimal spelling.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// An evaluated shell value.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Value {
    /// No value was produced.
    Absent,
    /// Explicit JSON null.
    Null,
    /// Boolean value.
    Bool(bool),
    /// Exact number.
    Number(Number),
    /// String value.
    String(String),
    /// Ordered list.
    List(Vec<Value>),
    /// Named fields.
    Record(BTreeMap<String, Value>),
    /// Captured command outcome, with a distinct primary when supplied.
    Outcome(Box<CommandResponse>),
}

impl Value {
    /// Render a scalar while preserving exact number spellings.
    #[must_use]
    pub fn scalar_text(&self) -> Option<String> {
        match self {
            Self::Null => Some("null".to_owned()),
            Self::Bool(value) => Some(value.to_string()),
            Self::Number(value) => Some(value.as_str().to_owned()),
            Self::String(value) => Some(value.clone()),
            _ => None,
        }
    }
}

/// One argv operand with its evaluation origin.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Argument {
    /// An unquoted argv word.
    Word(String),
    /// A quoted argv word.
    Quoted(String),
    /// An evaluated value.
    Value(Value),
}
