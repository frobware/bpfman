use crate::{Credentials, Error, Reference, reference::validate_digest};
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    io::Read,
    sync::atomic::{AtomicBool, Ordering},
    time::Duration,
};
use ureq::ResponseExt;

type Labels = BTreeMap<String, String>;
type Content = (Vec<u8>, Option<String>);

const ACCEPT: &str = "application/vnd.oci.image.index.v1+json, application/vnd.docker.distribution.manifest.list.v2+json, application/vnd.oci.image.manifest.v1+json, application/vnd.docker.distribution.manifest.v2+json";
pub(super) const MANIFEST_LIMIT: usize = 4 * 1024 * 1024;
pub(super) const BYTECODE_LIMIT: usize = 16 * 1024 * 1024;

#[derive(Clone, Deserialize)]
pub(super) struct Descriptor {
    pub(super) digest: String,
    pub(super) size: usize,
    #[serde(rename = "mediaType")]
    pub(super) media_type: String,
    #[serde(default)]
    platform: Option<Platform>,
}

#[derive(Clone, Deserialize)]
struct Platform {
    architecture: String,
    os: String,
}

pub(super) struct Manifest {
    pub(super) digest: String,
    pub(super) value: Value,
    pub(super) media_type: String,
}

pub(super) struct Registry<'a> {
    pub(super) reference: &'a Reference,
    pub(super) agent: ureq::Agent,
    pub(super) authorization: Option<ureq::http::HeaderValue>,
    pub(super) credentials: Option<&'a Credentials>,
    token_agent: ureq::Agent,
    pub(super) flag: &'a AtomicBool,
}

pub(super) fn check(flag: &AtomicBool) -> Result<(), Error> {
    if flag.load(Ordering::Relaxed) {
        Err(Error::cancelled())
    } else {
        Ok(())
    }
}

pub(super) fn hash(bytes: &[u8]) -> String {
    format!("sha256:{:x}", Sha256::digest(bytes))
}

pub(super) fn verify(bytes: &[u8], digest: &str) -> Result<(), Error> {
    validate_digest(digest)?;
    if hash(bytes) != digest {
        return Err(Error::invalid("OCI content digest mismatch"));
    }
    Ok(())
}

pub(super) fn agent_config(redirects: u32) -> ureq::config::Config {
    ureq::Agent::config_builder()
        .http_status_as_error(false)
        .max_redirects(redirects)
        .redirect_auth_headers(ureq::config::RedirectAuthHeaders::Never)
        .timeout_global(Some(Duration::from_secs(60)))
        .timeout_connect(Some(Duration::from_secs(5)))
        .timeout_resolve(Some(Duration::from_secs(5)))
        .timeout_recv_response(Some(Duration::from_secs(5)))
        .timeout_recv_body(Some(Duration::from_secs(5)))
        .timeout_send_request(Some(Duration::from_secs(5)))
        .timeout_send_body(Some(Duration::from_secs(5)))
        .build()
}

impl<'a> Registry<'a> {
    pub(super) fn new(
        reference: &'a Reference,
        credentials: Option<&'a Credentials>,
        flag: &'a AtomicBool,
    ) -> Self {
        Self::configured(reference, credentials, flag, 10)
    }

    pub(super) fn publisher(
        reference: &'a Reference,
        credentials: Option<&'a Credentials>,
        flag: &'a AtomicBool,
    ) -> Self {
        Self::configured(reference, credentials, flag, 0)
    }

    fn configured(
        reference: &'a Reference,
        credentials: Option<&'a Credentials>,
        flag: &'a AtomicBool,
        redirects: u32,
    ) -> Self {
        Self {
            reference,
            token_agent: crate::transport::agent(agent_config(0)),
            agent: crate::transport::agent(agent_config(redirects)),
            authorization: None,
            credentials,
            flag,
        }
    }

    fn request(&self, path: &str) -> Result<ureq::http::Response<ureq::Body>, Error> {
        check(self.flag)?;
        let mut request = self
            .agent
            .get(format!("{}{path}", self.reference.base()))
            .header("Accept", ACCEPT);
        if let Some(authorization) = &self.authorization {
            request = request.header("Authorization", authorization.clone());
        }
        let response = request
            .call()
            .map_err(|e| Error::caused("request OCI registry", e));
        check(self.flag)?;
        response
    }

    pub(super) fn authenticate(
        &mut self,
        response: &ureq::http::Response<ureq::Body>,
        actions: &str,
    ) -> Result<(), Error> {
        check(self.flag)?;
        let origin = self
            .reference
            .base()
            .parse::<ureq::http::Uri>()
            .map_err(|_| Error::invalid("invalid registry origin"))?;
        if response.get_uri().scheme() != origin.scheme()
            || response.get_uri().authority() != origin.authority()
        {
            return Err(Error::invalid(
                "refuse authentication challenge from redirected registry response",
            ));
        }
        let challenge = response
            .headers()
            .get("www-authenticate")
            .and_then(|v| v.to_str().ok())
            .ok_or_else(|| Error::invalid("registry authentication required; missing challenge"))?;
        let (scheme, _) = challenge
            .split_once(' ')
            .ok_or_else(|| Error::invalid("unsupported registry authentication challenge"))?;
        if scheme.eq_ignore_ascii_case("Basic") {
            let credentials = self
                .credentials
                .ok_or_else(|| Error::invalid("registry authentication requires credentials"))?;
            self.authorization = Some(secret_header(credentials.basic())?);
            return Ok(());
        }
        let (realm, service) = bearer_challenge(challenge)?;
        validate_realm(&realm, self.reference)?;
        // Token endpoints never follow redirects, even for public pulls. Registry
        // redirects strip Authorization independently of the destination origin.
        let mut request = self.token_agent.get(&realm).query(
            "scope",
            format!("repository:{}:{actions}", self.reference.repository),
        );
        if let Some(service) = service {
            request = request.query("service", service);
        }
        if let Some(credentials) = self.credentials {
            request = request.header("Authorization", secret_header(credentials.basic())?);
        }
        let token_response = request.call();
        check(self.flag)?;
        // A token endpoint URL may itself contain secrets. Do not retain transport
        // errors that can embed that URL in their diagnostic source chain.
        let token_response =
            token_response.map_err(|_| Error::invalid("request registry token failed"))?;
        if token_response.status() != 200 {
            return Err(Error::invalid("registry token request failed"));
        }
        let bytes = read_body(token_response, 1024 * 1024, self.flag)?;
        let value: Value = serde_json::from_slice(&bytes)
            .map_err(|_| Error::invalid("decode registry token failed"))?;
        let token = value
            .get("token")
            .or_else(|| value.get("access_token"))
            .and_then(Value::as_str)
            .filter(|token| {
                !token.is_empty()
                    && token.len() <= 64 * 1024
                    && token.bytes().all(|b| b.is_ascii_graphic())
            })
            .ok_or_else(|| Error::invalid("registry returned no usable token"))?;
        self.authorization = Some(secret_header(&format!("Bearer {token}"))?);
        Ok(())
    }

    fn get(&mut self, path: &str, limit: usize) -> Result<(Vec<u8>, Option<String>), Error> {
        self.get_optional(path, limit)?
            .ok_or_else(|| Error::invalid("OCI manifest not found"))
    }

    fn get_optional(&mut self, path: &str, limit: usize) -> Result<Option<Content>, Error> {
        let mut response = self.request(path)?;
        if response.status() == 401 && self.authorization.is_none() {
            self.authenticate(&response, "pull")?;
            response = self.request(path)?;
        }
        if response.status() == 404 {
            let bytes = read_body(response, MANIFEST_LIMIT, self.flag)?;
            let body: Value = serde_json::from_slice(&bytes)
                .map_err(|e| Error::caused("decode OCI not-found response", e))?;
            let missing = body["errors"].as_array().is_some_and(|errors| {
                !errors.is_empty()
                    && errors.iter().all(|error| {
                        matches!(
                            error["code"].as_str(),
                            Some("MANIFEST_UNKNOWN" | "NAME_UNKNOWN")
                        )
                    })
            });
            if missing {
                return Ok(None);
            }
            return Err(Error::invalid(
                "OCI registry returned an unrecognized HTTP 404",
            ));
        }
        if response.status() != 200 {
            return Err(Error::invalid(format!(
                "OCI registry returned HTTP {}",
                response.status()
            )));
        }
        let digest = response
            .headers()
            .get("docker-content-digest")
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned);
        let bytes = read_body(response, limit, self.flag)?;
        if let Some(digest) = &digest {
            verify(&bytes, digest)?;
        }
        Ok(Some((bytes, digest)))
    }

    pub(super) fn signature_manifest(&mut self, selector: &str) -> Result<Option<Manifest>, Error> {
        let Some((bytes, _)) =
            self.get_optional(&format!("/manifests/{selector}"), MANIFEST_LIMIT)?
        else {
            return Ok(None);
        };
        Self::decode_manifest(&bytes).map(Some)
    }

    pub(super) fn manifest(&mut self, selector: &str) -> Result<Manifest, Error> {
        let (bytes, _) = self.get(&format!("/manifests/{selector}"), MANIFEST_LIMIT)?;
        if selector.starts_with("sha256:") {
            verify(&bytes, selector)?;
        }
        Self::decode_manifest(&bytes)
    }

    fn decode_manifest(bytes: &[u8]) -> Result<Manifest, Error> {
        let value: Value =
            serde_json::from_slice(bytes).map_err(|e| Error::caused("decode OCI manifest", e))?;
        if value["schemaVersion"] != 2 {
            return Err(Error::invalid("unsupported OCI manifest schema"));
        }
        let media_type = value["mediaType"]
            .as_str()
            .ok_or_else(|| Error::invalid("OCI manifest has no mediaType"))?
            .to_owned();
        Ok(Manifest {
            digest: hash(bytes),
            value,
            media_type,
        })
    }

    pub(super) fn child(&mut self, descriptor: &Descriptor) -> Result<Manifest, Error> {
        validate_digest(&descriptor.digest)?;
        let (bytes, _) = self.get(&format!("/manifests/{}", descriptor.digest), MANIFEST_LIMIT)?;
        validate_descriptor(&bytes, descriptor)?;
        let value: Value = serde_json::from_slice(&bytes)
            .map_err(|e| Error::caused("decode platform manifest", e))?;
        if value["schemaVersion"] != 2 {
            return Err(Error::invalid("unsupported OCI manifest schema"));
        }
        Ok(Manifest {
            digest: descriptor.digest.clone(),
            value,
            media_type: descriptor.media_type.clone(),
        })
    }

    pub(super) fn blob(&mut self, descriptor: &Descriptor, limit: usize) -> Result<Vec<u8>, Error> {
        validate_digest(&descriptor.digest)?;
        if descriptor.size > limit {
            return Err(Error::invalid("OCI descriptor exceeds size limit"));
        }
        let (bytes, _) = self.get(&format!("/blobs/{}", descriptor.digest), limit)?;
        validate_descriptor(&bytes, descriptor)?;
        Ok(bytes)
    }

    pub(super) fn labels(&mut self, manifest: &Manifest) -> Result<(Labels, Labels), Error> {
        let config = descriptor(&manifest.value["config"])?;
        let bytes = self.blob(&config, MANIFEST_LIMIT)?;
        let value: Value =
            serde_json::from_slice(&bytes).map_err(|e| Error::caused("decode OCI config", e))?;
        let labels = &value["config"]["Labels"];
        if !labels.is_null() && !labels.is_object() {
            return Err(Error::invalid("OCI config Labels must be an object"));
        }
        let decode = |name| -> Result<Labels, Error> {
            match labels.get(name) {
                None => Ok(BTreeMap::new()),
                Some(Value::String(text)) if text.is_empty() => Ok(BTreeMap::new()),
                Some(Value::String(text)) => serde_json::from_str(text)
                    .map_err(|e| Error::caused("decode bytecode image labels", e)),
                Some(_) => Err(Error::invalid(
                    "OCI bytecode labels must contain JSON strings",
                )),
            }
        };
        Ok((decode("io.ebpf.programs")?, decode("io.ebpf.maps")?))
    }
}

pub(super) fn read_body(
    mut response: ureq::http::Response<ureq::Body>,
    limit: usize,
    flag: &AtomicBool,
) -> Result<Vec<u8>, Error> {
    let mut bytes = Vec::new();
    let mut reader = response.body_mut().as_reader();
    let mut buffer = [0; 16 * 1024];
    loop {
        check(flag)?;
        let count = reader.read(&mut buffer);
        check(flag)?;
        let count = count.map_err(|e| Error::caused("read OCI response", e))?;
        check(flag)?;
        if count == 0 {
            return Ok(bytes);
        }
        if bytes.len() + count > limit {
            return Err(Error::invalid("OCI response exceeds size limit"));
        }
        bytes.extend_from_slice(&buffer[..count]);
    }
}

fn validate_descriptor(bytes: &[u8], descriptor: &Descriptor) -> Result<(), Error> {
    if bytes.len() != descriptor.size {
        return Err(Error::invalid("OCI descriptor size mismatch"));
    }
    verify(bytes, &descriptor.digest)
}

pub(super) fn descriptor(value: &Value) -> Result<Descriptor, Error> {
    serde_json::from_value(value.clone()).map_err(|e| Error::caused("decode OCI descriptor", e))
}

pub(super) fn is_index(manifest: &Manifest) -> bool {
    matches!(
        manifest.media_type.as_str(),
        "application/vnd.oci.image.index.v1+json"
            | "application/vnd.docker.distribution.manifest.list.v2+json"
    )
}

pub(super) fn require_image(manifest: &Manifest) -> Result<(), Error> {
    if matches!(
        manifest.media_type.as_str(),
        "application/vnd.oci.image.manifest.v1+json"
            | "application/vnd.docker.distribution.manifest.v2+json"
    ) {
        Ok(())
    } else {
        Err(Error::invalid("unsupported OCI image media type"))
    }
}

pub(super) fn children(manifest: &Manifest) -> Result<Vec<Descriptor>, Error> {
    let values = manifest.value["manifests"]
        .as_array()
        .filter(|v| !v.is_empty() && v.len() <= 64)
        .ok_or_else(|| Error::invalid("OCI index must contain between 1 and 64 manifests"))?;
    values.iter().map(descriptor).collect()
}

pub(super) fn host_arch() -> &'static str {
    match std::env::consts::ARCH {
        "x86_64" => "amd64",
        "aarch64" => "arm64",
        other => other,
    }
}

pub(super) fn select(manifest: &Manifest) -> Result<Descriptor, Error> {
    children(manifest)?
        .into_iter()
        .find(|d| {
            d.platform
                .as_ref()
                .is_some_and(|p| p.os == "linux" && p.architecture == host_arch())
        })
        .ok_or_else(|| Error::invalid(format!("no linux/{} manifest in OCI index", host_arch())))
}

fn secret_header(value: &str) -> Result<ureq::http::HeaderValue, Error> {
    let mut header = ureq::http::HeaderValue::from_str(value)
        .map_err(|_| Error::invalid("invalid registry authorization header"))?;
    header.set_sensitive(true);
    Ok(header)
}

fn validate_realm(realm: &str, reference: &Reference) -> Result<(), Error> {
    let uri = realm
        .parse::<ureq::http::Uri>()
        .map_err(|_| Error::invalid("invalid registry token realm"))?;
    let authority = uri
        .authority()
        .filter(|a| !a.as_str().contains('@'))
        .ok_or_else(|| Error::invalid("invalid registry token realm"))?;
    if realm.contains('#')
        || !(uri.scheme_str() == Some("https")
            || (uri.scheme_str() == Some("http")
                && reference.base().starts_with("http://")
                && authority.as_str() == reference.registry))
    {
        return Err(Error::invalid("unsupported registry token realm"));
    }
    Ok(())
}

fn bearer_challenge(value: &str) -> Result<(String, Option<String>), Error> {
    let (_, params) = value
        .split_once(' ')
        .filter(|(scheme, _)| scheme.eq_ignore_ascii_case("Bearer"))
        .ok_or_else(|| Error::invalid("unsupported registry authentication challenge"))?;
    let mut values = BTreeMap::new();
    for part in params.split(',') {
        let (key, value) = part
            .trim()
            .split_once('=')
            .ok_or_else(|| Error::invalid("malformed bearer challenge"))?;
        let value = value
            .strip_prefix('"')
            .and_then(|v| v.strip_suffix('"'))
            .filter(|v| !v.contains(['"', '\\']))
            .ok_or_else(|| Error::invalid("malformed bearer challenge value"))?;
        if values.insert(key, value).is_some() {
            return Err(Error::invalid("duplicate bearer challenge field"));
        }
    }
    let realm = values
        .get("realm")
        .ok_or_else(|| Error::invalid("bearer challenge has no realm"))?;
    Ok(((*realm).into(), values.get("service").map(|v| (*v).into())))
}

fn summary(registry: &mut Registry<'_>, manifest: &Manifest) -> Result<Value, Error> {
    require_image(manifest)?;
    let (programs, maps) = registry.labels(manifest)?;
    let mut result = json!({});
    if !programs.is_empty() {
        result["programs"] = json!(programs);
    }
    if !maps.is_empty() {
        result["maps"] = json!(maps);
    }
    let layers = manifest.value["layers"]
        .as_array()
        .ok_or_else(|| Error::invalid("OCI manifest layers must be an array"))?;
    if !layers.is_empty() {
        result["layers"] = Value::Array(layers.iter().map(|value| {
            let layer = descriptor(value)?;
            Ok(json!({"digest":layer.digest,"media_type":layer.media_type,"size":layer.size}))
        }).collect::<Result<_,Error>>()?);
    }
    Ok(result)
}

/// Inspect image metadata without creating cache, store or kernel state.
/// Index observations retain the published child digests and their labels.
pub fn inspect(
    reference: &Reference,
    credentials: Option<&Credentials>,
    cancelled: &AtomicBool,
) -> Result<Value, Error> {
    let mut registry = Registry::new(reference, credentials, cancelled);
    let manifest = registry.manifest(&reference.selector)?;
    let mut result = json!({"reference": reference.normalized(), "digest": manifest.digest, "media_type": manifest.media_type});
    if is_index(&manifest) {
        let mut manifests = Vec::new();
        for child in children(&manifest)? {
            let manifest = registry.child(&child)?;
            let mut item = summary(&mut registry, &manifest)?;
            item["digest"] = json!(child.digest);
            item["media_type"] = json!(child.media_type);
            item["size"] = json!(child.size);
            if let Some(platform) = child.platform {
                item["platform"] = json!(format!("{}/{}", platform.os, platform.architecture));
            }
            manifests.push(item);
        }
        result["manifests"] = json!(manifests);
    } else {
        let fields = summary(&mut registry, &manifest)?;
        for (key, value) in fields
            .as_object()
            .ok_or_else(|| Error::invalid("invalid image summary"))?
        {
            result[key] = value.clone();
        }
    }
    Ok(result)
}
