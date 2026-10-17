//! Shared image credential admission. Decode after Clap to keep values out of errors.
use base64::{Engine, engine::general_purpose::STANDARD};
use bpfman_image_oci::Credentials;
use clap::Args;

#[derive(Args)]
pub(crate) struct RegistryAuth {
    /// Base64 username:password. Prefer BPFMAN_REGISTRY_AUTH to process arguments.
    #[arg(
        long,
        env = "BPFMAN_REGISTRY_AUTH",
        hide_env_values = true,
        hide_possible_values = true
    )]
    registry_auth: Option<String>,
}

impl RegistryAuth {
    pub(super) fn credentials(self) -> Result<Option<Credentials>, clap::Error> {
        self.registry_auth
            .map(parse)
            .transpose()
            .map(Option::flatten)
            .map_err(|message| clap::Error::raw(clap::error::ErrorKind::ValueValidation, message))
    }
}

fn parse(value: String) -> Result<Option<Credentials>, &'static str> {
    if value.is_empty() {
        return Ok(None);
    }
    if value.len() > 64 * 1024 {
        return Err("registry auth exceeds size limit");
    }
    let bytes = STANDARD
        .decode(value)
        .map_err(|_| "registry auth must be valid base64")?;
    let decoded = String::from_utf8(bytes)
        .map_err(|_| "registry auth must contain UTF-8 username:password")?;
    let (username, password) = decoded
        .split_once(':')
        .ok_or("registry auth requires username:password")?;
    Credentials::new(username, password)
        .map(Some)
        .map_err(|_| "registry auth requires nonempty username and password within the size limit")
}
