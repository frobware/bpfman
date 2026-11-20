//! Rust bpfman-shell command-line driver.

use std::{
    collections::BTreeMap,
    fs,
    io::{self, Write},
    path::{Path, PathBuf},
    process::ExitCode,
};

use bpfman_shell_check::{ExpandedProgram, check, expand_imports, prepare_imports};
use bpfman_shell_lower::{lower, lower_unchecked};
use bpfman_shell_source::Span;
use bpfman_shell_syntax::{ParsedProgram, dump_ast, format_source, parse};
use clap::Parser;

mod driver;
mod fixturemode;
mod scripts;
mod symbols;
mod version;

#[derive(Parser)]
#[command(about, max_term_width = 80)]
struct Options {
    /// Print build and compiler information.
    #[arg(short = 'V', long)]
    version: bool,
    /// Change directory before reading scripts or executing commands.
    #[arg(short = 'C', long)]
    directory: Option<PathBuf>,
    /// Parse and run static preflight without executing.
    #[arg(short, long, conflicts_with_all = ["no_check", "lowered"])]
    check: bool,
    /// Lower without static preflight.
    #[arg(long)]
    no_check: bool,
    /// Print the lowered IR without executing.
    #[arg(long)]
    lowered: bool,
    /// Print the whole program's parsed AST without executing.
    #[arg(long, conflicts_with_all = ["check", "no_check", "lowered", "symbols"])]
    ast: bool,
    /// Print canonical source without expanding imports or executing.
    #[arg(long, conflicts_with_all = ["check", "no_check", "lowered", "symbols", "ast"])]
    fmt: bool,
    /// Write formatted source back to the input file.
    #[arg(short = 'w', long, requires = "fmt")]
    write: bool,
    /// Print the parsed program's editor symbol table as JSON.
    #[arg(long, conflicts_with_all = ["check", "no_check", "lowered"])]
    symbols: bool,
    /// Trace resolved statements and control flow to stderr.
    #[arg(short = 'x', long)]
    trace: bool,
    /// List files with matching script metadata in the given paths.
    #[arg(long, conflicts_with_all = ["check", "no_check", "lowered", "symbols", "ast", "fmt"])]
    list_scripts: bool,
    /// Filter listed files by a Kubernetes label selector.
    #[arg(long, default_value = "")]
    selector: String,
    /// Script path. With no path or a single dash, read stdin.
    scripts: Vec<PathBuf>,
}

fn main() -> ExitCode {
    if let Some(mode) = std::env::var_os("BPFMAN_SHELL_MODE").filter(|mode| !mode.is_empty()) {
        let args = std::env::args_os()
            .skip(1)
            .map(|argument| {
                argument
                    .into_string()
                    .map_err(|_| "fixture arguments must be UTF-8".to_owned())
            })
            .collect::<Result<Vec<_>, _>>();
        return match args.and_then(|args| fixturemode::run(&mode, &args)) {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => {
                eprintln!("bpfman-shell: {error}");
                ExitCode::FAILURE
            }
        };
    }
    let options = Options::parse_from(std::env::args_os().enumerate().map(|(index, argument)| {
        if index == 1 && argument == "fmt" {
            "--fmt".into()
        } else {
            argument
        }
    }));
    let stdout = io::stdout();
    let result = if options.version {
        version::write(&mut stdout.lock())
            .map(|()| 0)
            .map_err(|error| error.to_string())
    } else {
        driver::Interrupt::register()
            .map_err(|error| error.to_string())
            .and_then(|interrupt| run(options, &mut stdout.lock(), &interrupt))
    };
    match result {
        Ok(status) => ExitCode::from(status),
        Err(error) => {
            eprintln!("{error}");
            ExitCode::FAILURE
        }
    }
}

fn run(
    options: Options,
    stdout: &mut impl Write,
    interrupt: &driver::Interrupt,
) -> Result<u8, String> {
    interrupt.check()?;
    if let Some(path) = options.directory {
        std::env::set_current_dir(&path).map_err(|error| format!("{}: {error}", path.display()))?;
    }
    if options.list_scripts {
        scripts::list(&options.scripts, &options.selector, stdout)?;
        return Ok(0);
    }
    let cwd = std::env::current_dir().map_err(|error| error.to_string())?;
    let path = match options.scripts.as_slice() {
        [] => Path::new("-"),
        [path] => path.as_path(),
        _ => return Err(
            "expected at most one script to run; use --list-scripts to inspect multiple scripts"
                .to_owned(),
        ),
    };
    if options.write && path == Path::new("-") {
        return Err("fmt --write requires a script file".to_owned());
    }
    let (name, source, base) = read_source(&cwd, path, interrupt)?;
    if (options.symbols || options.ast || options.fmt) && source.trim().is_empty() {
        return Ok(0);
    }
    let parsed = parse(name, &source).map_err(|error| error.to_string())?;
    if options.fmt {
        let formatted = format_source(&source, &parsed).map_err(|error| error.to_string())?;
        if options.write {
            if formatted != source {
                fs::write(cwd.join(path), formatted)
                    .map_err(|error| format!("{}: {error}", path.display()))?;
            }
        } else {
            stdout
                .write_all(formatted.as_bytes())
                .map_err(|error| error.to_string())?;
        }
        return Ok(0);
    }
    if options.symbols {
        symbols::write(
            &parsed,
            (path != Path::new("-")).then_some(base.as_path()),
            stdout,
        )?;
        return Ok(0);
    }
    if options.ast {
        let start = parsed.span().start().clone();
        let span = parsed
            .statements()
            .last()
            .and_then(|statement| Span::new(start, statement.span().end().clone()).ok());
        let program = gather(parsed, &base)?;
        stdout
            .write_all(dump_ast(program.statements(), span.as_ref()).as_bytes())
            .map_err(|error| error.to_string())?;
        return Ok(0);
    }
    let program = gather(parsed, &base)?;
    let ir = if options.no_check {
        lower_unchecked(&program)
    } else {
        let checked = check(program).map_err(|issues| {
            issues
                .iter()
                .map(|issue| format!("{}:{issue}", issue.span.start().name().as_str()))
                .collect::<Vec<_>>()
                .join("\n")
        })?;
        if options.check {
            return Ok(0);
        }
        lower(&checked)
    }
    .map_err(|error| error.to_string())?;

    if options.lowered {
        write!(stdout, "{ir}").map_err(|error| error.to_string())?;
        Ok(0)
    } else {
        driver::run(
            &ir,
            stdout,
            &mut driver::ImageRegistry::default(),
            options.trace,
            interrupt,
        )
    }
}

fn read_source(
    cwd: &Path,
    path: &Path,
    interrupt: &driver::Interrupt,
) -> Result<(String, String, PathBuf), String> {
    if path == Path::new("-") {
        let stdin = rustix::io::dup(io::stdin()).map_err(|error| format!("<stdin>: {error}"))?;
        let source = interrupt
            .read_program(fs::File::from(stdin))
            .map_err(|error| format!("<stdin>: {error}"))?;
        Ok(("<stdin>".to_owned(), source, cwd.to_path_buf()))
    } else {
        let full = cwd.join(path);
        let file = fs::File::open(&full).map_err(|error| format!("{}: {error}", path.display()))?;
        let source = interrupt
            .read_program(file)
            .map_err(|error| format!("{}: {error}", path.display()))?;
        let base = full.parent().unwrap_or(cwd).to_path_buf();
        Ok((path.display().to_string(), source, base))
    }
}

fn gather(parsed: ParsedProgram, base: &Path) -> Result<ExpandedProgram, String> {
    let plan = prepare_imports(parsed, base).map_err(|error| error.to_string())?;
    let mut gathered = BTreeMap::new();
    for request in plan.requests() {
        if !gathered.contains_key(request.path()) {
            let source = fs::read_to_string(request.path())
                .map_err(|error| format!("{}: {error}", request.path().display()))?;
            gathered.insert(request.path().to_owned(), source);
        }
    }
    expand_imports(plan, &gathered).map_err(|error| error.to_string())
}
