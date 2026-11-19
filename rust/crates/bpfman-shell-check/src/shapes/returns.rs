//! Infer definition bind results in isolated lexical frames.

use bpfman_shell_semantics::{OriginKind, Shape, bind_primary_shape, collect_shape, kind_shape};
use bpfman_shell_syntax::{BindMode, Command, ExpressionKind, Statement, StatementKind};

use super::Analysis;

pub(super) enum Inference {
    Visiting,
    Inferred(Shape),
}

enum Returns {
    None,
    Possible(Shape),
    Guaranteed(Shape),
}

impl Returns {
    fn shape(self) -> Shape {
        match self {
            Self::None => kind_shape(OriginKind::Result),
            Self::Possible(_) => Shape::unknown(),
            Self::Guaranteed(shape) => shape,
        }
    }

    fn optional(self) -> Self {
        match self {
            Self::Guaranteed(shape) => Self::Possible(shape),
            other => other,
        }
    }

    fn join_branches(self, other: Self) -> Self {
        match (self, other) {
            (Self::Guaranteed(left), Self::Guaranteed(right)) => {
                Self::Guaranteed(join_shapes(left, right))
            }
            (Self::None, other) | (other, Self::None) => other.optional(),
            (Self::Possible(left), Self::Possible(right) | Self::Guaranteed(right))
            | (Self::Guaranteed(left), Self::Possible(right)) => {
                Self::Possible(join_shapes(left, right))
            }
        }
    }

    fn then(self, next: Self) -> Self {
        match (self, next) {
            (Self::None, next) => next,
            (Self::Possible(shape), Self::None) => Self::Possible(shape),
            (Self::Possible(left), Self::Possible(right)) => {
                Self::Possible(join_shapes(left, right))
            }
            (Self::Possible(left), Self::Guaranteed(right)) => {
                Self::Guaranteed(join_shapes(left, right))
            }
            (guaranteed @ Self::Guaranteed(_), _) => guaranteed,
        }
    }
}

fn join_shapes(left: Shape, right: Shape) -> Shape {
    if left == right {
        left
    } else {
        Shape::unknown()
    }
}

impl Analysis<'_> {
    pub(super) fn primary_shape(&mut self, command: &Command) -> Shape {
        let ExpressionKind::Word(head) = command.head().kind() else {
            return Shape::unknown();
        };
        let Some(definition) = self.defs.get(head.as_str()).copied() else {
            return bind_primary_shape(command);
        };
        let name = definition.name().text();
        match self.returns.get(name) {
            Some(Inference::Visiting) => return Shape::unknown(),
            Some(Inference::Inferred(shape)) => return shape.clone(),
            None => {}
        }

        self.returns.insert(name, Inference::Visiting);
        let caller = std::mem::take(&mut self.frames);
        let shape = self
            .scoped_returns(
                definition
                    .parameters()
                    .iter()
                    .map(|param| param.name().text().to_owned()),
                definition.body(),
            )
            .shape();
        self.frames = caller;
        self.returns
            .insert(name, Inference::Inferred(shape.clone()));
        shape
    }

    fn scoped_returns(
        &mut self,
        names: impl IntoIterator<Item = String>,
        body: &[Statement],
    ) -> Returns {
        self.push_scope(names);
        let returns = self.block_returns(body);
        self.frames.pop();
        returns
    }

    fn block_returns(&mut self, body: &[Statement]) -> Returns {
        let mut returns = Returns::None;
        for statement in body {
            if matches!(
                statement.kind(),
                StatementKind::Break | StatementKind::Continue
            ) {
                break;
            }
            returns = returns.then(self.statement_returns(statement.kind()));
            if matches!(returns, Returns::Guaranteed(_)) {
                break;
            }
        }
        returns
    }

    fn statement_returns(&mut self, statement: &StatementKind) -> Returns {
        match statement {
            StatementKind::Return(value) => Returns::Guaranteed(self.infer(value)),
            StatementKind::If {
                first,
                elif,
                else_body,
            } => {
                let mut returns = self.scoped_returns([], first.body());
                for branch in elif {
                    returns = returns.join_branches(self.scoped_returns([], branch.body()));
                }
                returns.join_branches(
                    else_body
                        .as_ref()
                        .map_or(Returns::None, |body| self.scoped_returns([], body)),
                )
            }
            StatementKind::Foreach { bindings, body, .. } => self
                .scoped_returns(
                    bindings.names().iter().map(|name| name.text().to_owned()),
                    body,
                )
                .optional(),
            StatementKind::Poll { body, .. } => self.scoped_returns([], body).optional(),
            StatementKind::Let { name, value } => {
                self.define(name.text(), self.infer(value));
                Returns::None
            }
            StatementKind::LetDestructure { names, .. } => {
                for name in names {
                    self.define(name.text(), Shape::unknown());
                }
                Returns::None
            }
            StatementKind::Bind {
                mode,
                name,
                command,
            } => {
                let shape = self.bind_shape(*mode, command);
                self.define(name.text(), shape);
                Returns::None
            }
            StatementKind::BindCollect {
                mode,
                name,
                bindings,
                body,
                ..
            } => {
                let returns = self.scoped_returns(
                    bindings.names().iter().map(|name| name.text().to_owned()),
                    body,
                );
                let primary = self.collect_primary_shape(body);
                self.define(
                    name.text(),
                    collect_shape(primary, *mode == BindMode::Guard),
                );
                returns.optional()
            }
            _ => Returns::None,
        }
    }
}
