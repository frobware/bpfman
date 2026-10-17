use super::OutputFormat;
use bpfman_model::Tracepoint;
use clap::{
    Args, Subcommand,
    builder::{PossibleValuesParser, TypedValueParser},
};
use std::num::{NonZeroU32, NonZeroU64};

#[derive(Subcommand)]
pub(crate) enum LinkCommand {
    /// Attach an already loaded program.
    Attach {
        #[command(subcommand)]
        target: AttachCommand,
    },
    /// Detach one managed link, keeping its program loaded.
    Detach {
        #[arg(value_name = "LINK_ID")]
        id: NonZeroU64,
    },
    /// Get stored intent and current kernel/pin observations.
    Get {
        #[arg(value_name = "LINK_ID")]
        id: NonZeroU64,
        #[arg(short, long, value_enum, default_value_t = OutputFormat::Text)]
        output: OutputFormat,
    },
    /// List stored links, including pending intent, without a writer lock.
    List(ListArgs),
}

#[derive(Args)]
pub(crate) struct ListArgs {
    /// Output format.
    #[arg(short, long, value_enum, default_value_t = OutputFormat::Text)]
    output: OutputFormat,
    /// Print only link IDs, one per line.
    #[arg(short, long)]
    quiet: bool,
    /// Filter by owning program ID.
    #[arg(long)]
    program_id: Option<NonZeroU32>,
    /// Filter by attachment kind; comma-separated or repeated.
    #[arg(long, value_delimiter = ',', value_parser = link_kind_parser())]
    kind: Vec<bpfman_model::LinkKind>,
    /// Filter by the owning program's type; comma-separated or repeated.
    #[arg(short = 'p', long = "program-type", value_delimiter = ',', ignore_case = true, value_parser = super::program_type_parser())]
    program_type: Vec<bpfman_model::ProgramType>,
    /// Filter by the owning program's application metadata.
    #[arg(long)]
    application: Option<String>,
    /// Filter by the owning program's KEY=VALUE metadata; repeat for more pairs.
    #[arg(short = 'm', long = "metadata-selector", value_name = "KEY=VALUE", value_parser = super::list_metadata)]
    metadata: Vec<(String, String)>,
}

fn link_kind_parser() -> impl TypedValueParser<Value = bpfman_model::LinkKind> {
    use bpfman_model::LinkKind;
    PossibleValuesParser::new(LinkKind::ALL.map(LinkKind::as_str))
        .try_map(|value| value.parse::<LinkKind>())
}

impl ListArgs {
    fn filter(self) -> bpfman_core::LinkFilter {
        let program = bpfman_core::ProgramFilter {
            types: self.program_type,
            application: self.application.filter(|app| !app.is_empty()),
            metadata: self.metadata.into_iter().collect(),
            ..Default::default()
        };
        let scoped = !program.types.is_empty()
            || program.application.is_some()
            || !program.metadata.is_empty();
        bpfman_core::LinkFilter {
            program_id: self.program_id,
            kinds: self.kind,
            program: scoped.then_some(program),
        }
    }
}

impl LinkCommand {
    pub(super) fn validate(&self) -> Result<(), crate::error::Error> {
        if let Self::Attach {
            target:
                AttachCommand::Uprobe {
                    target,
                    fn_name,
                    offset,
                    pid,
                    container_pid,
                    ..
                },
        } = self
        {
            bpfman_model::UprobeTarget::new(target, fn_name.clone(), offset.unwrap_or(0), *pid)
                .and_then(|target| target.with_container_pid(*container_pid))
                .map_err(anyhow::Error::from)?;
        }
        Ok(())
    }

    pub(super) fn xdp_interface(&self) -> Option<&bpfman_model::InterfaceName> {
        match self {
            Self::Attach {
                target: AttachCommand::Xdp { interface, .. },
            } => Some(interface),
            _ => None,
        }
    }
}

#[derive(Subcommand)]
pub(crate) enum AttachCommand {
    /// Attach a user-space entry or return probe, as selected when loading.
    Uprobe {
        program_id: NonZeroU32,
        #[arg(value_parser = user_target)]
        target: String,
        #[arg(long, short = 'f', required_unless_present = "offset", required_if_eq("offset", "0"), value_parser = user_symbol)]
        fn_name: Option<bpfman_model::Symbol>,
        #[arg(long, value_parser = clap::value_parser!(u64).range(0..=i64::MAX as u64))]
        offset: Option<u64>,
        #[arg(long, default_value_t = 0, value_parser = clap::value_parser!(u32).range(0..=i32::MAX as i64))]
        pid: u32,
        /// Host PID selecting the target mount namespace (absolute paths or library names).
        #[arg(long, default_value_t = 0, value_parser = clap::value_parser!(u32).range(0..=i32::MAX as i64))]
        container_pid: u32,
        #[arg(short = 'm', long, value_name = "KEY=VALUE", value_parser = metadata)]
        metadata: Vec<(String, String)>,
        #[arg(short, long, value_enum, default_value_t = OutputFormat::Text)]
        output: OutputFormat,
    },
    /// Attach an fentry program to its load-time function.
    Fentry {
        program_id: NonZeroU32,
        #[arg(short = 'm', long, value_name = "KEY=VALUE", value_parser = metadata)]
        metadata: Vec<(String, String)>,
        #[arg(short, long, value_enum, default_value_t = OutputFormat::Text)]
        output: OutputFormat,
    },
    /// Attach an fexit program to its load-time function.
    Fexit {
        program_id: NonZeroU32,
        #[arg(short = 'm', long, value_name = "KEY=VALUE", value_parser = metadata)]
        metadata: Vec<(String, String)>,
        #[arg(short, long, value_enum, default_value_t = OutputFormat::Text)]
        output: OutputFormat,
    },
    /// Attach an LSM program to its load-time hook.
    Lsm {
        program_id: NonZeroU32,
        #[arg(short = 'm', long, value_name = "KEY=VALUE", value_parser = metadata)]
        metadata: Vec<(String, String)>,
        #[arg(short, long, value_enum, default_value_t = OutputFormat::Text)]
        output: OutputFormat,
    },
    /// Attach a kernel entry or return probe, as selected when loading.
    Kprobe {
        program_id: NonZeroU32,
        function: bpfman_model::KernelFunction,
        #[arg(long, default_value_t = 0, value_parser = clap::value_parser!(u64).range(0..=i64::MAX as u64))]
        offset: u64,
        #[arg(short = 'm', long, value_name = "KEY=VALUE", value_parser = metadata)]
        metadata: Vec<(String, String)>,
        #[arg(short, long, value_enum, default_value_t = OutputFormat::Text)]
        output: OutputFormat,
    },
    /// Attach a native TCX classifier without a dispatcher.
    Tcx {
        program_id: NonZeroU32,
        interface: bpfman_model::InterfaceName,
        direction: bpfman_model::TcDirection,
        #[arg(long, default_value = "")]
        netns: bpfman_model::NetworkNamespace,
        #[arg(short = 'p', long, value_parser = clap::value_parser!(u32).range(0..=i32::MAX as i64))]
        priority: u32,
        #[arg(short = 'm', long, value_name = "KEY=VALUE", value_parser = metadata)]
        metadata: Vec<(String, String)>,
        #[arg(short, long, value_enum, default_value_t = OutputFormat::Text)]
        output: OutputFormat,
    },
    /// Attach a TC extension through a legacy TC dispatcher.
    Tc {
        program_id: NonZeroU32,
        interface: bpfman_model::InterfaceName,
        direction: bpfman_model::TcDirection,
        #[arg(long, default_value = "")]
        netns: bpfman_model::NetworkNamespace,
        #[arg(short = 'p', long, value_parser = clap::value_parser!(u32).range(0..=i32::MAX as i64))]
        priority: u32,
        #[arg(long, value_delimiter = ',', default_value = "pipe,dispatcher_return", value_parser = tc_action)]
        proceed_on: Vec<i32>,
        #[arg(short = 'm', long, value_name = "KEY=VALUE", value_parser = metadata)]
        metadata: Vec<(String, String)>,
        #[arg(short, long, value_enum, default_value_t = OutputFormat::Text)]
        output: OutputFormat,
    },
    /// Attach an XDP extension to an interface in a selected network namespace.
    Xdp {
        program_id: NonZeroU32,
        interface: bpfman_model::InterfaceName,
        #[arg(long, default_value = "")]
        netns: bpfman_model::NetworkNamespace,
        #[arg(short = 'p', long, value_parser = clap::value_parser!(u32).range(0..=i32::MAX as i64))]
        priority: u32,
        #[arg(long, value_delimiter = ',', default_value = "pass,dispatcher_return", value_parser = action)]
        proceed_on: Vec<u32>,
        #[arg(short = 'm', long, value_name = "KEY=VALUE", value_parser = metadata)]
        metadata: Vec<(String, String)>,
        #[arg(short, long, value_enum, default_value_t = OutputFormat::Text)]
        output: OutputFormat,
    },
    /// Attach a managed tracepoint program to GROUP/NAME.
    Tracepoint {
        #[arg(value_name = "PROGRAM_ID")]
        program_id: NonZeroU32,
        #[arg(value_name = "GROUP/NAME")]
        target: Tracepoint,
        #[arg(short = 'm', long, value_name = "KEY=VALUE", value_parser = metadata)]
        metadata: Vec<(String, String)>,
        #[arg(short, long, value_enum, default_value_t = OutputFormat::Text)]
        output: OutputFormat,
    },
}

fn metadata(raw: &str) -> Result<(String, String), String> {
    let (key, value) = raw.split_once('=').ok_or("expected KEY=VALUE")?;
    let key = key.trim();
    if key.is_empty() {
        return Err("metadata key must not be empty".into());
    }

    Ok((key.into(), value.into()))
}

impl LinkCommand {
    pub(crate) fn execute<S>(
        self,
        app: &bpfman_runtime::Bpfman<S, bpfman_kernel_aya::Kernel>,
        cancellation: &bpfman_runtime::Cancellation,
        mode: bpfman_model::XdpMode,
    ) -> Result<(), crate::error::Error>
    where
        S: bpfman_store::OpenStore
            + bpfman_store::LinkStore
            + bpfman_store::XdpReplacementStore
            + bpfman_store::TcStore
            + 'static,
        S::Reader: bpfman_store::LinkReader,
    {
        match self {
            Self::Attach {
                target:
                    AttachCommand::Uprobe {
                        program_id,
                        target,
                        fn_name,
                        offset,
                        pid,
                        container_pid,
                        metadata,
                        output,
                    },
            } => {
                let target =
                    bpfman_model::UprobeTarget::new(&target, fn_name, offset.unwrap_or(0), pid)
                        .and_then(|target| target.with_container_pid(container_pid))
                        .map_err(anyhow::Error::from)?;
                let record = app.attach_uprobe_with_cancellation(
                    bpfman_runtime::UprobeAttach {
                        program_id,
                        target,
                        metadata: metadata.into_iter().collect(),
                    },
                    cancellation,
                )?;
                let observed = app.get_link(record.id).map_err(|error| {
                    anyhow::Error::from(error).context(format!(
                        "attachment committed as link {}; observation failed",
                        record.id
                    ))
                })?;
                crate::output::link(&mut std::io::stdout().lock(), &observed, output)?;
            }
            Self::Attach {
                target:
                    AttachCommand::Fentry {
                        program_id,
                        metadata,
                        output,
                    },
            } => {
                let record = app.attach_tracing_with_cancellation(
                    bpfman_runtime::TracingAttach {
                        program_id,
                        kind: bpfman_model::TracingKind::Fentry,
                        metadata: metadata.into_iter().collect(),
                    },
                    cancellation,
                )?;
                let observed = app.get_link(record.id).map_err(|error| {
                    anyhow::Error::from(error).context(format!(
                        "attachment committed as link {}; observation failed",
                        record.id
                    ))
                })?;
                crate::output::link(&mut std::io::stdout().lock(), &observed, output)?;
            }
            Self::Attach {
                target:
                    AttachCommand::Fexit {
                        program_id,
                        metadata,
                        output,
                    },
            } => {
                let record = app.attach_tracing_with_cancellation(
                    bpfman_runtime::TracingAttach {
                        program_id,
                        kind: bpfman_model::TracingKind::Fexit,
                        metadata: metadata.into_iter().collect(),
                    },
                    cancellation,
                )?;
                let observed = app.get_link(record.id).map_err(|error| {
                    anyhow::Error::from(error).context(format!(
                        "attachment committed as link {}; observation failed",
                        record.id
                    ))
                })?;
                crate::output::link(&mut std::io::stdout().lock(), &observed, output)?;
            }
            Self::Attach {
                target:
                    AttachCommand::Lsm {
                        program_id,
                        metadata,
                        output,
                    },
            } => {
                let record = app.attach_tracing_with_cancellation(
                    bpfman_runtime::TracingAttach {
                        program_id,
                        kind: bpfman_model::TracingKind::Lsm,
                        metadata: metadata.into_iter().collect(),
                    },
                    cancellation,
                )?;
                let observed = app.get_link(record.id).map_err(|error| {
                    anyhow::Error::from(error).context(format!(
                        "attachment committed as link {}; observation failed",
                        record.id
                    ))
                })?;
                crate::output::link(&mut std::io::stdout().lock(), &observed, output)?;
            }
            Self::Attach {
                target:
                    AttachCommand::Kprobe {
                        program_id,
                        function,
                        offset,
                        metadata,
                        output,
                    },
            } => {
                let record = app.attach_kprobe_with_cancellation(
                    bpfman_runtime::KprobeAttach {
                        program_id,
                        function,
                        offset,
                        metadata: metadata.into_iter().collect(),
                    },
                    cancellation,
                )?;
                let observed = app.get_link(record.id).map_err(|error| {
                    anyhow::Error::from(error).context(format!(
                        "attachment committed as link {}; observation failed",
                        record.id
                    ))
                })?;
                crate::output::link(&mut std::io::stdout().lock(), &observed, output)?;
            }
            Self::Attach {
                target:
                    AttachCommand::Tcx {
                        program_id,
                        interface,
                        direction,
                        netns,
                        priority,
                        metadata,
                        output,
                    },
            } => {
                let record = app.attach_tcx_with_cancellation(
                    bpfman_runtime::TcxAttach {
                        program_id,
                        interface,
                        direction,
                        netns,
                        priority,
                        metadata: metadata.into_iter().collect(),
                    },
                    cancellation,
                )?;
                let observed = app.get_link(record.id).map_err(|error| {
                    anyhow::Error::from(error).context(format!(
                        "link {} was committed but could not be observed; it remains attached",
                        record.id
                    ))
                })?;
                crate::output::link(&mut std::io::stdout().lock(), &observed, output)?;
            }
            Self::Attach {
                target:
                    AttachCommand::Tc {
                        program_id,
                        interface,
                        direction,
                        netns,
                        priority,
                        proceed_on,
                        metadata,
                        output,
                    },
            } => {
                let mask = proceed_on
                    .into_iter()
                    .fold(0u32, |mask, code| mask | (1 << (code + 1)));
                let record = app.attach_tc_with_cancellation(
                    bpfman_runtime::TcAttach {
                        program_id,
                        interface,
                        direction,
                        netns,
                        priority,
                        proceed_on: mask.try_into().map_err(anyhow::Error::from)?,
                        metadata: metadata.into_iter().collect(),
                    },
                    cancellation,
                )?;
                let observed = app.get_link(record.id).map_err(|error| {
                    anyhow::Error::from(error).context(format!(
                        "link {} was committed but could not be observed; it remains attached",
                        record.id
                    ))
                })?;
                crate::output::link(&mut std::io::stdout().lock(), &observed, output)?;
            }
            Self::Attach {
                target:
                    AttachCommand::Xdp {
                        program_id,
                        interface,
                        netns,
                        priority,
                        proceed_on,
                        metadata,
                        output,
                    },
            } => {
                let mask = proceed_on
                    .into_iter()
                    .fold(0, |mask, code| mask | (1 << code));
                let proceed_on = mask.try_into().map_err(anyhow::Error::from)?;
                let record = app.attach_xdp_with_cancellation(
                    bpfman_runtime::XdpAttach {
                        netns,
                        mode,
                        program_id,
                        interface,
                        priority,
                        proceed_on,
                        metadata: metadata.into_iter().collect(),
                    },
                    cancellation,
                )?;
                let observed = app.get_link(record.id).map_err(|error| {
                    anyhow::Error::from(error).context(format!(
                        "link {} was committed but could not be observed; it remains attached",
                        record.id
                    ))
                })?;
                crate::output::link(&mut std::io::stdout().lock(), &observed, output)?;
            }
            Self::Attach {
                target:
                    AttachCommand::Tracepoint {
                        program_id,
                        target,
                        metadata,
                        output,
                    },
            } => {
                let record = app.attach_tracepoint_with_cancellation(
                    bpfman_runtime::TracepointAttach {
                        program_id,
                        target,
                        metadata: metadata.into_iter().collect(),
                    },
                    cancellation,
                )?;
                // Finalisation has committed. Observation/output failure cannot
                // undo the link, and a late signal must not start compensation.
                let observed = app.get_link(record.id).map_err(|error| {
                    anyhow::Error::from(error).context(format!(
                        "link {} was committed but could not be observed; it remains attached",
                        record.id
                    ))
                })?;
                crate::output::link(&mut std::io::stdout().lock(), &observed, output)?;
            }
            Self::Detach { id } => {
                let record = app
                    .list_link_records_with_cancellation(cancellation)?
                    .into_iter()
                    .find(|r| r.id == id);
                match record.map(|r| r.details) {
                    Some(bpfman_model::LinkDetails::Xdp(_)) => {
                        let _report = app.detach_xdp_with_cancellation(id, cancellation)?;
                    }
                    Some(bpfman_model::LinkDetails::Tc(_)) => {
                        let _report = app.detach_tc_with_cancellation(id, cancellation)?;
                    }
                    _ => {
                        let _report = app.detach_with_cancellation(id, cancellation)?;
                    }
                }
            }
            Self::Get { id, output } => {
                let link = app.get_link_with_cancellation(id, cancellation)?;
                crate::output::link(&mut std::io::stdout().lock(), &link, output)?;
            }
            Self::List(args) => {
                let output = args.output;
                let quiet = args.quiet;
                let links = app.list_links_with_cancellation(&args.filter(), cancellation)?;
                crate::output::links(&mut std::io::stdout().lock(), &links, output, quiet)?;
            }
        }

        Ok(())
    }
}

fn action(raw: &str) -> Result<u32, String> {
    match raw {
        "aborted" => Ok(0),
        "drop" => Ok(1),
        "pass" => Ok(2),
        "tx" => Ok(3),
        "redirect" => Ok(4),
        "dispatcher_return" => Ok(31),
        _ => Err("expected aborted, drop, pass, tx, redirect, or dispatcher_return".into()),
    }
}

fn tc_action(raw: &str) -> Result<i32, String> {
    match raw {
        "unspec" => Ok(-1),
        "ok" => Ok(0),
        "reclassify" => Ok(1),
        "shot" => Ok(2),
        "pipe" => Ok(3),
        "stolen" => Ok(4),
        "queued" => Ok(5),
        "repeat" => Ok(6),
        "redirect" => Ok(7),
        "trap" => Ok(8),
        "dispatcher_return" => Ok(30),
        _ => Err("expected unspec, ok, reclassify, shot, pipe, stolen, queued, repeat, redirect, trap, or dispatcher_return".into()),
    }
}

fn user_target(raw: &str) -> Result<String, String> {
    bpfman_model::UprobeTarget::new(raw, None, 1, 0).map_err(|e| e.to_string())?;
    Ok(raw.into())
}

fn user_symbol(raw: &str) -> Result<bpfman_model::Symbol, bpfman_model::InvalidSymbol> {
    raw.try_into()
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[test]
    fn list_filters_parse_aliases_unions_and_raw_pairs() -> Result<(), clap::Error> {
        let cli = crate::cli::Cli::try_parse_from([
            "bpfman",
            "link",
            "list",
            "-q",
            "-o",
            "json",
            "--program-id",
            "42",
            "--kind",
            "kprobe,tracepoint",
            "--kind",
            "kretprobe",
            "-p",
            "kprobe,tracepoint",
            "--program-type",
            "kretprobe",
            "--application",
            "demo",
            "-m",
            "owner=old",
            "--metadata-selector",
            " owner =acme",
            "-m",
            "arbitrary key=raw=value with spaces",
        ])?;
        let crate::cli::Command::Link {
            command: LinkCommand::List(args),
        } = cli.command
        else {
            unreachable!("link list");
        };
        assert!(args.quiet);
        assert!(matches!(args.output, OutputFormat::Json));
        let filter = args.filter();
        assert_eq!(filter.program_id.map(NonZeroU32::get), Some(42));
        assert_eq!(
            filter.kinds,
            [
                bpfman_model::LinkKind::Kprobe,
                bpfman_model::LinkKind::Tracepoint,
                bpfman_model::LinkKind::Kretprobe
            ]
        );
        let program = filter.program.expect("scope");
        assert_eq!(program.application.as_deref(), Some("demo"));
        assert_eq!(
            program.types,
            [
                bpfman_model::ProgramType::Kprobe,
                bpfman_model::ProgramType::Tracepoint,
                bpfman_model::ProgramType::Kretprobe
            ]
        );
        assert_eq!(program.metadata["owner"], "acme");
        assert_eq!(program.metadata["arbitrary key"], "raw=value with spaces");
        for application in [None, Some("")] {
            let mut argv = vec!["bpfman", "link", "list"];
            if let Some(app) = application {
                argv.extend(["--application", app]);
            }
            let cli = crate::cli::Cli::try_parse_from(argv)?;
            let crate::cli::Command::Link {
                command: LinkCommand::List(args),
            } = cli.command
            else {
                unreachable!("link list");
            };
            assert!(args.filter().program.is_none());
        }
        Ok(())
    }
}
