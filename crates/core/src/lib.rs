//! Frozen domain contracts and event bus for Sotto's headless pipeline.

#![deny(warnings)]

pub mod bus;
pub mod error;
pub mod pipeline;
pub mod timeline;
pub mod traits;
pub mod types;

pub use bus::{EventBus, EventReceiver, ReceiveError};
pub use error::{AsrError, CaptureError, PipelineError, ProviderError, RagError, VadError};
pub use pipeline::{
    BackpressureCounters, CallSession, NoopAnnotator, NoopPersistence, PersistenceSink, Pipeline,
    PipelineBuilder, PipelineConfig, PipelineHandle, RecordingState, SessionView,
    TranscriptAnnotator,
};
pub use timeline::{
    CaptureTarget, EventClass, EventId, EventKind, EventPayload, FrameRef, LenientReplay, MarkKind,
    ProposalDisposition, ProposalDispositionKind, ProposalEvent, ProposalPhase, ProposalRunAudit,
    ProposalRunAuditError, ProposalRunOutcome, ProsodyDelta, ReplayState, SQLITE_SCHEMA,
    ScreenSnapshot, Session, SessionId, TargetKind, TimelineBuilder, TimelineError, TimelineEvent,
    UserAnnotation, checked_user_annotation, replay, replay_lenient,
};
pub use traits::{
    BoxFuture, BoxStream, CancellationToken, CaptureBackend, CompletionProvider, RecordingStatus,
    RecordingTranscriber, Retriever, Transcriber, TranscriptUpdate, VoiceActivityDetector,
};
pub use types::{
    Annotation, AudioFrame, Chunk, CompletionMessage, CompletionRequest, Delta,
    ExternalEvidenceRef, JsonSchemaConstraint, MessageRole, PermissionStatus, Proposal,
    ProposalError, ProposalKind, ProposalTrigger, ReasoningOutput, ReasoningRequest,
    ReasoningRequestError, Source, SpeechState, StopReason, Usage, Utterance, VadSegment,
};
