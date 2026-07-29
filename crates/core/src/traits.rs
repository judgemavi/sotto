//! Hard contracts implemented by the capture, intelligence, and retrieval crates.

use std::{
    future::Future,
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

use futures_core::Stream;
use tokio::sync::{Notify, broadcast};

use crate::{
    AudioFrame, CaptureError, Chunk, CompletionRequest, Delta, PermissionStatus, ProviderError,
    RagError, Utterance, VadSegment,
};

/// A boxed, sendable stream with a caller-selected lifetime.
pub type BoxStream<'a, T> = Pin<Box<dyn Stream<Item = T> + Send + 'a>>;

/// A boxed, sendable future with a caller-selected lifetime.
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// Cloneable cancellation signal retained by caller and completion stream.
#[derive(Clone, Default)]
pub struct CancellationToken {
    cancelled: Arc<AtomicBool>,
    notify: Arc<Notify>,
}

impl CancellationToken {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::Release);
        self.notify.notify_waiters();
    }

    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Acquire)
    }

    pub async fn cancelled(&self) {
        loop {
            let notified = self.notify.notified();
            if self.is_cancelled() {
                return;
            }
            notified.await;
        }
    }
}

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
        cancellation: CancellationToken,
    ) -> BoxFuture<'_, Result<BoxStream<'static, Result<Delta, ProviderError>>, ProviderError>>;

    fn model_id(&self) -> &str;
}

fn _assert_dyn_compatible(_: &dyn CompletionProvider, _: &dyn Retriever) {}
