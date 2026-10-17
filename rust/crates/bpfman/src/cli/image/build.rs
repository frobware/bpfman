//! Front-end build contract. Retain validated bytes before any registry effects.
use anyhow::{Context, bail};
use bpfman_image_oci::{Bytecode, Publication};
use clap::Args;
use std::{
    collections::{BTreeMap, BTreeSet},
    io::Read,
    path::PathBuf,
};

#[derive(Args)]
pub(crate) struct Inputs {
    /// One host BYTECODE file, or linux/arch=BYTECODE mappings.
    #[arg(value_name = "BYTECODE", conflicts_with = "cilium_ebpf_project")]
    bytecode: Vec<String>,
    /// Directory containing architecture-named cilium/bpf2go objects.
    #[arg(short = 'c', long, value_name = "DIR")]
    cilium_ebpf_project: Option<PathBuf>,
}

struct Slot {
    arch: &'static str,
    argument: &'static str,
    little: bool,
    cilium: &'static [&'static str],
}

const SLOTS: &[Slot] = &[
    Slot {
        arch: "386",
        argument: "BC_386_EL",
        little: true,
        cilium: &["386"],
    },
    Slot {
        arch: "amd64",
        argument: "BC_AMD64_EL",
        little: true,
        cilium: &["x86"],
    },
    Slot {
        arch: "arm",
        argument: "BC_ARM_EL",
        little: true,
        cilium: &["arm"],
    },
    Slot {
        arch: "arm64",
        argument: "BC_ARM64_EL",
        little: true,
        cilium: &["arm64"],
    },
    Slot {
        arch: "loong64",
        argument: "BC_LOONG64_EL",
        little: true,
        cilium: &["loongarch", "loong64"],
    },
    Slot {
        arch: "mips",
        argument: "BC_MIPS_EB",
        little: false,
        cilium: &["mips"],
    },
    Slot {
        arch: "mipsle",
        argument: "BC_MIPSLE_EL",
        little: true,
        cilium: &["mipsle"],
    },
    Slot {
        arch: "mips64",
        argument: "BC_MIPS64_EB",
        little: false,
        cilium: &["mips64"],
    },
    Slot {
        arch: "mips64le",
        argument: "BC_MIPS64LE_EL",
        little: true,
        cilium: &["mips64le"],
    },
    Slot {
        arch: "ppc64",
        argument: "BC_PPC64_EB",
        little: false,
        cilium: &["powerpc", "ppc64"],
    },
    Slot {
        arch: "ppc64le",
        argument: "BC_PPC64LE_EL",
        little: true,
        cilium: &["powerpc", "ppc64le"],
    },
    Slot {
        arch: "riscv64",
        argument: "BC_RISCV64_EL",
        little: true,
        cilium: &["riscv", "riscv64"],
    },
    Slot {
        arch: "s390x",
        argument: "BC_S390X_EB",
        little: false,
        cilium: &["s390", "s390x"],
    },
];

struct Input {
    slot: &'static Slot,
    path: PathBuf,
}

enum Source {
    Host(PathBuf),
    Platforms(Vec<Input>),
}

impl Inputs {
    fn source(self) -> anyhow::Result<Source> {
        if let Some(dir) = self.cilium_ebpf_project {
            let mut entries = std::fs::read_dir(&dir)?.collect::<Result<Vec<_>, _>>()?;
            entries.sort_by_key(|entry| entry.file_name());
            let mut inputs = Vec::new();
            for entry in entries {
                if entry.file_type()?.is_dir()
                    || entry.path().extension().is_none_or(|ext| ext != "o")
                {
                    continue;
                }
                let name = entry.file_name().to_string_lossy().to_ascii_lowercase();
                let parts = name
                    .trim_end_matches(".o")
                    .split(['_', '-', '.'])
                    .collect::<Vec<_>>();
                let endian = parts
                    .iter()
                    .rev()
                    .find(|p| **p == "bpfel" || **p == "bpfeb");
                let slot = parts
                    .iter()
                    .find_map(|part| {
                        SLOTS.iter().find(|slot| {
                            slot.cilium.contains(part)
                                && endian.is_some_and(|e| (*e == "bpfel") == slot.little)
                        })
                    })
                    .with_context(|| {
                        format!(
                            "unrecognised cilium/ebpf bytecode file {}",
                            entry.path().display()
                        )
                    })?;
                inputs.push(Input {
                    slot,
                    path: entry.path(),
                });
            }
            inputs.sort_by_key(|input| input.slot.arch);
            return platforms(inputs);
        }
        let mut bare = Vec::new();
        let mut inputs = Vec::new();
        for value in self.bytecode {
            if let Some((platform, path)) = value.split_once('=').filter(|(p, _)| p.contains('/')) {
                if path.is_empty() {
                    bail!("bytecode path is required for {platform}");
                }
                let slot = SLOTS
                    .iter()
                    .find(|slot| platform == format!("linux/{}", slot.arch))
                    .with_context(|| format!("unsupported OCI platform {platform:?}"))?;
                inputs.push(Input {
                    slot,
                    path: path.into(),
                });
            } else {
                bare.push(PathBuf::from(value));
            }
        }
        if !inputs.is_empty() {
            if !bare.is_empty() {
                bail!("cannot mix bare bytecode inputs with platform-mapped inputs");
            }
            return platforms(inputs);
        }
        if bare.len() > 1 {
            bail!(
                "cannot infer OCI platforms from multiple BPF objects; pass one BYTECODE file, linux/arch=BYTECODE mappings, or --cilium-ebpf-project"
            );
        }
        Ok(Source::Host(
            bare.pop().context("one bytecode input is required")?,
        ))
    }

    pub(super) fn prepare(
        self,
        cancellation: &bpfman_runtime::Cancellation,
    ) -> Result<Plan, crate::error::Error> {
        check(cancellation)?;
        let source = self.source()?;
        let (inputs, multi) = match source {
            Source::Host(path) => {
                let arch = match std::env::consts::ARCH {
                    "x86_64" => "amd64",
                    "aarch64" => "arm64",
                    "x86" => "386",
                    "powerpc64" if cfg!(target_endian = "little") => "ppc64le",
                    "powerpc64" => "ppc64",
                    "loongarch64" => "loong64",
                    other => other,
                };
                let slot = SLOTS
                    .iter()
                    .find(|slot| slot.arch == arch)
                    .context("unsupported host OCI platform")?;
                (vec![Input { slot, path }], false)
            }
            Source::Platforms(inputs) => (inputs, true),
        };
        let mut platforms = Vec::new();
        let mut arguments = Vec::new();
        let mut programs = BTreeMap::new();
        let mut maps = BTreeMap::new();
        let mut images = Vec::new();
        for input in inputs {
            check(cancellation)?;
            let mut file = std::fs::File::open(&input.path).context("open bytecode ELF")?;
            let mut bytes = Vec::new();
            let mut buffer = [0; 16 * 1024];
            loop {
                check(cancellation)?;
                let count = file.read(&mut buffer).context("read bytecode ELF")?;
                if count == 0 {
                    break;
                }
                if bytes.len() + count > 16 * 1024 * 1024 {
                    return Err(anyhow::anyhow!("bytecode exceeds size limit").into());
                }
                bytes.extend_from_slice(&buffer[..count]);
            }
            let metadata = bpfman_kernel_aya::inspect_bytecode(&bytes, input.slot.little)
                .map_err(anyhow::Error::from)?;
            check(cancellation)?;
            if images.is_empty() {
                programs = metadata.programs;
                maps = metadata.maps;
            }
            if multi {
                platforms.push(format!("linux/{}", input.slot.arch));
            }
            let argument = if multi {
                input.slot.argument
            } else {
                "BYTECODE_FILE"
            };
            arguments.push((argument.into(), input.path.to_string_lossy().into_owned()));
            let filename = input
                .path
                .file_name()
                .and_then(|p| p.to_str())
                .context("bytecode filename is not UTF-8")?;
            images.push(Bytecode::new(input.slot.arch, filename, bytes)?);
        }
        let publication = if multi {
            Publication::Index(images)
        } else {
            Publication::Image(images.pop().context("missing bytecode input")?)
        };
        Ok(Plan {
            platforms,
            arguments,
            programs,
            maps,
            publication,
        })
    }
}

fn check(cancellation: &bpfman_runtime::Cancellation) -> Result<(), crate::error::Error> {
    if cancellation.is_cancelled() {
        return Err(bpfman_image_oci::Error::cancelled().into());
    }
    Ok(())
}

fn platforms(inputs: Vec<Input>) -> anyhow::Result<Source> {
    if inputs.is_empty() || inputs.len() > 64 {
        bail!("between 1 and 64 bytecode inputs are required");
    }
    let mut seen = BTreeSet::new();
    for input in &inputs {
        if !seen.insert(input.slot.arch) {
            bail!(
                "platform linux/{} specified more than once",
                input.slot.arch
            );
        }
    }
    Ok(Source::Platforms(inputs))
}

pub(super) struct Plan {
    platforms: Vec<String>,
    arguments: Vec<(String, String)>,
    programs: BTreeMap<String, String>,
    maps: BTreeMap<String, String>,
    publication: Publication,
}

impl Plan {
    pub(super) fn render(&self, json: bool) -> anyhow::Result<String> {
        if json {
            let value = serde_json::json!({"platforms": self.platforms,
                "build_args": self.arguments.iter().cloned().collect::<BTreeMap<_, _>>(),
                "programs": self.programs, "maps": self.maps});
            return Ok(format!("{}\n", serde_json::to_string_pretty(&value)?));
        }
        let mut output = String::new();
        for (key, value) in &self.arguments {
            output.push_str(&format!("{key}={value}\n"));
        }
        output.push_str(&format!(
            "PROGRAMS={}\nMAPS={}\n",
            serde_json::to_string(&self.programs)?,
            serde_json::to_string(&self.maps)?
        ));
        Ok(output)
    }

    pub(super) fn publication(self) -> anyhow::Result<(Publication, BTreeMap<String, String>)> {
        let labels = BTreeMap::from([
            (
                "io.ebpf.programs".into(),
                serde_json::to_string(&self.programs)?,
            ),
            ("io.ebpf.maps".into(), serde_json::to_string(&self.maps)?),
        ]);
        Ok((self.publication, labels))
    }
}
