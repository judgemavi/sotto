//! App-side assembly for the markdown vault projection.
//!
//! `rag` owns file safety and markdown diffs. This module is the intentional upward seam which
//! composes `insight` documents, translates external changes to overlay operations, and supplies
//! the append-only record projection.

use std::{collections::BTreeMap, fmt, path::Path, time::SystemTime};

use chrono::{DateTime, Utc};
use insight::{
    NotesOverlayOperation, OverlayTarget, PresentedNotesBlock, PresentedNotesBlockId,
    PresentedNotesDocument, RecordingNotes, RecordingNotesSectionKind,
    append_notes_overlay_operation, load_latest_grounded_notes_status,
    load_presented_notes_document,
};
use rag::{
    Store, VaultBlock, VaultCitation, VaultEdit, VaultEntry, VaultError, VaultMirror, VaultSync,
};
use sotto_core::{Entry, EntryId, EventPayload, RagError, SessionId};

use crate::persistence_runtime::block_on;

#[derive(Debug)]
pub enum VaultControllerError {
    Projection(VaultError),
    Persistence(RagError),
    Notes(String),
}

impl fmt::Display for VaultControllerError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Projection(error) => error.fmt(formatter),
            Self::Persistence(error) => error.fmt(formatter),
            Self::Notes(error) => formatter.write_str(error),
        }
    }
}

impl std::error::Error for VaultControllerError {}

impl From<VaultError> for VaultControllerError {
    fn from(value: VaultError) -> Self {
        Self::Projection(value)
    }
}

impl From<RagError> for VaultControllerError {
    fn from(value: RagError) -> Self {
        Self::Persistence(value)
    }
}

struct ComposedEntry {
    projection: VaultEntry,
    artifact: Option<RecordingNotes>,
    document: Option<PresentedNotesDocument>,
}

/// Rebuilds the complete vault and ingests at most one observed external edit batch.
///
/// `VaultMirror` returns before writing when an editor raced Sotto. The controller appends those
/// changes to the canonical overlay, recomposes from SQLite, and retries; the retry is what proves
/// neither the external edit nor a concurrent in-app edit is overwritten.
pub fn sync_vault(database: &Path, root: &Path) -> Result<VaultSync, VaultControllerError> {
    block_on(async {
        let store = Store::open(database).await?;
        let composed = compose_library(&store).await?;
        let inputs = composed
            .values()
            .map(|entry| entry.projection.clone())
            .collect::<Vec<_>>();
        let mirror = VaultMirror::new(root);
        match mirror.sync(&inputs)? {
            VaultSync::Synced { files } => Ok(VaultSync::Synced { files }),
            VaultSync::NeedsIngestion {
                entry_id,
                revision,
                edits,
                ..
            } => {
                let entry = composed.get(&entry_id).ok_or_else(|| {
                    VaultControllerError::Notes(
                        "vault edit refers to an entry no longer in the library".to_owned(),
                    )
                })?;
                let artifact = entry.artifact.as_ref().ok_or_else(|| {
                    VaultControllerError::Notes(
                        "vault note has no generated artifact to receive an overlay".to_owned(),
                    )
                })?;
                let document = entry.document.as_ref().ok_or_else(|| {
                    VaultControllerError::Notes(
                        "vault note has no composed document to receive an overlay".to_owned(),
                    )
                })?;
                append_external_edits(&store, entry_id, artifact, document, &edits).await?;
                mirror.acknowledge_external_edit(entry_id, &revision)?;
                let recomposed = compose_library(&store).await?;
                let inputs = recomposed
                    .values()
                    .map(|entry| entry.projection.clone())
                    .collect::<Vec<_>>();
                Ok(mirror.sync(&inputs)?)
            }
        }
    })
}

async fn compose_library(
    store: &Store,
) -> Result<BTreeMap<EntryId, ComposedEntry>, VaultControllerError> {
    let entries = store.list_entries().await?;
    let sessions = store
        .list_sessions()
        .await?
        .into_iter()
        .map(|session| (session.id, session))
        .collect::<BTreeMap<_, _>>();
    let mut output = BTreeMap::new();
    for entry in entries {
        let mut artifact = None;
        let mut document = None;
        let mut artifact_session = None;
        for session_id in entry.session_ids().iter().rev() {
            let cached = load_latest_grounded_notes_status(store, *session_id)
                .await
                .map_err(|error| VaultControllerError::Notes(error.to_string()))?;
            let Some(cached) = cached else {
                continue;
            };
            let presented =
                load_presented_notes_document(store, entry.id(), &cached.report.artifact)
                    .await
                    .map_err(|error| VaultControllerError::Notes(error.to_string()))?;
            artifact_session = Some(*session_id);
            artifact = Some(cached.report.artifact);
            document = Some(presented);
            break;
        }
        let title = entry.title().map_or_else(
            || {
                entry
                    .session_ids()
                    .first()
                    .and_then(|id| sessions.get(id))
                    .map_or_else(
                        || format!("Entry {}", entry.id().get()),
                        |session| session.capture_target.display_name.clone(),
                    )
            },
            ToString::to_string,
        );
        let capture_targets = entry
            .session_ids()
            .iter()
            .filter_map(|id| sessions.get(id))
            .map(|session| session.capture_target.display_name.clone())
            .fold(Vec::<String>::new(), |mut targets, target| {
                if !targets.contains(&target) {
                    targets.push(target);
                }
                targets
            });
        let date = entry_date(&entry, &sessions);
        let blocks = document.as_ref().map_or_else(Vec::new, |document| {
            document
                .blocks
                .iter()
                .map(|block| vault_block(block, artifact_session))
                .collect()
        });
        let mut transcript = Vec::new();
        for session_id in entry.session_ids() {
            transcript.push(format!("### Recording {}", session_id.get()));
            let mut events = store.load_session(*session_id).await?;
            events.sort_by_key(sotto_core::TimelineEvent::id);
            let replayed = sotto_core::replay(&events)
                .map_err(|error| RagError::Storage(error.to_string()))?;
            for event in replayed.active().values() {
                if let EventPayload::UtteranceFinal(utterance) = event.payload() {
                    transcript.push(format!(
                        "[{}] {}: {} ^event-{}-{}",
                        timecode(utterance.start),
                        utterance.source.speaker_name(),
                        utterance.text,
                        session_id.get(),
                        event.id().get(),
                    ));
                }
            }
        }
        output.insert(
            entry.id(),
            ComposedEntry {
                projection: VaultEntry {
                    id: entry.id(),
                    title,
                    date,
                    participants: Vec::new(),
                    capture_targets,
                    series: entry.series().map(str::to_owned),
                    linked_entries: Vec::new(),
                    blocks,
                    transcript,
                },
                artifact,
                document,
            },
        );
    }
    Ok(output)
}

fn entry_date(entry: &Entry, sessions: &BTreeMap<SessionId, rag::SessionSummary>) -> String {
    let millis = entry
        .session_ids()
        .iter()
        .filter_map(|id| sessions.get(id))
        .map(|session| session.started_at_unix_ms)
        .min()
        .unwrap_or_else(|| entry.created_at_unix_ms());
    i64::try_from(millis)
        .ok()
        .and_then(DateTime::<Utc>::from_timestamp_millis)
        .map_or_else(
            || "unknown".to_owned(),
            |date| date.format("%Y-%m-%d").to_string(),
        )
}

fn vault_block(block: &PresentedNotesBlock, session_id: Option<SessionId>) -> VaultBlock {
    let id = presented_id(&block.id);
    let citations = session_id.map_or_else(Vec::new, |session_id| {
        block
            .meeting_citations
            .iter()
            .map(|event_id| VaultCitation {
                label: format!("event {}", event_id.get()),
                session_id,
                event_id: *event_id,
            })
            .collect()
    });
    VaultBlock {
        id,
        section: section_title(block.section).to_owned(),
        text: block.text.clone(),
        action: block.action,
        checked: block.checked,
        owner: block.owner.clone(),
        due_date: block.due_date.clone(),
        citations,
    }
}

async fn append_external_edits(
    store: &Store,
    entry_id: EntryId,
    artifact: &RecordingNotes,
    document: &PresentedNotesDocument,
    edits: &[VaultEdit],
) -> Result<(), VaultControllerError> {
    let mut targets = document
        .blocks
        .iter()
        .map(|block| (presented_id(&block.id), target(block)))
        .collect::<BTreeMap<_, _>>();
    let base_time = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_err(|error| VaultControllerError::Notes(error.to_string()))?
        .as_millis();
    let base_time =
        u64::try_from(base_time).map_err(|error| VaultControllerError::Notes(error.to_string()))?;
    let mut operations = Vec::new();
    for edit in edits {
        match edit {
            VaultEdit::Add {
                block_id,
                section,
                text,
                action,
                checked,
            } => {
                let section = parse_section(section).ok_or_else(|| {
                    VaultControllerError::Notes(format!(
                        "external block uses unknown section {section:?}"
                    ))
                })?;
                operations.push(NotesOverlayOperation::Add {
                    user_block_id: block_id.clone(),
                    section,
                    text: text.clone(),
                    action: *action,
                    owner: None,
                    due_date: None,
                });
                let added = OverlayTarget {
                    block_id: block_id.clone(),
                    section,
                    action: *action,
                    meeting_citations: Vec::new(),
                    external_citations: Vec::new(),
                };
                targets.insert(block_id.clone(), added.clone());
                if *action && *checked {
                    operations.push(NotesOverlayOperation::SetChecked {
                        target: added,
                        checked: true,
                    });
                }
            }
            VaultEdit::Reword { block_id, text } => {
                let target = require_target(&targets, block_id)?;
                let current = document
                    .blocks
                    .iter()
                    .find(|block| presented_id(&block.id) == *block_id);
                operations.push(NotesOverlayOperation::Reword {
                    target,
                    text: text.clone(),
                    owner: current.and_then(|block| block.owner.clone()),
                    due_date: current.and_then(|block| block.due_date.clone()),
                });
            }
            VaultEdit::SetChecked { block_id, checked } => {
                operations.push(NotesOverlayOperation::SetChecked {
                    target: require_target(&targets, block_id)?,
                    checked: *checked,
                });
            }
            VaultEdit::Hide { block_id } => operations.push(NotesOverlayOperation::Hide {
                target: require_target(&targets, block_id)?,
            }),
            VaultEdit::Reorder { block_id, after } => {
                operations.push(NotesOverlayOperation::Reorder {
                    target: require_target(&targets, block_id)?,
                    after: after
                        .as_ref()
                        .map(|id| require_target(&targets, id))
                        .transpose()?,
                });
            }
        }
    }
    for (index, operation) in operations.iter().enumerate() {
        append_notes_overlay_operation(
            store,
            entry_id,
            artifact,
            operation,
            base_time.saturating_add(index as u64),
        )
        .await
        .map_err(|error| VaultControllerError::Notes(error.to_string()))?;
    }
    Ok(())
}

fn require_target(
    targets: &BTreeMap<String, OverlayTarget>,
    id: &str,
) -> Result<OverlayTarget, VaultControllerError> {
    targets.get(id).cloned().ok_or_else(|| {
        VaultControllerError::Notes(format!("external edit names unknown block {id:?}"))
    })
}

fn target(block: &PresentedNotesBlock) -> OverlayTarget {
    OverlayTarget {
        block_id: presented_id(&block.id),
        section: block.section,
        action: block.action,
        meeting_citations: block.meeting_citations.clone(),
        external_citations: block.external_citations.clone(),
    }
}

fn presented_id(id: &PresentedNotesBlockId) -> String {
    match id {
        PresentedNotesBlockId::Generated(id) => id.as_str().to_owned(),
        PresentedNotesBlockId::User(id) => id.clone(),
    }
}

const fn section_title(section: RecordingNotesSectionKind) -> &'static str {
    match section {
        RecordingNotesSectionKind::Overview => "Overview",
        RecordingNotesSectionKind::Topics => "Topics",
        RecordingNotesSectionKind::Explanations => "Explanations",
        RecordingNotesSectionKind::Findings => "Findings",
        RecordingNotesSectionKind::Decisions => "Decisions",
        RecordingNotesSectionKind::ActionItems => "Action items",
        RecordingNotesSectionKind::OpenQuestions => "Open questions",
        RecordingNotesSectionKind::Risks => "Risks",
        RecordingNotesSectionKind::FollowUps => "Follow-ups",
    }
}

fn parse_section(value: &str) -> Option<RecordingNotesSectionKind> {
    [
        RecordingNotesSectionKind::Overview,
        RecordingNotesSectionKind::Topics,
        RecordingNotesSectionKind::Explanations,
        RecordingNotesSectionKind::Findings,
        RecordingNotesSectionKind::Decisions,
        RecordingNotesSectionKind::ActionItems,
        RecordingNotesSectionKind::OpenQuestions,
        RecordingNotesSectionKind::Risks,
        RecordingNotesSectionKind::FollowUps,
    ]
    .into_iter()
    .find(|section| section_title(*section) == value)
}

fn timecode(value: std::time::Duration) -> String {
    let seconds = value.as_secs();
    let minutes = seconds / 60;
    format!("{minutes:02}:{:02}", seconds % 60)
}

#[cfg(test)]
mod tests {
    use insight::{compose_notes_document, load_presented_notes_document};

    use super::*;

    #[tokio::test(flavor = "multi_thread")]
    async fn external_reword_and_check_append_verbatim_overlay_operations()
    -> Result<(), Box<dyn std::error::Error>> {
        let store = Store::open_in_memory().await?;
        let entry_id = EntryId::new(88);
        store.create_entry(&Entry::new(entry_id, 1, None)).await?;
        let artifact: RecordingNotes = serde_json::from_value(serde_json::json!({
            "sections": [{
                "kind": "action_items",
                "blocks": [{
                    "type": "action",
                    "id": "action-1",
                    "text": "Draft wording",
                    "meeting_citations": [],
                    "external_citations": [],
                    "owner": null,
                    "owner_meeting_citations": [],
                    "owner_external_citations": [],
                    "due_date": null,
                    "due_date_meeting_citations": [],
                    "due_date_external_citations": []
                }]
            }]
        }))?;
        let document = compose_notes_document(&artifact, &[])?;
        append_external_edits(
            &store,
            entry_id,
            &artifact,
            &document,
            &[
                VaultEdit::Reword {
                    block_id: "action-1".to_owned(),
                    text: "The person's exact words".to_owned(),
                },
                VaultEdit::SetChecked {
                    block_id: "action-1".to_owned(),
                    checked: true,
                },
            ],
        )
        .await?;
        let presented = load_presented_notes_document(&store, entry_id, &artifact).await?;
        assert_eq!(presented.blocks[0].text, "The person's exact words");
        assert!(presented.blocks[0].checked);
        assert_eq!(
            presented.blocks[0].provenance,
            insight::NotesBlockProvenance::EditedFromDraft
        );
        Ok(())
    }
}
