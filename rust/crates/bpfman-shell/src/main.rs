//! Rust bpfman-shell command-line driver.

use std::{
    collections::BTreeMap,
    fs,
    io::{self, Read, Write},
    path::{Path, PathBuf},
    process::ExitCode,
};

use bpfman_shell_check::{ExpandedProgram, check, expand_imports, prepare_imports};
use bpfman_shell_lower::{lower, lower_unchecked};
use bpfman_shell_syntax::parse;
use clap::Parser;

mod driver;
mod fixturemode;

#[derive(Parser)]
#[command(version, about, max_term_width = 80)]
struct Options {
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
    /// Script paths. With no paths or a single dash, read stdin.
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
    let options = Options::parse();
    match run(options, &mut io::stdout().lock()) {
        Ok(status) => ExitCode::from(status),
        Err(error) => {
            eprintln!("{error}");
            ExitCode::FAILURE
        }
    }
}

fn run(options: Options, stdout: &mut impl Write) -> Result<u8, String> {
    if let Some(path) = options.directory {
        std::env::set_current_dir(&path).map_err(|error| format!("{}: {error}", path.display()))?;
    }
    let cwd = std::env::current_dir().map_err(|error| error.to_string())?;
    let scripts = if options.scripts.is_empty() {
        vec![PathBuf::from("-")]
    } else {
        options.scripts
    };
    let mut status = 0;
    let mut registry = driver::ImageRegistry::default();
    for path in scripts {
        let program = gather(&cwd, &path)?;
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
                continue;
            }
            lower(&checked)
        }
        .map_err(|error| error.to_string())?;

        if options.lowered {
            write!(stdout, "{ir}").map_err(|error| error.to_string())?;
        } else {
            status = status.max(driver::run(&ir, stdout, &mut registry)?);
        }
    }
    Ok(status)
}

fn gather(cwd: &Path, path: &Path) -> Result<ExpandedProgram, String> {
    let (name, source, base) = if path == Path::new("-") {
        let mut source = String::new();
        io::stdin()
            .lock()
            .read_to_string(&mut source)
            .map_err(|error| format!("<stdin>: {error}"))?;
        ("<stdin>".to_owned(), source, cwd.to_path_buf())
    } else {
        let full = cwd.join(path);
        let source =
            fs::read_to_string(&full).map_err(|error| format!("{}: {error}", path.display()))?;
        let base = full.parent().unwrap_or(cwd).to_path_buf();
        (path.display().to_string(), source, base)
    };
    let parsed = parse(name, &source).map_err(|error| error.to_string())?;
    let plan = prepare_imports(parsed, &base).map_err(|error| error.to_string())?;
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
