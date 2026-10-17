use anyhow::Context;
use bpfman_model::{InterfaceName, XdpMode};
use serde::Deserialize;
use std::{collections::HashMap, path::Path};

const DEFAULT_CONFIG_PATH: &str = "/etc/bpfman/bpfman.toml";

#[derive(Default, Deserialize)]
pub(super) struct Config {
    #[serde(default)]
    interfaces: HashMap<String, InterfaceConfig>,
    #[serde(default)]
    signing: Signing,
}

#[derive(Deserialize)]
#[serde(default)]
struct Signing {
    verify_enabled: bool,
}

impl Default for Signing {
    fn default() -> Self {
        Self {
            verify_enabled: true,
        }
    }
}

#[derive(Deserialize)]
#[serde(default)]
struct InterfaceConfig {
    xdp_mode: String,
}

impl Default for InterfaceConfig {
    fn default() -> Self {
        Self {
            xdp_mode: XdpMode::default().as_str().into(),
        }
    }
}

impl Config {
    pub(super) fn admit_public_image(&self) -> anyhow::Result<()> {
        if self.signing.verify_enabled {
            anyhow::bail!(
                "OCI signature verification is not implemented; public image loading requires explicit [signing] verify_enabled = false"
            );
        }
        Ok(())
    }

    pub(super) fn load(path: Option<&Path>) -> anyhow::Result<Self> {
        let selected = path.unwrap_or(Path::new(DEFAULT_CONFIG_PATH));
        let contents = match std::fs::read_to_string(selected) {
            Ok(contents) => contents,
            Err(error) if path.is_none() && error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Self::default());
            }
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("read bpfman config {}", selected.display()));
            }
        };
        toml::from_str(&contents)
            .with_context(|| format!("parse bpfman config {}", selected.display()))
    }

    pub(super) fn xdp_mode(&self, interface: &InterfaceName) -> anyhow::Result<XdpMode> {
        let spelling = self
            .interfaces
            .get(interface.as_str())
            .map_or(XdpMode::default().as_str(), |config| {
                config.xdp_mode.as_str()
            });
        spelling
            .parse()
            .map_err(|error: bpfman_model::InvalidXdpMode| {
                anyhow::Error::new(error).context(format!(
                    "invalid xdp_mode for interface {}: {spelling}",
                    interface.as_str()
                ))
            })
    }
}
