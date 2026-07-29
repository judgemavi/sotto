//! Non-blocking broadcast bus for pipeline events.
//!
//! A subscriber can fall behind and lose events, but it can never apply backpressure
//! to capture or any other producer. Receivers log and count lag before continuing.
//! Capture deliberately publishes high-rate frames to its own
//! `broadcast::Sender<AudioFrame>`, not this UI-facing event bus. Pipeline orchestration
//! must run a bridge task that lifts those frames into `PipelineEvent::Audio`.

use std::{
    num::NonZeroUsize,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};

use thiserror::Error;
use tokio::sync::broadcast;

use crate::PipelineEvent;

/// Cloneable publishing side of the pipeline event bus.
#[derive(Clone, Debug)]
pub struct EventBus {
    sender: broadcast::Sender<PipelineEvent>,
    lagged_events: Arc<AtomicU64>,
}

impl EventBus {
    /// Creates a bus with a bounded, non-zero event capacity.
    #[must_use]
    pub fn new(capacity: NonZeroUsize) -> Self {
        let (sender, _) = broadcast::channel(capacity.get());
        Self {
            sender,
            lagged_events: Arc::new(AtomicU64::new(0)),
        }
    }

    /// Returns a sender for a pipeline stage that publishes directly.
    #[must_use]
    pub fn sender(&self) -> broadcast::Sender<PipelineEvent> {
        self.sender.clone()
    }

    /// Subscribes a named stage or UI consumer.
    #[must_use]
    pub fn subscribe(&self, stage: impl Into<Arc<str>>) -> EventReceiver {
        EventReceiver {
            stage: stage.into(),
            receiver: self.sender.subscribe(),
            lagged_events: Arc::clone(&self.lagged_events),
            subscriber_lagged_events: 0,
        }
    }

    /// Total events dropped by all receivers observed since bus creation.
    #[must_use]
    pub fn lagged_events(&self) -> u64 {
        self.lagged_events.load(Ordering::Relaxed)
    }
}

/// A named receiver that recovers automatically after broadcast lag.
#[derive(Debug)]
pub struct EventReceiver {
    stage: Arc<str>,
    receiver: broadcast::Receiver<PipelineEvent>,
    lagged_events: Arc<AtomicU64>,
    subscriber_lagged_events: u64,
}

impl EventReceiver {
    /// Receives the next available event, skipping and recording lagged events.
    pub async fn recv(&mut self) -> Result<PipelineEvent, ReceiveError> {
        loop {
            match self.receiver.recv().await {
                Ok(event) => return Ok(event),
                Err(broadcast::error::RecvError::Lagged(skipped)) => {
                    self.subscriber_lagged_events =
                        self.subscriber_lagged_events.saturating_add(skipped);
                    self.lagged_events.fetch_add(skipped, Ordering::Relaxed);
                    tracing::warn!(
                        stage = %self.stage,
                        skipped,
                        "pipeline subscriber lagged; events dropped"
                    );
                }
                Err(broadcast::error::RecvError::Closed) => {
                    return Err(ReceiveError::Closed);
                }
            }
        }
    }

    /// Events this subscriber has skipped due to lag.
    #[must_use]
    pub const fn lagged_events(&self) -> u64 {
        self.subscriber_lagged_events
    }
}

/// Terminal receive failure.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum ReceiveError {
    #[error("pipeline event bus closed")]
    Closed,
}
