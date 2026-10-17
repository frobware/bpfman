//! Kubernetes label selection over arbitrary stored program metadata.
//!
//! Grammar and matching follow the vendored k8s.io/apimachinery labels parser
//! used by Go. In particular, negative comparisons match missing keys, empty
//! values are valid, and numeric comparisons parse signed 64-bit integers.

use alloc::{collections::BTreeMap, string::String, vec, vec::Vec};
use core::{fmt, str::FromStr};

/// A validated, conjunctive Kubernetes-style selector over program metadata.
/// An empty selector imposes no restriction, including on unmanaged programs.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct MetadataSelector(Vec<Requirement>);

#[derive(Clone, Debug, Eq, PartialEq)]
struct Requirement {
    key: String,
    predicate: Predicate,
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum Predicate {
    In(Vec<String>),
    NotIn(Vec<String>),
    Exists,
    Absent,
    Greater(i64),
    Less(i64),
}

/// Invalid Kubernetes label-selector syntax, key, value or numeric operand.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct InvalidMetadataSelector(&'static str);

impl fmt::Display for InvalidMetadataSelector {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "invalid label selector: {}", self.0)
    }
}

impl core::error::Error for InvalidMetadataSelector {}

impl MetadataSelector {
    pub(super) fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub(super) fn matches(&self, metadata: &BTreeMap<String, String>) -> bool {
        self.0.iter().all(|requirement| {
            let value = metadata.get(&requirement.key);
            match &requirement.predicate {
                Predicate::In(values) => value.is_some_and(|v| values.contains(v)),
                Predicate::NotIn(values) => value.is_none_or(|v| !values.contains(v)),
                Predicate::Exists => value.is_some(),
                Predicate::Absent => value.is_none(),
                Predicate::Greater(n) => value
                    .and_then(|v| v.parse::<i64>().ok())
                    .is_some_and(|v| v > *n),
                Predicate::Less(n) => value
                    .and_then(|v| v.parse::<i64>().ok())
                    .is_some_and(|v| v < *n),
            }
        })
    }
}

impl FromStr for MetadataSelector {
    type Err = InvalidMetadataSelector;

    fn from_str(input: &str) -> Result<Self, Self::Err> {
        let mut parser = Parser(input.trim());
        let mut requirements = Vec::new();
        if parser.peek().is_none() {
            return Ok(Self::default());
        }

        loop {
            requirements.push(parser.requirement()?);
            match parser.next() {
                None => return Ok(Self(requirements)),
                Some(Token::Comma) if parser.peek().is_some() => {}
                _ => return Err(InvalidMetadataSelector("expected ',' or end of selector")),
            }
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Token<'a> {
    Identifier(&'a str),
    Equals,
    NotEquals,
    Not,
    Greater,
    Less,
    Open,
    Close,
    Comma,
}

#[derive(Clone, Copy)]
struct Parser<'a>(&'a str);

impl<'a> Parser<'a> {
    fn peek(self) -> Option<Token<'a>> {
        let mut copy = self;
        copy.next()
    }

    fn next(&mut self) -> Option<Token<'a>> {
        self.0 = self.0.trim_start_matches([' ', '\t', '\r', '\n']);
        let first = *self.0.as_bytes().first()?;
        let (token, size) = match first {
            b'=' if self.0.starts_with("==") => (Token::Equals, 2),
            b'!' if self.0.starts_with("!=") => (Token::NotEquals, 2),
            b'=' => (Token::Equals, 1),
            b'!' => (Token::Not, 1),
            b'>' => (Token::Greater, 1),
            b'<' => (Token::Less, 1),
            b'(' => (Token::Open, 1),
            b')' => (Token::Close, 1),
            b',' => (Token::Comma, 1),
            _ => {
                let end = self
                    .0
                    .find([' ', '\t', '\r', '\n', '=', '!', '>', '<', '(', ')', ','])
                    .unwrap_or(self.0.len());
                (Token::Identifier(&self.0[..end]), end)
            }
        };
        self.0 = &self.0[size..];
        Some(token)
    }

    fn requirement(&mut self) -> Result<Requirement, InvalidMetadataSelector> {
        let absent = self.peek() == Some(Token::Not);
        if absent {
            self.next();
        }
        let Some(Token::Identifier(key)) = self.next() else {
            return Err(InvalidMetadataSelector("expected label key"));
        };
        validate_key(key)?;

        // Unary absence consumes only the key. A following binary operator is
        // rejected by the outer requirement separator check, as it is in Go.
        let predicate = if absent {
            Predicate::Absent
        } else if matches!(self.peek(), None | Some(Token::Comma)) {
            Predicate::Exists
        } else {
            match self.next() {
                Some(Token::Identifier("in")) => Predicate::In(self.values()?),
                Some(Token::Identifier("notin")) => Predicate::NotIn(self.values()?),
                Some(Token::Equals) => Predicate::In(vec![self.exact_value()?.into()]),
                Some(Token::NotEquals) => Predicate::NotIn(vec![self.exact_value()?.into()]),
                Some(operator @ (Token::Greater | Token::Less)) => {
                    let n = self.exact_value()?.parse::<i64>().map_err(|_| {
                        InvalidMetadataSelector(
                            "numeric comparison requires a signed 64-bit integer",
                        )
                    })?;
                    if operator == Token::Greater {
                        Predicate::Greater(n)
                    } else {
                        Predicate::Less(n)
                    }
                }
                _ => {
                    return Err(InvalidMetadataSelector(
                        "expected label comparison operator",
                    ));
                }
            }
        };
        Ok(Requirement {
            key: key.into(),
            predicate,
        })
    }

    fn exact_value(&mut self) -> Result<&'a str, InvalidMetadataSelector> {
        let value = match self.peek() {
            None | Some(Token::Comma) => "",
            Some(Token::Identifier(value)) => {
                self.next();
                value
            }
            _ => return Err(InvalidMetadataSelector("expected label value")),
        };
        validate_value(value)?;
        Ok(value)
    }

    fn values(&mut self) -> Result<Vec<String>, InvalidMetadataSelector> {
        if self.next() != Some(Token::Open) {
            return Err(InvalidMetadataSelector("expected '(' after set operator"));
        }
        let mut values = Vec::new();
        loop {
            match self.next() {
                Some(Token::Identifier(value)) => {
                    validate_value(value)?;
                    values.push(value.into());
                    match self.next() {
                        Some(Token::Close) => return Ok(values),
                        Some(Token::Comma) => {}
                        _ => return Err(InvalidMetadataSelector("expected ',' or ')' in set")),
                    }
                }
                Some(Token::Comma) => values.push(String::new()),
                Some(Token::Close) => {
                    values.push(String::new());
                    return Ok(values);
                }
                _ => {
                    return Err(InvalidMetadataSelector(
                        "expected label value or ')' in set",
                    ));
                }
            }
        }
    }
}

fn label_name(value: &str) -> bool {
    let bytes = value.as_bytes();
    bytes.first().is_some_and(u8::is_ascii_alphanumeric)
        && bytes.last().is_some_and(u8::is_ascii_alphanumeric)
        && bytes
            .iter()
            .all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(b))
}

fn validate_key(key: &str) -> Result<(), InvalidMetadataSelector> {
    let name = if let Some((prefix, name)) = key.split_once('/') {
        let valid_prefix = prefix.len() <= 253
            && prefix.split('.').all(|part| {
                let alphanumeric = |b: &u8| b.is_ascii_lowercase() || b.is_ascii_digit();
                part.as_bytes().first().is_some_and(alphanumeric)
                    && part.as_bytes().last().is_some_and(alphanumeric)
                    && part
                        .bytes()
                        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
            });
        if !valid_prefix {
            return Err(InvalidMetadataSelector("invalid DNS prefix in label key"));
        }
        name
    } else {
        key
    };
    if name.len() > 63 || !label_name(name) {
        return Err(InvalidMetadataSelector("invalid label key name"));
    }
    Ok(())
}

fn validate_value(value: &str) -> Result<(), InvalidMetadataSelector> {
    if value.len() > 63 || (!value.is_empty() && !label_name(value)) {
        return Err(InvalidMetadataSelector("invalid label value"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use super::*;
    use alloc::format;

    #[test]
    fn kubernetes_operators_and_absent_keys() {
        let metadata = BTreeMap::from([
            ("app".into(), "demo".into()),
            ("empty".into(), String::new()),
            ("version".into(), "010".into()),
            ("signed".into(), "-2".into()),
            ("example.com/My_Name".into(), "Value.1".into()),
            ("in".into(), "notin".into()),
        ]);
        for (raw, expected) in [
            ("", true),
            (" \t\r\n", true),
            ("app=demo", true),
            ("app==demo", true),
            ("app!=demo", false),
            ("app!=other", true),
            ("missing!=demo", true),
            ("missing=", false),
            ("empty=", true),
            ("empty!=", false),
            ("app", true),
            ("!app", false),
            ("missing", false),
            ("!missing", true),
            ("app in (other,demo)", true),
            ("app notin (demo)", false),
            ("missing notin (demo)", true),
            ("missing in (demo)", false),
            ("empty in ()", true),
            ("empty in (,demo,)", true),
            ("empty in (demo,,other)", true),
            ("empty notin ()", false),
            ("app in (demo,demo)", true),
            ("app=demo,app=other", false),
            ("version>9,version<11", true),
            ("version>10", false),
            ("version<10", false),
            ("app>1", false),
            ("missing>1", false),
            ("signed<0", true),
            ("version>9223372036854775807", false),
            ("example.com/My_Name = Value.1", true),
            ("in=notin", true),
            ("in in (in,notin)", true),
            (" app in ( other , demo ),\n!missing, empty == \t", true),
        ] {
            let selector: MetadataSelector = raw.parse().expect(raw);
            assert_eq!(selector.matches(&metadata), expected, "{raw}");
        }
    }

    #[test]
    fn malformed_syntax_and_invalid_labels_are_rejected() {
        for raw in [
            ",",
            "app,",
            ",app",
            "app,,other",
            "!",
            "!!app",
            "!app=demo",
            "app other",
            "app=demo other",
            "app===demo",
            "app!==demo",
            "app<>1",
            "app IN (demo)",
            "app in demo",
            "app in (demo",
            "app in (demo other)",
            "app in ((demo))",
            "app in (demo))",
            "app in (!demo)",
            "app in (demo=)",
            "app in ()other",
            "app=>",
            "app>",
            "app>abc",
            "app>1.1",
            "app>9223372036854775808",
            "app<-1",
            "app>+1",
            "app>=1",
            "app=a/b",
            "app=é",
            "app=-demo",
            "app=demo_",
            "app=a\0b",
            "/app",
            "example.com/",
            "Example.com/app",
            "example..com/app",
            "-example.com/app",
            "example.com-/app",
            "example.com/a/b",
            "_app",
            "app_",
            "é",
            "app\u{000b}=demo",
        ] {
            assert!(raw.parse::<MetadataSelector>().is_err(), "{raw}");
        }
        for raw in [
            format!("{}=ok", "a".repeat(64)),
            format!("app={}", "a".repeat(64)),
            format!("{}/app", "a".repeat(254)),
        ] {
            assert!(raw.parse::<MetadataSelector>().is_err(), "{raw}");
        }
        // DNS subdomain validation limits the entire prefix, not each component.
        for raw in [
            format!("{}={}", "a".repeat(63), "b".repeat(63)),
            format!("{}/app", "a".repeat(253)),
        ] {
            assert!(raw.parse::<MetadataSelector>().is_ok(), "{raw}");
        }
    }
}
