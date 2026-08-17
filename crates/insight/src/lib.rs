//! Offline reasoning over persisted timelines.

#![deny(warnings)]

pub mod ask;
pub mod clustering;
pub mod context;
pub mod notes;
pub mod summarizer;

pub use ask::{
    AskAnswer, AskCitation, AskClaim, AskEngine, AskError, AskEvidence, AskReply, AskResult,
    AskTurn,
};
pub use context::{
    ConsultationOutcome, ConsultedEvidence, ConsultedMoment, ConsultedPrecision,
    ReasoningContextError, SCREEN_INSPECTION_BUDGET, ScreenConsultation, ScreenConsultationLog,
    ScreenInspectionBudget,
};

pub use clustering::{
    ClusterError, ClusterReport, Clusterer, DerivedView, OpenThread, OpenThreadKind, TopicLink,
    TopicRegion,
};

pub use notes::{
    AppliedNotesOverlayOperation, CachedGroundedMeetingNotes, GroundedMeetingNotesReport,
    GroundingInput, MeetingNotesError, MeetingNotesGenerator, NotesBlockProvenance,
    NotesOverlayError, NotesOverlayOperation, OverlayTarget, PresentedNotesBlock,
    PresentedNotesBlockId, PresentedNotesDocument, RecordingNotes, RecordingNotesBlock,
    RecordingNotesBlockId, RecordingNotesSection, RecordingNotesSectionKind, RecordingNotesVersion,
    SourceStatus, append_notes_overlay_operation, compose_notes_document,
    load_latest_grounded_notes, load_latest_grounded_notes_status, load_notes_overlay,
    load_presented_notes_document, recording_notes_version,
};

pub use summarizer::{
    Attendee, Claim, Commitment, ContextMode, Cost, Objection, Pricing, Recap, Summarizer,
    SummaryError, SummaryReport, TalkTime,
};
