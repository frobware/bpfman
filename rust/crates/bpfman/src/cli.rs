use std::time::Duration;

use bpfman_fs::{DEFAULT_RUNTIME_ROOT, RuntimeLayout};
use bpfman_model::ProgramType;
use clap::{
    Args, Parser, Subcommand, ValueEnum,
    builder::{PathBufValueParser, PossibleValuesParser, TypedValueParser},
};

mod config;
mod dispatcher;
mod image;
mod link;
mod load;
mod registry_auth;

#[derive(Parser)]
#[command(name = "bpfman", version, about, max_term_width = 80)]
pub(super) struct Cli {
    /// bpfman TOML configuration file (defaults to /etc/bpfman/bpfman.toml).
    #[arg(long, global = true, env = "BPFMAN_CONFIG", value_name = "FILE")]
    pub(super) config: Option<std::path::PathBuf>,
    /// Root directory for runtime files (must be absolute).
    #[arg(long = "runtime-dir", value_name = "RUNTIME_DIR", global = true, env = "BPFMAN_RUNTIME_DIR", default_value = DEFAULT_RUNTIME_ROOT, value_parser = PathBufValueParser::new().try_map(RuntimeLayout::try_from))]
    pub(super) layout: RuntimeLayout,
    /// Persistent cache root for OCI bytecode images.
    #[arg(long = "image-cache-dir", global = true, default_value = "/var/cache/bpfman", value_parser = PathBufValueParser::new().try_map(RuntimeLayout::try_from))]
    pub(super) image_cache: RuntimeLayout,
    /// Sigstore TUF cache root (defaults to the user's cache directory).
    #[arg(long = "sigstore-cache-dir", global = true, env = "BPFMAN_SIGSTORE_CACHE_DIR", value_parser = PathBufValueParser::new().try_map(RuntimeLayout::try_from))]
    pub(super) trust_cache: Option<RuntimeLayout>,
    /// Timeout for acquiring the writer lock (0 waits indefinitely).
    #[arg(long, global = true, env = "BPFMAN_LOCK_TIMEOUT", default_value = "30s", value_parser = lock_timeout)]
    pub(super) lock_timeout: Duration,
    /// Write a new Perfetto/Chrome timeline file, filtered by RUST_LOG.
    #[arg(long, global = true, value_name = "PATH")]
    pub(super) trace_file: Option<std::path::PathBuf>,
    #[command(subcommand)]
    pub(super) command: Command,
}

pub(super) enum PreparedCommand {
    Get {
        id: std::num::NonZeroU32,
        output: OutputFormat,
    },
    Unload {
        id: std::num::NonZeroU32,
    },
    List(ListArgs),
    Load(load::PreparedLoad),
    Image(image::ImageCommand),
    Link(link::LinkCommand, bpfman_model::XdpMode),
    Dispatcher(dispatcher::DispatcherCommand),
}

impl Command {
    pub(super) fn prepare(
        self,
        layout: &RuntimeLayout,
        cancellation: &bpfman_runtime::Cancellation,
        config: Option<&std::path::Path>,
        cache: &RuntimeLayout,
        lock_timeout: Duration,
        trust_cache: Option<&RuntimeLayout>,
    ) -> Result<PreparedCommand, crate::error::Error> {
        let command = match self {
            Self::Program { command } => command,
            Self::Image { command } => return Ok(PreparedCommand::Image(command)),
            Self::NsHelper { .. } => {
                return Err(anyhow::anyhow!(
                    "namespace helper must dispatch before runtime initialization"
                )
                .into());
            }
            Self::Dispatcher { command } => return Ok(PreparedCommand::Dispatcher(command)),
            Self::Link { command } => {
                command.validate()?;
                let mode = match command.xdp_interface() {
                    Some(interface) => config::Config::load(config)?.xdp_mode(interface)?,
                    None => Default::default(),
                };
                return Ok(PreparedCommand::Link(command, mode));
            }
        };

        Ok(match command {
            ProgramCommand::Get { id, output } => PreparedCommand::Get { id, output },
            ProgramCommand::Unload { id } => PreparedCommand::Unload { id },
            ProgramCommand::List(args) => PreparedCommand::List(args),
            ProgramCommand::Load { source } => PreparedCommand::Load(source.prepare(
                layout,
                cancellation,
                config,
                cache,
                lock_timeout,
                trust_cache,
            )?),
        })
    }
}

#[derive(Subcommand)]
pub(super) enum Command {
    /// Inspect public OCI bytecode images.
    Image {
        #[command(subcommand)]
        command: image::ImageCommand,
    },
    #[command(name = "__ns-helper", hide = true)]
    NsHelper {
        #[command(subcommand)]
        command: crate::ns_helper::Helper,
    },
    /// Inspect dispatcher snapshots.
    Dispatcher {
        #[command(subcommand)]
        command: dispatcher::DispatcherCommand,
    },
    /// Manage standalone BPF links.
    Link {
        #[command(subcommand)]
        command: link::LinkCommand,
    },
    /// Manage BPF programs.
    Program {
        #[command(subcommand)]
        command: ProgramCommand,
    },
}

#[derive(Subcommand)]
pub(super) enum ProgramCommand {
    /// Get a managed program with kernel, map, and link observations.
    Get {
        /// Managed kernel program ID.
        #[arg(value_name = "PROGRAM_ID")]
        id: std::num::NonZeroU32,
        /// Output format.
        #[arg(short,long,value_enum,default_value_t=OutputFormat::Text)]
        output: OutputFormat,
    },
    /// Unload a tracepoint with its links or an unattached XDP program.
    Unload {
        /// Managed kernel program ID.
        #[arg(value_name = "PROGRAM_ID")]
        id: std::num::NonZeroU32,
    },
    /// List programs; --all includes unmanaged kernel programs.
    ///
    /// Text/quiet output reads stored summaries without kernel privileges.
    /// --all observes the kernel for every output format.
    List(ListArgs),
    /// Load a local tracepoint/XDP/TC batch; other requests are explicitly rejected.
    Load {
        #[command(subcommand)]
        source: load::LoadCommand,
    },
}

#[derive(Args)]
pub(super) struct ListArgs {
    /// Include unmanaged kernel programs.
    #[arg(short = 'a', long)]
    pub(super) all: bool,
    /// Print only program IDs, one per line.
    #[arg(short, long)]
    pub(super) quiet: bool,
    /// Output format.
    #[arg(short, long, value_enum, default_value_t = OutputFormat::Text)]
    pub(super) output: OutputFormat,
    /// Filter by program type; comma-separated or repeated, case-insensitive.
    #[arg(long = "type", visible_alias = "program-type", short = 'p', value_delimiter = ',', ignore_case = true, value_parser = program_type_parser())]
    pub(super) types: Vec<ProgramType>,
    /// Filter by exact bpfman.io/application metadata value.
    #[arg(long)]
    pub(super) application: Option<String>,
}

#[derive(Clone, Copy, PartialEq, Eq, ValueEnum)]
pub(super) enum OutputFormat {
    Text,
    Json,
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

    #[test]
    fn all_program_listing_accepts_long_and_short_flags() -> Result<(), clap::Error> {
        for flag in ["--all", "-a"] {
            let cli = Cli::try_parse_from(["bpfman", "program", "list", flag])?;
            assert!(matches!(
                cli.command,
                Command::Program {
                    command: ProgramCommand::List(ListArgs { all: true, .. })
                }
            ));
        }
        Ok(())
    }
}
