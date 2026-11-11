//! Byte-based source coordinates shared by the bpfman-shell language phases.
//!
//! Positions refer to the original source, including comments and UTF-8 bytes.
//! Spans are half-open. No parser or interpreter state belongs in this crate.

use std::num::NonZeroUsize;
use std::sync::Arc;

/// The identity shown in diagnostics for one source unit.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct SourceName(Arc<str>);

impl SourceName {
    /// Name a file or synthetic input such as `<stdin>`.
    #[must_use]
    pub fn new(name: impl Into<Arc<str>>) -> Self {
        Self(name.into())
    }

    /// Return the name exactly as supplied by the caller.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// A one-based line and byte column in one source unit.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct Position {
    name: SourceName,
    line: NonZeroUsize,
    column: NonZeroUsize,
}

impl Position {
    /// The first byte of a named source unit.
    #[must_use]
    pub fn origin(name: SourceName) -> Self {
        Self {
            name,
            line: NonZeroUsize::MIN,
            column: NonZeroUsize::MIN,
        }
    }

    /// Construct a position; zero lines and columns are invalid.
    #[must_use]
    pub fn new(name: SourceName, line: usize, column: usize) -> Option<Self> {
        Some(Self {
            name,
            line: NonZeroUsize::new(line)?,
            column: NonZeroUsize::new(column)?,
        })
    }

    /// Return the source identity.
    #[must_use]
    pub fn name(&self) -> &SourceName {
        &self.name
    }

    /// Return the one-based line number.
    #[must_use]
    pub fn line(&self) -> usize {
        self.line.get()
    }

    /// Return the one-based byte column.
    #[must_use]
    pub fn column(&self) -> usize {
        self.column.get()
    }
}

/// The reason two positions cannot form a half-open span.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InvalidSpan {
    /// The positions belong to different source units.
    DifferentSources,
    /// The exclusive end precedes the start.
    Reversed,
}

/// A half-open range in one source unit.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Span {
    start: Position,
    end: Position,
}

impl Span {
    /// A zero-length span at one valid source position.
    #[must_use]
    pub fn point(at: Position) -> Self {
        Self {
            start: at.clone(),
            end: at,
        }
    }

    /// Form a span with an exclusive end in the same source unit.
    pub fn new(start: Position, end: Position) -> Result<Self, InvalidSpan> {
        if start.name != end.name {
            return Err(InvalidSpan::DifferentSources);
        }
        if (start.line, start.column) > (end.line, end.column) {
            return Err(InvalidSpan::Reversed);
        }
        Ok(Self { start, end })
    }

    /// Return the inclusive start.
    #[must_use]
    pub fn start(&self) -> &Position {
        &self.start
    }

    /// Return the exclusive end.
    #[must_use]
    pub fn end(&self) -> &Position {
        &self.end
    }
}

/// An immutable source whose original byte offsets map to diagnostic positions.
#[derive(Clone, Debug)]
pub struct SourceText<'a> {
    text: &'a str,
    start: Position,
    line_starts: Vec<usize>,
}

impl<'a> SourceText<'a> {
    /// Index the original input, which may begin inside an imported source line.
    #[must_use]
    pub fn new(start: Position, text: &'a str) -> Self {
        let mut line_starts = vec![0];
        for (offset, byte) in text.bytes().enumerate() {
            if byte == b'\n' {
                line_starts.push(offset + 1);
            }
        }
        Self {
            text,
            start,
            line_starts,
        }
    }

    /// Position of the first byte in this source fragment.
    #[must_use]
    pub fn start(&self) -> &Position {
        &self.start
    }

    /// Map a byte offset, including the one-past-end offset, to a position.
    #[must_use]
    pub fn position_at(&self, offset: usize) -> Option<Position> {
        if offset > self.text.len() {
            return None;
        }
        let line_index = self.line_starts.partition_point(|&start| start <= offset) - 1;
        let line = self.start.line.get().checked_add(line_index)?;
        let first_column = if line_index == 0 {
            self.start.column.get()
        } else {
            1
        };
        let column = first_column.checked_add(offset - self.line_starts[line_index])?;
        Position::new(self.start.name.clone(), line, column)
    }

    /// Map a half-open byte range to a validated source span.
    #[must_use]
    pub fn span(&self, start: usize, end: usize) -> Option<Span> {
        Span::new(self.position_at(start)?, self.position_at(end)?).ok()
    }
}

#[cfg(test)]
mod tests {
    use super::{InvalidSpan, Position, SourceName, SourceText, Span};

    fn position(name: &str, line: usize, column: usize) -> Position {
        Position::new(SourceName::new(name), line, column).unwrap_or_else(|| unreachable!())
    }

    #[test]
    fn byte_columns_and_half_open_end_survive_comments_and_utf8() {
        let source = SourceText::new(position("main.bpfman", 7, 4), "aé#x\n$v");
        let first = source.span(0, 5).unwrap_or_else(|| unreachable!());
        assert_eq!((first.start().line(), first.start().column()), (7, 4));
        assert_eq!((first.end().line(), first.end().column()), (7, 9));
        let second = source.span(6, 8).unwrap_or_else(|| unreachable!());
        assert_eq!((second.start().line(), second.start().column()), (8, 1));
        assert_eq!((second.end().line(), second.end().column()), (8, 3));
        assert!(source.position_at(9).is_none());
    }

    #[test]
    fn invalid_spans_cannot_be_constructed() {
        let start = position("main.bpfman", 2, 4);
        assert_eq!(
            Span::new(start.clone(), position("lib.bpfman", 2, 5)),
            Err(InvalidSpan::DifferentSources)
        );
        assert_eq!(
            Span::new(start.clone(), position("main.bpfman", 2, 3)),
            Err(InvalidSpan::Reversed)
        );
        assert!(Span::new(start.clone(), start).is_ok());
        assert!(Position::new(SourceName::new("main.bpfman"), 0, 1).is_none());
        assert!(Position::new(SourceName::new("main.bpfman"), 1, 0).is_none());
    }
}
