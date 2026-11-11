//! Pure direct-import planning and expansion. The caller gathers file bytes.

use std::collections::{BTreeMap, HashMap};
use std::ffi::OsString;
use std::fmt;
use std::path::{Component, Path, PathBuf};

use bpfman_shell_source::{SourceName, Span};
use bpfman_shell_syntax::{
    Command, ExpressionKind, ParsedProgram, Statement, StatementKind, SyntaxError, parse,
};

use crate::analyze_scopes;

/// A direct library path the caller must gather before expansion.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ImportRequest {
    path: PathBuf,
    span: Span,
    statement_index: usize,
}

impl ImportRequest {
    /// Resolved, lexically cleaned path to gather.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Source extent of the literal import path.
    #[must_use]
    pub fn span(&self) -> &Span {
        &self.span
    }
}

/// A parsed main program whose direct imports have been validated and planned.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ImportPlan {
    main: ParsedProgram,
    requests: Vec<ImportRequest>,
}

impl ImportPlan {
    /// Requests in source order. Duplicate paths retain distinct call sites.
    #[must_use]
    pub fn requests(&self) -> &[ImportRequest] {
        &self.requests
    }
}

/// A program with direct imports replaced by their parsed library definitions.
/// This state has not passed the full semantic checker.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExpandedProgram {
    source: SourceName,
    span: Span,
    statements: Vec<Statement>,
}

impl ExpandedProgram {
    /// Identity of the main source unit.
    #[must_use]
    pub fn source(&self) -> &SourceName {
        &self.source
    }

    /// Extent of the main source unit.
    #[must_use]
    pub fn span(&self) -> &Span {
        &self.span
    }

    /// Expanded statements; imported definitions retain their own source spans.
    #[must_use]
    pub fn statements(&self) -> &[Statement] {
        &self.statements
    }
}

/// A located import planning, gathering, parsing, or library-validation failure.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ImportError {
    span: Span,
    message: String,
}

impl ImportError {
    fn new(span: &Span, message: impl Into<String>) -> Self {
        Self {
            span: span.clone(),
            message: message.into(),
        }
    }

    /// Source extent of the failed form.
    #[must_use]
    pub fn span(&self) -> &Span {
        &self.span
    }

    /// Diagnostic without location formatting.
    #[must_use]
    pub fn message(&self) -> &str {
        &self.message
    }
}

impl fmt::Display for ImportError {
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

impl std::error::Error for ImportError {}

impl From<SyntaxError> for ImportError {
    fn from(error: SyntaxError) -> Self {
        Self::new(error.span(), error.message())
    }
}

/// Validate all import sites and return paths to gather at the driver edge.
/// Relative main-file imports use that file's directory; stdin uses `base_dir`.
pub fn prepare_imports(main: ParsedProgram, base_dir: &Path) -> Result<ImportPlan, ImportError> {
    let mut requests = Vec::new();
    for (index, statement) in main.statements().iter().enumerate() {
        visit_imports(
            statement,
            0,
            index,
            main.source().as_str(),
            base_dir,
            &mut requests,
        )?;
    }
    Ok(ImportPlan { main, requests })
}

/// Parse supplied library text and splice definitions in source order.
///
/// `gathered` is keyed by the exact paths returned from `ImportPlan::requests`.
/// It is read only; this function never accesses the filesystem. Missing bytes,
/// invalid library syntax, non-definition statements, and duplicate visible
/// definitions stop expansion before an expanded program is produced.
pub fn expand_imports(
    plan: ImportPlan,
    gathered: &BTreeMap<PathBuf, String>,
) -> Result<ExpandedProgram, ImportError> {
    let mut definitions = HashMap::<String, Span>::new();
    for statement in plan.main.statements() {
        if let StatementKind::Definition(definition) = statement.kind() {
            definitions
                .entry(definition.name().text().to_owned())
                .or_insert_with(|| definition.name().span().clone());
        }
    }
    let mut out = Vec::new();
    let mut requests = plan.requests.iter().peekable();
    for (index, statement) in plan.main.statements().iter().enumerate() {
        let Some(request) = requests
            .peek()
            .filter(|request| request.statement_index == index)
        else {
            out.push(statement.clone());
            continue;
        };
        let request = *request;
        requests.next();
        let Some(text) = gathered.get(&request.path) else {
            return Err(ImportError::new(
                &request.span,
                format!("import source was not gathered: {}", request.path.display()),
            ));
        };
        let library = parse(request.path.to_string_lossy().into_owned(), text)?;
        for statement in library.statements() {
            if !matches!(statement.kind(), StatementKind::Definition(_)) {
                return Err(ImportError::new(
                    statement.span(),
                    "imported files may contain only top-level def statements",
                ));
            }
        }
        let mut local_definitions = HashMap::<String, Span>::new();
        for statement in library.statements() {
            let StatementKind::Definition(definition) = statement.kind() else {
                unreachable!()
            };
            let name = definition.name();
            if let Some(previous) = definitions
                .get(name.text())
                .or_else(|| local_definitions.get(name.text()))
            {
                return Err(ImportError::new(
                    name.span(),
                    format!(
                        "duplicate top-level def {:?}; previous declaration at {}:{}:{}",
                        name.text(),
                        previous.start().name().as_str(),
                        previous.start().line(),
                        previous.start().column()
                    ),
                ));
            }
            local_definitions.insert(name.text().to_owned(), name.span().clone());
        }
        if let Some(issue) = analyze_scopes(&library).into_iter().next() {
            return Err(ImportError::new(&issue.span, issue.message));
        }
        definitions.extend(local_definitions);
        for statement in library.statements() {
            out.push(statement.clone());
        }
    }
    Ok(ExpandedProgram {
        source: plan.main.source().clone(),
        span: plan.main.span().clone(),
        statements: out,
    })
}

fn visit_imports(
    statement: &Statement,
    depth: usize,
    index: usize,
    file: &str,
    base_dir: &Path,
    requests: &mut Vec<ImportRequest>,
) -> Result<(), ImportError> {
    if let StatementKind::Command(command) = statement.kind() {
        if let Some((path, span)) = import_path(command)? {
            if depth != 0 {
                return Err(ImportError::new(
                    &span,
                    "import must be declared at top level",
                ));
            }
            requests.push(ImportRequest {
                path: resolve(file, base_dir, &path),
                span,
                statement_index: index,
            });
        }
    }
    match statement.kind() {
        StatementKind::If {
            first,
            elif,
            else_body,
        } => {
            visit_body(first.body(), depth + 1, index, file, base_dir, requests)?;
            for branch in elif {
                visit_body(branch.body(), depth + 1, index, file, base_dir, requests)?;
            }
            if let Some(body) = else_body {
                visit_body(body, depth + 1, index, file, base_dir, requests)?;
            }
        }
        StatementKind::Foreach { body, .. }
        | StatementKind::BindCollect { body, .. }
        | StatementKind::Poll { body, .. } => {
            visit_body(body, depth + 1, index, file, base_dir, requests)?;
        }
        StatementKind::Definition(definition) => {
            visit_body(
                definition.body(),
                depth + 1,
                index,
                file,
                base_dir,
                requests,
            )?;
        }
        _ => {}
    }
    Ok(())
}

fn visit_body(
    body: &[Statement],
    depth: usize,
    index: usize,
    file: &str,
    base_dir: &Path,
    requests: &mut Vec<ImportRequest>,
) -> Result<(), ImportError> {
    for statement in body {
        visit_imports(statement, depth, index, file, base_dir, requests)?;
    }
    Ok(())
}

fn import_path(command: &Command) -> Result<Option<(String, Span)>, ImportError> {
    if !matches!(command.head().kind(), ExpressionKind::Word(head) if head == "import") {
        return Ok(None);
    }
    let [argument] = command.arguments() else {
        return Err(ImportError::new(
            command.span(),
            "import requires exactly one file argument",
        ));
    };
    match argument.kind() {
        ExpressionKind::Word(path) | ExpressionKind::Quoted(path) => {
            Ok(Some((path.clone(), argument.span().clone())))
        }
        _ => Err(ImportError::new(
            command.span(),
            "import path must be a literal file argument",
        )),
    }
}

fn resolve(file: &str, base_dir: &Path, path: &str) -> PathBuf {
    let import = Path::new(path);
    if import.is_absolute() {
        return clean(import);
    }
    let base = if matches!(file, "" | "-" | "<stdin>") {
        base_dir
    } else {
        Path::new(file).parent().unwrap_or(Path::new(""))
    };
    clean(&base.join(import))
}

fn clean(path: &Path) -> PathBuf {
    let absolute = path.is_absolute();
    let mut prefix = PathBuf::new();
    let mut parts: Vec<OsString> = Vec::new();
    for component in path.components() {
        match component {
            Component::Prefix(value) => prefix.push(value.as_os_str()),
            Component::RootDir | Component::CurDir => {}
            Component::Normal(value) => parts.push(value.to_os_string()),
            Component::ParentDir => {
                if parts.last().is_some_and(|part| part != "..") {
                    parts.pop();
                } else if !absolute {
                    parts.push(OsString::from(".."));
                }
            }
        }
    }
    let mut result = prefix;
    if absolute {
        result.push(Path::new("/"));
    }
    for part in parts {
        result.push(part);
    }
    if result.as_os_str().is_empty() {
        result.push(".");
    }
    result
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::path::{Path, PathBuf};

    use bpfman_shell_syntax::{StatementKind, parse};

    use super::{expand_imports, prepare_imports};
    use crate::analyze_expanded_scopes;

    #[test]
    fn direct_imports_are_planned_then_spliced_without_io() {
        let main = parse(
            "/tmp/sub/main.bpfman",
            "import ../lib.bpfman\nprint before\nimport ../other.bpfman\nprint after",
        )
        .unwrap_or_else(|_| unreachable!());
        let plan = prepare_imports(main, Path::new("/unused")).unwrap_or_else(|_| unreachable!());
        assert_eq!(
            plan.requests()
                .iter()
                .map(|request| request.path())
                .collect::<Vec<_>>(),
            [Path::new("/tmp/lib.bpfman"), Path::new("/tmp/other.bpfman")]
        );
        let gathered = BTreeMap::from([
            (
                PathBuf::from("/tmp/lib.bpfman"),
                "def one() { print one }".to_owned(),
            ),
            (
                PathBuf::from("/tmp/other.bpfman"),
                "def two() { one }".to_owned(),
            ),
        ]);
        let expanded = expand_imports(plan, &gathered).unwrap_or_else(|_| unreachable!());
        assert_eq!(expanded.statements().len(), 4);
        assert!(matches!(
            expanded.statements()[0].kind(),
            StatementKind::Definition(_)
        ));
        assert!(matches!(
            expanded.statements()[1].kind(),
            StatementKind::Command(_)
        ));
        assert_eq!(
            expanded.statements()[0].span().start().name().as_str(),
            "/tmp/lib.bpfman"
        );
        assert_eq!(expanded.source().as_str(), "/tmp/sub/main.bpfman");
        assert!(analyze_expanded_scopes(&expanded).is_empty());
    }

    #[test]
    fn stdin_uses_explicit_base_and_duplicate_paths_keep_call_sites() {
        let main = parse("<stdin>", "import lib.bpfman\nimport lib.bpfman")
            .unwrap_or_else(|_| unreachable!());
        let plan = prepare_imports(main, Path::new("/work")).unwrap_or_else(|_| unreachable!());
        assert_eq!(plan.requests().len(), 2);
        assert_eq!(plan.requests()[0].path(), Path::new("/work/lib.bpfman"));
        assert_ne!(plan.requests()[0].span(), plan.requests()[1].span());
        let gathered =
            BTreeMap::from([(PathBuf::from("/work/lib.bpfman"), "def d() {}".to_owned())]);
        let error = expand_imports(plan, &gathered)
            .err()
            .unwrap_or_else(|| unreachable!());
        assert!(error.message().contains("duplicate top-level def"));
    }

    #[test]
    fn invalid_imports_fail_before_gathering() {
        for (source, message) in [
            ("import", "exactly one file argument"),
            ("import a b", "exactly one file argument"),
            ("import $path", "literal file argument"),
            ("if true { import lib.bpfman }", "top level"),
            ("def f() { import lib.bpfman }", "top level"),
            (
                "let xs <- foreach x in [1] { import lib.bpfman }",
                "top level",
            ),
        ] {
            let main = parse("main.bpfman", source).unwrap_or_else(|_| unreachable!());
            let error = prepare_imports(main, Path::new("."))
                .err()
                .unwrap_or_else(|| unreachable!());
            assert!(error.message().contains(message), "{source}: {error}");
        }
    }

    #[test]
    fn libraries_must_be_def_only_and_have_valid_scopes() {
        for (text, message) in [
            ("let x = 1", "only top-level def"),
            ("import next.bpfman", "only top-level def"),
            ("def f() { print $missing }", "undefined variable"),
            ("def f() {}\ndef f() {}", "duplicate top-level def"),
        ] {
            let main = parse("main.bpfman", "import lib.bpfman").unwrap_or_else(|_| unreachable!());
            let plan = prepare_imports(main, Path::new(".")).unwrap_or_else(|_| unreachable!());
            let gathered = BTreeMap::from([(PathBuf::from("lib.bpfman"), text.to_owned())]);
            let error = expand_imports(plan, &gathered)
                .err()
                .unwrap_or_else(|| unreachable!());
            assert!(error.message().contains(message), "{text}: {error}");
            assert_eq!(error.span().start().name().as_str(), "lib.bpfman");
        }
    }

    #[test]
    fn missing_source_has_import_location() {
        let main = parse("main.bpfman", "import lib.bpfman").unwrap_or_else(|_| unreachable!());
        let plan = prepare_imports(main, Path::new(".")).unwrap_or_else(|_| unreachable!());
        let error = expand_imports(plan, &BTreeMap::new())
            .err()
            .unwrap_or_else(|| unreachable!());
        assert_eq!(error.span().start().name().as_str(), "main.bpfman");
        assert!(error.message().contains("not gathered"));
    }
}
