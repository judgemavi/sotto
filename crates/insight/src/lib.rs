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
pub use context::ReasoningContextError;

pub use clustering::{
    ClusterError, ClusterReport, Clusterer, DerivedView, OpenThread, OpenThreadKind, TopicLink,
    TopicRegion,
};

pub use notes::{
    ActionItem, CachedGroundedMeetingNotes, EvidenceBasis, GroundedActionItem,
    GroundedMeetingNotes, GroundedMeetingNotesReport, GroundedNoteItem, GroundingInput,
    MeetingNotes, MeetingNotesError, MeetingNotesGenerator, MeetingNotesReport, NoteItem,
    SourceStatus, load_latest_grounded_notes, load_latest_grounded_notes_status,
};

pub use summarizer::{
    Attendee, Claim, Commitment, ContextMode, Cost, Objection, Pricing, Recap, Summarizer,
    SummaryError, SummaryReport, TalkTime,
};
