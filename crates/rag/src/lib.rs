//! Local SQLite persistence and hybrid retrieval.
//!
//! `fastembed` downloads model weights on first use and caches them; weights are not
//! bundled into Sotto. The model is loaded lazily and can be explicitly unloaded.

#![deny(warnings)]

#[cfg(test)]
mod annotation_tests;
mod entities;
mod notes_overlay;
mod schema;
mod store_async;
mod vault;

pub use notes_overlay::NotesOverlayRow;
pub use store_async::{
    DEFAULT_RECORDING_BUDGET_BYTES, DerivedTranscript, DocumentKind, GroundedDerivedArtifact,
    GroundedDerivedView, IngestMetadata, LocalEvidenceReceipt, LocalProvenance, QuarantineRecovery,
    RecordingReference, RecordingUsage, SearchFilter, SessionSummary, Store, TimelinePersistence,
};
pub use vault::{
    SottoLink, VaultBlock, VaultCitation, VaultEdit, VaultEntry, VaultError, VaultMirror,
    VaultStatus, VaultSync,
};
