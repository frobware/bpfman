use crate::{Error, registry::hash};
use regex::Regex;
use sigstore_verify::trust_root::SIGSTORE_PRODUCTION_TRUSTED_ROOT;
use std::{
    sync::{Arc, atomic::AtomicBool},
    time::Duration,
};

/// Validated certificate identity and OIDC issuer rule. Rules are ORed.
#[derive(Clone, Debug)]
pub struct TrustedIdentity {
    subject: Matcher,
    issuer: Matcher,
}

#[derive(Clone, Debug)]
enum Matcher {
    Exact(String),
    Pattern(Regex),
}

impl Matcher {
    fn new(exact: &str, pattern: &str) -> Result<Self, Error> {
        let (exact, pattern) = (exact.trim(), pattern.trim());
        match (exact.is_empty(), pattern.is_empty()) {
            (false, true) => Ok(Self::Exact(exact.into())),
            (true, false) => Regex::new(&format!("^(?:{pattern})$"))
                .map(Self::Pattern)
                .map_err(|e| Error::caused("invalid signing identity regexp", e)),
            (false, false) => Err(Error::invalid(
                "signing identity exact and regexp forms are mutually exclusive",
            )),
            (true, true) => Err(Error::invalid(
                "certificate identity and OIDC issuer must be configured together",
            )),
        }
    }

    fn matches(&self, value: &str) -> bool {
        match self {
            Self::Exact(expected) => expected == value,
            Self::Pattern(pattern) => pattern.is_match(value),
        }
    }

    fn canonical(&self) -> (&str, &str) {
        match self {
            Self::Exact(value) => ("exact", value),
            Self::Pattern(pattern) => ("regexp", pattern.as_str()),
        }
    }

    fn wildcard(&self) -> bool {
        matches!(self, Self::Pattern(pattern) if matches!(pattern.as_str(), "^(?:.*)$" | "^(?:^.*$)$" | "^(?:^(?:.*)$)$"))
    }
}

impl TrustedIdentity {
    /// Refine configuration strings. Exactly one form of each field is required;
    /// regexp matching is anchored to the entire certificate value.
    pub fn new(
        subject: &str,
        subject_regexp: &str,
        issuer: &str,
        issuer_regexp: &str,
    ) -> Result<Self, Error> {
        Ok(Self {
            subject: Matcher::new(subject, subject_regexp)?,
            issuer: Matcher::new(issuer, issuer_regexp)?,
        })
    }

    pub(super) fn matches(&self, subject: &str, issuer: &str) -> bool {
        self.subject.matches(subject) && self.issuer.matches(issuer)
    }
}

/// Signing admission, independent of image pull policy and registry credentials.
#[derive(Clone, Debug)]
pub struct SigningPolicy {
    pub(super) verification: Option<Verification>,
    trust: Option<crate::trust::TrustCache>,
}

#[derive(Clone, Debug)]
pub(super) struct Verification {
    pub(super) allow_unsigned: bool,
    pub(super) identities: Vec<TrustedIdentity>,
    id: String,
    pub(super) root: Arc<str>,
}

impl SigningPolicy {
    /// Explicitly disable all signature verification, including for signed images.
    pub fn disabled() -> Self {
        Self {
            verification: None,
            trust: None,
        }
    }

    /// Verify keyless Cosign signatures against the bundled Sigstore production
    /// trust root. An empty identity list accepts any valid Sigstore signer.
    pub fn verify(allow_unsigned: bool, identities: Vec<TrustedIdentity>) -> Result<Self, Error> {
        if identities.len() > 1
            && identities
                .iter()
                .any(|rule| rule.subject.wildcard() && rule.issuer.wildcard())
        {
            return Err(Error::invalid(
                "wildcard signing identity cannot be mixed with specific identities",
            ));
        }
        let root: Arc<str> = SIGSTORE_PRODUCTION_TRUSTED_ROOT.into();
        let mut policy = Self {
            verification: Some(Verification {
                allow_unsigned,
                identities,
                id: String::new(),
                root,
            }),
            trust: None,
        };
        policy.identify()?;
        Ok(policy)
    }

    /// Refresh Sigstore production trust through TUF before image admission.
    /// Construction performs no I/O. Operations reuse an authenticated cache for
    /// up to 24 hours, subject to all metadata expiry checks. Never does no network
    /// I/O and requires a valid cache. Disabled verification never accesses TUF.
    pub fn with_trust_cache(mut self, layout: bpfman_fs::RuntimeLayout, timeout: Duration) -> Self {
        if self.verification.is_some() {
            self.trust = Some(crate::trust::TrustCache { layout, timeout });
        }
        self
    }

    pub(super) fn resolve(&self, online: bool, flag: &AtomicBool) -> Result<Self, Error> {
        let mut policy = self.clone();
        if let Some(cache) = &self.trust {
            let root = crate::trust::resolve(cache, online, flag)?;
            if let Some(verification) = &mut policy.verification {
                verification.root = root.into();
            }
            policy.trust = None;
            policy.identify()?;
        }
        Ok(policy)
    }

    fn identify(&mut self) -> Result<(), Error> {
        let Some(verification) = &mut self.verification else {
            return Ok(());
        };
        let mut canonical: Vec<_> = verification
            .identities
            .iter()
            .map(|rule| (rule.subject.canonical(), rule.issuer.canonical()))
            .collect();
        canonical.sort_unstable();
        canonical.dedup();
        let policy = serde_json::to_vec(&(
            "sigstore-verify-0.14.0-cosign-v1",
            hash(verification.root.as_bytes()),
            verification.allow_unsigned,
            canonical,
        ))
        .map_err(|e| Error::caused("encode signing policy", e))?;
        verification.id = hash(&policy);
        Ok(())
    }

    /// Whether this policy requires signature admission.
    pub fn verification_enabled(&self) -> bool {
        self.verification.is_some()
    }

    pub(super) fn id(&self) -> &str {
        self.verification
            .as_ref()
            .map_or("verification-disabled-v1", |verification| &verification.id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::signed_image;

    #[test]
    fn identity_pairs_are_anchored_and_configuration_is_unambiguous() {
        let rule = TrustedIdentity::new("", "user|release", "issuer", "").expect("rule");
        assert!(rule.matches("release", "issuer"));
        assert!(!rule.matches("untrusted-release", "issuer"));
        assert!(!rule.matches("release", "untrusted-issuer"));
        for fields in [
            ("", "", "issuer", ""),
            ("subject", "", "", ""),
            ("subject", ".*", "issuer", ""),
            ("subject", "", "issuer", ".*"),
            ("", "[", "issuer", ""),
        ] {
            assert!(TrustedIdentity::new(fields.0, fields.1, fields.2, fields.3).is_err());
        }
        for wildcard in [".*", "^.*$", "^(?:.*)$"] {
            let wildcard = TrustedIdentity::new("", wildcard, "", wildcard).expect("wildcard");
            assert!(SigningPolicy::verify(false, vec![wildcard, rule.clone()]).is_err());
        }
    }

    #[test]
    fn policy_identity_is_order_independent_and_binds_unsigned_admission() {
        let first = TrustedIdentity::new("subject", "", "issuer", "").expect("rule");
        let second = TrustedIdentity::new("release", "", "issuer", "").expect("rule");
        let original =
            SigningPolicy::verify(false, vec![first.clone(), second.clone()]).expect("policy");
        let reordered =
            SigningPolicy::verify(false, vec![second.clone(), first.clone(), first.clone()])
                .expect("policy");
        let permissive = SigningPolicy::verify(true, vec![first, second]).expect("policy");
        assert_eq!(original.id(), reordered.id());
        assert_ne!(original.id(), permissive.id());
        assert_ne!(original.id(), SigningPolicy::disabled().id());
    }
    #[test]
    fn changed_trust_material_requires_image_revalidation_before_cache_reuse() {
        use bpfman_model::ImagePullPolicy::{Always, IfNotPresent, Never};
        use std::sync::atomic::Ordering;
        let server = signed_image::registry();
        let temp = tempfile::tempdir().expect("temporary");
        let layout =
            bpfman_fs::RuntimeLayout::try_from(temp.path().join("images")).expect("layout");
        let flag = AtomicBool::new(false);
        let reference = crate::Reference::parse(&server.reference).expect("reference");
        let original = SigningPolicy::verify(
            false,
            vec![
                TrustedIdentity::new(signed_image::SUBJECT, "", signed_image::ISSUER, "")
                    .expect("identity"),
            ],
        )
        .expect("policy");
        let resolve = |policy: &SigningPolicy, mode| {
            crate::pull(
                &reference,
                None,
                policy,
                mode,
                &layout,
                Duration::ZERO,
                &flag,
                |_| Ok::<_, std::convert::Infallible>(()),
            )
        };
        let image = resolve(&original, Always).expect("signed image");
        let mut changed = original.clone();
        // TUF target bytes, even with the same semantic keys, are bound into
        // admission. Actual root rotations also change this identity.
        changed.verification.as_mut().expect("verification").root =
            format!("{}\n", SIGSTORE_PRODUCTION_TRUSTED_ROOT).into();
        changed.identify().expect("policy identity");
        assert_ne!(original.id(), changed.id());
        let count = server.requests.load(Ordering::Relaxed);
        assert!(resolve(&changed, Never).is_err());
        assert_eq!(server.requests.load(Ordering::Relaxed), count);
        assert_eq!(
            resolve(&changed, IfNotPresent).expect("reverified").bytes(),
            image.bytes()
        );
        let count = server.requests.load(Ordering::Relaxed);
        assert!(resolve(&original, Never).is_err());
        assert_eq!(
            resolve(&changed, Never).expect("new policy cached").bytes(),
            image.bytes()
        );
        assert_eq!(server.requests.load(Ordering::Relaxed), count);
        assert_eq!(image.digest(), server.digest);
        assert!(server.bearer.load(Ordering::Relaxed));
    }
}
