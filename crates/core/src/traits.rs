//! Hard contracts implemented by the capture, intelligence, and retrieval crates.

use std::{future::Future, pin::Pin};

use futures_core::Stream;
use tokio::sync::broadcast;

use crate::{
    AudioFrame, CaptureError, Chunk, CompletionRequest, Delta, PermissionStatus, ProviderError,
    RagError, Utterance, VadSegment,
};

/// A boxed, sendable stream with a caller-selected lifetime.
pub type BoxStream<'a, T> = Pin<Box<dyn Stream<Item = T> + Send + 'a>>;

/// A boxed, sendable future with a caller-selected lifetime.
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// Starts and stops a platform capture implementation.
pub trait CaptureBackend: Send {
    fn start(&mut self, sink: broadcast::Sender<AudioFrame>) -> Result<(), CaptureError>;
    fn stop(&mut self);
    fn permission_status(&self) -> PermissionStatus;
}

/// Performs incremental speech-state detection on audio frames.
pub trait VoiceActivityDetector: Send {
    fn push(&mut self, frame: &AudioFrame) -> Option<VadSegment>;
    fn reset(&mut self);
}

/// Produces sliding-window partials and final utterances from audio frames.
pub trait Transcriber: Send {
    fn push(&mut self, frame: &AudioFrame);
    fn poll(&mut self) -> Vec<Utterance>;
}

/// Searches the local knowledge index.
pub trait Retriever: Send + Sync {
    fn search<'a>(
        &'a self,
        query: &'a str,
        k: usize,
    ) -> BoxFuture<'a, Result<Vec<Chunk>, RagError>>;
}

/// Streams provider-neutral completion deltas.
pub trait CompletionProvider: Send + Sync {
    fn stream(
        &self,
        req: CompletionRequest,
    ) -> BoxFuture<'_, Result<BoxStream<'static, Result<Delta, ProviderError>>, ProviderError>>;

    fn model_id(&self) -> &str;
}

fn _assert_dyn_compatible(_: &dyn CompletionProvider, _: &dyn Retriever) {}
