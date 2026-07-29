//! Frozen domain contracts and event bus for Sotto's headless pipeline.

#![deny(warnings)]

pub mod bus;
pub mod error;
pub mod traits;
pub mod types;

pub use bus::{EventBus, EventReceiver, ReceiveError};
pub use error::{AsrError, CaptureError, PipelineError, ProviderError, RagError, VadError};
pub use traits::{
    BoxFuture, BoxStream, CaptureBackend, CompletionProvider, Retriever, Transcriber,
    VoiceActivityDetector,
};
pub use types::{
    Annotation, AudioFrame, Chunk, Citation, CompletionMessage, CompletionRequest, Delta,
    MessageRole, PermissionStatus, PipelineEvent, Source, SpeechState, StopReason, Suggestion,
    Trigger, TriggerKind, Usage, Utterance, UtteranceSpan, VadSegment,
};
