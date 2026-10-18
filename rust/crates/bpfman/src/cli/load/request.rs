use bpfman_kernel::ObjectLoader;
use clap::error::ErrorKind;

use super::{Global, LoadCommand, LoadOptions, LoadRequest, LoadSource, Metadata, PreparedLoad};

impl LoadCommand {
    pub(crate) fn prepare(
        self,
        layout: &bpfman_fs::RuntimeLayout,
        cancellation: &bpfman_runtime::Cancellation,
        config: Option<&std::path::Path>,
        cache: &bpfman_fs::RuntimeLayout,
        lock_timeout: std::time::Duration,
        trust_cache: Option<&bpfman_fs::RuntimeLayout>,
    ) -> Result<PreparedLoad, crate::error::Error> {
        let request = self.into_request().unwrap_or_else(|error| error.exit());
        request.prepare(
            layout,
            cancellation,
            config,
            cache,
            lock_timeout,
            trust_cache,
        )
    }

    fn into_request(self) -> Result<LoadRequest, clap::Error> {
        let (source, options) = match self {
            Self::File(args) => {
                let path = args
                    .path
                    .ok_or_else(|| invalid("object path is required"))?;
                let path = match path.into_os_string().into_string() {
                    Ok(path) => std::path::PathBuf::from(path.trim()),
                    Err(path) => std::path::PathBuf::from(path),
                };
                if path.as_os_str().is_empty() {
                    return Err(invalid("object path must not be empty"));
                }

                (LoadSource::File(path), args.options)
            }
            Self::Image(args) => {
                let parsed = bpfman_image_oci::Reference::parse(&args.reference)
                    .map_err(|_| invalid("invalid OCI image reference"))?;
                let auth = args.registry_auth.credentials(&parsed)?;
                (
                    LoadSource::Image {
                        reference: args.reference,
                        pull_policy: args.pull_policy,
                        auth,
                    },
                    args.options,
                )
            }
        };
        options.into_request(source)
    }
}

fn invalid(message: impl Into<String>) -> clap::Error {
    clap::Error::raw(ErrorKind::ValueValidation, message.into())
}

impl LoadOptions {
    fn into_request(self, source: LoadSource) -> Result<LoadRequest, clap::Error> {
        let mut programs = self.programs.into_iter();
        let first = programs
            .next()
            .ok_or_else(|| invalid("at least one program is required"))?;
        let mut metadata = self
            .metadata
            .into_iter()
            .map(|Metadata(k, v)| (k, v))
            .collect::<super::BTreeMap<_, _>>();

        if let Some(application) = self.application.filter(|v| !v.is_empty()) {
            metadata.insert("bpfman.io/application".into(), application);
        }

        Ok(LoadRequest {
            source,
            first,
            remaining: programs.collect(),
            metadata,
            globals: self
                .globals
                .into_iter()
                .map(|Global(k, v)| (k, v))
                .collect(),
            map_owner_id: self.map_owner_id.and_then(std::num::NonZeroU32::new),
            output: self.output,
        })
    }
}

impl LoadRequest {
    fn prepare(
        self,
        layout: &bpfman_fs::RuntimeLayout,
        cancellation: &bpfman_runtime::Cancellation,
        config: Option<&std::path::Path>,
        cache: &bpfman_fs::RuntimeLayout,
        lock_timeout: std::time::Duration,
        trust_cache: Option<&bpfman_fs::RuntimeLayout>,
    ) -> Result<PreparedLoad, crate::error::Error> {
        let Self {
            source,
            first,
            remaining,
            metadata,
            globals,
            map_owner_id,
            output,
        } = self;
        if layout.root().to_str().is_none() {
            return Err(anyhow::anyhow!("load persistence requires a UTF-8 runtime path").into());
        }
        let kernel = bpfman_kernel_aya::Kernel::default();
        let request = match source {
            LoadSource::File(path) => bpfman_runtime::PreparedProgram::new_with_cancellation(
                &kernel,
                &path,
                first,
                metadata,
                cancellation,
            )?,
            LoadSource::Image {
                reference,
                pull_policy,
                auth,
            } => {
                let config = super::super::config::Config::load(config)?;
                let signing = config.trust(config.signing()?, trust_cache, lock_timeout)?;
                let parsed = bpfman_image_oci::Reference::parse(&reference)?;
                let image = bpfman_image_oci::pull(
                    &parsed,
                    Some(&auth),
                    &signing,
                    pull_policy.into(),
                    cache,
                    lock_timeout,
                    cancellation.flag(),
                    |bytes| {
                        for spec in std::iter::once(&first).chain(&remaining) {
                            kernel.validate_object(bytes, spec)?;
                        }
                        if globals.is_empty() {
                            Ok(())
                        } else {
                            kernel.validate_globals(bytes, &globals)
                        }
                    },
                )?;
                let provenance = bpfman_model::ProgramSource::Image {
                    url: reference,
                    digest: image.digest().into(),
                    pull_policy: pull_policy.into(),
                };
                bpfman_runtime::PreparedProgram::from_image(
                    &kernel,
                    image.bytes().to_vec(),
                    provenance,
                    first,
                    metadata,
                    cancellation,
                )?
            }
        }
        .with_globals(&kernel, globals)?;
        let request = match map_owner_id {
            Some(owner) => request.with_map_owner(owner),
            None => request,
        };
        Ok(PreparedLoad {
            request: request
                .with_additional_programs(&bpfman_kernel_aya::Kernel::default(), remaining)?,
            output,
        })
    }
}

impl PreparedLoad {
    pub(crate) fn execute<S: bpfman_store::OpenStore + bpfman_store::CommitLoad>(
        self,
        bpfman: &bpfman_runtime::Bpfman<S, bpfman_kernel_aya::Kernel>,
        cancellation: &bpfman_runtime::Cancellation,
    ) -> Result<(), crate::error::Error> {
        let stored = bpfman.load_batch_with_cancellation(self.request, cancellation)?;

        // Output is deliberately outside the operation: delivery failure must
        // never compensate a committed program.
        crate::output::loaded_programs(
            &mut std::io::stdout().lock(),
            &stored,
            match self.output {
                super::LoadOutput::Text => crate::cli::OutputFormat::Text,
                super::LoadOutput::Json => crate::cli::OutputFormat::Json,
            },
        )?;

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::{Cli, Command, ProgramCommand};
    use bpfman_model::ProgramSpec;
    use clap::Parser;

    fn request(args: &[&str]) -> Result<LoadRequest, clap::Error> {
        let cli = Cli::try_parse_from(
            ["bpfman", "program", "load"]
                .into_iter()
                .chain(args.iter().copied()),
        )?;

        let Command::Program(program) = cli.command else {
            return Err(invalid("expected program"));
        };
        match program.into_command() {
            ProgramCommand::Load(source) => source.into_command().into_request(),
            _ => Err(invalid("expected load")),
        }
    }

    #[test]
    fn constructs_typed_batch_and_go_compatible_overrides() -> Result<(), clap::Error> {
        let req = request(&[
            "file",
            "missing.o",
            "--programs",
            "xdp:pass,tc:stats",
            "--programs",
            "fentry:enter:do_open",
            "-m",
            " k =old",
            "-m",
            "k=a=b,c",
            "-m",
            "bpfman.io/application=old",
            "-a",
            "demo",
            "-g",
            "counter=00",
            "-g",
            "counter=0X0102",
            "--map-owner-id",
            "42",
            "-o",
            "json",
        ])?;

        assert!(matches!(req.source, LoadSource::File(_)));
        assert!(matches!(req.first, ProgramSpec::Xdp(_)));
        assert!(
            matches!(&req.remaining[1], ProgramSpec::Fentry { target, .. } if target.as_str() == "do_open")
        );
        assert_eq!(req.metadata["k"], "a=b,c");
        assert_eq!(req.metadata["bpfman.io/application"], "demo");
        assert_eq!(req.globals["counter"], [1, 2]);
        assert_eq!(req.map_owner_id.map(|id| id.get()), Some(42));
        assert_eq!(req.output, super::super::LoadOutput::Json);

        Ok(())
    }

    #[test]
    fn image_options_are_source_specific_and_auth_is_redacted() -> Result<(), clap::Error> {
        let req = request(&[
            "image",
            "quay.io/bpfman/test:latest",
            "--programs",
            "lsm:check:file_open",
            "-p",
            " nEvEr ",
            "--registry-auth",
            "dXNlcjpwYXNzOndvcmQ=",
        ])?;

        assert!(matches!(req.first, ProgramSpec::Lsm { .. }));

        match req.source {
            LoadSource::Image {
                pull_policy, auth, ..
            } => {
                assert_eq!(pull_policy, super::super::PullPolicy::Never);
                assert!(!format!("{auth:?}").contains("pass:word"));
            }
            _ => return Err(invalid("expected authenticated image")),
        }

        assert!(
            request(&[
                "file",
                "x.o",
                "--programs",
                "xdp:p",
                "--pull-policy",
                "Never"
            ])
            .is_err()
        );

        Ok(())
    }
}
