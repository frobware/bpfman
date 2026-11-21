use super::{FailureFor, LoadEffects, Published, RuntimeWriter};
use crate::load_error::Failure;

/// Published programs retain their cleanup authority until the store commits.
pub(in crate::load) struct PublishedBatch<F: LoadEffects> {
    first: Published<F>,
    remaining: Vec<Published<F>>,
}

impl<F: LoadEffects> PublishedBatch<F> {
    pub(in crate::load) fn new(first: Published<F>) -> Self {
        Self {
            first,
            remaining: Vec::new(),
        }
    }

    pub(in crate::load) fn push(&mut self, program: Published<F>) {
        self.remaining.push(program);
    }

    pub(in crate::load) fn committed(self) {
        for program in std::iter::once(self.first).chain(self.remaining) {
            program.committed();
        }
    }

    pub(in crate::load) fn commit_failed(
        mut self,
        writer: &RuntimeWriter<'_>,
        effects: &mut F,
        cause: F::Error,
    ) -> FailureFor<F> {
        match self.remaining.pop() {
            Some(last) => {
                let primary = last.abort(writer, effects, cause);
                self.abort(writer, effects, primary)
            }
            None => self.first.abort(writer, effects, cause),
        }
    }

    pub(in crate::load) fn abort(
        self,
        writer: &RuntimeWriter<'_>,
        effects: &mut F,
        primary: FailureFor<F>,
    ) -> FailureFor<F> {
        let previous = self
            .remaining
            .into_iter()
            .rev()
            .chain(std::iter::once(self.first))
            .map(|program| {
                let cause = effects.batch_aborted();
                program.abort(writer, effects, cause)
            })
            .collect();
        Failure::Batch {
            primary: Box::new(primary),
            previous,
        }
    }
}
