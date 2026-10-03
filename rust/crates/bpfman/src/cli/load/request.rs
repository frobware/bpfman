use std::collections::BTreeSet;

use clap::error::ErrorKind;

use super::{Global, LoadCommand, LoadOptions, LoadRequest, LoadSource, Metadata, parse};

impl LoadCommand {
    pub(crate) fn execute<S: bpfman_store::OpenStore + bpfman_store::CommitLoad>(
        self,
        store: &S,
        layout: &bpfman_fs::RuntimeLayout,
        timeout: std::time::Duration,
    ) -> anyhow::Result<()> {
        let request = self.into_request().unwrap_or_else(|error| error.exit());
        request.execute(store, layout, timeout)
    }

    fn into_request(self) -> Result<LoadRequest, clap::Error> {
        let (source, options) = match self {
            Self::File(args) => {
                if args.path.as_os_str().is_empty() {
                    return Err(invalid("object path must not be empty"));
                }
                (LoadSource::File(args.path), args.options)
            }
            Self::Image(args) => {
                let auth = args
                    .registry_auth
                    .map(parse::registry_auth)
                    .transpose()
                    .map_err(invalid)?
                    .flatten();
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
        let mut seen = BTreeSet::new();
        for program in &self.programs {
            if !seen.insert(program.name()) {
                return Err(invalid("each ELF program must be selected only once"));
            }
        }
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
            map_owner_id: self.map_owner_id,
            output: self.output,
        })
    }
}

impl LoadRequest {
    fn execute<S: bpfman_store::OpenStore + bpfman_store::CommitLoad>(
        self,
        store: &S,
        layout: &bpfman_fs::RuntimeLayout,
        timeout: std::time::Duration,
    ) -> anyhow::Result<()> {
        let Self {
            source,
            first,
            remaining,
            metadata,
            globals,
            map_owner_id,
            output,
        } = self;
        let path = match source {
            LoadSource::File(path) => path,
            LoadSource::Image {
                reference,
                pull_policy,
                auth,
            } => {
                let _image = (reference, pull_policy);
                if let Some(super::RegistryAuth { username, password }) = auth {
                    let _credentials = (username, password);
                }
                anyhow::bail!(
                    "program load image execution is not implemented in the Rust CLI; no runtime state was changed"
                );
            }
        };
        // Reject the complete unsupported request before touching even the source.
        let reason = if !remaining.is_empty() {
            Some("multiple programs")
        } else if !matches!(first, bpfman_model::ProgramSpec::Tracepoint(_)) {
            Some("program types other than tracepoint")
        } else if !globals.is_empty() {
            Some("global overrides")
        } else if map_owner_id.is_some() {
            Some("map-owner sharing")
        } else {
            None
        };
        if let Some(reason) = reason {
            anyhow::bail!(
                "program load file execution is not implemented for {reason}; no runtime state was changed"
            );
        }
        let bpfman_model::ProgramSpec::Tracepoint(name) = first else {
            anyhow::bail!("unsupported program type");
        };
        let stored =
            bpfman_runtime::load_tracepoint(store, layout, &path, name, &metadata, timeout)?;
        // Output is deliberately outside the operation: delivery failure must
        // never compensate a committed program.
        crate::output::program(
            &mut std::io::stdout().lock(),
            &stored,
            match output {
                super::LoadOutput::Text => crate::cli::OutputFormat::Text,
                super::LoadOutput::Json => crate::cli::OutputFormat::Json,
            },
            true,
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
        match cli.command {
            Command::Program {
                command: ProgramCommand::Load { source },
            } => source.into_request(),
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
    fn image_options_are_source_specific_and_auth_preserves_password_colons()
    -> Result<(), clap::Error> {
        let req = request(&[
            "image",
            "quay.io/bpfman/test:latest",
            "--programs",
            "lsm:check:file_open",
            "-p",
            "nEvEr",
            "--registry-auth",
            "dXNlcjpwYXNzOndvcmQ=",
        ])?;
        assert!(matches!(req.first, ProgramSpec::Lsm { .. }));
        match req.source {
            LoadSource::Image {
                pull_policy,
                auth: Some(auth),
                ..
            } => {
                assert_eq!(pull_policy, super::super::PullPolicy::Never);
                assert_eq!(auth.username, "user");
                assert_eq!(auth.password, "pass:word");
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
