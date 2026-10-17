//! Deferred, bounded operator-selected Sigstore trust material.
use crate::{Error, registry::check};
use sigstore_verify::{Verifier, trust_root::TrustedRoot};
use std::{io::Read, path::Path, sync::atomic::AtomicBool};

const LIMIT: usize = 1024 * 1024;
const MEDIA_TYPE: &str = "application/vnd.dev.sigstore.trustedroot+json;version=0.1";

pub(super) fn resolve(path: &Path, flag: &AtomicBool) -> Result<String, Error> {
    check(flag)?;
    let file = bpfman_fs::open_sigstore_trust_root(path)
        .map_err(|e| Error::caused("open selected Sigstore trust root", e))?;
    let bytes = read(file, flag)?;
    validate(&bytes, flag)?;
    Ok(bytes)
}

fn read(reader: impl Read, flag: &AtomicBool) -> Result<String, Error> {
    let mut reader = reader.take((LIMIT + 1) as u64);
    let mut bytes = Vec::new();
    let mut chunk = [0; 16 * 1024];
    loop {
        check(flag)?;
        let count = match reader.read(&mut chunk) {
            Ok(count) => count,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(Error::caused("read selected Sigstore trust root", e)),
        };
        check(flag)?;
        if count == 0 {
            break;
        }
        bytes.extend_from_slice(&chunk[..count]);
        if bytes.len() > LIMIT {
            return Err(Error::invalid("selected Sigstore trust root exceeds 1 MiB"));
        }
    }
    String::from_utf8(bytes)
        .map_err(|_| Error::invalid("selected Sigstore trust root is not UTF-8"))
}

fn validate(bytes: &str, flag: &AtomicBool) -> Result<(), Error> {
    check(flag)?;
    // Parser diagnostics may quote operator input. Keep root contents out of
    // errors and tracing, including malformed keys and certificate bytes.
    let root = TrustedRoot::from_json(bytes)
        .map_err(|_| Error::invalid("invalid selected Sigstore trust root JSON"))?;
    if root.media_type != MEDIA_TYPE {
        return Err(Error::invalid(
            "unsupported selected Sigstore trust root media type",
        ));
    }
    if root.certificate_authorities.is_empty() || root.tlogs.is_empty() || root.ctlogs.is_empty() {
        return Err(Error::invalid(
            "selected Sigstore trust root requires Fulcio, Rekor and CT authorities",
        ));
    }
    Verifier::new(&root)
        .map_err(|_| Error::invalid("invalid selected Sigstore trust root material"))?;
    check(flag)
}

#[cfg(test)]
mod tests;
