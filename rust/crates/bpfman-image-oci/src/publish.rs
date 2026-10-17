//! Deterministic OCI packaging. ELF interpretation is supplied by the caller.
use crate::{
    Credentials, Error, Reference,
    registry::{BYTECODE_LIMIT, Registry, check, hash},
};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::atomic::AtomicBool,
};

const CONFIG: &str = "application/vnd.oci.image.config.v1+json";
const MANIFEST: &str = "application/vnd.oci.image.manifest.v1+json";
const INDEX: &str = "application/vnd.oci.image.index.v1+json";
const LAYER: &str = "application/vnd.oci.image.layer.v1.tar";

/// Owned bytecode for one Linux OCI architecture. Callers validate the ELF
/// before construction; packaging never reopens the source file.
pub struct Bytecode {
    architecture: String,
    filename: String,
    bytes: Vec<u8>,
}

impl Bytecode {
    /// Retain a bounded payload and a single archive filename.
    pub fn new(architecture: &str, filename: &str, bytes: Vec<u8>) -> Result<Self, Error> {
        if architecture.is_empty()
            || !architecture
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_')
        {
            return Err(Error::invalid("invalid OCI architecture"));
        }
        if filename.is_empty()
            || filename.len() > 255
            || matches!(filename, "." | "..")
            || filename.contains(['/', '\\', '\0'])
        {
            return Err(Error::invalid("invalid bytecode archive filename"));
        }
        if bytes.is_empty() || bytes.len() > BYTECODE_LIMIT {
            return Err(Error::invalid("bytecode exceeds size limit or is empty"));
        }
        Ok(Self {
            architecture: architecture.into(),
            filename: filename.into(),
            bytes,
        })
    }
}

/// Publish either a host image or an explicitly requested platform index.
pub enum Publication {
    /// A single image, without an enclosing index.
    Image(Bytecode),
    /// An OCI index, including when only one platform was explicitly mapped.
    Index(Vec<Bytecode>),
}

struct Image {
    architecture: String,
    layer: Vec<u8>,
    config: Vec<u8>,
    manifest: Vec<u8>,
}

fn encode(value: &Value) -> Result<Vec<u8>, Error> {
    let bytes = serde_json::to_vec(value).map_err(|e| Error::caused("encode OCI image", e))?;
    if bytes.len() > crate::registry::MANIFEST_LIMIT {
        return Err(Error::invalid("OCI image metadata exceeds size limit"));
    }
    Ok(bytes)
}

fn descriptor(bytes: &[u8], media_type: &str) -> Value {
    json!({"mediaType": media_type, "size": bytes.len(), "digest": hash(bytes)})
}

fn image(
    input: Bytecode,
    labels: &BTreeMap<String, String>,
    timestamp: u64,
) -> Result<Image, Error> {
    let created = i64::try_from(timestamp)
        .ok()
        .and_then(|t| chrono::DateTime::from_timestamp(t, 0))
        .filter(|t| (0..=9999).contains(&chrono::Datelike::year(t)))
        .ok_or_else(|| Error::invalid("SOURCE_DATE_EPOCH is outside the OCI timestamp range"))?
        .to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    let mut archive = tar::Builder::new(Vec::new());
    let mut header = tar::Header::new_gnu();
    header.set_size(input.bytes.len() as u64);
    header.set_mode(0o644);
    header.set_mtime(timestamp);
    header.set_uid(0);
    header.set_gid(0);
    header.set_cksum();
    archive
        .append_data(&mut header, input.filename, input.bytes.as_slice())
        .map_err(|e| Error::caused("create bytecode layer", e))?;
    let layer = archive
        .into_inner()
        .map_err(|e| Error::caused("finish bytecode layer", e))?;
    let config = encode(
        &json!({"architecture": input.architecture, "os": "linux", "created": created,
        "config": {"Labels": labels}, "rootfs": {"type": "layers", "diff_ids": [hash(&layer)]}}),
    )?;
    let manifest = encode(&json!({"schemaVersion": 2, "mediaType": MANIFEST,
        "config": descriptor(&config, CONFIG), "layers": [descriptor(&layer, LAYER)]}))?;
    Ok(Image {
        architecture: input.architecture,
        layer,
        config,
        manifest,
    })
}

/// Package and publish OCI bytecode without cache or runtime state.
/// Inputs are packaged before I/O. Child manifests are addressed by digest;
/// the requested tag is written only after every dependency succeeds. A completed
/// final manifest PUT wins over concurrent cancellation. Registry transport
/// failures during that PUT can leave the publication outcome unknown.
pub fn publish(
    reference: &Reference,
    credentials: Option<&Credentials>,
    publication: Publication,
    labels: &BTreeMap<String, String>,
    timestamp: u64,
    cancelled: &AtomicBool,
) -> Result<String, Error> {
    check(cancelled)?;
    let (inputs, index) = match publication {
        Publication::Image(input) => (vec![input], false),
        Publication::Index(inputs) => (inputs, true),
    };
    if inputs.is_empty() || inputs.len() > 64 {
        return Err(Error::invalid(
            "OCI index must contain between 1 and 64 platforms",
        ));
    }
    let mut seen = BTreeSet::new();
    let mut images = Vec::new();
    for input in inputs {
        check(cancelled)?;
        if !seen.insert(input.architecture.clone()) {
            return Err(Error::invalid("duplicate OCI platform"));
        }
        images.push(image(input, labels, timestamp)?);
    }
    let mut manifests = Vec::new();
    for image in &images {
        let mut child = descriptor(&image.manifest, MANIFEST);
        child["platform"] = json!({"os": "linux", "architecture": image.architecture});
        manifests.push(child);
    }
    let (bytes, media_type) = if index {
        (
            encode(&json!({"schemaVersion": 2, "mediaType": INDEX, "manifests": manifests}))?,
            INDEX,
        )
    } else {
        (
            images
                .first()
                .ok_or_else(|| Error::invalid("missing bytecode image"))?
                .manifest
                .clone(),
            MANIFEST,
        )
    };
    publish_images(
        reference,
        credentials,
        &images,
        &bytes,
        media_type,
        index,
        cancelled,
    )
}

fn publish_images(
    reference: &Reference,
    credentials: Option<&Credentials>,
    images: &[Image],
    bytes: &[u8],
    media_type: &str,
    index: bool,
    cancelled: &AtomicBool,
) -> Result<String, Error> {
    let digest = hash(bytes);
    if reference.selector.starts_with("sha256:") && reference.selector != digest {
        return Err(Error::invalid(
            "publication digest does not match destination digest",
        ));
    }
    let mut registry = Registry::publisher(reference, credentials, cancelled);
    for image in images {
        registry.upload_blob(&image.layer)?;
        registry.upload_blob(&image.config)?;
        if index {
            registry.put_manifest(&hash(&image.manifest), &image.manifest, MANIFEST)?;
        }
    }
    registry.put_manifest(&reference.selector, bytes, media_type)?;
    Ok(format!(
        "{}/{}@{digest}",
        reference.registry, reference.repository
    ))
}
