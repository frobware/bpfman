use std::fmt;

use crate::layout::{Exit, Label, Layout};
use bpfman_shell_check::{CheckedProgram, ExpandedProgram};
use bpfman_shell_ir::{
    BinaryOperator, BranchTrace, Command, Expression, ExpressionKind, Instruction, Located,
    LoopBindings, LoopTrace, Program, Temp, VerifyError,
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
    /// A reserved block was left unpublished or unterminated.
    IncompleteLayout(Span),
    /// A loop-control statement was encountered outside any foreach.
    OutsideLoop {
        /// Control keyword.
        keyword: &'static str,
        /// Source extent.
        span: Span,
    },
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
            Self::OutsideLoop { keyword, span } => write!(
                f,
                "{}:{}:{}: {keyword} outside any foreach loop",
                span.start().name().as_str(),
                span.start().line(),
                span.start().column(),
            ),
            Self::IncompleteLayout(span) => write!(
                f,
                "incomplete block layout at {}:{}",
                span.start().line(),
                span.start().column()
            ),
        }
    }
}

impl std::error::Error for LowerError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::InvalidIr(error) => Some(error),
            Self::Unsupported { .. } | Self::IncompleteLayout(_) | Self::OutsideLoop { .. } => None,
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
        layout: Layout::new(span),
        next_temp: 0,
        current_loop: None,
    };
    body.emit(span, Instruction::EnterProgramDefers);
    for statement in statements {
        body.statement(statement)?;
    }
    body.emit(span, Instruction::RunProgramDefers);
    body.layout.terminate(located(span, Exit::Stop));
    Program::new(span.clone(), body.layout.finish()?).map_err(LowerError::InvalidIr)
}

struct Body {
    layout: Layout,
    next_temp: usize,
    current_loop: Option<Label>,
}

impl Body {
    fn allocate(&mut self) -> Temp {
        let temp = Temp(self.next_temp);
        self.next_temp += 1;
        temp
    }

    fn emit(&mut self, span: &Span, instruction: Instruction) {
        self.layout.emit(located(span, instruction));
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
            S::If {
                first,
                elif,
                else_body,
            } => {
                return self.conditional(span, first, elif, else_body.as_deref());
            }
            S::Foreach {
                bindings,
                list,
                body,
            } => {
                return self.foreach(span, bindings, list, body);
            }
            S::Poll { .. } => return Err(unsupported(span, "poll")),
            S::Retry { .. } => return Err(unsupported(span, "retry")),
            S::Assertion { .. } => return Err(unsupported(span, "assertion")),
            S::LetDestructure { .. } => return Err(unsupported(span, "destructuring")),
            S::Bind { .. } => return Err(unsupported(span, "command bind")),
            S::BindCollect { .. } => return Err(unsupported(span, "collect")),
            S::Break => return self.loop_control(span, LoopTrace::Break),
            S::Continue => return self.loop_control(span, LoopTrace::Continue),
        };
        self.emit(span, instruction);
        Ok(())
    }

    fn foreach(
        &mut self,
        span: &Span,
        bindings: &syntax::LoopBindings,
        list: &syntax::Expression,
        statements: &[syntax::Statement],
    ) -> Result<(), LowerError> {
        let owner = self.layout.current();
        let list = self.eval(span, list)?;
        let bindings = LoopBindings::new(
            bindings
                .names()
                .iter()
                .map(|name| name.text().to_owned())
                .collect(),
        )
        .ok_or_else(|| LowerError::IncompleteLayout(span.clone()))?;
        let body = self.layout.new_block(span);
        let exit = self.layout.new_block(span);
        self.layout.terminate(located(
            span,
            Exit::ForEach {
                list,
                bindings,
                body,
                exit,
            },
        ));

        let outer = self.current_loop.replace(owner);
        self.layout.select(body);
        for statement in statements {
            self.statement(statement)?;
        }
        self.layout
            .terminate(located(span, Exit::ForEachContinue(owner)));
        self.current_loop = outer;
        self.layout.select(exit);
        Ok(())
    }

    fn loop_control(&mut self, span: &Span, trace: LoopTrace) -> Result<(), LowerError> {
        let Some(owner) = self.current_loop else {
            let keyword = match trace {
                LoopTrace::Break => "break",
                LoopTrace::Continue => "continue",
            };
            return Err(LowerError::OutsideLoop {
                keyword,
                span: span.clone(),
            });
        };
        let exit = match trace {
            LoopTrace::Break => Exit::BreakLoop(owner),
            LoopTrace::Continue => Exit::ForEachContinue(owner),
        };
        self.emit(span, Instruction::TraceLoop(trace));
        self.layout.terminate(located(span, exit));
        let unreachable = self.layout.new_block(span);
        self.layout.select(unreachable);
        Ok(())
    }

    fn conditional(
        &mut self,
        span: &Span,
        first: &syntax::IfBranch,
        elif: &[syntax::IfBranch],
        else_body: Option<&[syntax::Statement]>,
    ) -> Result<(), LowerError> {
        let merge = self.layout.reserve(span);
        for (index, branch) in std::iter::once(first).chain(elif).enumerate() {
            let branch_span = if index == 0 { span } else { branch.span() };
            let condition = self.eval(branch_span, branch.condition())?;
            let on_true = self.layout.new_block(branch_span);
            let next_span = elif.get(index).map_or(branch_span, syntax::IfBranch::span);
            let on_false = self.layout.new_block(next_span);
            self.layout.terminate(located(
                branch_span,
                Exit::Branch {
                    condition,
                    on_true,
                    on_false,
                },
            ));

            self.layout.select(on_true);
            let trace = if index == 0 {
                BranchTrace::Then
            } else {
                BranchTrace::Elif
            };
            self.branch_body(span, branch_span, trace, branch.body(), merge)?;
            self.layout.select(on_false);
        }
        if let Some(body) = else_body {
            let else_span = elif.last().map_or(span, syntax::IfBranch::span);
            self.branch_body(span, else_span, BranchTrace::Else, body, merge)?;
        } else {
            self.emit(span, Instruction::TraceBranch(BranchTrace::Skip));
            self.layout.terminate(located(span, Exit::Jump(merge)));
        }

        self.layout.publish(merge);
        self.layout.select(merge);
        Ok(())
    }

    fn branch_body(
        &mut self,
        trace_span: &Span,
        frame_span: &Span,
        trace: BranchTrace,
        body: &[syntax::Statement],
        merge: Label,
    ) -> Result<(), LowerError> {
        self.emit(trace_span, Instruction::TraceBranch(trace));
        self.emit(frame_span, Instruction::EnterIfFrame);
        for statement in body {
            self.statement(statement)?;
        }
        self.emit(frame_span, Instruction::ExitFrame);
        self.layout
            .terminate(located(frame_span, Exit::Jump(merge)));
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
        E::Binary {
            left,
            operator,
            right,
        } => ExpressionKind::Binary {
            left: Box::new(lower_expression(left)?),
            operator: lower_operator(*operator),
            right: Box::new(lower_expression(right)?),
        },
        _ => return Err(unsupported(value.span(), "expression")),
    };
    Ok(located(value.span(), node))
}

fn lower_operator(operator: syntax::BinaryOperator) -> BinaryOperator {
    use syntax::BinaryOperator as O;
    match operator {
        O::Add => BinaryOperator::Add,
        O::Subtract => BinaryOperator::Subtract,
        O::Multiply => BinaryOperator::Multiply,
        O::Divide => BinaryOperator::Divide,
        O::Remainder => BinaryOperator::Remainder,
        O::Equal => BinaryOperator::Equal,
        O::NotEqual => BinaryOperator::NotEqual,
        O::Less => BinaryOperator::Less,
        O::LessEqual => BinaryOperator::LessEqual,
        O::Greater => BinaryOperator::Greater,
        O::GreaterEqual => BinaryOperator::GreaterEqual,
    }
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

        let program = check(expand("print ok\ndef later() { print ok }"))
            .unwrap_or_else(|issues| unreachable!("{issues:?}"));
        assert!(matches!(
            lower(&program),
            Err(LowerError::Unsupported { form: "definition", span }) if &span == program.statements()[1].span()
        ));
    }

    #[test]
    fn nested_if_chain_matches_go_dump_and_preserves_trace_and_condition_spans() {
        use bpfman_shell_ir::{BlockId, BranchTrace, Temp, Terminator};

        let source = include_str!("../testdata/conditionals.bpfman");
        let program = check(expand(source)).unwrap_or_else(|issues| unreachable!("{issues:?}"));
        let ir = lower(&program).unwrap_or_else(|error| unreachable!("{error}"));

        assert_eq!(
            ir.to_string(),
            include_str!("../testdata/conditionals.lowered")
        );
        assert_eq!(ir.num_temps(), 9);
        assert_eq!(
            ir.blocks()[0].terminator.node,
            Terminator::Branch {
                condition: Temp(1),
                on_true: BlockId(1),
                on_false: BlockId(2),
            }
        );
        assert_eq!(
            &ir.blocks()[0].terminator.span,
            program.statements()[1].span()
        );
        assert_eq!(ir.blocks()[2].terminator.span.start().line(), 5);
        assert_eq!(
            ir.blocks()[4].instructions[0].node,
            Instruction::TraceBranch(BranchTrace::Skip)
        );
        assert_eq!(ir.blocks()[4].instructions[0].span.start().line(), 3);
    }

    #[test]
    fn nested_foreach_matches_go_and_uses_the_innermost_loop_owner() {
        use bpfman_shell_ir::{BlockId, LoopId, LoopTrace, Terminator};

        let source = include_str!("../testdata/foreach.bpfman");
        let program = check(expand(source)).unwrap_or_else(|issues| unreachable!("{issues:?}"));
        let ir = lower(&program).unwrap_or_else(|error| unreachable!("{error}"));
        assert_eq!(ir.to_string(), include_str!("../testdata/foreach.lowered"));
        assert_eq!(
            ir.blocks()[5].terminator.node,
            Terminator::ForEachContinue(LoopId(BlockId(1)))
        );
        assert_eq!(
            ir.blocks()[8].terminator.node,
            Terminator::ExitLoop(LoopId(BlockId(1)))
        );
        assert_eq!(
            ir.blocks()[10].terminator.node,
            Terminator::ExitLoop(LoopId(BlockId(0)))
        );
        assert_eq!(ir.blocks()[10].terminator.span.start().line(), 6);
        assert_eq!(
            ir.blocks()[10].instructions[2].node,
            Instruction::TraceLoop(LoopTrace::Break)
        );
    }

    #[test]
    fn unchecked_loop_control_still_requires_a_lexical_owner() {
        for keyword in ["break", "continue"] {
            let program = expand(keyword);
            assert!(matches!(
                lower_unchecked(&program),
                Err(LowerError::OutsideLoop { keyword: actual, span })
                    if actual == keyword && &span == program.statements()[0].span()
            ));
        }
    }
}
