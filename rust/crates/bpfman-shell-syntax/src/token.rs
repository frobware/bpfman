//! Shell-mode tokenisation. Positions count bytes, including UTF-8 bytes.

use std::error::Error;
use std::fmt;

use bpfman_shell_source::{Position, SourceName, SourceText, Span};

/// A variable name and optional field/index path.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VariableRef {
    /// The original spelling, including `$` and any braces.
    pub source: String,
    /// The ASCII identifier after `$` or `${`.
    pub name: String,
    /// The path without an initial dot, if one was present.
    pub path: String,
}

/// An interpolation's source text, without its `${` and `}` delimiters.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Interpolation {
    /// Expression source for the parser to tokenise in expression context.
    pub inner: String,
    /// Span of the whole `${...}` form.
    pub span: Span,
}

/// One segment of a double-quoted interpolated string.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Segment {
    /// Decoded literal text and its original source span.
    Literal {
        /// Decoded text.
        text: String,
        /// Original byte extent.
        span: Span,
    },
    /// An expression to parse later.
    Expression(Interpolation),
}

/// A shell token. Data is carried only by variants that need it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TokenKind {
    /// Opaque CLI-shaped word or single expression punctuation.
    Word(String),
    /// Standalone assignment operator.
    Assign,
    /// Variable reference.
    Variable(VariableRef),
    /// Literal single- or double-quoted text with delimiters removed.
    Quoted(String),
    /// Known adapter invocation such as `file:$value`.
    Adapter {
        /// Registered adapter name.
        name: String,
        /// Variable passed to the adapter.
        variable: VariableRef,
    },
    /// A newline or semicolon.
    Separator(char),
    /// Value-threading operator `|>`.
    Thread,
    /// Command-result binding operator `<-`.
    Bind,
    /// Double-quoted text containing at least one interpolation.
    Interpolated(Vec<Segment>),
}

/// A token and its original half-open source span.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Token {
    /// Lexical class and its data.
    pub kind: TokenKind,
    /// Position of the token in the unmodified source.
    pub span: Span,
}

/// A lexical error at an original source position.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LexError {
    /// Source location of the invalid token or opening quote.
    pub span: Span,
    /// Explanation without source rendering.
    pub message: String,
}

impl fmt::Display for LexError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}:{}:{}: {}",
            self.span.start().name().as_str(),
            self.span.start().line(),
            self.span.start().column(),
            self.message
        )
    }
}

impl Error for LexError {}

/// Tokenise source starting at line one, byte column one.
pub fn tokenise(input: &str) -> Result<Vec<Token>, LexError> {
    tokenise_at(Position::origin(SourceName::new("")), input)
}

/// Tokenise a source fragment at its absolute source position.
pub fn tokenise_at(start: Position, input: &str) -> Result<Vec<Token>, LexError> {
    let stripped = strip_comments(input);
    let source = SourceText::new(start, &stripped);
    let bytes = stripped.as_bytes();
    let mut tokens = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        if matches!(b, b' ' | b'\t' | b'\r') {
            i += 1;
            continue;
        }
        if b == b'\\' && bytes.get(i + 1) == Some(&b'\n') {
            i += 2;
            continue;
        }
        if b == b'\\' && bytes.get(i + 1) == Some(&b'\r') && bytes.get(i + 2) == Some(&b'\n') {
            i += 3;
            continue;
        }
        let (kind, end) = match b {
            b'\n' | b';' => (TokenKind::Separator(char::from(b)), i + 1),
            b'{' | b'}' | b'(' | b')' | b'[' | b']' | b'+' | b'*' | b'%' => {
                (TokenKind::Word(char::from(b).to_string()), i + 1)
            }
            b'=' if !tokens.is_empty() => {
                if bytes.get(i + 1) == Some(&b'=') {
                    (TokenKind::Word("==".into()), i + 2)
                } else {
                    (TokenKind::Assign, i + 1)
                }
            }
            b'$' => {
                let (variable, end) = lex_variable(&stripped, i)
                    .map_err(|failure| lex_error(&source, i, i + 1, failure))?;
                (TokenKind::Variable(variable), end)
            }
            b'\'' => {
                let end = bytes[i + 1..]
                    .iter()
                    .position(|&c| c == b'\'')
                    .map(|n| i + 1 + n)
                    .ok_or_else(|| {
                        lex_error(
                            &source,
                            i,
                            i,
                            "unterminated single-quoted string (no matching ' before end of input)",
                        )
                    })?;
                (TokenKind::Quoted(stripped[i + 1..end].into()), end + 1)
            }
            b'"' => lex_double_quoted(&stripped, &source, i)?,
            b'|' if bytes.get(i + 1) == Some(&b'>') => (TokenKind::Thread, i + 2),
            b'<' if bytes.get(i + 1) == Some(&b'-') => (TokenKind::Bind, i + 2),
            _ => {
                if let Some((variable, end)) = lex_adapter(&stripped, i) {
                    (
                        TokenKind::Adapter {
                            name: "file".into(),
                            variable,
                        },
                        end,
                    )
                } else {
                    let end = lex_word_end(bytes, i);
                    (TokenKind::Word(stripped[i..end].into()), end)
                }
            }
        };
        let span = source
            .span(i, end)
            .ok_or_else(|| lex_error(&source, i, i, "source span overflow"))?;
        tokens.push(Token { kind, span });
        i = end;
    }
    Ok(tokens)
}

fn lex_error(
    source: &SourceText<'_>,
    start: usize,
    end: usize,
    message: impl Into<String>,
) -> LexError {
    let span = source
        .span(start, end)
        .unwrap_or_else(|| Span::point(source.start().clone()));
    LexError {
        span,
        message: message.into(),
    }
}

fn strip_comments(input: &str) -> String {
    let mut bytes = input.as_bytes().to_vec();
    let mut single = false;
    let mut double = false;
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'\\' if double && i + 1 < bytes.len() => i += 2,
            b'\'' if !double => {
                single = !single;
                i += 1;
            }
            b'"' if !single => {
                double = !double;
                i += 1;
            }
            b'#' if !single && !double => {
                while i < bytes.len() && bytes[i] != b'\n' {
                    bytes[i] = b' ';
                    i += 1;
                }
            }
            _ => i += 1,
        }
    }
    String::from_utf8_lossy(&bytes).into_owned()
}

fn lex_word_end(bytes: &[u8], start: usize) -> usize {
    let mut i = start;
    while i < bytes.len()
        && !matches!(
            bytes[i],
            b' ' | b'\t'
                | b'\r'
                | b'\n'
                | b';'
                | b'$'
                | b'"'
                | b'\''
                | b'#'
                | b'['
                | b']'
                | b'{'
                | b'}'
                | b'('
                | b')'
                | b'+'
                | b'*'
                | b'%'
        )
    {
        i += 1;
    }
    i
}

fn ident_start(b: u8) -> bool {
    b.is_ascii_alphabetic() || b == b'_'
}
fn ident_continue(b: u8) -> bool {
    ident_start(b) || b.is_ascii_digit()
}

fn lex_variable(input: &str, start: usize) -> Result<(VariableRef, usize), String> {
    let bytes = input.as_bytes();
    if start + 1 >= bytes.len() {
        return Err("unexpected end of input after $".into());
    }
    let braced = bytes[start + 1] == b'{';
    let mut i = start + if braced { 2 } else { 1 };
    if braced && (i >= bytes.len() || bytes[i] == b'}') {
        return Err("empty variable reference: ${}".into());
    }
    if i >= bytes.len() || !ident_start(bytes[i]) {
        return Err(if braced {
            "invalid variable reference: expected identifier after ${"
        } else {
            "invalid variable reference: expected identifier after $"
        }
        .into());
    }
    let name_start = i;
    while i < bytes.len() && ident_continue(bytes[i]) {
        i += 1;
    }
    let name = input[name_start..i].to_owned();
    let path_start = i;
    while i < bytes.len() && (!braced || bytes[i] != b'}') {
        match bytes[i] {
            b'.' => {
                i += 1;
                if i >= bytes.len() || !ident_start(bytes[i]) {
                    return Err(format!(
                        "invalid variable reference: expected identifier after '.'{}",
                        if braced { " in ${...}" } else { "" }
                    ));
                }
                while i < bytes.len() && ident_continue(bytes[i]) {
                    i += 1;
                }
            }
            b'[' => i = lex_index(bytes, i, braced)?,
            _ if braced => {
                return Err(format!(
                    "invalid variable reference: unexpected character {:?} in ${{...}} path",
                    char::from(bytes[i])
                ));
            }
            _ => break,
        }
    }
    if braced {
        if i >= bytes.len() {
            return Err("unterminated variable reference: missing }".into());
        }
        i += 1;
    }
    let path = input[path_start..if braced { i - 1 } else { i }]
        .trim_start_matches('.')
        .to_owned();
    Ok((
        VariableRef {
            source: input[start..i].into(),
            name,
            path,
        },
        i,
    ))
}

fn lex_index(bytes: &[u8], start: usize, braced: bool) -> Result<usize, String> {
    let suffix = if braced { " in ${...}" } else { "" };
    let mut i = start + 1;
    if bytes.get(i) == Some(&b'$') {
        i += 1;
        if bytes.get(i).is_none_or(|b| !ident_start(*b)) {
            return Err(format!("expected identifier after '[$'{suffix}"));
        }
        let name_start = i;
        while i < bytes.len() && ident_continue(bytes[i]) {
            i += 1;
        }
        if bytes.get(i) != Some(&b']') {
            let name = String::from_utf8_lossy(&bytes[name_start..i]);
            return Err(format!("expected ']' after '[${name}'{suffix}"));
        }
        return Ok(i + 1);
    }
    let first = i;
    while i < bytes.len() && bytes[i].is_ascii_digit() {
        i += 1;
    }
    if i == first {
        return Err(format!("expected digits or '$ident' inside '[]'{suffix}"));
    }
    if bytes.get(i) != Some(&b']') {
        return Err(format!("expected ']' after index{suffix}"));
    }
    Ok(i + 1)
}

fn lex_adapter(input: &str, start: usize) -> Option<(VariableRef, usize)> {
    let rest = input.get(start..)?;
    if !rest.starts_with("file:$") {
        return None;
    }
    lex_variable(input, start + 5).ok()
}

fn lex_double_quoted(
    input: &str,
    source: &SourceText<'_>,
    start: usize,
) -> Result<(TokenKind, usize), LexError> {
    let bytes = input.as_bytes();
    let mut i = start + 1;
    let mut segments = Vec::new();
    let mut literal = Vec::new();
    let mut literal_start = None;
    while i < bytes.len() {
        match bytes[i] {
            b'"' => {
                flush_literal(&mut segments, &mut literal, &mut literal_start, source, i)?;
                let kind = if segments
                    .iter()
                    .all(|segment| matches!(segment, Segment::Literal { .. }))
                {
                    let text = segments
                        .into_iter()
                        .filter_map(|segment| match segment {
                            Segment::Literal { text, .. } => Some(text),
                            Segment::Expression(_) => None,
                        })
                        .collect::<String>();
                    TokenKind::Quoted(text)
                } else {
                    TokenKind::Interpolated(segments)
                };
                return Ok((kind, i + 1));
            }
            b'\\' => {
                let Some(&escape) = bytes.get(i + 1) else {
                    return Err(lex_error(
                        source,
                        start,
                        start,
                        "unterminated escape sequence at end of string",
                    ));
                };
                let decoded = match escape {
                    b'n' => b'\n',
                    b't' => b'\t',
                    b'r' => b'\r',
                    b'\\' => b'\\',
                    b'"' => b'"',
                    b'$' => b'$',
                    _ => {
                        return Err(lex_error(
                            source,
                            start,
                            start,
                            format!(
                                "unknown escape sequence '\\{}' in double-quoted string",
                                char::from(escape)
                            ),
                        ));
                    }
                };
                literal_start.get_or_insert(i);
                literal.push(decoded);
                i += 2;
            }
            b'$' => {
                if bytes.get(i + 1) != Some(&b'{') {
                    return Err(lex_error(
                        source,
                        start,
                        start,
                        "'$' in double-quoted string must be followed by '{...}' (use single quotes or '\\$' for a literal '$')",
                    ));
                }
                flush_literal(&mut segments, &mut literal, &mut literal_start, source, i)?;
                let end = scan_interpolation(bytes, i + 2)
                    .map_err(|message| lex_error(source, start, start, message))?;
                let inner = &input[i + 2..end];
                if inner.trim().is_empty() {
                    return Err(lex_error(
                        source,
                        start,
                        start,
                        "empty interpolation '${}' in string",
                    ));
                }
                let span = source
                    .span(i, end + 1)
                    .ok_or_else(|| lex_error(source, i, i, "source span overflow"))?;
                segments.push(Segment::Expression(Interpolation {
                    inner: inner.into(),
                    span,
                }));
                i = end + 1;
            }
            _ => {
                literal_start.get_or_insert(i);
                literal.push(bytes[i]);
                i += 1;
            }
        }
    }
    Err(lex_error(
        source,
        start,
        start,
        "unterminated double-quoted string (no matching \" before end of input)",
    ))
}

fn flush_literal(
    segments: &mut Vec<Segment>,
    literal: &mut Vec<u8>,
    start: &mut Option<usize>,
    source: &SourceText<'_>,
    end: usize,
) -> Result<(), LexError> {
    if let Some(at) = start.take() {
        let text = String::from_utf8_lossy(literal).into_owned();
        let span = source
            .span(at, end)
            .ok_or_else(|| lex_error(source, at, at, "source span overflow"))?;
        segments.push(Segment::Literal { text, span });
        literal.clear();
    }
    Ok(())
}

fn scan_interpolation(bytes: &[u8], start: usize) -> Result<usize, &'static str> {
    let mut depth = 1;
    let mut i = start;
    while i < bytes.len() {
        match bytes[i] {
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return Ok(i);
                }
            }
            b'\'' => {
                i += 1;
                while i < bytes.len() && bytes[i] != b'\'' {
                    i += 1;
                }
                if i == bytes.len() {
                    return Err("unterminated single-quoted string inside ${...}");
                }
            }
            b'"' => {
                return Err(
                    "unterminated interpolation in string: missing '}' (use single quotes for strings inside ${...})",
                );
            }
            _ => {}
        }
        i += 1;
    }
    Err("unterminated interpolation in string: missing '}'")
}

#[cfg(test)]
mod tests {
    use super::{Segment, TokenKind, tokenise, tokenise_at};
    use bpfman_shell_source::{Position, SourceName};

    fn kinds(input: &str) -> Vec<TokenKind> {
        tokenise(input)
            .unwrap_or_else(|_| unreachable!())
            .into_iter()
            .map(|token| token.kind)
            .collect()
    }

    #[test]
    fn argv_words_and_boundary_operators_match_go() {
        assert_eq!(
            kinds("load file foo.o -m app=test"),
            vec![
                TokenKind::Word("load".into()),
                TokenKind::Word("file".into()),
                TokenKind::Word("foo.o".into()),
                TokenKind::Word("-m".into()),
                TokenKind::Word("app=test".into()),
            ]
        );
        assert_eq!(
            kinds("a|>b a<-b x=1 1+1 7%3"),
            vec![
                TokenKind::Word("a|>b".into()),
                TokenKind::Word("a<-b".into()),
                TokenKind::Word("x=1".into()),
                TokenKind::Word("1".into()),
                TokenKind::Word("+".into()),
                TokenKind::Word("1".into()),
                TokenKind::Word("7".into()),
                TokenKind::Word("%".into()),
                TokenKind::Word("3".into()),
            ]
        );
        assert_eq!(
            kinds("let r <- $x |> jq .a; r = 1"),
            vec![
                TokenKind::Word("let".into()),
                TokenKind::Word("r".into()),
                TokenKind::Bind,
                TokenKind::Variable(super::VariableRef {
                    source: "$x".into(),
                    name: "x".into(),
                    path: "".into()
                }),
                TokenKind::Thread,
                TokenKind::Word("jq".into()),
                TokenKind::Word(".a".into()),
                TokenKind::Separator(';'),
                TokenKind::Word("r".into()),
                TokenKind::Assign,
                TokenKind::Word("1".into()),
            ]
        );
    }

    #[test]
    fn paths_adapters_quotes_and_interpolations_carry_data() {
        let tokens =
            kinds("$xs.field[$i].name file:${data.items[0]} 'literal $x' \"id=${x.id}; \\$\"");
        assert!(
            matches!(&tokens[0], TokenKind::Variable(v) if v.name == "xs" && v.path == "field[$i].name")
        );
        assert!(
            matches!(&tokens[1], TokenKind::Adapter { name, variable } if name == "file" && variable.name == "data" && variable.path == "items[0]")
        );
        assert_eq!(tokens[2], TokenKind::Quoted("literal $x".into()));
        let TokenKind::Interpolated(segments) = &tokens[3] else {
            unreachable!();
        };
        assert!(matches!(&segments[0], Segment::Literal { text, .. } if text == "id="));
        assert!(matches!(&segments[1], Segment::Expression(expr) if expr.inner == "x.id"));
        assert!(matches!(&segments[2], Segment::Literal { text, .. } if text == "; $"));
    }

    #[test]
    fn byte_spans_survive_comments_continuations_and_absolute_start() {
        let start =
            Position::new(SourceName::new("lib.bpfman"), 7, 4).unwrap_or_else(|| unreachable!());
        let tokens = tokenise_at(start, "é # ignored\n\\\n$x").unwrap_or_else(|_| unreachable!());
        assert_eq!(tokens.len(), 3);
        assert_eq!(
            (
                tokens[0].span.start().line(),
                tokens[0].span.start().column()
            ),
            (7, 4)
        );
        assert_eq!(
            (tokens[0].span.end().line(), tokens[0].span.end().column()),
            (7, 6)
        );
        assert_eq!(
            (
                tokens[2].span.start().line(),
                tokens[2].span.start().column()
            ),
            (9, 1)
        );
        assert_eq!(tokens[2].span.start().name().as_str(), "lib.bpfman");
    }

    #[test]
    fn malformed_references_and_quotes_fail_locally() {
        for (input, message) in [
            ("$", "unexpected end of input after $"),
            ("$123", "expected identifier after $"),
            ("$x.", "expected identifier after '.'"),
            ("$x[]", "expected digits or '$ident'"),
            ("${x[$i}", "expected ']' after '[$i'"),
            ("'unclosed", "unterminated single-quoted string"),
            ("\"$x\"", "must be followed by '{...}'"),
            ("\"${}\"", "empty interpolation"),
        ] {
            let error = tokenise(input).err().unwrap_or_else(|| unreachable!());
            assert!(error.message.contains(message), "{input:?}: {error}");
            assert_eq!(error.span.start().column(), 1, "{input:?}");
        }
    }

    #[test]
    fn escaped_quote_does_not_turn_hash_into_a_comment() {
        let tokens = kinds("let x = \"he said \\\"hi#nope\\\"\"\nprint done");
        assert_eq!(tokens[3], TokenKind::Quoted("he said \"hi#nope\"".into()));
        assert_eq!(tokens[4], TokenKind::Separator('\n'));
        assert_eq!(tokens[5], TokenKind::Word("print".into()));
        assert_eq!(tokens[6], TokenKind::Word("done".into()));
    }

    #[test]
    fn interpolation_skips_braces_inside_single_quotes() {
        let tokens = kinds("\"${$x eq 'a}b'}\"");
        let TokenKind::Interpolated(segments) = &tokens[0] else {
            unreachable!()
        };
        assert!(matches!(&segments[0], Segment::Expression(expr) if expr.inner == "$x eq 'a}b'"));
    }

    #[test]
    fn crlf_continuation_is_whitespace_without_separator() {
        assert_eq!(
            kinds("print \\\r\nnext"),
            vec![
                TokenKind::Word("print".into()),
                TokenKind::Word("next".into()),
            ]
        );
    }
}
