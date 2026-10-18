//! Collection policy belongs to the CLI; adapters only emit spans and events.

use std::{fs::OpenOptions, path::Path};
use tracing_subscriber::{EnvFilter, fmt::format::FmtSpan, prelude::*};

mod spec;

pub(super) fn validate_spec(value: &str) -> anyhow::Result<()> {
    spec::directive(value).map(|_| ())
}

pub(super) fn init(
    path: Option<&Path>,
    log: Option<&str>,
    config: Option<&Path>,
) -> anyhow::Result<Option<tracing_chrome::FlushGuard>> {
    let config = crate::cli::config::Config::load(config)?;
    let default = if path.is_some() {
        tracing::level_filters::LevelFilter::DEBUG
    } else {
        tracing::level_filters::LevelFilter::WARN
    };
    let builder = EnvFilter::builder()
        .with_regex(false)
        .with_default_directive(default.into());
    let legacy = log.is_none() && std::env::var_os("RUST_LOG").is_some();
    let specification = log.map(spec::directive).transpose()?;
    let detailed = specification.as_ref().is_some_and(|specification| {
        specification
            .split(',')
            .any(|directive| matches!(directive.rsplit('=').next(), Some("debug" | "trace")))
    });
    let ordinary_diagnostics = !detailed && !legacy && path.is_none();
    let filter = match specification {
        Some(specification) => builder.parse(specification)?,
        None => builder.from_env()?,
    };
    let formatter = tracing_subscriber::fmt::layer()
        .with_writer(std::io::stderr)
        .with_ansi(false)
        .with_span_events(FmtSpan::NEW | FmtSpan::CLOSE);
    let formatter = if legacy || config.json_logs() {
        formatter.json().boxed()
    } else {
        formatter.boxed()
    };

    let (timeline, guard) = if let Some(path) = path {
        // Never truncate an existing file, including an accidentally selected
        // runtime store. Opening errors are reported before application effects.
        let writer = OpenOptions::new().write(true).create_new(true).open(path)?;
        let (layer, guard) = tracing_chrome::ChromeLayerBuilder::new()
            .writer(writer)
            .include_args(true)
            .include_locations(false)
            .build();
        (Some(layer), Some(guard))
    } else {
        (None, None)
    };

    tracing_subscriber::registry()
        // ureq-proto's trace logs contain raw HTTP bytes and bypass sensitive
        // HeaderValue formatting. ureq also reveals signed Location URLs when
        // trace is enabled. Keep both below trace even with explicit RUST_LOG
        // directives, for stderr and the timeline. Application trace stays on.
        .with(tracing_subscriber::filter::filter_fn(move |metadata| {
            // The front end renders ordinary command failures once. Instrumented
            // error returns add the same source chain at several nested spans;
            // retain those only for detailed logging or a timeline. This also
            // keeps --ignore-missing silent with an explicit --log=warn.
            if ordinary_diagnostics
                && metadata.is_event()
                && *metadata.level() == tracing::Level::ERROR
                && metadata.fields().field("error").is_some()
            {
                return false;
            }
            let target = metadata.target();
            *metadata.level() != tracing::Level::TRACE
                || !(target == "ureq"
                    || target.starts_with("ureq::")
                    || target == "ureq_proto"
                    || target.starts_with("ureq_proto::"))
        }))
        .with(filter)
        .with(formatter)
        .with(timeline)
        .try_init()?;

    Ok(guard)
}
