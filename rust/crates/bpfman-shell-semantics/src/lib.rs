//! Closed origin and structural-shape vocabulary shared by checker and runtime.
//!
//! Only fields whose JSON projection is established here are sealed. Program
//! and link payloads remain open until checked against Rust's actual output.

use std::collections::BTreeMap;
use std::fmt;

use bpfman_shell_syntax::{Command, Expression, ExpressionKind, PureBuiltin, parse_go_duration};

/// Validate the literal value of a `kill` flag. Computed arguments remain
/// runtime checked. A definition named `kill` must be resolved before calling.
#[must_use]
pub fn kill_flag_issue(argument: &Expression) -> Option<String> {
    let text = match argument.kind() {
        ExpressionKind::Word(text) | ExpressionKind::Quoted(text) => text.as_str(),
        _ => return None,
    };
    if let Some(name) = text.strip_prefix("--signal=") {
        let normalized = name.trim().to_ascii_uppercase();
        let normalized = normalized.strip_prefix("SIG").unwrap_or(&normalized);
        if !matches!(
            normalized,
            "TERM" | "KILL" | "INT" | "QUIT" | "HUP" | "USR1" | "USR2" | "STOP" | "CONT"
        ) {
            return Some(format!("kill --signal: unknown signal {name:?}"));
        }
    } else if let Some(duration) = text.strip_prefix("--grace=") {
        match parse_go_duration(duration) {
            None => return Some(format!("kill --grace: time: invalid duration {duration:?}")),
            Some(nanos) if nanos < 0 => {
                return Some(format!(
                    "kill --grace: duration must not be negative (got {duration})"
                ));
            }
            Some(_) => {}
        }
    }
    None
}

/// Static positional contract for a native command. The driver validates the
/// complete command, including any dynamic flags, when it executes.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CommandArity {
    /// No positional arguments.
    Zero,
    /// Exactly one positional argument.
    One,
    /// At least one positional argument.
    AtLeastOne,
}

/// Return the native job-control command's positional contract, when known.
/// A caller must resolve user definitions before applying this rule.
#[must_use]
pub fn command_arity(name: &str) -> Option<CommandArity> {
    Some(match name {
        "start" | "fire" => CommandArity::AtLeastOne,
        "wait" | "kill" => CommandArity::One,
        "jobs" | "reap" => CommandArity::Zero,
        _ => return None,
    })
}

/// Check the narrow literal contract of a pure expression builtin. Dynamic
/// arguments are checked when the builtin executes, not inferred here.
#[must_use]
pub fn pure_literal_issue(builtin: PureBuiltin, argument: &Expression) -> Option<String> {
    let (name, max, kind) = match builtin {
        PureBuiltin::U32Le => ("u32le", u64::from(u32::MAX), LiteralKind::Unsigned(32)),
        PureBuiltin::U64Le => ("u64le", u64::MAX, LiteralKind::Unsigned(64)),
        PureBuiltin::Range => ("range", i32::MAX as u64, LiteralKind::RangeBound),
        _ => return None,
    };
    let text = match argument.kind() {
        ExpressionKind::Word(text) | ExpressionKind::Quoted(text) => text.clone(),
        ExpressionKind::Negate(inner) => match inner.kind() {
            ExpressionKind::Word(text) | ExpressionKind::Quoted(text) => format!("-{text}"),
            _ => return None,
        },
        _ => return None,
    };
    let text = text.trim();
    if text.is_empty() {
        return Some(format!("{name}: empty argument"));
    }
    if text.starts_with('-') {
        return Some(match kind {
            LiteralKind::Unsigned(_) => {
                format!("{name}: negative values are not representable (got {text:?})")
            }
            LiteralKind::RangeBound => {
                format!("{name}: negative bound is not allowed (got {text:?})")
            }
        });
    }
    let Some(value) = parse_go_unsigned(text) else {
        return Some(format!("{name}: invalid integer {text:?}"));
    };
    if value > max {
        return Some(match kind {
            LiteralKind::Unsigned(bits) => {
                format!("{name}: value {value} does not fit in {bits} bits")
            }
            LiteralKind::RangeBound => {
                format!("{name}: bound {value} exceeds the maximum of {max}")
            }
        });
    }
    None
}

#[derive(Clone, Copy)]
enum LiteralKind {
    Unsigned(u8),
    RangeBound,
}

// Go's strconv.ParseUint(text, 0, 64): base prefixes and legacy leading-zero
// octal syntax are part of the current shell's literal contract.
fn parse_go_unsigned(text: &str) -> Option<u64> {
    if text.starts_with('+') {
        return None;
    }
    let unsigned = text;
    let (radix, digits, prefixed) = if let Some(rest) = unsigned
        .strip_prefix("0x")
        .or_else(|| unsigned.strip_prefix("0X"))
    {
        (16, rest, true)
    } else if let Some(rest) = unsigned
        .strip_prefix("0b")
        .or_else(|| unsigned.strip_prefix("0B"))
    {
        (2, rest, true)
    } else if let Some(rest) = unsigned
        .strip_prefix("0o")
        .or_else(|| unsigned.strip_prefix("0O"))
    {
        (8, rest, true)
    } else if unsigned.starts_with('0') && unsigned.len() > 1 {
        (8, &unsigned[1..], true)
    } else {
        (10, unsigned, false)
    };
    if digits.is_empty() {
        return if unsigned == "0" { Some(0) } else { None };
    }
    let bytes = digits.as_bytes();
    let mut cleaned = String::with_capacity(digits.len());
    for (index, byte) in bytes.iter().enumerate() {
        if *byte == b'_' {
            if !(prefixed && index == 0 || index > 0 && bytes[index - 1] != b'_')
                || bytes
                    .get(index + 1)
                    .is_none_or(|next| !next.is_ascii_hexdigit())
            {
                return None;
            }
        } else {
            char::from(*byte).to_digit(radix)?;
            cleaned.push(char::from(*byte));
        }
    }
    u64::from_str_radix(&cleaned, radix).ok()
}

/// The declared origin of a shell value.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum OriginKind {
    /// No origin can be proved.
    Unknown,
    /// One renderable scalar token.
    Scalar,
    /// Boolean value.
    Bool,
    /// Explicit JSON null.
    Null,
    /// Captured command result.
    Result,
    /// Background job handle.
    Job,
    /// Open map or object.
    Map,
    /// Bpfman program.
    Program,
    /// Bpfman link.
    Link,
    /// Dispatcher snapshot.
    Dispatcher,
    /// Network pair handle.
    NetPair,
    /// Kernel-function lease.
    Kfunc,
    /// Symmetric namespace pair.
    NetnsVethPair,
    /// One side of a namespace pair.
    NetnsVethEndpoint,
    /// Kernel link information.
    LinkInfo,
    /// Kernel program information.
    ProgInfo,
    /// LSM probe handle.
    Lsm,
}

impl OriginKind {
    /// User-facing kind label used in diagnostics.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Unknown => "unknown",
            Self::Scalar => "scalar",
            Self::Bool => "boolean",
            Self::Null => "null",
            Self::Result => "result",
            Self::Job => "job",
            Self::Map => "map",
            Self::Program => "program",
            Self::Link => "link",
            Self::Dispatcher => "dispatcher",
            Self::NetPair => "net pair",
            Self::Kfunc => "kernel function",
            Self::NetnsVethPair => "netns-veth-pair",
            Self::NetnsVethEndpoint => "netns-veth-pair endpoint",
            Self::LinkInfo => "link info",
            Self::ProgInfo => "program info",
            Self::Lsm => "lsm probe",
        }
    }
}

impl fmt::Display for OriginKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.label())
    }
}

/// Structural information proved at check time. Each variant states exactly
/// what a field or index lookup can establish.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Shape {
    /// Field names are unknown and must stay runtime checked.
    Open(OriginKind),
    /// A sealed leaf has no fields.
    Leaf(OriginKind),
    /// A sealed record contains exactly these fields.
    Record {
        /// Declared origin.
        origin: OriginKind,
        /// Valid fields and their child shapes.
        fields: BTreeMap<String, Shape>,
    },
    /// An open collection with a proved element shape after indexing.
    List {
        /// Declared origin.
        origin: OriginKind,
        /// Shape of an indexed element.
        element: Box<Shape>,
    },
}

/// Result of asking a shape for one field.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum FieldLookup<'a> {
    /// The field has a proved child shape.
    Found(&'a Shape),
    /// The parent is open, so no conclusion is possible.
    Open,
    /// The parent has no fields.
    Leaf(OriginKind),
    /// The field is absent from a sealed record.
    Missing {
        /// Parent origin.
        origin: OriginKind,
        /// Valid field names, sorted by map order.
        valid: Vec<&'a str>,
    },
}

impl Shape {
    /// Open unknown shape used when inference cannot prove more.
    #[must_use]
    pub fn unknown() -> Self {
        Self::Open(OriginKind::Unknown)
    }

    /// Origin carried by this shape.
    #[must_use]
    pub fn origin(&self) -> OriginKind {
        match self {
            Self::Open(origin)
            | Self::Leaf(origin)
            | Self::Record { origin, .. }
            | Self::List { origin, .. } => *origin,
        }
    }

    /// Look up a field, leaving open shapes undecided.
    #[must_use]
    pub fn field(&self, name: &str) -> FieldLookup<'_> {
        match self {
            Self::Open(_) | Self::List { .. } => FieldLookup::Open,
            Self::Leaf(origin) => FieldLookup::Leaf(*origin),
            Self::Record { origin, fields } => match fields.get(name) {
                Some(child) => FieldLookup::Found(child),
                None => FieldLookup::Missing {
                    origin: *origin,
                    valid: fields.keys().map(String::as_str).collect(),
                },
            },
        }
    }

    /// Descend through an index. Non-list parents become unknown, mirroring
    /// Go's conservative static policy for unproved indexing.
    #[must_use]
    pub fn index(&self) -> Self {
        match self {
            Self::List { element, .. } => (**element).clone(),
            _ => Self::unknown(),
        }
    }

    /// Construct a sealed record from named child shapes.
    #[must_use]
    pub fn record(origin: OriginKind, fields: impl IntoIterator<Item = (String, Shape)>) -> Self {
        Self::Record {
            origin,
            fields: fields.into_iter().collect(),
        }
    }

    /// Construct a list with a known element shape.
    #[must_use]
    pub fn list(element: Shape) -> Self {
        Self::List {
            origin: OriginKind::Unknown,
            element: Box::new(element),
        }
    }
}

fn scalar_fields(origin: OriginKind, names: &[(&str, OriginKind)]) -> Shape {
    Shape::record(
        origin,
        names
            .iter()
            .map(|(name, kind)| ((*name).to_owned(), kind_shape(*kind))),
    )
}

/// Return a shape for a known origin. Program and link stay open until their
/// schemas are tied to Rust's actual JSON projection.
#[must_use]
pub fn kind_shape(kind: OriginKind) -> Shape {
    use OriginKind as K;
    match kind {
        K::Scalar | K::Bool | K::Null => Shape::Leaf(kind),
        K::Result => scalar_fields(
            kind,
            &[
                ("ok", K::Bool),
                ("exit_code", K::Scalar),
                ("stdout", K::Scalar),
                ("stderr", K::Scalar),
                ("killed", K::Bool),
                ("signal", K::Scalar),
                ("pid", K::Scalar),
            ],
        ),
        K::Job => scalar_fields(kind, &[("pid", K::Scalar), ("target_binary", K::Scalar)]),
        K::NetPair => scalar_fields(
            kind,
            &[
                ("ns", K::Scalar),
                ("host_link", K::Scalar),
                ("peer_link", K::Scalar),
                ("host_addr", K::Scalar),
                ("peer_addr", K::Scalar),
                ("host_ifindex", K::Scalar),
                ("host_nsid", K::Scalar),
            ],
        ),
        K::Kfunc => scalar_fields(
            kind,
            &[
                ("index", K::Scalar),
                ("name", K::Scalar),
                ("trigger", K::Scalar),
                ("count", K::Scalar),
            ],
        ),
        K::NetnsVethEndpoint => scalar_fields(
            kind,
            &[
                ("ns", K::Scalar),
                ("link", K::Scalar),
                ("addr", K::Scalar),
                ("ifindex", K::Scalar),
                ("nsid", K::Scalar),
            ],
        ),
        K::NetnsVethPair => Shape::record(
            kind,
            [
                ("a".to_owned(), kind_shape(K::NetnsVethEndpoint)),
                ("b".to_owned(), kind_shape(K::NetnsVethEndpoint)),
            ],
        ),
        K::LinkInfo => scalar_fields(
            kind,
            &[
                ("id", K::Scalar),
                ("prog_id", K::Scalar),
                ("type", K::Scalar),
            ],
        ),
        K::ProgInfo => scalar_fields(
            kind,
            &[
                ("id", K::Scalar),
                ("type", K::Scalar),
                ("name", K::Scalar),
                ("tag", K::Scalar),
            ],
        ),
        K::Lsm => scalar_fields(
            kind,
            &[
                ("marker", K::Scalar),
                ("marker_hex", K::Scalar),
                ("file", K::Scalar),
                ("dir", K::Scalar),
            ],
        ),
        K::Unknown | K::Map | K::Program | K::Link | K::Dispatcher => Shape::Open(kind),
    }
}

/// The declared return shape of one pure expression builtin.
#[must_use]
pub fn pure_shape(builtin: PureBuiltin) -> Shape {
    match builtin {
        PureBuiltin::Jq => Shape::unknown(),
        PureBuiltin::U32Le | PureBuiltin::U64Le => kind_shape(OriginKind::Scalar),
        PureBuiltin::Range => Shape::list(kind_shape(OriginKind::Scalar)),
        PureBuiltin::Zip => Shape::list(Shape::unknown()),
        PureBuiltin::PathExists
        | PureBuiltin::Contains
        | PureBuiltin::Null
        | PureBuiltin::Present
        | PureBuiltin::Missing
        | PureBuiltin::Empty => kind_shape(OriginKind::Bool),
    }
}

/// Infer the primary value of a `<-` command, before the `let` outcome wrapper.
/// Unknown dynamic heads remain open; unrecognized literal heads are external
/// commands and return a result envelope.
#[must_use]
pub fn bind_primary_shape(command: &Command) -> Shape {
    let ExpressionKind::Word(head) = command.head().kind() else {
        return Shape::unknown();
    };
    let first = command
        .arguments()
        .first()
        .and_then(|arg| match arg.kind() {
            ExpressionKind::Word(word) => Some(word.as_str()),
            _ => None,
        });
    use OriginKind as K;
    match head.as_str() {
        "start" | "fire" => kind_shape(K::Job),
        "exec" | "wait" | "kill" => kind_shape(K::Result),
        "linkinfo" => kind_shape(K::LinkInfo),
        "proginfo" => kind_shape(K::ProgInfo),
        "registry" => kind_shape(K::Scalar),
        "net" => match first {
            Some("veth-pair") => kind_shape(K::NetPair),
            Some("netns-veth-pair") => kind_shape(K::NetnsVethPair),
            Some("start") => kind_shape(K::Job),
            Some("release" | "exec") => kind_shape(K::Result),
            _ => Shape::unknown(),
        },
        "kfunc" => match first {
            Some("acquire") => kind_shape(K::Kfunc),
            Some("release" | "fire") => kind_shape(K::Result),
            _ => Shape::unknown(),
        },
        "lsm" => match first {
            Some("probe") => kind_shape(K::Lsm),
            Some("fire") => kind_shape(K::Result),
            _ => Shape::unknown(),
        },
        "bpfman" | "file" | "process" | "tempdir" | "uprobe-target" => Shape::unknown(),
        "uprobe" => match first {
            Some("fire") => kind_shape(K::Result),
            _ => Shape::unknown(),
        },
        _ => PureBuiltin::from_name(head).map_or_else(|| kind_shape(K::Result), pure_shape),
    }
}

/// The outcome visible from a `let NAME <- COMMAND` bind.
#[must_use]
pub fn outcome_shape(primary: Shape) -> Shape {
    let Shape::Record { origin, mut fields } = kind_shape(OriginKind::Result) else {
        unreachable!()
    };
    if primary.origin() != OriginKind::Result {
        fields.insert("value".to_owned(), primary);
    }
    Shape::Record { origin, fields }
}

/// Aggregate shape of a bind-collect loop, including indexed result shapes.
#[must_use]
pub fn collect_shape(primary: Shape, guard: bool) -> Shape {
    let values = Shape::list(primary.clone());
    if guard {
        return values;
    }
    Shape::record(
        OriginKind::Result,
        [
            ("ok".to_owned(), kind_shape(OriginKind::Bool)),
            ("results".to_owned(), Shape::list(outcome_shape(primary))),
            ("values".to_owned(), values),
        ],
    )
}

#[cfg(test)]
mod tests {
    use bpfman_shell_syntax::{StatementKind, parse};

    use super::{FieldLookup, OriginKind, Shape, bind_primary_shape, kind_shape, outcome_shape};

    #[test]
    fn sealed_intrinsics_and_open_domain_shapes_are_distinct() {
        assert!(matches!(
            kind_shape(OriginKind::Job).field("pid"),
            FieldLookup::Found(_)
        ));
        assert!(matches!(
            kind_shape(OriginKind::Job).field("missing"),
            FieldLookup::Missing { .. }
        ));
        assert!(matches!(
            kind_shape(OriginKind::Scalar).field("pid"),
            FieldLookup::Leaf(OriginKind::Scalar)
        ));
        assert!(matches!(
            kind_shape(OriginKind::Program).field("anything"),
            FieldLookup::Open
        ));
    }

    #[test]
    fn bind_and_outcome_shapes_follow_go_wrapping() {
        let program = parse("shape.bpfman", "guard j <- start sleep 1\nlet r <- echo hi")
            .unwrap_or_else(|_| unreachable!());
        let StatementKind::Bind { command: job, .. } = program.statements()[0].kind() else {
            unreachable!()
        };
        let StatementKind::Bind {
            command: external, ..
        } = program.statements()[1].kind()
        else {
            unreachable!()
        };
        assert_eq!(bind_primary_shape(job).origin(), OriginKind::Job);
        assert_eq!(bind_primary_shape(external).origin(), OriginKind::Result);
        assert!(matches!(
            outcome_shape(bind_primary_shape(job)).field("value"),
            FieldLookup::Found(Shape::Record { .. })
        ));
        assert!(matches!(
            outcome_shape(bind_primary_shape(external)).field("value"),
            FieldLookup::Missing { .. }
        ));
    }
}
