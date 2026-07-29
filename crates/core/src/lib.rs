//! Frozen domain contracts and event bus for Sotto's headless pipeline.

#![deny(warnings)]

pub mod bus;
pub mod error;
pub mod timeline;
pub mod traits;
pub mod types;

pub use bus::{EventBus, EventReceiver, ReceiveError};
pub use error::{AsrError, CaptureError, PipelineError, ProviderError, RagError, VadError};
pub use timeline::{
    CaptureTarget, EventId, EventKind, EventPayload, FrameRef, LenientReplay, MarkKind,
    ProsodyDelta, ReplayState, SQLITE_SCHEMA, ScreenSnapshot, Session, SessionId, TargetKind,
    TimelineBuilder, TimelineError, TimelineEvent, UserAnnotation, replay, replay_lenient,
};
pub use traits::{
    BoxFuture, BoxStream, CancellationToken, CaptureBackend, CompletionProvider, Retriever,
    Transcriber, VoiceActivityDetector,
};
pub use types::{
    Annotation, AudioFrame, Chunk, Citation, CompletionMessage, CompletionRequest, Delta,
    MessageRole, PermissionStatus, Source, SpeechState, StopReason, Suggestion, Trigger,
    TriggerKind, Usage, Utterance, UtteranceSpan, VadSegment,
};
