use std::time::Duration;

use bpfman_fs::{DEFAULT_RUNTIME_ROOT, RuntimeLayout};
use bpfman_model::ProgramType;
use clap::{
    Args, Parser, Subcommand, ValueEnum,
    builder::{PathBufValueParser, StringValueParser, TypedValueParser},
};

pub(super) mod config;
mod delete;
mod dispatcher;
mod duration;
mod image;
mod link;
mod load;
mod number;
mod registry_auth;

#[derive(Parser)]
#[command(
    name = "bpfman",
    version,
    about,
    max_term_width = 80,
    args_override_self = true
)]
pub(super) struct Cli {
    /// bpfman TOML configuration file (defaults to /etc/bpfman/bpfman.toml).
    #[arg(long, global = true, env = "BPFMAN_CONFIG", value_name = "FILE")]
    pub(super) config: Option<std::path::PathBuf>,
    /// Log specification (for example warn,manager=debug).
    #[arg(long, global = true, env = "BPFMAN_LOG", value_name = "SPEC")]
    pub(super) log: Option<String>,
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
    #[arg(long, global = true, env = "BPFMAN_LOCK_TIMEOUT", default_value = "30s", allow_hyphen_values = true, value_parser = duration::lock_timeout)]
    pub(super) lock_timeout: Duration,
    /// Write a new Perfetto/Chrome timeline file, filtered by RUST_LOG.
    #[arg(long, global = true, value_name = "PATH")]
    pub(super) trace_file: Option<std::path::PathBuf>,
    #[command(subcommand)]
    pub(super) command: Command,
}

impl Cli {
    pub(super) fn validate(&self) -> Result<(), clap::Error> {
        if let Command::Program(args) = &self.command {
            let list = match &args.command {
                Some(ProgramCommand::Delete(args)) => return args.validate(),
                Some(ProgramCommand::List(list)) => Some(list),
                None => Some(&args.list),
                _ => None,
            };
            if list.is_some_and(|list| list.attached && list.unattached) {
                return Err(clap::Error::raw(
                    clap::error::ErrorKind::ArgumentConflict,
                    "--attached and --unattached are mutually exclusive",
                ));
            }
        }
        Ok(())
    }
}

pub(super) fn validate_scopes(
    command: &clap::Command,
    matches: &clap::ArgMatches,
) -> Result<(), clap::Error> {
    if let Some((name, child_matches)) = matches.subcommand() {
        // Clap's blanket args_conflicts_with_subcommands also rejects inherited
        // global options. Check only local, explicitly supplied parent args so
        // defaults and globals can coexist with a selected child, as in Kong.
        if let Some(argument) = command.get_arguments().find(|argument| {
            !argument.is_global_set()
                && matches.value_source(argument.get_id().as_str())
                    == Some(clap::parser::ValueSource::CommandLine)
        }) {
            return Err(clap::Error::raw(
                clap::error::ErrorKind::ArgumentConflict,
                format!(
                    "{} cannot be used before subcommand {name}",
                    argument.get_id()
                ),
            ));
        }
        if let Some(child) = command.find_subcommand(name) {
            validate_scopes(child, child_matches)?;
        }
    }
    Ok(())
}

pub(super) enum PreparedCommand {
    Get {
        id: std::num::NonZeroU32,
        output: OutputFormat,
    },
    Unload {
        ids: Vec<u32>,
        ignore_missing: bool,
    },
    Delete(delete::DeletePrograms),
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
            Self::Program(args) => args.into_command(),
            Self::Get { command } => match command {
                GetCommand::Program { id, output } => ProgramCommand::Get { id, output },
                GetCommand::Link { id, output } => {
                    return Ok(PreparedCommand::Link(
                        link::LinkCommand::Get { id, output },
                        Default::default(),
                    ));
                }
            },
            Self::Version => {
                return Err(
                    anyhow::anyhow!("version must dispatch before runtime initialization").into(),
                );
            }
            Self::Image { command } => return Ok(PreparedCommand::Image(command)),
            Self::NsHelper { .. } => {
                return Err(anyhow::anyhow!(
                    "namespace helper must dispatch before runtime initialization"
                )
                .into());
            }
            Self::Dispatcher(args) => return Ok(PreparedCommand::Dispatcher(args.into_command())),
            Self::Link(args) => {
                let command = args.into_command();
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
            ProgramCommand::Unload {
                ids,
                ignore_missing,
            } => PreparedCommand::Unload {
                ids,
                ignore_missing,
            },
            ProgramCommand::Delete(args) => PreparedCommand::Delete(args),
            ProgramCommand::List(args) => PreparedCommand::List(args),
            ProgramCommand::Load(source) => PreparedCommand::Load(source.into_command().prepare(
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
    /// Print build version information.
    Version,
    #[command(hide = true)]
    Get {
        #[command(subcommand)]
        command: GetCommand,
    },
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
    #[command(hide = true)]
    Dispatcher(dispatcher::DispatcherArgs),
    /// Manage standalone BPF links.
    Link(link::LinkArgs),
    /// Manage BPF programs.
    Program(ProgramArgs),
}

#[derive(Subcommand)]
pub(super) enum GetCommand {
    /// Get a managed program.
    Program {
        #[arg(value_parser = number::program_id)]
        id: std::num::NonZeroU32,
        #[arg(short, long, value_enum, default_value_t = OutputFormat::Text)]
        output: OutputFormat,
    },
    /// Get a managed link.
    Link {
        #[arg(value_parser = number::link_id)]
        id: std::num::NonZeroU64,
        #[arg(short, long, value_enum, default_value_t = OutputFormat::Text)]
        output: OutputFormat,
    },
}

#[derive(Args)]
#[command(subcommand_negates_reqs = true)]
pub(super) struct ProgramArgs {
    #[command(subcommand)]
    command: Option<ProgramCommand>,
    #[command(flatten)]
    list: ListArgs,
}

impl ProgramArgs {
    fn into_command(self) -> ProgramCommand {
        self.command.unwrap_or(ProgramCommand::List(self.list))
    }
}

#[derive(Subcommand)]
pub(super) enum ProgramCommand {
    #[command(hide = true)]
    Delete(delete::DeletePrograms),
    /// Get a managed program with kernel, map, and link observations.
    Get {
        /// Managed kernel program ID.
        #[arg(value_name = "PROGRAM_ID", value_parser = number::program_id)]
        id: std::num::NonZeroU32,
        /// Output format.
        #[arg(short,long,value_enum,default_value_t=OutputFormat::Text)]
        output: OutputFormat,
    },
    /// Unload managed programs and clean up their attachments.
    Unload {
        /// Managed kernel program IDs, attempted in order.
        #[arg(value_name = "PROGRAM_ID", required = true, num_args = 1.., value_parser = number::owner)]
        ids: Vec<u32>,
        /// Treat a missing managed program as success.
        #[arg(long, action = clap::ArgAction::Set, value_parser = number::boolean, num_args = 0..=1, require_equals = true, default_missing_value = "true", default_value_t = false)]
        ignore_missing: bool,
    },
    /// List programs; --all includes unmanaged kernel programs.
    ///
    /// Text/quiet output reads stored summaries without kernel privileges.
    /// --all observes the kernel for every output format.
    List(ListArgs),
    /// Load programs from a local file (the default) or OCI image.
    Load(load::LoadArgs),
}

#[derive(Args)]
pub(super) struct ListArgs {
    /// Show only managed programs with active kernel links.
    #[arg(long, action = clap::ArgAction::Set, value_parser = number::boolean, num_args = 0..=1, require_equals = true, default_missing_value = "true", default_value_t = false)]
    pub(super) attached: bool,
    /// Show only managed programs without active kernel links.
    #[arg(long, action = clap::ArgAction::Set, value_parser = number::boolean, num_args = 0..=1, require_equals = true, default_missing_value = "true", default_value_t = false)]
    pub(super) unattached: bool,
    /// Include unmanaged kernel programs.
    #[arg(short = 'a', long, action = clap::ArgAction::Set, value_parser = number::boolean, num_args = 0..=1, require_equals = true, default_missing_value = "true", default_value_t = false)]
    pub(super) all: bool,
    /// Print only program IDs, one per line.
    #[arg(short = 'q', long, action = clap::ArgAction::Set, value_parser = number::boolean, num_args = 0..=1, require_equals = true, default_missing_value = "true", default_value_t = false)]
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
    /// Filter by KEY=VALUE metadata; repeat to require several pairs.
    #[arg(short = 'm', long = "metadata-selector", value_name = "KEY=VALUE", value_parser = list_metadata)]
    pub(super) metadata: Vec<(String, String)>,
    /// Kubernetes label selector over program metadata.
    #[arg(short = 'l', long, default_value = "")]
    pub(super) selector: bpfman_core::MetadataSelector,
}

impl ListArgs {
    pub(super) fn attachment(&self) -> Option<bpfman_core::AttachmentFilter> {
        if self.attached {
            Some(bpfman_core::AttachmentFilter::Attached)
        } else if self.unattached {
            Some(bpfman_core::AttachmentFilter::Unattached)
        } else {
            None
        }
    }
}

fn list_metadata(raw: &str) -> Result<(String, String), String> {
    let (key, value) = raw.split_once('=').ok_or("expected KEY=VALUE")?;
    let key = key.trim();
    if key.is_empty() {
        return Err("metadata key must not be empty".into());
    }
    Ok((key.into(), value.into()))
}

#[derive(Clone, Copy, PartialEq, Eq, ValueEnum)]
pub(super) enum OutputFormat {
    Text,
    Json,
}

fn program_type_parser() -> impl TypedValueParser<Value = ProgramType> {
    StringValueParser::new()
        .try_map(|value| value.trim().to_ascii_lowercase().parse::<ProgramType>())
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
        let Command::Program(program) = cli.command else {
            return Err(clap::Error::raw(
                clap::error::ErrorKind::InvalidSubcommand,
                "expected program",
            ));
        };
        let ProgramCommand::List(args) = program.into_command() else {
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
                Command::Program(ProgramArgs {
                    command: Some(ProgramCommand::List(ListArgs { all: true, .. })),
                    ..
                })
            ));
        }
        Ok(())
    }

    #[test]
    fn attachment_filters_parse_and_conflict_before_execution() -> Result<(), clap::Error> {
        for (flag, expected) in [
            ("--attached", bpfman_core::AttachmentFilter::Attached),
            ("--unattached", bpfman_core::AttachmentFilter::Unattached),
        ] {
            let cli = Cli::try_parse_from(["bpfman", "program", "list", "--all", flag])?;
            assert!(matches!(cli.command,
                Command::Program(ProgramArgs { command: Some(ProgramCommand::List(args)), .. }) if args.attachment() == Some(expected)
            ));
        }
        let error =
            Cli::try_parse_from(["bpfman", "program", "list", "--attached", "--unattached"])
                .and_then(|cli| cli.validate().map(|()| cli))
                .err()
                .ok_or_else(|| {
                    clap::Error::raw(clap::error::ErrorKind::InvalidValue, "expected conflict")
                })?;
        assert_eq!(error.kind(), clap::error::ErrorKind::ArgumentConflict);
        Ok(())
    }

    #[test]
    fn metadata_inputs_keep_raw_pairs_and_parse_label_requirements() -> Result<(), clap::Error> {
        let cli = Cli::try_parse_from([
            "bpfman",
            "program",
            "list",
            "-m",
            " owner =old",
            "--metadata-selector",
            "owner=acme",
            "-m",
            "arbitrary key=value=with spaces",
            "--selector",
            "owner in (acme,other),!debug",
        ])?;
        let Command::Program(program) = cli.command else {
            return Err(clap::Error::raw(
                clap::error::ErrorKind::InvalidSubcommand,
                "expected program",
            ));
        };
        let ProgramCommand::List(args) = program.into_command() else {
            return Err(clap::Error::raw(
                clap::error::ErrorKind::InvalidSubcommand,
                "expected list",
            ));
        };
        assert_eq!(
            args.metadata
                .into_iter()
                .collect::<std::collections::BTreeMap<_, _>>(),
            std::collections::BTreeMap::from([
                ("owner".into(), "acme".into()),
                ("arbitrary key".into(), "value=with spaces".into()),
            ])
        );
        assert_eq!(
            args.selector,
            "owner in (acme,other),!debug".parse().map_err(|error| {
                clap::Error::raw(clap::error::ErrorKind::InvalidValue, error)
            })?
        );
        // The short alias shares the same typed parser, including an empty selector.
        Cli::try_parse_from(["bpfman", "program", "list", "-l", ""])?;
        Ok(())
    }
}
