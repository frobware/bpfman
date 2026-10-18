//! Typed load input vocabulary and supported-slice dispatch.

use std::{collections::BTreeMap, num::NonZeroU32, path::PathBuf};

use bpfman_model::{ProgramSpec, Symbol};
use clap::{Args, Subcommand, ValueEnum};

mod parse;
mod request;

pub(crate) struct PreparedLoad {
    request: bpfman_runtime::PreparedPrograms,
    output: LoadOutput,
}

#[derive(Subcommand)]
pub(crate) enum LoadCommand {
    /// Load programs from a local ELF object file.
    File(FileArgs),
    /// Load from an OCI bytecode image.
    Image(ImageArgs),
}

#[derive(Args)]
#[command(subcommand_negates_reqs = true)]
pub(crate) struct LoadArgs {
    #[command(subcommand)]
    command: Option<LoadCommand>,
    #[command(flatten)]
    file: FileArgs,
}

impl LoadArgs {
    pub(super) fn into_command(self) -> LoadCommand {
        self.command.unwrap_or(LoadCommand::File(self.file))
    }
}

#[derive(Args)]
pub(crate) struct FileArgs {
    /// Path to the ELF object. File access happens only during execution.
    #[arg(value_name = "PATH", required = true)]
    path: Option<PathBuf>,
    #[command(flatten)]
    options: LoadOptions,
}

#[derive(Args)]
pub(crate) struct ImageArgs {
    /// OCI image reference; registry resolution is deferred to execution.
    #[arg(value_name = "IMAGE", value_parser = parse::image_reference)]
    reference: String,
    /// When to pull the image.
    #[arg(short = 'p', long, value_parser = parse::pull_policy, default_value = "IfNotPresent")]
    pull_policy: PullPolicy,
    #[command(flatten)]
    registry_auth: super::registry_auth::RegistryAuth,
    #[command(flatten)]
    options: LoadOptions,
}

#[derive(Args)]
struct LoadOptions {
    /// TYPE:NAME; fentry/fexit/lsm require TYPE:NAME:TARGET. Repeat or use commas.
    #[arg(long, required = true, value_delimiter = ',', value_name = "TYPE:NAME[:TARGET]", value_parser = parse::program)]
    programs: Vec<ProgramSpec>,
    /// KEY=VALUE metadata; repeatable. Last value for a key wins.
    #[arg(short = 'm', long, value_name = "KEY=VALUE", value_parser = parse::metadata)]
    metadata: Vec<Metadata>,
    /// NAME=HEX global bytes; repeatable. Optional 0x prefix.
    #[arg(short = 'g', long = "global", value_name = "NAME=HEX", value_parser = parse::global)]
    globals: Vec<Global>,
    /// Set bpfman.io/application, overriding that metadata key.
    #[arg(short = 'a', long)]
    application: Option<String>,
    /// Map-set owner ID whose maps should be shared; zero selects private maps.
    #[arg(long, value_parser = super::number::owner)]
    map_owner_id: Option<u32>,
    /// Requested result format.
    #[arg(short, long, value_enum, default_value_t = LoadOutput::Text)]
    output: LoadOutput,
}

#[derive(Clone, Debug)]
struct Metadata(String, String);

#[derive(Clone, Debug)]
struct Global(String, Vec<u8>);

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
enum PullPolicy {
    #[value(name = "Always")]
    Always,
    #[value(name = "IfNotPresent")]
    IfNotPresent,
    #[value(name = "Never")]
    Never,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
enum LoadOutput {
    Text,
    Json,
}

enum LoadSource {
    File(PathBuf),
    Image {
        reference: String,
        pull_policy: PullPolicy,
        auth: bpfman_image_oci::Credentials,
    },
}

struct LoadRequest {
    source: LoadSource,
    first: ProgramSpec,
    remaining: Vec<ProgramSpec>,
    metadata: BTreeMap<String, String>,
    globals: BTreeMap<String, Vec<u8>>,
    map_owner_id: Option<NonZeroU32>,
    output: LoadOutput,
}

impl From<PullPolicy> for bpfman_model::ImagePullPolicy {
    fn from(policy: PullPolicy) -> Self {
        match policy {
            PullPolicy::Always => Self::Always,
            PullPolicy::IfNotPresent => Self::IfNotPresent,
            PullPolicy::Never => Self::Never,
        }
    }
}
