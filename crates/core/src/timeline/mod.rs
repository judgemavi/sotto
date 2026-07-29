//! Canonical append-only session timeline.
//!
//! Every durable pipeline result is appended as a new [`TimelineEvent`]. Corrections
//! reference an earlier event through `supersedes`; the corrected event remains in the
//! log so replay is deterministic and UI layout never retroactively moves. Event ids
//! are allocated monotonically within a session, establishing stable total order.
//! Events expose no mutable access to their envelope or payload after construction.
//!
//! [`TimelineBuilder::checkpoint`] bounds payload memory by draining events already
//! handed to persistence while retaining compact identity/supersession state. T011
//! must persist each checkpoint before discarding it. An active evicted event remains
//! supersedable because its id stays in the builder; inactive targets are rejected.
//!
//! [`replay`] is strict and validates newly built or trusted logs. Persisted sessions
//! should use [`replay_lenient`]: malformed events are reported and skipped so one bad
//! row cannot make the post-call board unopenable.

mod builder;
mod event;
mod schema;
mod session;

pub use builder::{
    LenientReplay, ReplayState, TimelineBuilder, TimelineError, replay, replay_lenient,
};
pub use event::{
    EventId, EventKind, EventPayload, FrameRef, MarkKind, ProsodyDelta, ScreenSnapshot, SessionId,
    TimelineEvent, UserAnnotation,
};
pub use schema::SQLITE_SCHEMA;
pub use session::{CaptureTarget, Session, TargetKind};
