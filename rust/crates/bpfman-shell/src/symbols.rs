//! Editor JSON transport for local symbols and visible imported definitions.

use std::{fs, io::Write, path::Path};

use bpfman_shell_check::{ImportDefinitions, direct_imports};
use bpfman_shell_source::{Position, Span};
use bpfman_shell_syntax::{ParsedProgram, StatementKind, SymbolKind, program_scope, symbols};
use serde::Serialize;

#[derive(Serialize)]
struct Document<'a> {
    version: u8,
    file: &'a str,
    symbols: Vec<SymbolEntry>,
    errors: Vec<SymbolError>,
}

#[derive(Serialize)]
struct SymbolEntry {
    name: String,
    kind: &'static str,
    #[serde(rename = "def")]
    definition: Location,
    scope: Scope,
}

#[derive(Serialize)]
struct Location {
    file: String,
    line: usize,
    col: usize,
}

#[derive(Serialize)]
struct Coordinate {
    line: usize,
    col: usize,
}

#[derive(Serialize)]
struct Scope {
    file: String,
    start: Coordinate,
    end: Coordinate,
}

#[derive(Serialize)]
struct SymbolError {
    #[serde(flatten)]
    location: Location,
    message: String,
}

pub(super) fn write(
    program: &ParsedProgram,
    import_base: Option<&Path>,
    output: &mut impl Write,
) -> Result<(), String> {
    let mut document = Document {
        version: 1,
        file: program.source().as_str(),
        symbols: symbols(program)
            .into_iter()
            .map(|symbol| {
                SymbolEntry::new(symbol.name, symbol.kind, &symbol.definition, &symbol.scope)
            })
            .collect(),
        errors: Vec::new(),
    };
    if let Some(base) = import_base {
        document.add_imports(program, base);
    }
    serde_json::to_writer_pretty(&mut *output, &document).map_err(|error| error.to_string())?;
    writeln!(output).map_err(|error| error.to_string())
}

impl Document<'_> {
    fn add_imports(&mut self, program: &ParsedProgram, base: &Path) {
        let mut definitions = ImportDefinitions::new(program);
        let root = program_scope(program);
        for request in direct_imports(program, base) {
            let request = match request {
                Ok(request) => request,
                Err(error) => {
                    self.error(error.span(), error.message());
                    continue;
                }
            };
            let text = match fs::read_to_string(request.path()) {
                Ok(text) => text,
                Err(error) => {
                    self.error(
                        request.span(),
                        format!("read import {}: {error}", request.path().display()),
                    );
                    continue;
                }
            };
            match definitions.add(&request, &text) {
                Ok(library) => {
                    for statement in library.statements() {
                        if let StatementKind::Definition(definition) = statement.kind() {
                            let name = definition.name();
                            self.symbols.push(SymbolEntry::new(
                                name.text().to_owned(),
                                SymbolKind::Definition,
                                name.span().start(),
                                &root,
                            ));
                        }
                    }
                }
                Err(error) => self.error(error.span(), error.message()),
            }
        }
    }

    fn error(&mut self, span: &Span, message: impl Into<String>) {
        self.errors.push(SymbolError {
            location: Location::new(span.start()),
            message: message.into(),
        });
    }
}

impl SymbolEntry {
    fn new(name: String, kind: SymbolKind, definition: &Position, scope: &Span) -> Self {
        Self {
            name,
            kind: kind.as_str(),
            definition: Location::new(definition),
            scope: Scope {
                file: scope.start().name().as_str().to_owned(),
                start: Coordinate::new(scope.start()),
                end: Coordinate::new(scope.end()),
            },
        }
    }
}

impl Location {
    fn new(position: &Position) -> Self {
        Self {
            file: position.name().as_str().to_owned(),
            line: position.line(),
            col: position.column(),
        }
    }
}

impl Coordinate {
    fn new(position: &Position) -> Self {
        Self {
            line: position.line(),
            col: position.column(),
        }
    }
}
