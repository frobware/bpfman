use std::time::Duration;

use bpfman_fs::{DEFAULT_RUNTIME_ROOT, RuntimeLayout};
use bpfman_model::ProgramType;
use clap::{
    Args, Parser, Subcommand, ValueEnum,
    builder::{PathBufValueParser, PossibleValuesParser, TypedValueParser},
};

mod load;

#[derive(Parser)]
#[command(name = "bpfman", version, about, max_term_width = 80)]
pub(super) struct Cli {
    /// Root directory for runtime files (must be absolute).
    #[arg(long = "runtime-dir", value_name = "RUNTIME_DIR", global = true, env = "BPFMAN_RUNTIME_DIR", default_value = DEFAULT_RUNTIME_ROOT, value_parser = PathBufValueParser::new().try_map(RuntimeLayout::try_from))]
    pub(super) layout: RuntimeLayout,
    /// Timeout for acquiring the writer lock (0 waits indefinitely).
    #[arg(long, global = true, env = "BPFMAN_LOCK_TIMEOUT", default_value = "30s", value_parser = lock_timeout)]
    pub(super) lock_timeout: Duration,
    #[command(subcommand)]
    pub(super) command: Command,
}

#[derive(Subcommand)]
pub(super) enum Command {
    /// Manage BPF programs (loading currently supports parsing only).
    Program {
        #[command(subcommand)]
        command: ProgramCommand,
    },
}

#[derive(Subcommand)]
pub(super) enum ProgramCommand {
    /// List managed programs from the Go database (text and quiet output).
    ///
    /// This initial reader does not observe the kernel. JSON, --all, and
    /// attachment-state filtering will arrive with kernel observation support.
    /// A missing database is created under the runtime writer lock.
    List(ListArgs),
    /// Parse a load request; execution is not implemented yet.
    Load {
        #[command(subcommand)]
        source: load::LoadCommand,
    },
}

#[derive(Args)]
pub(super) struct ListArgs {
    /// Print only program IDs, one per line.
    #[arg(short, long)]
    pub(super) quiet: bool,
    /// Output format (JSON is not yet implemented).
    #[arg(short, long, value_enum, default_value_t = OutputFormat::Text)]
    pub(super) output: OutputFormat,
    /// Filter by program type; comma-separated or repeated, case-insensitive.
    #[arg(long = "type", visible_alias = "program-type", short = 'p', value_delimiter = ',', ignore_case = true, value_parser = program_type_parser())]
    pub(super) types: Vec<ProgramType>,
    /// Filter by exact bpfman.io/application metadata value.
    #[arg(long)]
    pub(super) application: Option<String>,
}

#[derive(Clone, Copy, ValueEnum)]
pub(super) enum OutputFormat {
    Text,
}

fn program_type_parser() -> impl TypedValueParser<Value = ProgramType> {
    PossibleValuesParser::new(ProgramType::ALL.map(ProgramType::as_str))
        .try_map(|value| value.to_ascii_lowercase().parse::<ProgramType>())
}

fn lock_timeout(value: &str) -> Result<Duration, String> {
    if value == "0" {
        return Ok(Duration::ZERO);
    }
    humantime::parse_duration(value).map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use clap::CommandFactory;

    use super::*;

    #[test]
    fn clap_command_definition_is_consistent() {
        Cli::command().debug_assert();
    }

    #[test]
    fn clap_constructs_the_validated_layout() -> Result<(), clap::Error> {
        let cli = Cli::try_parse_from([
            "bpfman",
            "--runtime-dir",
            "/tmp/unused/../runtime/./",
            "program",
            "list",
        ])?;
        assert_eq!(cli.layout.root(), std::path::Path::new("/tmp/runtime"));
        Ok(())
    }

    #[test]
    fn runtime_parser_preserves_native_paths() -> Result<(), clap::Error> {
        use std::{ffi::OsString, os::unix::ffi::OsStringExt, path::PathBuf};
        let root = OsString::from_vec(b"/tmp/runtime-\xff".to_vec());
        let cli = Cli::try_parse_from([
            OsString::from("bpfman"),
            OsString::from("--runtime-dir"),
            root.clone(),
            OsString::from("program"),
            OsString::from("list"),
        ])?;
        assert_eq!(cli.layout.root(), PathBuf::from(root));
        Ok(())
    }

    #[test]
    fn aliases_and_repeated_values_form_one_typed_selection() -> Result<(), clap::Error> {
        let cli = Cli::try_parse_from([
            "bpfman",
            "program",
            "list",
            "--type",
            "XDP,tc",
            "--program-type",
            "lsm",
            "-p",
            "tcx",
        ])?;
        let Command::Program {
            command: ProgramCommand::List(args),
        } = cli.command
        else {
            return Err(clap::Error::raw(
                clap::error::ErrorKind::InvalidSubcommand,
                "expected list",
            ));
        };
        assert_eq!(
            args.types,
            [
                ProgramType::Xdp,
                ProgramType::Tc,
                ProgramType::Lsm,
                ProgramType::Tcx
            ]
        );
        Ok(())
    }
}
