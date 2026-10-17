use bpfman_image_oci::{Reference, TrustedIdentity, VerificationStatus};
use clap::Args;
use std::{io::Write, path::Path};

#[derive(Args)]
pub(crate) struct Verify {
    /// Image reference whose resolved digest is verified.
    #[arg(value_name = "IMAGE", value_parser = reference)]
    reference: String,
    #[command(flatten)]
    registry_auth: super::super::registry_auth::RegistryAuth,
    /// Allow unsigned images, overriding the configuration.
    #[arg(long, num_args = 0..=1, require_equals = true, default_missing_value = "true")]
    allow_unsigned: Option<bool>,
    /// Expected signing certificate identity, overriding configuration.
    #[arg(long, conflicts_with = "certificate_identity_regexp")]
    certificate_identity: Option<String>,
    /// Expected signing certificate identity regexp, overriding configuration.
    #[arg(long)]
    certificate_identity_regexp: Option<String>,
    /// Expected certificate OIDC issuer, overriding configuration.
    #[arg(long, conflicts_with = "certificate_oidc_issuer_regexp")]
    certificate_oidc_issuer: Option<String>,
    /// Expected certificate OIDC issuer regexp, overriding configuration.
    #[arg(long)]
    certificate_oidc_issuer_regexp: Option<String>,
}

fn reference(value: &str) -> Result<String, String> {
    Reference::parse(value).map_err(|e| e.to_string())?;
    Ok(value.into())
}

impl Verify {
    pub(super) fn execute(
        self,
        cancellation: &bpfman_runtime::Cancellation,
        config: Option<&Path>,
    ) -> Result<(), crate::error::Error> {
        let credentials = self
            .registry_auth
            .credentials()
            .unwrap_or_else(|error| error.exit());
        let identity = if self.certificate_identity.is_some()
            || self.certificate_identity_regexp.is_some()
            || self.certificate_oidc_issuer.is_some()
            || self.certificate_oidc_issuer_regexp.is_some()
        {
            Some(TrustedIdentity::new(
                self.certificate_identity.as_deref().unwrap_or(""),
                self.certificate_identity_regexp.as_deref().unwrap_or(""),
                self.certificate_oidc_issuer.as_deref().unwrap_or(""),
                self.certificate_oidc_issuer_regexp.as_deref().unwrap_or(""),
            )?)
        } else {
            None
        };
        // This command always enables verification, including when load policy
        // explicitly disables it. Optional flags replace the configured rule set.
        let policy = super::super::config::Config::load(config)?
            .verification(self.allow_unsigned, identity)?;
        let status = bpfman_image_oci::verify(
            &Reference::parse(&self.reference)?,
            credentials.as_ref(),
            &policy,
            cancellation.flag(),
        )?;
        let result = match status {
            VerificationStatus::Verified => "signature verified",
            VerificationStatus::UnsignedAccepted => "unsigned image accepted by policy",
            VerificationStatus::Disabled => {
                return Err(anyhow::anyhow!("image verify requires enabled verification").into());
            }
        };
        writeln!(
            std::io::stdout().lock(),
            "Image {}: {result}",
            self.reference
        )?;
        Ok(())
    }
}
