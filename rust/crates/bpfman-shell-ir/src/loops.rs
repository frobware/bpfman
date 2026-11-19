//! Foreach binding data and control-flow verification.

use crate::{Block, BlockId, Terminator, VerifyError};

/// The block whose foreach terminator owns an iterator.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LoopId(pub BlockId);

/// Nonempty foreach bindings. Multiple names destructure the iteration value.
#[derive(Debug, Eq, PartialEq)]
pub struct LoopBindings(Vec<String>);

impl LoopBindings {
    /// Construct bindings in source order, with `_` denoting a discard slot.
    /// Returns `None` for an empty list or a destructure with only discards.
    #[must_use]
    pub fn new(names: Vec<String>) -> Option<Self> {
        if names.is_empty() || (names.len() > 1 && names.iter().all(|name| name == "_")) {
            return None;
        }
        Some(Self(names))
    }

    /// Binding slots in order, including explicit discards.
    #[must_use]
    pub fn names(&self) -> &[String] {
        &self.0
    }
}

/// Trace events attached to explicit loop control.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LoopTrace {
    /// A break statement.
    Break,
    /// A continue statement.
    Continue,
}

pub(super) fn targets(blocks: &[Block], owner: LoopId) -> Result<(BlockId, BlockId), VerifyError> {
    match blocks.get(owner.0.0).map(|block| &block.terminator.node) {
        Some(Terminator::ForEach { body, exit, .. }) => Ok((*body, *exit)),
        _ => Err(VerifyError::InvalidLoopOwner(owner)),
    }
}

// Verify reachable iterator stacks. Go emits unreachable continuation blocks
// after explicit loop exits. Their references still pass structural validation.
pub(super) fn verify_context(blocks: &[Block]) -> Result<(), VerifyError> {
    let mut contexts = vec![None; blocks.len()];
    let mut pending = Vec::new();
    enqueue(BlockId(0), &[], &mut contexts, &mut pending)?;
    while let Some(id) = pending.pop() {
        let mut stack = contexts[id.0].clone().unwrap_or_default();
        let mut follow =
            |target, stack: &[LoopId]| enqueue(target, stack, &mut contexts, &mut pending);
        match blocks[id.0].terminator.node {
            Terminator::Jump(target) => follow(target, &stack)?,
            Terminator::Branch {
                on_true, on_false, ..
            } => {
                follow(on_true, &stack)?;
                follow(on_false, &stack)?;
            }
            Terminator::ForEach { body, exit, .. } => {
                follow(exit, &stack)?;
                stack.push(LoopId(id));
                follow(body, &stack)?;
            }
            Terminator::ForEachContinue(owner) | Terminator::ExitLoop(owner) => {
                if stack.last() != Some(&owner) {
                    return Err(VerifyError::InvalidLoopContext(id));
                }
                let (body, exit) = targets(blocks, owner)?;
                if matches!(blocks[id.0].terminator.node, Terminator::ForEachContinue(_)) {
                    follow(body, &stack)?;
                }
                stack.pop();
                follow(exit, &stack)?;
            }
            Terminator::Stop if !stack.is_empty() => {
                return Err(VerifyError::InvalidLoopContext(id));
            }
            Terminator::Stop => {}
        }
    }
    Ok(())
}

fn enqueue(
    id: BlockId,
    stack: &[LoopId],
    contexts: &mut [Option<Vec<LoopId>>],
    pending: &mut Vec<BlockId>,
) -> Result<(), VerifyError> {
    let known = &mut contexts[id.0];
    if let Some(known) = known {
        if known != stack {
            return Err(VerifyError::InvalidLoopContext(id));
        }
    } else {
        *known = Some(stack.to_vec());
        pending.push(id);
    }
    Ok(())
}
