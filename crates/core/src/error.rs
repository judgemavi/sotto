//! Typed errors crossing stage boundaries.

use std::time::Duration;

use thiserror::Error;

use crate::PermissionStatus;

#[derive(Clone, Debug, Eq, Error, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Deserialize, serde::Serialize))]
pub enum CaptureError {
    #[error("capture permission denied: {status:?}")]
    PermissionDenied { status: PermissionStatus },
    #[error("capture permission was revoked")]
    PermissionRevoked,
    #[error("capture device unavailable: {device}")]
    DeviceUnavailable { device: String },
    #[error("capture stream failed: {0}")]
    StreamFailed(String),
    #[error("unsupported capture configuration: {0}")]
    Unsupported(String),
}

#[derive(Clone, Debug, Eq, Error, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Deserialize, serde::Serialize))]
pub enum VadError {
    #[error("failed to load VAD model: {0}")]
    ModelLoad(String),
    #[error("VAD inference failed: {0}")]
    Inference(String),
}

#[derive(Clone, Debug, Eq, Error, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Deserialize, serde::Serialize))]
pub enum AsrError {
    #[error("ASR model not found: {path}")]
    ModelNotFound { path: String },
    #[error("failed to load ASR model: {0}")]
    ModelLoad(String),
    #[error("ASR inference failed: {0}")]
    Inference(String),
}

#[derive(Clone, Debug, Eq, Error, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Deserialize, serde::Serialize))]
pub enum ProviderError {
    #[error("provider authentication failed")]
    Auth,
    #[error("provider rate limit exceeded")]
    RateLimit { retry_after: Option<Duration> },
    #[error("provider context length exceeded: limit {limit}, requested {requested}")]
    ContextLengthExceeded { limit: u32, requested: u32 },
    #[error("provider network failure: {0}")]
    Network(String),
    #[error("provider returned HTTP {status}: {message}")]
    Upstream { status: u16, message: String },
    /// An intentionally aborted speculative call; never retry or surface to the user.
    #[error("provider call cancelled")]
    Cancelled,
    #[error("failed to decode provider response: {0}")]
    Decode(String),
}

#[derive(Clone, Debug, Eq, Error, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Deserialize, serde::Serialize))]
pub enum RagError {
    #[error("retrieval storage failure: {0}")]
    Storage(String),
    #[error("embedding failure: {0}")]
    Embedding(String),
    #[error("retrieval migration failed from version {from} to {to}")]
    Migration { from: u32, to: u32 },
    #[error("retrieval item not found: {id}")]
    NotFound { id: String },
}

/// An error emitted by any stage of the pipeline.
#[derive(Clone, Debug, Eq, Error, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Deserialize, serde::Serialize))]
pub enum PipelineError {
    #[error(transparent)]
    Capture(#[from] CaptureError),
    #[error(transparent)]
    Vad(#[from] VadError),
    #[error(transparent)]
    Asr(#[from] AsrError),
    #[error(transparent)]
    Provider(#[from] ProviderError),
    #[error(transparent)]
    Rag(#[from] RagError),
}

#[cfg(test)]
mod tests {
    #[cfg(feature = "serde")]
    use super::{PipelineError, ProviderError};

    #[cfg(feature = "serde")]
    #[test]
    fn cancelled_provider_error_survives_pipeline_json_round_trip()
    -> Result<(), Box<dyn std::error::Error>> {
        let original = PipelineError::Provider(ProviderError::Cancelled);
        let json = serde_json::to_string(&original)?;
        let decoded: PipelineError = serde_json::from_str(&json)?;

        assert_eq!(decoded, original);
        Ok(())
    }
}
