use std::{collections::BTreeMap, fmt};

use crate::{
    BinaryOperator, Command, Expression, ExpressionKind, Instruction, Program, Terminator,
};

impl fmt::Display for Program {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "body entry=bb{}", self.entry().0)?;
        for (id, block) in self.blocks().iter().enumerate() {
            let mut heads = BTreeMap::new();
            writeln!(f, "\nbb{id}:")?;
            for instruction in &block.instructions {
                if matches!(
                    instruction.node,
                    Instruction::TraceBranch(_) | Instruction::TraceLoop(_)
                ) {
                    continue;
                }
                f.write_str("  ")?;
                match &instruction.node {
                    Instruction::TraceBranch(_) | Instruction::TraceLoop(_) => {}
                    Instruction::EnterIfFrame => writeln!(f, "EnterFrame kind=if-branch")?,
                    Instruction::ExitFrame => writeln!(f, "ExitFrame")?,
                    Instruction::EnterProgramDefers => {
                        writeln!(f, "EnterDeferScope kind=program")?;
                    }
                    Instruction::RunProgramDefers => writeln!(f, "RunDefers policy=program")?,
                    Instruction::Eval { dst, expression } => {
                        writeln!(f, "Eval t{} = {expression}", dst.0)?;
                    }
                    Instruction::BindName { name, src } => {
                        writeln!(f, "BindName {name} = t{}", src.0)?;
                    }
                    Instruction::EmitResult { src } => writeln!(f, "EmitResult t{}", src.0)?,
                    Instruction::BuildArgs { dst, command } => {
                        heads.insert(dst.0, lane(command));
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
                Terminator::Stop => writeln!(f, "  Stop")?,
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
                Terminator::ExitLoop(_) => writeln!(f, "  ExitLoop")?,
            }
        }
        Ok(())
    }
}

fn lane(command: &Command) -> String {
    let head = match &command.head.node {
        ExpressionKind::Word(head) | ExpressionKind::Quoted(head) => head.as_str(),
        _ => return "unresolved-head".to_owned(),
    };
    // This is the Go IR dumper's command vocabulary.
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
            ExpressionKind::Variable { name, path } => {
                write!(f, "${name}")?;
                if !path.is_empty() {
                    write!(f, ".{path}")?;
                }
                Ok(())
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
    if matches!(expression.node, ExpressionKind::Binary { .. }) {
        write!(f, "({expression})")
    } else {
        write!(f, "{expression}")
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
