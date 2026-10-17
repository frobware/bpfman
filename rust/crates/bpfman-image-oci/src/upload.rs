//! Registry-owned upload sessions, with explicit abort and retry ownership.
use crate::{
    Credentials, Error, Reference,
    registry::{Registry, check, hash, verify},
};
use std::sync::atomic::AtomicBool;

pub(super) struct Upload {
    reference: Reference,
    location: String,
    credentials: Option<Credentials>,
}

impl std::fmt::Debug for Upload {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Registry upload URLs may contain signed state in their query string.
        f.debug_struct("Upload")
            .field("reference", &self.reference)
            .finish_non_exhaustive()
    }
}

impl Upload {
    pub(super) fn abort(&self) -> Result<(), Error> {
        let flag = AtomicBool::new(false);
        let mut registry = Registry::publisher(&self.reference, self.credentials.as_ref(), &flag);
        let response =
            registry.write_request("DELETE", &self.location, &[], "application/octet-stream")?;
        if matches!(response.status().as_u16(), 202 | 204 | 404) {
            Ok(())
        } else {
            Err(Error::invalid(format!(
                "abort OCI upload returned HTTP {}",
                response.status()
            )))
        }
    }
}

impl Registry<'_> {
    fn write_request(
        &mut self,
        method: &str,
        url: &str,
        bytes: &[u8],
        content_type: &str,
    ) -> Result<ureq::http::Response<ureq::Body>, Error> {
        check(self.flag)?;
        let mut response = self.send(method, url, bytes, content_type)?;
        if response.status() == 401 && self.authorization.is_none() {
            self.authenticate(&response, "pull,push")?;
            check(self.flag)?;
            response = self.send(method, url, bytes, content_type)?;
        }
        // The caller must acquire any returned upload receipt before checking
        // cancellation, and must observe a successful commit before returning.
        Ok(response)
    }

    fn send(
        &self,
        method: &str,
        url: &str,
        bytes: &[u8],
        content_type: &str,
    ) -> Result<ureq::http::Response<ureq::Body>, Error> {
        let mut request = ureq::http::Request::builder()
            .method(method)
            .uri(url)
            .header("Content-Type", content_type)
            .header("Content-Length", bytes.len());
        if let Some(authorization) = &self.authorization {
            request = request.header("Authorization", authorization.clone());
        }
        let request = request
            .body(bytes)
            .map_err(|e| Error::caused("build OCI publication request", e))?;
        match self.agent.run(request) {
            Ok(response) => Ok(response),
            Err(cause) => {
                check(self.flag)?;
                Err(Error::caused("publish OCI content", cause))
            }
        }
    }

    pub(super) fn upload_blob(&mut self, bytes: &[u8]) -> Result<(), Error> {
        check(self.flag)?;
        let digest = hash(bytes);
        let url = format!("{}/blobs/{digest}", self.reference.base());
        let response = self.write_request("HEAD", &url, &[], "application/octet-stream")?;
        match response.status().as_u16() {
            200 => return Ok(()),
            404 => {}
            _ => {
                return Err(Error::invalid(format!(
                    "OCI blob lookup returned HTTP {}",
                    response.status()
                )));
            }
        }
        check(self.flag)?;
        let response = self.write_request(
            "POST",
            &format!("{}/blobs/uploads/", self.reference.base()),
            &[],
            "application/octet-stream",
        )?;
        if response.status() != 202 {
            return Err(Error::invalid(format!(
                "start OCI upload returned HTTP {}",
                response.status()
            )));
        }
        let location = response
            .headers()
            .get("location")
            .and_then(|v| v.to_str().ok())
            .ok_or_else(|| Error::invalid("OCI upload response has no Location"))?;
        let location = location_url(self.reference, location)?;
        let upload = Upload {
            reference: self.reference.clone(),
            location,
            credentials: self.credentials.as_deref().cloned(),
        };
        let result = (|| {
            check(self.flag)?;
            let separator = if upload.location.contains('?') {
                '&'
            } else {
                '?'
            };
            let response = self.write_request(
                "PUT",
                &format!("{}{separator}digest={digest}", upload.location),
                bytes,
                "application/octet-stream",
            )?;
            if response.status() != 201 {
                check(self.flag)?;
                return Err(Error::invalid(format!(
                    "complete OCI upload returned HTTP {}",
                    response.status()
                )));
            }
            response_digest(&response, bytes)
        })();
        match result {
            Ok(()) => Ok(()),
            Err(cause) => match upload.abort() {
                Ok(()) => Err(cause),
                Err(cleanup) => Err(cause.upload_cleanup_failed(upload, cleanup)),
            },
        }
    }

    pub(super) fn put_manifest(
        &mut self,
        selector: &str,
        bytes: &[u8],
        media_type: &str,
    ) -> Result<(), Error> {
        check(self.flag)?;
        let response = self.write_request(
            "PUT",
            &format!("{}/manifests/{selector}", self.reference.base()),
            bytes,
            media_type,
        )?;
        if response.status() != 201 {
            check(self.flag)?;
            return Err(Error::invalid(format!(
                "publish OCI manifest returned HTTP {}",
                response.status()
            )));
        }
        response_digest(&response, bytes)
    }
}

fn response_digest(response: &ureq::http::Response<ureq::Body>, bytes: &[u8]) -> Result<(), Error> {
    if let Some(value) = response.headers().get("docker-content-digest") {
        verify(
            bytes,
            value
                .to_str()
                .map_err(|e| Error::caused("read published digest", e))?,
        )?;
    }
    Ok(())
}

fn location_url(reference: &Reference, location: &str) -> Result<String, Error> {
    let base = reference.base();
    let origin = base
        .split_once("/v2/")
        .map(|(origin, _)| origin)
        .ok_or_else(|| Error::invalid("invalid registry origin"))?;
    let url = if location.starts_with('/') && !location.starts_with("//") {
        format!("{origin}{location}")
    } else {
        location.to_owned()
    };
    let suffix = url
        .strip_prefix(&format!("{base}/blobs/uploads/"))
        .ok_or_else(|| Error::invalid("OCI upload Location escapes registry repository"))?;
    let path = suffix.split('?').next().unwrap_or_default();
    if path.is_empty()
        || path.contains(['%', '#', '\\'])
        || path.split('/').any(|p| matches!(p, "." | ".."))
        || url
            .bytes()
            .any(|b| b.is_ascii_whitespace() || b.is_ascii_control())
        || url.contains('#')
    {
        return Err(Error::invalid("invalid OCI upload Location"));
    }
    Ok(url)
}
