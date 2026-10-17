//! Collection policy belongs to the CLI; adapters only emit spans and events.

use std::{fs::OpenOptions, path::Path};
use tracing_subscriber::{EnvFilter, fmt::format::FmtSpan, prelude::*};

pub(super) fn init(path: Option<&Path>) -> anyhow::Result<Option<tracing_chrome::FlushGuard>> {
    let default = if path.is_some() {
        tracing::level_filters::LevelFilter::DEBUG
    } else {
        tracing::level_filters::LevelFilter::OFF
    };
    let filter = EnvFilter::builder()
        .with_regex(false)
        .with_default_directive(default.into())
        .from_env()?;

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
        .with(tracing_subscriber::filter::filter_fn(|metadata| {
            let target = metadata.target();
            *metadata.level() != tracing::Level::TRACE
                || !(target == "ureq"
                    || target.starts_with("ureq::")
                    || target == "ureq_proto"
                    || target.starts_with("ureq_proto::"))
        }))
        .with(filter)
        .with(
            tracing_subscriber::fmt::layer()
                .json()
                .with_writer(std::io::stderr)
                .with_span_events(FmtSpan::NEW | FmtSpan::CLOSE),
        )
        .with(timeline)
        .try_init()?;

    Ok(guard)
}
