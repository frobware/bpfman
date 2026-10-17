use crate::{Error, registry::hash};
use regex::Regex;
use sigstore_verify::trust_root::SIGSTORE_PRODUCTION_TRUSTED_ROOT;

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
}

#[derive(Clone, Debug)]
pub(super) struct Verification {
    pub(super) allow_unsigned: bool,
    pub(super) identities: Vec<TrustedIdentity>,
    id: String,
}

impl SigningPolicy {
    /// Explicitly disable all signature verification, including for signed images.
    pub fn disabled() -> Self {
        Self { verification: None }
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
        let mut canonical: Vec<_> = identities
            .iter()
            .map(|rule| (rule.subject.canonical(), rule.issuer.canonical()))
            .collect();
        canonical.sort_unstable();
        canonical.dedup();
        // Include implementation and trust material: a changed root or verifier
        // must never inherit another build's cached admission decision.
        let policy = serde_json::to_vec(&(
            "sigstore-verify-0.14.0-cosign-v1",
            hash(SIGSTORE_PRODUCTION_TRUSTED_ROOT.as_bytes()),
            allow_unsigned,
            canonical,
        ))
        .map_err(|e| Error::caused("encode signing policy", e))?;
        Ok(Self {
            verification: Some(Verification {
                allow_unsigned,
                identities,
                id: hash(&policy),
            }),
        })
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
}
