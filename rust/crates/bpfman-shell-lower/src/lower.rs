use std::fmt;

use bpfman_shell_check::{CheckedProgram, ExpandedProgram};
use bpfman_shell_ir::{
    Block, Command, Expression, ExpressionKind, Instruction, Located, Program, Temp, Terminator,
    VerifyError,
};
use bpfman_shell_source::Span;
use bpfman_shell_syntax as syntax;

/// A failure to lower the input into the supported IR.
#[derive(Debug, Eq, PartialEq)]
pub enum LowerError {
    /// The input uses a form that has no lowering yet.
    Unsupported {
        /// Description of the unsupported form.
        form: &'static str,
        /// Extent of the source form.
        span: Span,
    },
    /// The generated IR failed structural verification.
    InvalidIr(VerifyError),
}

impl fmt::Display for LowerError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unsupported { form, span } => {
                let start = span.start();
                write!(
                    f,
                    "{}:{}:{}: lowering {form} is not yet supported",
                    start.name().as_str(),
                    start.line(),
                    start.column()
                )
            }
            Self::InvalidIr(error) => write!(f, "invalid lowered program: {error}"),
        }
    }
}

impl std::error::Error for LowerError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::InvalidIr(error) => Some(error),
            Self::Unsupported { .. } => None,
        }
    }
}

/// Lower a program admitted by preflight.
///
/// # Errors
///
/// Returns a source-located error for unsupported syntax or invalid IR.
pub fn lower(program: &CheckedProgram) -> Result<Program, LowerError> {
    lower_body(program.span(), program.statements())
}

/// Lower expanded syntax without static preflight, for the explicit
/// `--no-check` path. Structural IR verification still applies.
///
/// # Errors
///
/// Returns a source-located error for unsupported syntax or invalid IR.
pub fn lower_unchecked(program: &ExpandedProgram) -> Result<Program, LowerError> {
    lower_body(program.span(), program.statements())
}

fn lower_body(span: &Span, statements: &[syntax::Statement]) -> Result<Program, LowerError> {
    let mut body = Body {
        instructions: Vec::new(),
        next_temp: 0,
    };
    body.emit(span, Instruction::EnterProgramDefers);
    for statement in statements {
        body.statement(statement)?;
    }
    body.emit(span, Instruction::RunProgramDefers);
    let block = Block {
        span: span.clone(),
        instructions: body.instructions,
        terminator: located(span, Terminator::Stop),
    };
    Program::new(span.clone(), vec![block]).map_err(LowerError::InvalidIr)
}

struct Body {
    instructions: Vec<Located<Instruction>>,
    next_temp: usize,
}

impl Body {
    fn allocate(&mut self) -> Temp {
        let temp = Temp(self.next_temp);
        self.next_temp += 1;
        temp
    }

    fn emit(&mut self, span: &Span, instruction: Instruction) {
        self.instructions.push(located(span, instruction));
    }

    fn statement(&mut self, statement: &syntax::Statement) -> Result<(), LowerError> {
        use syntax::StatementKind as S;
        let span = statement.span();
        let instruction = match statement.kind() {
            S::Let { name, value } => Instruction::BindName {
                name: name.text().to_owned(),
                src: self.eval(span, value)?,
            },
            S::Expression(value) => Instruction::EmitResult {
                src: self.eval(span, value)?,
            },
            S::Command(command) => Instruction::DispatchCommand {
                argv: self.build_args(span, command)?,
            },
            S::Defer(command) => Instruction::RegisterDefer {
                argv: self.build_args(command.span(), command)?,
            },
            S::Definition(_) => return Err(unsupported(span, "definition")),
            S::Return(_) => return Err(unsupported(span, "return")),
            S::If { .. } => return Err(unsupported(span, "if")),
            S::Foreach { .. } => return Err(unsupported(span, "foreach")),
            S::Poll { .. } => return Err(unsupported(span, "poll")),
            S::Retry { .. } => return Err(unsupported(span, "retry")),
            S::Assertion { .. } => return Err(unsupported(span, "assertion")),
            S::LetDestructure { .. } => return Err(unsupported(span, "destructuring")),
            S::Bind { .. } => return Err(unsupported(span, "command bind")),
            S::BindCollect { .. } => return Err(unsupported(span, "collect")),
            S::Break => return Err(unsupported(span, "break")),
            S::Continue => return Err(unsupported(span, "continue")),
        };
        self.emit(span, instruction);
        Ok(())
    }

    fn eval(&mut self, span: &Span, value: &syntax::Expression) -> Result<Temp, LowerError> {
        let expression = lower_expression(value)?;
        let dst = self.allocate();
        self.emit(span, Instruction::Eval { dst, expression });
        Ok(dst)
    }

    fn build_args(&mut self, span: &Span, command: &syntax::Command) -> Result<Temp, LowerError> {
        let command = Command {
            head: lower_expression(command.head())?,
            arguments: lower_expressions(command.arguments())?,
        };
        let dst = self.allocate();
        self.emit(span, Instruction::BuildArgs { dst, command });
        Ok(dst)
    }
}

fn lower_expressions(values: &[syntax::Expression]) -> Result<Vec<Expression>, LowerError> {
    values.iter().map(lower_expression).collect()
}

fn lower_expression(value: &syntax::Expression) -> Result<Expression, LowerError> {
    use syntax::ExpressionKind as E;
    let node = match value.kind() {
        E::Word(word) => ExpressionKind::Word(word.clone()),
        E::Quoted(word) => ExpressionKind::Quoted(word.clone()),
        E::Variable(variable) => ExpressionKind::Variable {
            name: variable.name.clone(),
            path: variable.path.clone(),
        },
        E::List(items) => ExpressionKind::List(lower_expressions(items)?),
        _ => return Err(unsupported(value.span(), "expression")),
    };
    Ok(located(value.span(), node))
}

fn unsupported(span: &Span, form: &'static str) -> LowerError {
    LowerError::Unsupported {
        form,
        span: span.clone(),
    }
}

fn located<T>(span: &Span, node: T) -> Located<T> {
    Located {
        node,
        span: span.clone(),
    }
}

#[cfg(test)]
mod tests {
    use std::{collections::BTreeMap, path::Path};

    use bpfman_shell_check::{ExpandedProgram, check, expand_imports, prepare_imports};
    use bpfman_shell_ir::Instruction;
    use bpfman_shell_syntax::parse;

    use super::{LowerError, lower, lower_unchecked};

    fn expand(source: &str) -> ExpandedProgram {
        let parsed = parse("slice.bpfman", source).unwrap_or_else(|error| unreachable!("{error}"));
        let plan =
            prepare_imports(parsed, Path::new(".")).unwrap_or_else(|error| unreachable!("{error}"));
        expand_imports(plan, &BTreeMap::new()).unwrap_or_else(|error| unreachable!("{error}"))
    }

    #[test]
    fn straight_line_dump_matches_go_and_keeps_spans() {
        let program = check(expand(
            "let items = [1 2]\nprint \"ready\" $items\ndefer echo cleanup\n",
        ))
        .unwrap_or_else(|issues| unreachable!("{issues:?}"));
        let ir = lower(&program).unwrap_or_else(|error| unreachable!("{error}"));
        assert_eq!(
            ir.to_string(),
            concat!(
                "body entry=bb0\n\nbb0:\n",
                "  EnterDeferScope kind=program\n",
                "  Eval t0 = [1 2]\n",
                "  BindName items = t0\n",
                "  BuildArgs t1 = [print \"ready\" $items]\n",
                "  DispatchCommand argv=t1 policy=def-then-exec-command lane=builtin(print)\n",
                "  BuildArgs t2 = [echo cleanup]\n",
                "  RegisterDefer argv=t2 policy=def-then-exec-bind lane=exec\n",
                "  RunDefers policy=program\n",
                "  Stop\n",
            )
        );
        assert_eq!(ir.span(), program.span());
        assert_eq!(ir.num_temps(), 3);
        let eval = &ir.blocks()[0].instructions[1];
        assert_eq!(&eval.span, program.statements()[0].span());
        let Instruction::Eval { expression, .. } = &eval.node else {
            unreachable!("expected Eval");
        };
        assert_eq!(expression.span.start().column(), 13);
    }

    #[test]
    fn unchecked_entry_is_explicit_and_unsupported_forms_fail_with_spans() {
        let unknown = expand("print $missing");
        assert!(check(unknown.clone()).is_err());
        assert!(lower_unchecked(&unknown).is_ok());

        let program = check(expand("print ok\nif true { print later }"))
            .unwrap_or_else(|issues| unreachable!("{issues:?}"));
        assert!(matches!(
            lower(&program),
            Err(LowerError::Unsupported { form: "if", span }) if &span == program.statements()[1].span()
        ));
    }
}
