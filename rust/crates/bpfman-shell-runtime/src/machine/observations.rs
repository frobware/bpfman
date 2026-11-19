use bpfman_shell_ir::Expression;
use bpfman_shell_source::Span;

use super::{Argument, Machine, Step, Value};

pub(super) type EvalResult<T> = Result<T, EvalError>;

pub(super) enum EvalError {
    Message(String),
    Path {
        key: *const Expression,
        request: PathRequest,
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
                self.machine.observed_paths.insert(self.key, exists);
            }
            Err(error) => {
                self.machine.observed_paths.clear();
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
        if let Some(exists) = self.observed_paths.get(&key) {
            return Ok(Value::Bool(*exists));
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
