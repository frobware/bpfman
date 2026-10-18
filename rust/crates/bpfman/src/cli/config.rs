use anyhow::Context;
use bpfman_model::{InterfaceName, XdpMode};
use serde::Deserialize;
use std::{collections::HashMap, path::Path};

const DEFAULT_CONFIG_PATH: &str = "/etc/bpfman/bpfman.toml";

#[derive(Default, Deserialize)]
pub(crate) struct Config {
    #[serde(default)]
    interfaces: HashMap<String, InterfaceConfig>,
    #[serde(default)]
    signing: Signing,
    #[serde(default)]
    logging: Logging,
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct Logging {
    level: String,
    format: String,
    components: HashMap<String, String>,
}

#[derive(Deserialize)]
#[serde(default)]
struct Signing {
    verify_enabled: bool,
    allow_unsigned: bool,
    trusted_identities: Vec<Identity>,
    trust_source: TrustSource,
    trusted_root: Option<std::path::PathBuf>,
}

#[derive(Default, Deserialize)]
#[serde(rename_all = "lowercase")]
enum TrustSource {
    #[default]
    Tuf,
    Bundled,
}

#[derive(Deserialize)]
#[serde(default)]
#[derive(Default)]
struct Identity {
    certificate_identity: String,
    certificate_identity_regexp: String,
    certificate_oidc_issuer: String,
    certificate_oidc_issuer_regexp: String,
}

impl Default for Signing {
    fn default() -> Self {
        Self {
            verify_enabled: true,
            allow_unsigned: true,
            trusted_identities: Vec::new(),
            trust_source: TrustSource::default(),
            trusted_root: None,
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
    pub(crate) fn json_logs(&self) -> bool {
        self.logging.format.trim().eq_ignore_ascii_case("json")
    }

    pub(super) fn trust(
        &self,
        policy: bpfman_image_oci::SigningPolicy,
        cache: Option<&bpfman_fs::RuntimeLayout>,
        timeout: std::time::Duration,
    ) -> anyhow::Result<bpfman_image_oci::SigningPolicy> {
        if !policy.verification_enabled() {
            return Ok(policy);
        }
        if let Some(path) = &self.signing.trusted_root {
            return Ok(policy.with_trusted_root_file(path.clone()));
        }
        if matches!(self.signing.trust_source, TrustSource::Bundled) {
            return Ok(policy);
        }
        let layout = match cache {
            Some(layout) => layout.clone(),
            None => {
                let root = if let Some(cache) =
                    std::env::var_os("XDG_CACHE_HOME").filter(|value| !value.is_empty())
                {
                    std::path::PathBuf::from(cache)
                } else {
                    std::path::PathBuf::from(std::env::var_os("HOME").context("Sigstore trust cache requires HOME, XDG_CACHE_HOME or --sigstore-cache-dir")?).join(".cache")
                };
                bpfman_fs::RuntimeLayout::try_from(root.join("bpfman/sigstore"))?
            }
        };
        Ok(policy.with_trust_cache(layout, timeout))
    }

    pub(super) fn signing(&self) -> anyhow::Result<bpfman_image_oci::SigningPolicy> {
        let policy = self.verification(None, None)?;
        Ok(if self.signing.verify_enabled {
            policy
        } else {
            bpfman_image_oci::SigningPolicy::disabled()
        })
    }

    pub(super) fn verification(
        &self,
        allow_unsigned: Option<bool>,
        override_identity: Option<bpfman_image_oci::TrustedIdentity>,
    ) -> anyhow::Result<bpfman_image_oci::SigningPolicy> {
        let identities = self
            .signing
            .trusted_identities
            .iter()
            .map(|identity| {
                bpfman_image_oci::TrustedIdentity::new(
                    &identity.certificate_identity,
                    &identity.certificate_identity_regexp,
                    &identity.certificate_oidc_issuer,
                    &identity.certificate_oidc_issuer_regexp,
                )
            })
            .collect::<Result<Vec<_>, _>>()?;
        // Validate the configured policy even when flags replace its identities.
        let configured = bpfman_image_oci::SigningPolicy::verify(
            self.signing.allow_unsigned,
            identities.clone(),
        )?;
        if allow_unsigned.is_none() && override_identity.is_none() {
            return Ok(configured);
        }
        Ok(bpfman_image_oci::SigningPolicy::verify(
            allow_unsigned.unwrap_or(self.signing.allow_unsigned),
            override_identity.map_or(identities, |identity| vec![identity]),
        )?)
    }

    pub(crate) fn load(path: Option<&Path>) -> anyhow::Result<Self> {
        let path = path.filter(|path| !path.as_os_str().is_empty());
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
        let config: Self = toml::from_str(&contents)
            .with_context(|| format!("parse bpfman config {}", selected.display()))?;
        match config.logging.format.trim().to_ascii_lowercase().as_str() {
            "" | "text" | "json" => {}
            _ => anyhow::bail!("unknown log format: {:?}", config.logging.format),
        }
        let mut spec = config.logging.level.clone();
        if spec.is_empty() && !config.logging.components.is_empty() {
            spec = "info".into();
        }
        for (component, level) in &config.logging.components {
            spec.push_str(&format!(",{component}={level}"));
        }
        crate::telemetry::validate_spec(&spec)?;
        config.verification(None, None)?;
        Ok(config)
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
