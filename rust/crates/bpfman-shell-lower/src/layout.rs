//! Reserve branch joins independently of their final dump order.

use bpfman_shell_ir::{Block, BlockId, Instruction, Located, Temp, Terminator};
use bpfman_shell_source::Span;

use crate::LowerError;

#[derive(Clone, Copy)]
pub(super) struct Label(usize);

pub(super) enum Exit {
    Stop,
    Jump(Label),
    Branch {
        condition: Temp,
        on_true: Label,
        on_false: Label,
    },
}

struct Draft {
    span: Span,
    instructions: Vec<Located<Instruction>>,
    exit: Option<Located<Exit>>,
    id: Option<BlockId>,
}

pub(super) struct Layout {
    blocks: Vec<Draft>,
    order: Vec<Label>,
    current: Label,
}

impl Layout {
    pub(super) fn new(span: &Span) -> Self {
        let mut layout = Self {
            blocks: Vec::new(),
            order: Vec::new(),
            current: Label(0),
        };
        layout.current = layout.new_block(span);
        layout
    }

    pub(super) fn reserve(&mut self, span: &Span) -> Label {
        let label = Label(self.blocks.len());
        self.blocks.push(Draft {
            span: span.clone(),
            instructions: Vec::new(),
            exit: None,
            id: None,
        });
        label
    }

    pub(super) fn publish(&mut self, label: Label) {
        self.blocks[label.0].id = Some(BlockId(self.order.len()));
        self.order.push(label);
    }

    pub(super) fn new_block(&mut self, span: &Span) -> Label {
        let label = self.reserve(span);
        self.publish(label);
        label
    }

    pub(super) fn select(&mut self, label: Label) {
        self.current = label;
    }

    pub(super) fn emit(&mut self, instruction: Located<Instruction>) {
        self.blocks[self.current.0].instructions.push(instruction);
    }

    pub(super) fn terminate(&mut self, exit: Located<Exit>) {
        self.blocks[self.current.0].exit = Some(exit);
    }

    pub(super) fn finish(mut self) -> Result<Vec<Block>, LowerError> {
        let ids: Vec<_> = self
            .blocks
            .iter()
            .map(|block| {
                block
                    .id
                    .ok_or_else(|| LowerError::IncompleteLayout(block.span.clone()))
            })
            .collect::<Result<_, _>>()?;
        self.order
            .into_iter()
            .map(|label| {
                let block = &mut self.blocks[label.0];
                let invalid = || LowerError::IncompleteLayout(block.span.clone());
                let exit = block.exit.take().ok_or_else(invalid)?;
                let resolve = |label: Label| ids[label.0];
                let node = match exit.node {
                    Exit::Stop => Terminator::Stop,
                    Exit::Jump(label) => Terminator::Jump(resolve(label)),
                    Exit::Branch {
                        condition,
                        on_true,
                        on_false,
                    } => Terminator::Branch {
                        condition,
                        on_true: resolve(on_true),
                        on_false: resolve(on_false),
                    },
                };
                Ok(Block {
                    span: block.span.clone(),
                    instructions: std::mem::take(&mut block.instructions),
                    terminator: Located {
                        node,
                        span: exit.span,
                    },
                })
            })
            .collect()
    }
}
