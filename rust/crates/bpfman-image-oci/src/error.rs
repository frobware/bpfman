use bpfman_fs::{ImageCacheStage, RuntimeWriter};

/// OCI admission failure. Failed stage cleanup retains its consuming receipt.
#[derive(Debug, thiserror::Error)]
#[error("{message}")]
pub struct Error {
    message: String,
    #[source]
    source: Option<Box<dyn std::error::Error + Send + Sync>>,
    remaining: Option<ImageCacheStage>,
    cancelled: bool,
}

impl Error {
    pub(super) fn invalid(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            source: None,
            remaining: None,
            cancelled: false,
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

    pub(super) fn cancelled() -> Self {
        Self {
            message: "image operation cancelled".into(),
            source: None,
            remaining: None,
            cancelled: true,
        }
    }

    /// Whether the operation stopped at a caller cancellation boundary.
    pub fn is_cancelled(&self) -> bool {
        self.cancelled
    }

    /// Whether cleanup retained ownership requiring an explicit retry.
    pub fn has_pending_cleanup(&self) -> bool {
        self.remaining.is_some()
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
