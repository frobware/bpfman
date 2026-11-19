use crate::CommandResponse;
use std::{
    collections::BTreeMap,
    num::{NonZeroU32, NonZeroU64},
};

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

/// A background process capability and its immutable script-visible fields.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct JobHandle {
    id: NonZeroU64,
    fields: BTreeMap<String, Value>,
}

impl JobHandle {
    /// Associate a driver-issued job identity with the current process ID.
    #[must_use]
    pub fn new(id: NonZeroU64, pid: NonZeroU32, target_binary: String) -> Self {
        Self {
            id,
            fields: BTreeMap::from([
                ("pid".to_owned(), Value::Number(Number(pid.to_string()))),
                ("target_binary".to_owned(), Value::String(target_binary)),
            ]),
        }
    }

    /// Identify the job without using its potentially reusable process ID.
    #[must_use]
    pub fn id(&self) -> NonZeroU64 {
        self.id
    }
}

/// A leased kernel function fixture and its immutable script-visible fields.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct KfuncHandle {
    id: NonZeroU64,
    fields: BTreeMap<String, Value>,
}

impl KfuncHandle {
    /// Associate a driver lease with the module's target fields.
    #[must_use]
    pub fn new(id: NonZeroU64, index: u8, name: String, trigger: String, count: String) -> Self {
        Self {
            id,
            fields: BTreeMap::from([
                ("index".to_owned(), Value::Number(Number(index.to_string()))),
                ("name".to_owned(), Value::String(name)),
                ("trigger".to_owned(), Value::String(trigger)),
                ("count".to_owned(), Value::String(count)),
            ]),
        }
    }

    /// Identify this lease independently of the reused slot index.
    #[must_use]
    pub fn id(&self) -> NonZeroU64 {
        self.id
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
    /// A bpfman CLI record with a resolved ID for subsequent commands.
    BpfmanRecord {
        /// Fields from the CLI's JSON projection.
        fields: BTreeMap<String, Value>,
        /// Program or link ID accepted by the CLI.
        id: Number,
    },
    /// A capability for a background process owned by the driver.
    Job(JobHandle),
    /// A capability for a leased kernel function fixture.
    Kfunc(KfuncHandle),
    /// Captured command outcome, with a distinct primary when supplied.
    Outcome(Box<CommandResponse>),
}

impl Value {
    /// Named fields of a record or a typed value with a record projection.
    #[must_use]
    pub fn record_fields(&self) -> Option<&BTreeMap<String, Self>> {
        match self {
            Self::Record(fields) | Self::BpfmanRecord { fields, .. } => Some(fields),
            Self::Job(job) => Some(&job.fields),
            Self::Kfunc(function) => Some(&function.fields),
            _ => None,
        }
    }

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
    /// Driver adapter with its already resolved value.
    Adapter {
        /// Adapter name.
        name: String,
        /// Value that the adapter will render or consume.
        value: Value,
    },
}
