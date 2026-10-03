//! Typed load input vocabulary and supported-slice dispatch.

use std::{collections::BTreeMap, num::NonZeroU32, path::PathBuf};

use bpfman_model::{ProgramSpec, Symbol};
use clap::{Args, Subcommand, ValueEnum};

mod parse;
mod request;

pub(crate) struct PreparedLoad {
    request: bpfman_runtime::PreparedTracepoint,
    output: LoadOutput,
}

#[derive(Subcommand)]
pub(crate) enum LoadCommand {
    /// Load one tracepoint from a local ELF object file.
    File(FileArgs),
    /// Load from an OCI image (execution not implemented).
    Image(ImageArgs),
}

#[derive(Args)]
pub(crate) struct FileArgs {
    /// Path to the ELF object. File access happens only during execution.
    #[arg(value_name = "PATH")]
    path: PathBuf,
    #[command(flatten)]
    options: LoadOptions,
}

#[derive(Args)]
pub(crate) struct ImageArgs {
    /// OCI image reference; registry resolution is deferred to execution.
    #[arg(value_name = "IMAGE", value_parser = parse::image_reference)]
    reference: String,
    /// When to pull the image.
    #[arg(short = 'p', long, value_enum, ignore_case = true, default_value_t = PullPolicy::IfNotPresent)]
    pull_policy: PullPolicy,
    /// Base64 username:password. Prefer the environment to process arguments.
    #[arg(
        long,
        env = "BPFMAN_REGISTRY_AUTH",
        hide_env_values = true,
        hide_possible_values = true
    )]
    registry_auth: Option<String>,
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
    /// Nonzero kernel program ID whose maps should be shared.
    #[arg(long)]
    map_owner_id: Option<NonZeroU32>,
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

// Deliberately no Debug for credentials or requests that contain them.
struct RegistryAuth {
    username: String,
    password: String,
}

enum LoadSource {
    File(PathBuf),
    Image {
        reference: String,
        pull_policy: PullPolicy,
        auth: Option<RegistryAuth>,
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
