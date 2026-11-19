use std::{fmt, num::NonZeroU64};

use crate::expr::{lower_expression, lower_expressions};
use crate::layout::{Exit, Label, Layout};
use bpfman_shell_check::{CheckedProgram, ExpandedProgram};
use bpfman_shell_ir::{
    Assertion, BindPolicy, BranchTrace, CollectMode, Command, CommandStatus, Definition,
    Instruction, Located, LoopBindings, LoopTrace, Parameter, ParameterType, PollSchedule,
    PollTrace, Program, Temp, VerifyError,
};
use bpfman_shell_source::Span;
use bpfman_shell_syntax as syntax;

/// A failure to lower the input into the supported IR.
#[derive(Debug, Eq, PartialEq)]
pub enum LowerError {
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
    /// A definition appears inside another body.
    NestedDefinition(Span),
    /// A return appears outside any definition.
    OutsideDefinition(Span),
    /// A status assertion is missing its command.
    MissingAssertionCommand(Span),
    /// A retry appears outside a poll or definition.
    OutsidePoll(Span),
}

impl fmt::Display for LowerError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::OutsidePoll(span) => write!(
                f,
                "{}:{}:{}: retry outside any poll or helper def",
                span.start().name().as_str(),
                span.start().line(),
                span.start().column()
            ),
            Self::InvalidIr(error) => write!(f, "invalid lowered program: {error}"),
            Self::MissingAssertionCommand(span) => write!(
                f,
                "{}:{}:{}: expected command after assertion status",
                span.start().name().as_str(),
                span.start().line(),
                span.start().column()
            ),
            Self::NestedDefinition(span) => write!(
                f,
                "{}:{}:{}: def must be declared at top level",
                span.start().name().as_str(),
                span.start().line(),
                span.start().column()
            ),
            Self::OutsideDefinition(span) => write!(
                f,
                "{}:{}:{}: return outside a def body",
                span.start().name().as_str(),
                span.start().line(),
                span.start().column()
            ),
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
            Self::IncompleteLayout(_)
            | Self::OutsideLoop { .. }
            | Self::NestedDefinition(_)
            | Self::OutsideDefinition(_)
            | Self::MissingAssertionCommand(_)
            | Self::OutsidePoll(_) => None,
        }
    }
}

/// Lower a program admitted by preflight.
///
/// # Errors
///
/// Returns a source-located error for invalid lowering or invalid IR.
pub fn lower(program: &CheckedProgram) -> Result<Program, LowerError> {
    lower_body(program.span(), program.statements())
}

/// Lower expanded syntax without static preflight, for the explicit
/// `--no-check` path. Structural IR verification still applies.
///
/// # Errors
///
/// Returns a source-located error for invalid lowering or invalid IR.
pub fn lower_unchecked(program: &ExpandedProgram) -> Result<Program, LowerError> {
    lower_body(program.span(), program.statements())
}

fn lower_body(span: &Span, statements: &[syntax::Statement]) -> Result<Program, LowerError> {
    let mut body = Body {
        layout: Layout::new(span),
        next_temp: 0,
        current_loop: None,
        current_poll: None,
        epilogue: None,
    };
    let mut definitions = Vec::new();
    body.emit(span, Instruction::EnterProgramDefers);
    for statement in statements {
        if let syntax::StatementKind::Definition(definition) = statement.kind() {
            definitions.push(lower_definition(definition, statement.span())?);
        } else {
            body.statement(statement)?;
        }
    }
    body.emit(span, Instruction::RunProgramDefers);
    body.layout.terminate(located(span, Exit::Stop));
    Program::with_definitions(span.clone(), body.layout.finish()?, definitions)
        .map_err(LowerError::InvalidIr)
}

fn lower_definition(
    definition: &syntax::Definition,
    span: &Span,
) -> Result<Definition, LowerError> {
    let parameters: Vec<_> = definition
        .parameters()
        .iter()
        .map(|parameter| Parameter {
            name: parameter.name().text().to_owned(),
            annotation: parameter.annotation().map(|annotation| match annotation {
                syntax::ParameterType::Number => ParameterType::Number,
                syntax::ParameterType::String => ParameterType::String,
                syntax::ParameterType::Bool => ParameterType::Bool,
            }),
        })
        .collect();
    let mut layout = Layout::new(span);
    let epilogue = layout.reserve(span);
    let mut body = Body {
        layout,
        next_temp: parameters.len(),
        current_loop: None,
        current_poll: None,
        epilogue: Some(epilogue),
    };
    body.emit(span, Instruction::EnterDefinitionFrame);
    body.emit(span, Instruction::EnterDefinitionDefers);
    for (index, parameter) in parameters.iter().enumerate() {
        body.emit(
            span,
            Instruction::BindName {
                name: parameter.name.clone(),
                src: Temp(index),
            },
        );
    }
    for statement in definition.body() {
        body.statement(statement)?;
    }
    body.layout.terminate(located(span, Exit::Jump(epilogue)));
    body.layout.publish(epilogue);
    body.layout.select(epilogue);
    body.emit(span, Instruction::RunDefinitionDefers);
    body.emit(span, Instruction::ExitFrame);
    body.layout.terminate(located(span, Exit::FinishDefinition));
    Definition::new(
        definition.name().text().to_owned(),
        parameters,
        span.clone(),
        body.layout.finish()?,
    )
    .map_err(LowerError::InvalidIr)
}

struct Body {
    layout: Layout,
    next_temp: usize,
    current_loop: Option<Label>,
    current_poll: Option<Label>,
    epilogue: Option<Label>,
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
                src: self.eval(span, value),
            },
            S::Expression(value) => Instruction::EmitResult {
                src: self.eval(span, value),
            },
            S::Command(command) => Instruction::DispatchCommand {
                argv: self.build_args(span, command),
            },
            S::Defer(command) => Instruction::RegisterDefer {
                argv: self.build_args(command.span(), command),
            },
            S::Definition(_) => return Err(LowerError::NestedDefinition(span.clone())),
            S::Return(value) => return self.return_value(span, value),
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
            S::Poll {
                timeout,
                every,
                body,
            } => return self.poll(span, *timeout, *every, body),
            S::Retry { message, unless } => {
                return self.retry(span, message.as_ref(), unless.as_ref());
            }
            S::Assertion { mode, clause } => Instruction::Assert {
                required: *mode == syntax::AssertionMode::Require,
                clause: Box::new(lower_assertion(clause)?),
            },
            S::LetDestructure { names, value } => Instruction::BindDestructure {
                src: self.eval(span, value),
                bindings: lower_bindings(span, names)?,
            },
            S::Bind {
                mode,
                name,
                command,
            } => {
                return self.bind_command(span, *mode, name.text(), command);
            }
            S::BindCollect {
                foreach_span,
                mode,
                name,
                bindings,
                list,
                body,
            } => {
                return self.collect(
                    span,
                    foreach_span,
                    (name.text(), *mode),
                    bindings,
                    list,
                    body,
                );
            }
            S::Break => return self.loop_control(span, LoopTrace::Break),
            S::Continue => return self.loop_control(span, LoopTrace::Continue),
        };
        self.emit(span, instruction);
        Ok(())
    }

    fn return_value(&mut self, span: &Span, value: &syntax::Expression) -> Result<(), LowerError> {
        let Some(epilogue) = self.epilogue else {
            return Err(LowerError::OutsideDefinition(span.clone()));
        };
        let src = self.eval(span, value);
        self.layout
            .terminate(located(span, Exit::ReturnValue { src, epilogue }));
        let unreachable = self.layout.new_block(span);
        self.layout.select(unreachable);
        Ok(())
    }

    fn bind_command(
        &mut self,
        span: &Span,
        mode: syntax::BindMode,
        target: &str,
        command: &syntax::Command,
    ) -> Result<(), LowerError> {
        let (argv, result) = self.dispatch_bind(span, mode, target, command);
        let policy = match mode {
            syntax::BindMode::Let => BindPolicy::Capture,
            syntax::BindMode::Guard => {
                let success = self.layout.current();
                let failure = self.layout.new_block(span);
                self.layout.select(failure);
                if self.current_poll.is_some() {
                    self.emit(span, Instruction::RunAttemptDefers);
                    self.emit(span, Instruction::ExitFrame);
                } else if self.epilogue.is_some() {
                    self.emit(span, Instruction::RunDefinitionDefers);
                    self.emit(span, Instruction::ExitFrame);
                } else {
                    self.emit(span, Instruction::RunProgramDefers);
                }
                self.layout.terminate(located(span, Exit::PropagateError));
                self.layout.select(success);
                BindPolicy::Guard {
                    on_failure: self.layout.published_id(failure)?,
                }
            }
        };
        self.emit(
            span,
            Instruction::ApplyBind {
                src: result,
                argv,
                target: target.to_owned(),
                policy,
            },
        );
        Ok(())
    }

    fn dispatch_bind(
        &mut self,
        span: &Span,
        mode: syntax::BindMode,
        target: &str,
        command: &syntax::Command,
    ) -> (Temp, Temp) {
        let argv = self.build_args(command.span(), command);
        let result = self.allocate();
        let keyword = match mode {
            syntax::BindMode::Let => "let",
            syntax::BindMode::Guard => "guard",
        };
        self.emit(
            span,
            Instruction::DispatchBind {
                dst: result,
                argv,
                call_span: command.span().clone(),
                trace_header: format!("{keyword} {target}"),
            },
        );
        (argv, result)
    }

    fn collect(
        &mut self,
        span: &Span,
        foreach_span: &Span,
        (target, mode): (&str, syntax::BindMode),
        bindings: &syntax::LoopBindings,
        list: &syntax::Expression,
        statements: &[syntax::Statement],
    ) -> Result<(), LowerError> {
        let Some((producer, prefix)) = statements.split_last() else {
            return Err(LowerError::IncompleteLayout(span.clone()));
        };
        let syntax::StatementKind::Command(command) = producer.kind() else {
            return Err(LowerError::IncompleteLayout(producer.span().clone()));
        };
        let owner = self.layout.current();
        let list = self.eval(foreach_span, list);
        let bindings = lower_bindings(foreach_span, bindings.names())?;
        let body = self.layout.new_block(foreach_span);
        let exit = self.layout.new_block(foreach_span);
        let collect_mode = match mode {
            syntax::BindMode::Let => CollectMode::Capture,
            syntax::BindMode::Guard => CollectMode::Guard,
        };
        self.layout.terminate(located(
            foreach_span,
            Exit::ForEachCollect {
                list,
                bindings,
                target: target.to_owned(),
                mode: collect_mode,
                body,
                exit,
            },
        ));
        let outer = self.current_loop.replace(owner);
        self.layout.select(body);
        for statement in prefix {
            self.statement(statement)?;
        }
        let (_, result) = self.dispatch_bind(producer.span(), mode, target, command);
        self.layout.terminate(located(
            producer.span(),
            Exit::CollectProduce {
                owner,
                result,
                bind_span: span.clone(),
            },
        ));
        self.current_loop = outer;
        self.layout.select(exit);
        Ok(())
    }

    fn poll(
        &mut self,
        span: &Span,
        timeout: syntax::PositiveDuration,
        every: syntax::PositiveDuration,
        statements: &[syntax::Statement],
    ) -> Result<(), LowerError> {
        let owner = self.layout.current();
        let attempt = self.layout.new_block(span);
        let on_timeout = self.layout.new_block(span);
        let success = self.layout.new_block(span);
        let nanos = |duration: syntax::PositiveDuration| {
            NonZeroU64::new(duration.as_nanos())
                .ok_or_else(|| LowerError::IncompleteLayout(span.clone()))
        };
        let schedule = PollSchedule {
            timeout: nanos(timeout)?,
            every: nanos(every)?,
        };
        self.layout.terminate(located(
            span,
            Exit::BeginPoll {
                schedule,
                attempt,
                timeout: on_timeout,
                success,
            },
        ));
        let outer = self.current_poll.replace(owner);
        self.layout.select(attempt);
        self.emit(span, Instruction::TracePoll(PollTrace::Attempt));
        self.emit(span, Instruction::EnterAttemptFrame);
        self.emit(span, Instruction::EnterAttemptDefers);
        for statement in statements {
            self.statement(statement)?;
        }
        self.emit(span, Instruction::RunAttemptDefers);
        self.emit(span, Instruction::ExitFrame);
        self.layout.terminate(located(span, Exit::Jump(success)));

        self.current_poll = outer;
        self.layout.select(on_timeout);
        self.emit(span, Instruction::TracePoll(PollTrace::Timeout));
        self.layout.terminate(located(span, Exit::PropagateError));
        self.layout.select(success);
        Ok(())
    }

    fn retry(
        &mut self,
        span: &Span,
        message: Option<&syntax::Expression>,
        unless: Option<&syntax::Expression>,
    ) -> Result<(), LowerError> {
        if self.current_poll.is_none() && self.epilogue.is_none() {
            return Err(LowerError::OutsidePoll(span.clone()));
        }
        let skip = unless.map(|condition| {
            let condition = self.eval(span, condition);
            let on_true = self.layout.new_block(span);
            let on_false = self.layout.new_block(span);
            self.layout.terminate(located(
                span,
                Exit::Branch {
                    condition,
                    on_true,
                    on_false,
                },
            ));
            self.layout.select(on_false);
            on_true
        });
        let message = message.map(|value| self.eval(span, value));
        self.emit(span, Instruction::RunAttemptDefers);
        self.emit(span, Instruction::ExitFrame);
        self.layout.terminate(located(
            span,
            Exit::RetryPoll {
                owner: self.current_poll,
                message,
            },
        ));
        let unreachable = self.layout.new_block(span);
        self.layout.select(unreachable);
        if let Some(skip) = skip {
            // Go preserves the empty block left by conditional retry in its dump.
            self.layout.terminate(located(span, Exit::Unreachable));
            self.layout.select(skip);
        }
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
        let list = self.eval(span, list);
        let bindings = lower_bindings(span, bindings.names())?;
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
            let condition = self.eval(branch_span, branch.condition());
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

    fn eval(&mut self, span: &Span, value: &syntax::Expression) -> Temp {
        let expression = lower_expression(value);
        let dst = self.allocate();
        self.emit(span, Instruction::Eval { dst, expression });
        dst
    }

    fn build_args(&mut self, span: &Span, command: &syntax::Command) -> Temp {
        let command = Command {
            head: lower_expression(command.head()),
            arguments: lower_expressions(command.arguments()),
        };
        let dst = self.allocate();
        self.emit(span, Instruction::BuildArgs { dst, command });
        dst
    }
}

fn lower_bindings(span: &Span, names: &[syntax::Identifier]) -> Result<LoopBindings, LowerError> {
    LoopBindings::new(names.iter().map(|name| name.text().to_owned()).collect())
        .ok_or_else(|| LowerError::IncompleteLayout(span.clone()))
}

fn lower_assertion(clause: &syntax::AssertionClause) -> Result<Assertion, LowerError> {
    Ok(match clause {
        syntax::AssertionClause::Expression(value) => {
            Assertion::Expression(lower_expression(value))
        }
        syntax::AssertionClause::CommandStatus {
            outcome,
            head_span,
            arguments,
            negated,
        } => {
            let (head, arguments) = arguments
                .split_first()
                .ok_or_else(|| LowerError::MissingAssertionCommand(head_span.clone()))?;
            Assertion::Command {
                expected: match outcome {
                    syntax::CommandOutcome::Ok => CommandStatus::Ok,
                    syntax::CommandOutcome::Fail => CommandStatus::Fail,
                },
                status_span: head_span.clone(),
                command: Command {
                    head: lower_expression(head),
                    arguments: lower_expressions(arguments),
                },
                negated: *negated,
            }
        }
    })
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
    fn unchecked_entry_is_explicit_and_invalid_retry_fails_with_span() {
        let unknown = expand("print $missing");
        assert!(check(unknown.clone()).is_err());
        assert!(lower_unchecked(&unknown).is_ok());

        let retry = expand("print ok\nretry");
        assert!(matches!(
            lower_unchecked(&retry),
            Err(LowerError::OutsidePoll(span)) if &span == retry.statements()[1].span()
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

    #[test]
    fn expression_operands_match_go_without_retaining_syntax_nodes() {
        let program = check(expand(include_str!("../testdata/expressions.bpfman")))
            .unwrap_or_else(|issues| unreachable!("{issues:?}"));
        let ir = lower(&program).unwrap_or_else(|error| unreachable!("{error}"));
        assert_eq!(
            ir.to_string(),
            include_str!("../testdata/expressions.lowered")
        );
    }

    #[test]
    fn assertion_and_destructure_lowering_matches_go() {
        let program = check(expand(include_str!("../testdata/assertions.bpfman")))
            .unwrap_or_else(|issues| unreachable!("{issues:?}"));
        let ir = lower(&program).unwrap_or_else(|error| unreachable!("{error}"));
        assert_eq!(
            ir.to_string(),
            include_str!("../testdata/assertions.lowered")
        );
    }

    #[test]
    fn full_go_language_golden_matches_after_import_expansion() {
        let source =
            include_str!("../../../../cmd/bpfman-shell/shell/lower/testdata/language.bpfman");
        let library =
            include_str!("../../../../cmd/bpfman-shell/shell/lower/testdata/language-lib.bpfman");
        let expected =
            include_str!("../../../../cmd/bpfman-shell/shell/lower/testdata/language.lowered");
        let parsed =
            parse("language.bpfman", source).unwrap_or_else(|error| unreachable!("{error}"));
        let plan =
            prepare_imports(parsed, Path::new(".")).unwrap_or_else(|error| unreachable!("{error}"));
        let gathered = BTreeMap::from([(plan.requests()[0].path().to_owned(), library.to_owned())]);
        let expanded =
            expand_imports(plan, &gathered).unwrap_or_else(|error| unreachable!("{error}"));
        let checked = check(expanded).unwrap_or_else(|issues| unreachable!("{issues:?}"));
        let ir = lower(&checked).unwrap_or_else(|error| unreachable!("{error}"));
        assert_eq!(ir.to_string(), expected);
    }

    #[test]
    fn retry_targets_the_lexical_poll_or_dynamic_caller() {
        use bpfman_shell_ir::{BlockId, RetryTarget, Terminator};

        let source =
            "def helper() { retry \"wait\" }\npoll timeout 1m every 1.5ms { retry unless true }";
        let program = check(expand(source)).unwrap_or_else(|issues| unreachable!("{issues:?}"));
        let ir = lower(&program).unwrap_or_else(|error| unreachable!("{error}"));
        assert!(matches!(
            ir.definitions()[0].blocks()[0].terminator.node,
            Terminator::RetryPoll {
                target: RetryTarget::Caller,
                message: Some(_)
            }
        ));
        assert!(ir.blocks().iter().any(|block| matches!(
            block.terminator.node,
            Terminator::RetryPoll {
                target: RetryTarget::Local(BlockId(0)),
                message: None
            }
        )));
        assert!(
            ir.to_string()
                .contains("BeginPoll timeout=1m0s every=1.5ms")
        );
    }

    #[test]
    fn collecting_guards_and_captures_match_go_and_keep_distinct_spans() {
        use bpfman_shell_ir::{BlockId, CollectMode, LoopId, Terminator};

        let program = check(expand(include_str!("../testdata/collect.bpfman")))
            .unwrap_or_else(|issues| unreachable!("{issues:?}"));
        let ir = lower(&program).unwrap_or_else(|error| unreachable!("{error}"));
        assert_eq!(ir.to_string(), include_str!("../testdata/collect.lowered"));
        let header = &ir.blocks()[0].terminator;
        assert!(matches!(
            header.node,
            Terminator::ForEachCollect {
                mode: CollectMode::Capture,
                ..
            }
        ));
        assert_eq!(header.span.start().column(), 17);
        let producer = ir
            .blocks()
            .iter()
            .find_map(|block| match &block.terminator.node {
                Terminator::CollectProduce {
                    owner, bind_span, ..
                } if *owner == LoopId(BlockId(0)) => Some(bind_span),
                _ => None,
            });
        assert_eq!(producer, Some(program.statements()[0].span()));
    }

    #[test]
    fn definitions_have_unit_local_ids_and_match_go_cleanup() {
        use bpfman_shell_ir::{BlockId, ParameterType, Temp, Terminator};

        let program = check(expand(include_str!("../testdata/definitions.bpfman")))
            .unwrap_or_else(|issues| unreachable!("{issues:?}"));
        let ir = lower(&program).unwrap_or_else(|error| unreachable!("{error}"));
        assert_eq!(
            ir.to_string(),
            include_str!("../testdata/definitions.lowered")
        );
        let def = &ir.definitions()[0];
        assert_eq!(def.parameters()[0].annotation, Some(ParameterType::Number));
        assert_eq!(def.num_temps(), 7);
        assert_eq!(
            def.blocks()[1].terminator.node,
            Terminator::ReturnValue {
                src: Temp(5),
                epilogue: BlockId(6),
            }
        );
        assert_eq!(ir.definitions()[1].num_temps(), 2);
        assert_eq!(ir.num_temps(), 3);
        assert!(matches!(
            lower_unchecked(&expand("return 1")),
            Err(LowerError::OutsideDefinition(_))
        ));
        assert!(matches!(
            lower_unchecked(&expand("if true { def nested() { print bad } }")),
            Err(LowerError::NestedDefinition(_))
        ));
    }

    #[test]
    fn command_binds_match_go_and_keep_failure_cleanup_separate() {
        use bpfman_shell_ir::{BindPolicy, BlockId, Terminator};

        let source = include_str!("../testdata/binds.bpfman");
        let program = check(expand(source)).unwrap_or_else(|issues| unreachable!("{issues:?}"));
        let ir = lower(&program).unwrap_or_else(|error| unreachable!("{error}"));
        assert_eq!(ir.to_string(), include_str!("../testdata/binds.lowered"));

        let entry = &ir.blocks()[0];
        assert!(entry.instructions.iter().any(|instruction| matches!(
            &instruction.node,
            Instruction::DispatchBind { call_span, trace_header, .. }
                if trace_header == "guard value" && call_span.start().column() == 16
        )));
        let nested = &ir.blocks()[4];
        assert!(nested.instructions.iter().any(|instruction| matches!(
            &instruction.node,
            Instruction::ApplyBind { policy: BindPolicy::Guard { on_failure: BlockId(6) }, target, .. }
                if target == "_"
        )));
        let failure = &ir.blocks()[6];
        assert_eq!(failure.instructions[0].node, Instruction::RunProgramDefers);
        assert_eq!(failure.terminator.node, Terminator::PropagateError);
        assert_eq!(failure.terminator.span.start().line(), 6);
    }
}
