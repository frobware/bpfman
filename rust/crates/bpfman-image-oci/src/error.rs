use bpfman_fs::{ImageCacheStage, RuntimeWriter};

/// OCI operation failure. Failed local or remote cleanup retains ownership.
#[derive(Debug, thiserror::Error)]
#[error("{message}")]
pub struct Error {
    message: String,
    #[source]
    source: Option<Box<dyn std::error::Error + Send + Sync>>,
    remaining: Option<ImageCacheStage>,
    cancelled: bool,
    uploads: Vec<crate::upload::Upload>,
}

impl Error {
    pub(super) fn invalid(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            source: None,
            remaining: None,
            cancelled: false,
            uploads: Vec::new(),
        }
    }

    pub(super) fn caused(
        message: &str,
        source: impl std::error::Error + Send + Sync + 'static,
    ) -> Self {
        Self {
            message: message.into(),
            source: Some(Box::new(source)),
            remaining: None,
            cancelled: false,
            uploads: Vec::new(),
        }
    }

    pub(super) fn publication(
        writer: &RuntimeWriter<'_>,
        cause: bpfman_fs::Error,
        stage: Option<ImageCacheStage>,
    ) -> Self {
        let mut error = Self::caused("publish image cache", cause);
        if let Some(stage) = stage {
            if let Err(failure) = writer.remove_image_cache_stage(stage) {
                error = error.cleanup_failed(failure.cause, failure.remaining);
            }
        }
        error
    }

    pub(super) fn cleanup_failed(
        mut self,
        cause: bpfman_fs::Error,
        stage: ImageCacheStage,
    ) -> Self {
        self.message
            .push_str(&format!("; cache stage cleanup failed: {}", chain(&cause)));
        self.remaining = Some(stage);
        self
    }

    /// Construct a cancellation result before acquiring external resources.
    pub fn cancelled() -> Self {
        Self {
            message: "image operation cancelled".into(),
            source: None,
            remaining: None,
            cancelled: true,
            uploads: Vec::new(),
        }
    }

    /// Whether the operation stopped at a caller cancellation boundary.
    pub fn is_cancelled(&self) -> bool {
        self.cancelled
    }

    /// Whether cleanup retained ownership requiring an explicit retry.
    pub fn has_pending_cleanup(&self) -> bool {
        self.remaining.is_some() || !self.uploads.is_empty()
    }

    pub(super) fn upload_cleanup_failed(
        mut self,
        upload: crate::upload::Upload,
        cause: Self,
    ) -> Self {
        self.message
            .push_str(&format!("; upload cleanup failed: {}", chain(&cause)));
        self.uploads.push(upload);
        self
    }

    /// Retry each unresolved registry upload abort once. Completed blobs and
    /// manifests are content-addressed registry objects, not upload sessions.
    pub fn retry_upload_cleanup(&mut self) -> Result<(), Error> {
        let mut first = None;
        for upload in std::mem::take(&mut self.uploads) {
            if let Err(cause) = upload.abort() {
                self.message
                    .push_str(&format!("; upload cleanup retry failed: {}", chain(&cause)));
                self.uploads.push(upload);
                if first.is_none() {
                    first = Some(cause);
                }
            }
        }
        first.map_or(Ok(()), Err)
    }

    /// Attempt unresolved stage removal once under the same cache root's writer.
    /// Failure retains the receipt; the original operation failure stays intact.
    pub fn retry_cleanup(&mut self, writer: &RuntimeWriter<'_>) -> Result<(), bpfman_fs::Error> {
        if let Some(stage) = self.remaining.take() {
            if let Err(failure) = writer.remove_image_cache_stage(stage) {
                self.message.push_str(&format!(
                    "; cache cleanup retry failed: {}",
                    chain(&failure.cause)
                ));
                self.remaining = Some(failure.remaining);
                return Err(failure.cause);
            }
        }
        Ok(())
    }
}

fn chain(error: &dyn std::error::Error) -> String {
    let mut text = error.to_string();
    let mut source = error.source();
    while let Some(cause) = source {
        text.push_str(": ");
        text.push_str(&cause.to_string());
        source = cause.source();
    }
    text
}
