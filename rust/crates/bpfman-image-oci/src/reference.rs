use crate::Error;

/// Validated Docker/OCI reference, normalized for registry access and cache keys.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Reference {
    pub(super) registry: String,
    pub(super) repository: String,
    pub(super) selector: String,
    normalized: String,
}

impl Reference {
    /// Parse a bare, tagged or SHA256 digest reference. Credentials and URL
    /// schemes are refused; the original operand is kept by the caller.
    pub fn parse(input: &str) -> Result<Self, Error> {
        if input.is_empty()
            || input.len() > 2048
            || input.contains("://")
            || input
                .bytes()
                .any(|b| b.is_ascii_whitespace() || !b.is_ascii())
        {
            return Err(Error::invalid("invalid OCI image reference"));
        }
        let (name, digest) = match input.split_once('@') {
            Some((name, digest)) => {
                validate_digest(digest)?;
                (name, Some(digest))
            }
            None => (input, None),
        };
        let (name, tag) = match name.rsplit_once(':') {
            Some((name, tag)) if !tag.contains('/') => {
                if tag.is_empty()
                    || tag.len() > 128
                    || !tag
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b"_.-".contains(&b))
                    || tag.starts_with(['.', '-'])
                {
                    return Err(Error::invalid("invalid OCI image tag"));
                }
                (name, Some(tag))
            }
            _ => (name, None),
        };
        let (registry, repository) = match name.split_once('/') {
            Some((host, path))
                if host.contains('.') || host.contains(':') || host == "localhost" =>
            {
                (host.to_owned(), path.to_owned())
            }
            _ => (
                "index.docker.io".into(),
                if name.contains('/') {
                    name.into()
                } else {
                    format!("library/{name}")
                },
            ),
        };
        // Hostnames and numeric ports only. Bracketed IPv6 can be added with
        // strict authority parsing separately; never interpret userinfo as a host.
        if registry.is_empty()
            || !registry
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b".:-".contains(&b))
            || registry.matches(':').count() > 1
        {
            return Err(Error::invalid("invalid OCI registry authority"));
        }
        if let Some((host, port)) = registry.split_once(':') {
            if host.is_empty() || port.parse::<u16>().ok().filter(|p| *p != 0).is_none() {
                return Err(Error::invalid("invalid OCI registry port"));
            }
        }
        if repository.len() > 255
            || repository.split('/').any(|p| {
                p.is_empty()
                    || p == "."
                    || p == ".."
                    || !p.bytes().all(|b| {
                        b.is_ascii_lowercase() || b.is_ascii_digit() || b"._-".contains(&b)
                    })
                    || !p.as_bytes().first().is_some_and(u8::is_ascii_alphanumeric)
                    || !p.as_bytes().last().is_some_and(u8::is_ascii_alphanumeric)
            })
        {
            return Err(Error::invalid("invalid OCI repository name"));
        }
        let selector = digest.or(tag).unwrap_or("latest").to_owned();
        let separator = if digest.is_some() { '@' } else { ':' };
        let normalized = format!("{registry}/{repository}{separator}{selector}");
        Ok(Self {
            registry,
            repository,
            selector,
            normalized,
        })
    }

    /// Fully qualified reference with the implicit latest tag made explicit.
    pub fn normalized(&self) -> &str {
        &self.normalized
    }

    pub(super) fn base(&self) -> String {
        let host = self.registry.split(':').next().unwrap_or("");
        let loopback = host == "localhost"
            || host
                .parse::<std::net::IpAddr>()
                .is_ok_and(|ip| ip.is_loopback());
        let registry = if self.registry == "index.docker.io" {
            "registry-1.docker.io"
        } else {
            &self.registry
        };
        format!(
            "{}://{registry}/v2/{}",
            if loopback { "http" } else { "https" },
            self.repository
        )
    }
}

pub(super) fn validate_digest(value: &str) -> Result<(), Error> {
    if value.strip_prefix("sha256:").is_some_and(|hex| {
        hex.len() == 64
            && hex
                .bytes()
                .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
    }) {
        Ok(())
    } else {
        Err(Error::invalid(
            "unsupported or malformed OCI digest; expected sha256",
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reference_forms_and_invalid_authorities() {
        for (input, expected) in [
            ("quay.io/team/image", "quay.io/team/image:latest"),
            ("alpine", "index.docker.io/library/alpine:latest"),
            ("localhost:5000/test:v1", "localhost:5000/test:v1"),
        ] {
            assert_eq!(
                Reference::parse(input).expect("valid").normalized(),
                expected
            );
        }
        for value in [
            "",
            "https://quay.io/a/b",
            "user:secret@quay.io/a/b",
            "a/b@sha256:123",
            "quay.io/../a",
            "quay.io/a?b",
            "quay.io/a\nb",
            "localhost:abc/test",
        ] {
            assert!(Reference::parse(value).is_err(), "{value}");
        }
    }
}
