use crate::{
    Error, SigningPolicy,
    registry::{self, MANIFEST_LIMIT, Registry, check, hash},
    signing::Verification,
};
use base64::{Engine, engine::general_purpose::STANDARD};
use serde::Deserialize;
use serde_json::{Value, json};
use sigstore_verify::{
    VerificationPolicy, Verifier,
    trust_root::{SIGSTORE_PRODUCTION_TRUSTED_ROOT, TrustedRoot},
    types::{Bundle, DerCertificate},
};
use std::sync::atomic::AtomicBool;

const SIGNATURE: &str = "dev.cosignproject.cosign/signature";
const CERTIFICATE: &str = "dev.sigstore.cosign/certificate";
const CHAIN: &str = "dev.sigstore.cosign/chain";
const BUNDLE: &str = "dev.sigstore.cosign/bundle";
const REKOR: &str = "https://rekor.sigstore.dev/api/v1/log/entries/retrieve";

/// The admission decision made by an explicit signing policy.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum VerificationStatus {
    /// Signature verification was explicitly disabled.
    Disabled,
    /// No signatures exist and policy permits unsigned images.
    UnsignedAccepted,
    /// A signature, certificate, transparency evidence and signer policy passed.
    Verified,
}

/// Resolve and verify an image without creating cache or runtime state.
/// Multi-platform images are verified using the resolved index digest.
pub fn verify(
    reference: &crate::Reference,
    credentials: Option<&crate::Credentials>,
    policy: &SigningPolicy,
    cancelled: &AtomicBool,
) -> Result<VerificationStatus, Error> {
    let mut registry = Registry::new(reference, credentials, cancelled);
    let manifest = registry.manifest(&reference.selector)?;
    if !registry::is_index(&manifest) {
        registry::require_image(&manifest)?;
    }
    admit(&mut registry, &manifest.digest, policy)
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct LegacyBundle {
    #[serde(rename = "SignedEntryTimestamp")]
    signature: String,
    #[serde(rename = "Payload")]
    payload: LogPayload,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct LogPayload {
    body: String,
    integrated_time: i64,
    log_index: u64,
    #[serde(rename = "logID")]
    log_id: String,
}

pub(super) fn admit(
    registry: &mut Registry<'_>,
    digest: &str,
    policy: &SigningPolicy,
) -> Result<VerificationStatus, Error> {
    check(registry.flag)?;
    let Some(policy) = &policy.verification else {
        return Ok(VerificationStatus::Disabled);
    };
    crate::reference::validate_digest(digest)?;
    let selector = format!("{}.sig", digest.replace(':', "-"));
    let Some(manifest) = registry.signature_manifest(&selector)? else {
        return if policy.allow_unsigned {
            Ok(VerificationStatus::UnsignedAccepted)
        } else {
            Err(Error::invalid(
                "image has no signatures and unsigned images are not allowed",
            ))
        };
    };
    registry::require_image(&manifest)?;
    let layers = manifest.value["layers"]
        .as_array()
        .filter(|layers| layers.len() <= 64)
        .ok_or_else(|| Error::invalid("invalid Cosign signature layer count"))?;
    if layers.is_empty() {
        return if policy.allow_unsigned {
            Ok(VerificationStatus::UnsignedAccepted)
        } else {
            Err(Error::invalid(
                "image has no signatures and unsigned images are not allowed",
            ))
        };
    }
    let root = TrustedRoot::from_json(SIGSTORE_PRODUCTION_TRUSTED_ROOT)
        .map_err(|e| Error::caused("decode bundled Sigstore trust root", e))?;
    let verifier =
        Verifier::new(&root).map_err(|e| Error::caused("prepare Sigstore verifier", e))?;
    let mut last = None;
    for layer in layers {
        check(registry.flag)?;
        let outcome = (|| {
            let descriptor = registry::descriptor(layer)?;
            if descriptor.media_type != "application/vnd.dev.cosign.simplesigning.v1+json" {
                return Err(Error::invalid("unsupported Cosign signature media type"));
            }
            let payload = registry.blob(&descriptor, MANIFEST_LIMIT)?;
            verify_layer(&verifier, policy, digest, layer, &payload, registry.flag)
        })();
        match outcome {
            Ok(()) => return Ok(VerificationStatus::Verified),
            Err(error) if error.is_cancelled() => return Err(error),
            Err(error) => last = Some(error),
        }
    }
    Err(Error::caused(
        "signature verification failed: no matching signatures",
        last.ok_or_else(|| Error::invalid("missing signature verification result"))?,
    ))
}

fn annotation<'a>(layer: &'a Value, name: &str) -> Result<&'a str, Error> {
    layer["annotations"][name]
        .as_str()
        .filter(|value| !value.is_empty())
        .ok_or_else(|| Error::invalid("missing Cosign signature annotation"))
}

fn certificates(pem: &str) -> Result<Vec<Value>, Error> {
    let mut result = Vec::new();
    let mut rest = pem.trim();
    while !rest.is_empty() {
        let end = rest
            .find("-----END CERTIFICATE-----")
            .filter(|_| rest.starts_with("-----BEGIN CERTIFICATE-----"))
            .ok_or_else(|| Error::invalid("invalid Cosign certificate chain"))?
            + "-----END CERTIFICATE-----".len();
        let certificate = DerCertificate::from_pem(&rest[..end])
            .map_err(|e| Error::caused("decode Cosign certificate", e))?;
        result.push(json!({"rawBytes": STANDARD.encode(certificate.as_bytes())}));
        if result.len() > 16 {
            return Err(Error::invalid("Cosign certificate chain exceeds limit"));
        }
        rest = rest[end..].trim();
    }
    Ok(result)
}

fn log_entry(payload: LogPayload, signature: &str) -> Result<Value, Error> {
    if payload.integrated_time < 0 || payload.log_index > i64::MAX as u64 {
        return Err(Error::invalid("invalid Rekor integrated time or log index"));
    }
    crate::reference::validate_digest(&format!("sha256:{}", payload.log_id))?;
    let id: Vec<u8> = (0..64)
        .step_by(2)
        .map(|offset| u8::from_str_radix(&payload.log_id[offset..offset + 2], 16))
        .collect::<Result<_, _>>()
        .map_err(|e| Error::caused("decode Rekor log ID", e))?;
    let body = STANDARD
        .decode(&payload.body)
        .map_err(|e| Error::caused("decode Rekor body", e))?;
    let body: Value =
        serde_json::from_slice(&body).map_err(|e| Error::caused("decode Rekor entry", e))?;
    if body["kind"] != "hashedrekord" || body["apiVersion"] != "0.0.1" {
        return Err(Error::invalid("unsupported Cosign Rekor entry type"));
    }
    Ok(json!({
        "logIndex": payload.log_index.to_string(),
        "logId": {"keyId": STANDARD.encode(id)},
        "kindVersion": {"kind": "hashedrekord", "version": "0.0.1"},
        "integratedTime": payload.integrated_time.to_string(),
        "inclusionPromise": {"signedEntryTimestamp": signature},
        "canonicalizedBody": payload.body,
    }))
}

fn verify_layer(
    verifier: &Verifier,
    policy: &Verification,
    digest: &str,
    layer: &Value,
    payload: &[u8],
    flag: &AtomicBool,
) -> Result<(), Error> {
    let claims: Value =
        serde_json::from_slice(payload).map_err(|e| Error::caused("decode Cosign claims", e))?;
    if claims["critical"]["type"] != "cosign container image signature"
        || claims["critical"]["image"]["docker-manifest-digest"] != digest
    {
        return Err(Error::invalid(
            "Cosign claims do not bind the resolved image digest",
        ));
    }
    let signature = annotation(layer, SIGNATURE)?;
    let pem = annotation(layer, CERTIFICATE)?;
    let mut chain = certificates(pem)?;
    if chain.len() != 1 {
        return Err(Error::invalid(
            "Cosign signing certificate must contain one certificate",
        ));
    }
    if let Some(pem) = layer["annotations"][CHAIN].as_str() {
        chain.extend(certificates(pem)?);
    }
    let entries = match layer["annotations"].get(BUNDLE) {
        Some(Value::String(bundle)) => {
            let legacy: LegacyBundle = serde_json::from_str(bundle)
                .map_err(|e| Error::caused("decode Cosign Rekor bundle", e))?;
            vec![log_entry(legacy.payload, &legacy.signature)?]
        }
        Some(_) => return Err(Error::invalid("invalid Cosign Rekor bundle annotation")),
        None => online_entries(payload, signature, pem, flag)?,
    };
    let mut last = None;
    // Log lookup can return multiple candidates. Each complete candidate must
    // pass independently; never combine one entry's time with another's body.
    for entry in entries {
        check(flag)?;
        let bundle = converted_bundle(&chain, entry, payload, signature)?;
        let outcome = verifier.verify(payload, &bundle, &VerificationPolicy::any_identity());
        check(flag)?;
        match outcome {
            Ok(result) => {
                let subject = result
                    .identity()
                    .map(|identity| identity.as_str())
                    .unwrap_or("");
                let issuer = result.issuer().unwrap_or("");
                if policy.identities.is_empty()
                    || policy
                        .identities
                        .iter()
                        .any(|rule| rule.matches(subject, issuer))
                {
                    return Ok(());
                }
                return Err(Error::invalid(
                    "signing certificate identity or OIDC issuer is not trusted",
                ));
            }
            Err(error) => last = Some(error),
        }
    }
    match last {
        Some(error) => Err(Error::caused(
            "verify Cosign certificate, signature and transparency evidence",
            error,
        )),
        None => Err(Error::invalid("signature not found in transparency log")),
    }
}

fn converted_bundle(
    chain: &[Value],
    entry: Value,
    payload: &[u8],
    signature: &str,
) -> Result<Bundle, Error> {
    let value = json!({
        "mediaType": "application/vnd.dev.sigstore.bundle+json;version=0.1",
        "verificationMaterial": {
            "x509CertificateChain": {"certificates": chain},
            "tlogEntries": [entry],
        },
        "messageSignature": {
            "messageDigest": {"algorithm": "SHA2_256", "digest": STANDARD.encode(sigstore_verify::crypto::sha256(payload).as_bytes())},
            "signature": signature,
        },
    });
    serde_json::from_value(value).map_err(|e| Error::caused("decode converted Cosign bundle", e))
}

fn online_entries(
    payload: &[u8],
    signature: &str,
    pem: &str,
    flag: &AtomicBool,
) -> Result<Vec<Value>, Error> {
    check(flag)?;
    let body = serde_json::to_vec(&json!({"entries": [{
        "kind": "hashedrekord", "apiVersion": "0.0.1",
        "spec": {
            "data": {"hash": {"algorithm": "sha256", "value": hash(payload).trim_start_matches("sha256:")}},
            "signature": {"content": signature, "publicKey": {"content": STANDARD.encode(pem)}},
        },
    }]})).map_err(|e| Error::caused("encode Rekor lookup", e))?;
    // Fixed public service, no registry authorization and no redirects.
    let agent = crate::transport::agent(registry::agent_config(0));
    let response = agent
        .post(REKOR)
        .header("Content-Type", "application/json")
        .send(body);
    check(flag)?;
    let response = response.map_err(|e| Error::caused("query Rekor transparency log", e))?;
    if response.status() != 200 {
        return Err(Error::invalid("Rekor transparency log lookup failed"));
    }
    let body = registry::read_body(response, MANIFEST_LIMIT, flag)?;
    convert_online_entries(&body)
}

fn convert_online_entries(body: &[u8]) -> Result<Vec<Value>, Error> {
    let groups: Vec<std::collections::BTreeMap<String, Value>> =
        serde_json::from_slice(body).map_err(|e| Error::caused("decode Rekor lookup", e))?;
    let mut entries = Vec::new();
    for entry in groups.into_iter().flat_map(|group| group.into_values()) {
        if entries.len() >= 64 {
            return Err(Error::invalid("Rekor lookup exceeds entry limit"));
        }
        let payload = serde_json::from_value(entry.clone())
            .map_err(|e| Error::caused("decode Rekor lookup entry", e))?;
        let signature = entry["verification"]["signedEntryTimestamp"]
            .as_str()
            .ok_or_else(|| Error::invalid("Rekor lookup has no signed entry timestamp"))?;
        let mut converted = log_entry(payload, signature)?;
        let proof = &entry["verification"]["inclusionProof"];
        if !proof.is_object() {
            return Err(Error::invalid("Rekor lookup has no inclusion proof"));
        }
        // Hashes in the legacy Rekor HTTP API use hex; the bundle uses base64.
        let convert_hash = |value: &Value| -> Result<String, Error> {
            let value = value
                .as_str()
                .ok_or_else(|| Error::invalid("invalid Rekor proof hash"))?;
            let value = sigstore_verify::types::Sha256Hash::from_hex(value)
                .map_err(|e| Error::caused("decode Rekor proof hash", e))?;
            Ok(STANDARD.encode(value.as_bytes()))
        };
        let hashes = proof["hashes"]
            .as_array()
            .ok_or_else(|| Error::invalid("invalid Rekor proof hashes"))?
            .iter()
            .map(convert_hash)
            .collect::<Result<Vec<_>, _>>()?;
        converted["inclusionProof"] = json!({
            "logIndex": proof["logIndex"].as_u64().ok_or_else(|| Error::invalid("invalid Rekor proof index"))?.to_string(),
            "rootHash": convert_hash(&proof["rootHash"])?,
            "treeSize": proof["treeSize"].as_u64().ok_or_else(|| Error::invalid("invalid Rekor proof tree size"))?.to_string(),
            "hashes": hashes,
            "checkpoint": {"envelope": proof["checkpoint"]},
        });
        entries.push(converted);
    }
    Ok(entries)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn real_rekor_lookup_conversion_verifies_proof_and_checkpoint_and_rejects_tampering() {
        let capture: Value = serde_json::from_str(include_str!(
            "../../../tests/fixtures/signed-xdp-v0.5.6.json"
        ))
        .expect("capture");
        let manifest = STANDARD.decode(capture["routes"]["/manifests/sha256-89728a806e0cccfaf6491e560a2624dc8f4b070547196fd9e9ce64c44afdf01b.sig"].as_str().expect("signature manifest")).expect("base64");
        let manifest: Value = serde_json::from_slice(&manifest).expect("manifest");
        let layer = &manifest["layers"][0];
        let pem = annotation(layer, CERTIFICATE).expect("certificate");
        let mut chain = certificates(pem).expect("chain");
        chain.extend(
            certificates(annotation(layer, CHAIN).expect("chain annotation")).expect("chain"),
        );
        let payload = STANDARD
            .decode(
                capture["routes"][format!("/blobs/{}", layer["digest"].as_str().expect("digest"))]
                    .as_str()
                    .expect("payload"),
            )
            .expect("base64");
        let entry = convert_online_entries(include_bytes!(
            "../../../tests/fixtures/signed-xdp-v0.5.6-rekor.json"
        ))
        .expect("lookup")
        .remove(0);
        let root = TrustedRoot::from_json(SIGSTORE_PRODUCTION_TRUSTED_ROOT).expect("root");
        let verifier = Verifier::new(&root).expect("verifier");
        let policy = VerificationPolicy::any_identity();
        let signature = annotation(layer, SIGNATURE).expect("signature");
        let bundle = converted_bundle(&chain, entry.clone(), &payload, signature).expect("bundle");
        let result = verifier
            .verify(payload.as_slice(), &bundle, &policy)
            .expect("valid lookup proof");
        assert!(result.certificate_verified() && result.sct_verified() && result.tlog_verified());
        for change in 0..5 {
            let mut entry = entry.clone();
            match change {
                0 => entry["inclusionProof"]["rootHash"] = json!(STANDARD.encode([0; 32])),
                1 => entry["inclusionProof"]["hashes"][0] = json!(STANDARD.encode([0; 32])),
                2 => {
                    entry["inclusionProof"]["checkpoint"]["envelope"] = json!("invalid checkpoint")
                }
                3 => entry["inclusionPromise"]["signedEntryTimestamp"] = json!("YWR2ZXJzYXJ5"),
                _ => entry["integratedTime"] = json!("1"),
            }
            let accepted = converted_bundle(&chain, entry, &payload, signature)
                .ok()
                .is_some_and(|bundle| {
                    verifier
                        .verify(payload.as_slice(), &bundle, &policy)
                        .is_ok()
                });
            assert!(!accepted);
        }
    }
}
