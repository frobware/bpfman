use bpfman_shell_ir::Expression;
use bpfman_shell_source::Span;

use super::{Argument, CommandRequest, CommandResponse, Machine, Step, Value};

pub(super) type EvalResult<T> = Result<T, EvalError>;

pub(super) enum EvalError {
    Message(String),
    Path {
        key: *const Expression,
        request: PathRequest,
    },
    Query {
        key: *const Expression,
        request: Box<QueryRequest>,
        mode: QueryMode,
    },
    Thread {
        key: *const Expression,
        request: Box<CommandRequest>,
    },
}

impl From<String> for EvalError {
    fn from(message: String) -> Self {
        Self::Message(message)
    }
}

impl From<&str> for EvalError {
    fn from(message: &str) -> Self {
        Self::Message(message.to_owned())
    }
}

/// A filesystem existence query.
pub struct PathRequest {
    /// Path as written or evaluated by the script.
    pub path: String,
    /// Extent of the query.
    pub span: Span,
}

/// An evaluator suspended at a filesystem query.
pub struct PathContinuation<'a> {
    pub(super) machine: Machine<'a>,
    pub(super) key: *const Expression,
    pub(super) span: Span,
}

impl<'a> PathContinuation<'a> {
    /// Continue with an observation supplied by the driver.
    pub fn resume(mut self, observation: Result<bool, String>) -> Step<'a> {
        match observation {
            Ok(exists) => {
                self.machine
                    .expression_observations
                    .insert(self.key, Value::Bool(exists));
            }
            Err(error) => {
                self.machine.expression_observations.clear();
                self.machine.error(&self.span, error);
            }
        }
        self.machine.advance()
    }
}

impl Machine<'_> {
    pub(super) fn path_exists(
        &self,
        expression: &Expression,
        arguments: &[Expression],
    ) -> EvalResult<Value> {
        // The expression's address identifies this occurrence in the immutable
        // program. It is never dereferenced. Keep observations only while the
        // containing instruction is evaluating, including across suspensions.
        let key = std::ptr::from_ref(expression);
        if let Some(exists) = self.expression_observations.get(&key) {
            return Ok(exists.clone());
        }
        let [argument] = arguments else {
            return Err("path-exists requires one argument".into());
        };
        let path = match self.argument(argument)? {
            Argument::Word(path) | Argument::Quoted(path) => path,
            Argument::Value(value) | Argument::Adapter { value, .. } => {
                value.scalar_text().ok_or("path-exists requires a scalar")?
            }
        };
        Err(EvalError::Path {
            key,
            request: PathRequest {
                path,
                span: expression.span.clone(),
            },
        })
    }
}

/// Input distinguished by its origin at the JSON query boundary.
pub enum QueryInput {
    /// Literal argv text that must be decoded as a single JSON value.
    JsonText(String),
    /// Already evaluated and typed shell value.
    Value(Value),
}

impl From<Argument> for QueryInput {
    fn from(argument: Argument) -> Self {
        match argument {
            Argument::Word(text) | Argument::Quoted(text) => Self::JsonText(text),
            Argument::Value(value) | Argument::Adapter { value, .. } => Self::Value(value),
        }
    }
}

/// A jq query request.
pub struct QueryRequest {
    /// Filter source.
    pub filter: String,
    /// Input and its origin.
    pub input: QueryInput,
    /// Source extent of the query.
    pub span: Span,
}

/// An evaluator suspended at a jq query.
pub struct QueryContinuation<'a> {
    pub(super) machine: Machine<'a>,
    pub(super) key: *const Expression,
    pub(super) span: Span,
    pub(super) mode: QueryMode,
}

#[derive(Clone, Copy)]
pub(super) enum QueryMode {
    Expression,
    Thread,
}

impl<'a> QueryContinuation<'a> {
    /// Continue with the values emitted by the query engine, in order.
    pub fn resume(mut self, observation: Result<Vec<Value>, String>) -> Step<'a> {
        match observation {
            Ok(mut results) => {
                let value = match results.len() {
                    0 if matches!(self.mode, QueryMode::Thread) => {
                        Value::Outcome(Box::new(CommandResponse::success(Value::Absent)))
                    }
                    0 => Value::Absent,
                    1 => results.pop().unwrap_or(Value::Absent),
                    _ => Value::List(results),
                };
                self.machine.expression_observations.insert(self.key, value);
            }
            Err(error) => {
                self.machine.expression_observations.clear();
                self.machine.error(&self.span, error);
            }
        }
        self.machine.advance()
    }
}

impl Machine<'_> {
    pub(super) fn query(
        &self,
        expression: &Expression,
        filter: Argument,
        input: Argument,
        mode: QueryMode,
    ) -> EvalResult<Value> {
        let key = std::ptr::from_ref(expression);
        if let Some(value) = self.expression_observations.get(&key) {
            return Ok(value.clone());
        }
        let filter = match filter {
            Argument::Word(text) | Argument::Quoted(text) => text,
            Argument::Value(value) | Argument::Adapter { value, .. } => {
                value.scalar_text().ok_or("jq requires a scalar filter")?
            }
        };
        Err(EvalError::Query {
            key,
            mode,
            request: Box::new(QueryRequest {
                filter,
                input: input.into(),
                span: expression.span.clone(),
            }),
        })
    }
}
