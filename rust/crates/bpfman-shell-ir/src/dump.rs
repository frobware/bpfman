use std::{
    collections::{BTreeMap, HashSet},
    fmt,
};

use crate::{
    Assertion, BinaryOperator, BindPolicy, Block, Command, CommandStatus, Expression,
    ExpressionKind, Instruction, InterpolatedPart, LogicalOperator, MatchBlock, MatchPattern,
    MatchPredicate, ParameterType, Program, PureBuiltin, Terminator, UnaryOperator, VariableRef,
};

impl fmt::Display for Program {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let names = self
            .definitions()
            .iter()
            .map(crate::Definition::name)
            .collect();
        for def in self.definitions() {
            write!(f, "def {}(", def.name())?;
            for (index, parameter) in def.parameters().iter().enumerate() {
                if index > 0 {
                    f.write_str(" ")?;
                }
                f.write_str(&parameter.name)?;
                if let Some(annotation) = parameter.annotation {
                    let name = match annotation {
                        ParameterType::Number => "number",
                        ParameterType::String => "string",
                        ParameterType::Bool => "bool",
                    };
                    write!(f, ": {name}")?;
                }
            }
            writeln!(f, ") entry=bb0")?;
            dump_blocks(f, def.blocks(), &names)?;
            writeln!(f)?;
        }
        writeln!(f, "body entry=bb{}", self.entry().0)?;
        dump_blocks(f, self.blocks(), &names)
    }
}

fn dump_blocks(
    f: &mut fmt::Formatter<'_>,
    blocks: &[Block],
    definitions: &HashSet<&str>,
) -> fmt::Result {
    for (id, block) in blocks.iter().enumerate() {
        let mut heads = BTreeMap::new();
        writeln!(f, "\nbb{id}:")?;
        for instruction in &block.instructions {
            if matches!(
                instruction.node,
                Instruction::TraceBranch(_) | Instruction::TraceLoop(_) | Instruction::TracePoll(_)
            ) {
                continue;
            }
            f.write_str("  ")?;
            match &instruction.node {
                Instruction::TraceBranch(_)
                | Instruction::TraceLoop(_)
                | Instruction::TracePoll(_) => {}
                Instruction::EnterIfFrame => writeln!(f, "EnterFrame kind=if-branch")?,
                Instruction::EnterDefinitionFrame => writeln!(f, "EnterFrame kind=def")?,
                Instruction::EnterDefinitionDefers => writeln!(f, "EnterDeferScope kind=def")?,
                Instruction::RunDefinitionDefers => writeln!(f, "RunDefers policy=def-local")?,
                Instruction::EnterAttemptFrame => writeln!(f, "EnterFrame kind=poll-attempt")?,
                Instruction::EnterAttemptDefers => {
                    writeln!(f, "EnterDeferScope kind=poll-attempt")?
                }
                Instruction::RunAttemptDefers => writeln!(f, "RunDefers policy=attempt-fatal")?,
                Instruction::ExitFrame => writeln!(f, "ExitFrame")?,
                Instruction::EnterProgramDefers => {
                    writeln!(f, "EnterDeferScope kind=program")?;
                }
                Instruction::RunProgramDefers => writeln!(f, "RunDefers policy=program")?,
                Instruction::Eval { dst, expression } => {
                    writeln!(f, "Eval t{} = {expression}", dst.0)?;
                }
                Instruction::BindName { name, src, .. } => {
                    writeln!(f, "BindName {name} = t{}", src.0)?;
                }
                Instruction::BindDestructure { bindings, src } => {
                    writeln!(
                        f,
                        "BindDestructure [{}] = t{}",
                        bindings.names().join(" "),
                        src.0
                    )?;
                }
                Instruction::Assert { required, clause } => {
                    f.write_str(if *required { "Require " } else { "Assert " })?;
                    match clause.as_ref() {
                        Assertion::Expression(expression) => write!(f, "{expression}")?,
                        Assertion::Command {
                            expected,
                            command,
                            negated,
                            ..
                        } => {
                            if *negated {
                                f.write_str("not ")?;
                            }
                            let status = match expected {
                                CommandStatus::Ok => "ok",
                                CommandStatus::Fail => "fail",
                            };
                            write!(f, "{status} {}", command.head)?;
                            for argument in &command.arguments {
                                write!(f, " {argument}")?;
                            }
                        }
                    }
                    writeln!(f)?;
                }
                Instruction::EmitResult { src } => writeln!(f, "EmitResult t{}", src.0)?,
                Instruction::BuildArgs { dst, command } => {
                    heads.insert(dst.0, lane(command, definitions));
                    write!(f, "BuildArgs t{} = [{}", dst.0, command.head)?;
                    for argument in &command.arguments {
                        write!(f, " {argument}")?;
                    }
                    writeln!(f, "]")?;
                }
                Instruction::DispatchCommand { argv } => {
                    writeln!(
                        f,
                        "DispatchCommand argv=t{} policy=def-then-exec-command lane={}",
                        argv.0,
                        heads.get(&argv.0).ok_or(fmt::Error)?,
                    )?;
                }
                Instruction::DispatchBind { dst, argv, .. } => {
                    writeln!(
                        f,
                        "DispatchBind t{} = t{} policy=def-then-exec-bind lane={}",
                        dst.0,
                        argv.0,
                        heads.get(&argv.0).ok_or(fmt::Error)?,
                    )?;
                }
                Instruction::ApplyBind {
                    src,
                    argv,
                    target,
                    policy,
                } => {
                    let guard = matches!(policy, BindPolicy::Guard { .. });
                    write!(
                        f,
                        "ApplyBind src=t{} argv=t{} target={target} guard={guard}",
                        src.0, argv.0
                    )?;
                    if let BindPolicy::Guard { on_failure } = policy {
                        write!(f, " fail=bb{}", on_failure.0)?;
                    }
                    writeln!(f)?;
                }
                Instruction::RegisterDefer { argv } => {
                    writeln!(
                        f,
                        "RegisterDefer argv=t{} policy=def-then-exec-bind lane={}",
                        argv.0,
                        heads.get(&argv.0).ok_or(fmt::Error)?,
                    )?;
                }
            }
        }
        match block.terminator.node {
            Terminator::Jump(target) => writeln!(f, "  Jump bb{}", target.0)?,
            Terminator::Branch {
                condition,
                on_true,
                on_false,
            } => writeln!(
                f,
                "  Branch cond=t{} true=bb{} false=bb{}",
                condition.0, on_true.0, on_false.0
            )?,
            Terminator::ReturnValue { src, epilogue } => {
                writeln!(f, "  ReturnValue t{} to=bb{}", src.0, epilogue.0)?
            }
            Terminator::FinishDefinition => {
                writeln!(f, "  EmitBindResult rc=synthetic primary=nil")?
            }
            Terminator::Stop => writeln!(f, "  Stop")?,
            Terminator::PropagateError => writeln!(f, "  PropagateError")?,
            Terminator::ForEach {
                list,
                ref bindings,
                body,
                exit,
            } => writeln!(
                f,
                "  ForEach list=t{} names=[{}] body=bb{} exit=bb{}",
                list.0,
                bindings.names().join(" "),
                body.0,
                exit.0,
            )?,
            Terminator::ForEachContinue(_) => writeln!(f, "  ForEachContinue")?,
            Terminator::ForEachCollect {
                list,
                ref bindings,
                ref target,
                mode,
                body,
                exit,
            } => {
                let guard = mode == crate::CollectMode::Guard;
                writeln!(
                    f,
                    "  ForEachCollect list=t{} names=[{}] target={target} guard={guard} body=bb{} exit=bb{}",
                    list.0,
                    bindings.names().join(" "),
                    body.0,
                    exit.0
                )?;
            }
            Terminator::CollectProduce { result, .. } => {
                writeln!(f, "  CollectProduce t{}", result.0)?
            }
            Terminator::ExitLoop(_) => writeln!(f, "  ExitLoop")?,
            Terminator::Unreachable => {}
            Terminator::BeginPoll {
                schedule,
                attempt,
                timeout,
                success,
            } => {
                write!(f, "  BeginPoll timeout=")?;
                write_duration(f, schedule.timeout.get())?;
                f.write_str(" every=")?;
                write_duration(f, schedule.every.get())?;
                writeln!(
                    f,
                    " attempt=bb{} timeout=bb{} success=bb{}",
                    attempt.0, timeout.0, success.0
                )?;
            }
            Terminator::RetryPoll { message, .. } => {
                f.write_str("  RetryPoll")?;
                if let Some(message) = message {
                    write!(f, " message=t{}", message.0)?;
                }
                writeln!(f)?;
            }
        }
    }
    Ok(())
}

fn write_duration(f: &mut fmt::Formatter<'_>, nanos: u64) -> fmt::Result {
    const SECOND: u64 = 1_000_000_000;
    let (nanos, unit, precision, suffix) = if nanos >= SECOND {
        let hours = nanos / (SECOND * 3600);
        let minutes = (nanos / (SECOND * 60)) % 60;
        if hours != 0 {
            write!(f, "{hours}h")?;
        }
        if hours != 0 || minutes != 0 {
            write!(f, "{minutes}m")?;
        }
        (nanos % (SECOND * 60), SECOND, 9, "s")
    } else if nanos >= 1_000_000 {
        (nanos, 1_000_000, 6, "ms")
    } else if nanos >= 1_000 {
        (nanos, 1_000, 3, "µs")
    } else {
        return write!(f, "{nanos}ns");
    };
    write!(f, "{}", nanos / unit)?;
    if nanos % unit != 0 {
        let fraction = format!("{:0precision$}", nanos % unit);
        write!(f, ".{}", fraction.trim_end_matches('0'))?;
    }
    f.write_str(suffix)
}

fn lane(command: &Command, definitions: &HashSet<&str>) -> String {
    let head = match &command.head.node {
        ExpressionKind::Word(head) | ExpressionKind::Quoted(head) => head.as_str(),
        _ => return "unresolved-head".to_owned(),
    };
    if definitions.contains(head) {
        return format!("def({head})");
    }
    // This is the Go IR dumper’s command vocabulary.
    match head {
        "defs" | "exec" | "file" | "import" | "jobs" | "jq" | "kill" | "print" | "registry"
        | "trace" | "reap" | "tempdir" | "start" | "fire" | "net" | "kfunc" | "bpfman"
        | "range" | "zip" | "u32le" | "u64le" | "uprobe" | "uprobe-target" | "wait" => {
            format!("builtin({head})")
        }
        _ => "exec".to_owned(),
    }
}

impl fmt::Display for Expression {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.node {
            ExpressionKind::Word(word) => f.write_str(word),
            ExpressionKind::Quoted(word) => write!(f, "{word:?}"),
            ExpressionKind::Variable(variable) => {
                f.write_str("$")?;
                write_variable(f, variable)
            }
            ExpressionKind::Adapter { name, variable } => {
                write!(f, "{name}:$")?;
                write_variable(f, variable)
            }
            ExpressionKind::Interpolated(parts) => {
                f.write_str("\"")?;
                for part in parts {
                    match part {
                        InterpolatedPart::Literal(text) => f.write_str(text)?,
                        InterpolatedPart::Expression(value) => {
                            f.write_str("${")?;
                            match &value.node {
                                ExpressionKind::Variable(variable) => write_variable(f, variable)?,
                                ExpressionKind::Adapter { name, variable } => {
                                    write!(f, "{name}:")?;
                                    write_variable(f, variable)?;
                                }
                                _ => write!(f, "{value}")?,
                            }
                            f.write_str("}")?;
                        }
                    }
                }
                f.write_str("\"")
            }
            ExpressionKind::Record(fields) => {
                f.write_str("record {")?;
                for (index, field) in fields.iter().enumerate() {
                    if index > 0 {
                        f.write_str(" ")?;
                    }
                    write!(f, "{}: {}", field.name, field.value)?;
                }
                f.write_str("}")
            }
            ExpressionKind::Unary { operator, operand } => {
                f.write_str(match operator {
                    UnaryOperator::Not => "not ",
                    UnaryOperator::Negate => "-",
                    UnaryOperator::NotEmpty => "not-empty ",
                })?;
                write_atom(f, operand)
            }
            ExpressionKind::Logical {
                left,
                operator,
                right,
            } => {
                write_atom(f, left)?;
                f.write_str(match operator {
                    LogicalOperator::And => " and ",
                    LogicalOperator::Or => " or ",
                })?;
                write_atom(f, right)
            }
            ExpressionKind::PureCall { builtin, arguments } => {
                f.write_str(builtin_name(*builtin))?;
                for argument in arguments {
                    write!(f, " {argument}")?;
                }
                Ok(())
            }
            ExpressionKind::Thread { value, command, .. } => {
                write_atom(f, value)?;
                write!(f, " |> {}", command.head)?;
                for argument in &command.arguments {
                    write!(f, " {argument}")?;
                }
                Ok(())
            }
            ExpressionKind::Matches { target, block } => {
                write_atom(f, target)?;
                f.write_str(" ")?;
                write_matches(f, block)
            }
            ExpressionKind::List(items) => {
                f.write_str("[")?;
                for (index, item) in items.iter().enumerate() {
                    if index > 0 {
                        f.write_str(" ")?;
                    }
                    write!(f, "{item}")?;
                }
                f.write_str("]")
            }
            ExpressionKind::Binary {
                left,
                operator,
                right,
            } => {
                write_atom(f, left)?;
                write!(f, " {operator} ")?;
                write_atom(f, right)
            }
        }
    }
}

fn write_atom(f: &mut fmt::Formatter<'_>, expression: &Expression) -> fmt::Result {
    if matches!(
        expression.node,
        ExpressionKind::Binary { .. }
            | ExpressionKind::Unary { .. }
            | ExpressionKind::Logical { .. }
            | ExpressionKind::Thread { .. }
            | ExpressionKind::Matches { .. }
    ) {
        write!(f, "({expression})")
    } else {
        write!(f, "{expression}")
    }
}

fn write_variable(f: &mut fmt::Formatter<'_>, variable: &VariableRef) -> fmt::Result {
    f.write_str(&variable.name)?;
    if !variable.path.is_empty() {
        write!(f, ".{}", variable.path)?;
    }
    Ok(())
}

fn write_matches(f: &mut fmt::Formatter<'_>, block: &MatchBlock) -> fmt::Result {
    f.write_str("matches")?;
    if block.exhaustive {
        f.write_str(" exhaustive")?;
    }
    f.write_str(" {")?;
    for (index, entry) in block.entries.iter().enumerate() {
        if index > 0 {
            f.write_str(",")?;
        }
        write!(f, " {}: ", entry.path)?;
        match &entry.pattern {
            MatchPattern::Value(value) => write!(f, "{value}")?,
            MatchPattern::Nested(block) => write_matches(f, block)?,
            MatchPattern::Predicate(predicate) => f.write_str(match predicate {
                MatchPredicate::NotEmpty => "not-empty",
                MatchPredicate::Null => "null",
                MatchPredicate::Empty => "empty",
            })?,
        }
    }
    if !block.entries.is_empty() {
        f.write_str(" ")?;
    }
    f.write_str("}")
}

fn builtin_name(builtin: PureBuiltin) -> &'static str {
    use PureBuiltin as B;
    match builtin {
        B::Jq => "jq",
        B::U32Le => "u32le",
        B::U64Le => "u64le",
        B::Range => "range",
        B::Zip => "zip",
        B::PathExists => "path-exists",
        B::Contains => "contains",
        B::Null => "null",
        B::Present => "present",
        B::Missing => "missing",
        B::Empty => "empty",
    }
}

impl fmt::Display for BinaryOperator {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        use BinaryOperator as O;
        f.write_str(match self {
            O::Add => "+",
            O::Subtract => "-",
            O::Multiply => "*",
            O::Divide => "/",
            O::Remainder => "%",
            O::Equal => "==",
            O::NotEqual => "!=",
            O::Less => "<",
            O::LessEqual => "<=",
            O::Greater => ">",
            O::GreaterEqual => ">=",
        })
    }
}
