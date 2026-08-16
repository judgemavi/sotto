use std::{
    collections::{BTreeMap, BTreeSet},
    fmt::Write as _,
};

use mcp::EvidenceId;
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use sotto_core::{EntryId, EventId};

use super::{
    RecordingNotes, RecordingNotesBlock, RecordingNotesBlockId, RecordingNotesSectionKind,
};

/// Immutable identity of one generated artifact version.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(transparent)]
pub struct RecordingNotesVersion(String);

impl RecordingNotesVersion {
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Stable description of the generated block an operation originally addressed.
///
/// Keeping the anchors in the operation is what permits a later artifact to re-bind by overlap
/// even when its newly derived block id is different.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct OverlayTarget {
    pub block_id: String,
    pub section: RecordingNotesSectionKind,
    pub action: bool,
    #[serde(default)]
    pub meeting_citations: Vec<EventId>,
    #[serde(default)]
    pub external_citations: Vec<EvidenceId>,
}

impl OverlayTarget {
    #[must_use]
    pub fn from_block(section: RecordingNotesSectionKind, block: &RecordingNotesBlock) -> Self {
        let mut meeting_citations = block_meeting_citations(block);
        meeting_citations.sort_unstable();
        meeting_citations.dedup();
        let mut external_citations = block_external_citations(block);
        external_citations.sort();
        external_citations.dedup();
        Self {
            block_id: block.id().as_str().to_owned(),
            section,
            action: matches!(block, RecordingNotesBlock::Action { .. }),
            meeting_citations,
            external_citations,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum NotesOverlayOperation {
    Add {
        user_block_id: String,
        section: RecordingNotesSectionKind,
        text: String,
        action: bool,
        owner: Option<String>,
        due_date: Option<String>,
    },
    Reword {
        target: OverlayTarget,
        text: String,
        owner: Option<String>,
        due_date: Option<String>,
    },
    SetChecked {
        target: OverlayTarget,
        checked: bool,
    },
    Hide {
        target: OverlayTarget,
    },
    Reorder {
        target: OverlayTarget,
        after: Option<OverlayTarget>,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AppliedNotesOverlayOperation {
    pub sequence: u64,
    pub artifact_version: RecordingNotesVersion,
    pub operation: NotesOverlayOperation,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NotesBlockProvenance {
    Generated,
    EditedFromDraft,
    UserAuthored,
}

#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum PresentedNotesBlockId {
    Generated(RecordingNotesBlockId),
    User(String),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PresentedNotesBlock {
    pub id: PresentedNotesBlockId,
    pub section: RecordingNotesSectionKind,
    pub text: String,
    pub action: bool,
    pub owner: Option<String>,
    pub due_date: Option<String>,
    pub checked: bool,
    pub provenance: NotesBlockProvenance,
    pub orphaned: bool,
    pub meeting_citations: Vec<EventId>,
    pub external_citations: Vec<EvidenceId>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PresentedNotesDocument {
    pub artifact_version: RecordingNotesVersion,
    pub blocks: Vec<PresentedNotesBlock>,
}

#[derive(Debug, thiserror::Error)]
pub enum NotesOverlayError {
    #[error(transparent)]
    Persistence(#[from] sotto_core::RagError),
    #[error(transparent)]
    Serialization(#[from] serde_json::Error),
    #[error("a notes overlay operation contains empty user text")]
    EmptyText,
    #[error("a notes overlay add operation has an empty block id")]
    EmptyUserBlockId,
    #[error("notes overlay contains duplicate user block id {0:?}")]
    DuplicateUserBlockId(String),
}

pub fn recording_notes_version(
    artifact: &RecordingNotes,
) -> Result<RecordingNotesVersion, serde_json::Error> {
    let encoded = serde_json::to_vec(artifact)?;
    let digest = Sha256::digest(encoded);
    let mut version = String::with_capacity(71);
    version.push_str("sha256:");
    for byte in digest {
        let _ = write!(version, "{byte:02x}");
    }
    Ok(RecordingNotesVersion(version))
}

pub async fn append_notes_overlay_operation(
    store: &rag::Store,
    entry_id: EntryId,
    artifact: &RecordingNotes,
    operation: &NotesOverlayOperation,
    created_at_unix_ms: u64,
) -> Result<u64, NotesOverlayError> {
    validate_operation(operation)?;
    let version = recording_notes_version(artifact)?;
    Ok(store
        .append_notes_overlay_operation(
            entry_id,
            version.as_str(),
            &serde_json::to_string(operation)?,
            created_at_unix_ms,
        )
        .await?)
}

pub async fn load_notes_overlay(
    store: &rag::Store,
    entry_id: EntryId,
) -> Result<Vec<AppliedNotesOverlayOperation>, NotesOverlayError> {
    store
        .load_notes_overlay(entry_id)
        .await?
        .into_iter()
        .map(|row| {
            let operation = serde_json::from_str(&row.operation)?;
            validate_operation(&operation)?;
            Ok(AppliedNotesOverlayOperation {
                sequence: row.sequence,
                artifact_version: RecordingNotesVersion(row.artifact_version),
                operation,
            })
        })
        .collect()
}

pub async fn load_presented_notes_document(
    store: &rag::Store,
    entry_id: EntryId,
    artifact: &RecordingNotes,
) -> Result<PresentedNotesDocument, NotesOverlayError> {
    let operations = load_notes_overlay(store, entry_id).await?;
    compose_notes_document(artifact, &operations)
}

pub fn compose_notes_document(
    artifact: &RecordingNotes,
    operations: &[AppliedNotesOverlayOperation],
) -> Result<PresentedNotesDocument, NotesOverlayError> {
    let artifact_version = recording_notes_version(artifact)?;
    let mut blocks = artifact
        .sections
        .iter()
        .flat_map(|section| {
            section
                .blocks
                .iter()
                .map(move |block| presented_generated(section.kind, block))
        })
        .collect::<Vec<_>>();
    let mut bindings = BTreeMap::<String, PresentedNotesBlockId>::new();
    let mut user_ids = BTreeSet::new();

    for applied in operations {
        validate_operation(&applied.operation)?;
        match &applied.operation {
            NotesOverlayOperation::Add {
                user_block_id,
                section,
                text,
                action,
                owner,
                due_date,
            } => {
                if !user_ids.insert(user_block_id.clone()) {
                    return Err(NotesOverlayError::DuplicateUserBlockId(
                        user_block_id.clone(),
                    ));
                }
                blocks.push(PresentedNotesBlock {
                    id: PresentedNotesBlockId::User(user_block_id.clone()),
                    section: *section,
                    text: text.clone(),
                    action: *action,
                    owner: owner.clone(),
                    due_date: due_date.clone(),
                    checked: false,
                    provenance: NotesBlockProvenance::UserAuthored,
                    orphaned: false,
                    meeting_citations: Vec::new(),
                    external_citations: Vec::new(),
                });
                bindings.insert(
                    user_block_id.clone(),
                    PresentedNotesBlockId::User(user_block_id.clone()),
                );
            }
            NotesOverlayOperation::Reword {
                target,
                text,
                owner,
                due_date,
            } => {
                let resolved = resolve_target(target, &blocks, &bindings);
                let id = if let Some(index) = resolved {
                    let block = &mut blocks[index];
                    block.text.clone_from(text);
                    block.owner.clone_from(owner);
                    block.due_date.clone_from(due_date);
                    if block.provenance == NotesBlockProvenance::Generated {
                        block.provenance = NotesBlockProvenance::EditedFromDraft;
                    }
                    block.id.clone()
                } else {
                    let id = PresentedNotesBlockId::User(format!("orphan-{}", applied.sequence));
                    blocks.push(PresentedNotesBlock {
                        id: id.clone(),
                        section: target.section,
                        text: text.clone(),
                        action: target.action,
                        owner: owner.clone(),
                        due_date: due_date.clone(),
                        checked: false,
                        provenance: NotesBlockProvenance::UserAuthored,
                        orphaned: true,
                        meeting_citations: Vec::new(),
                        external_citations: Vec::new(),
                    });
                    id
                };
                bindings.insert(target.block_id.clone(), id);
            }
            NotesOverlayOperation::SetChecked { target, checked } => {
                if let Some(index) = resolve_target(target, &blocks, &bindings)
                    && blocks[index].action
                {
                    blocks[index].checked = *checked;
                }
            }
            NotesOverlayOperation::Hide { target } => {
                if let Some(index) = resolve_target(target, &blocks, &bindings) {
                    blocks.remove(index);
                }
            }
            NotesOverlayOperation::Reorder { target, after } => {
                let Some(from) = resolve_target(target, &blocks, &bindings) else {
                    continue;
                };
                let block = blocks.remove(from);
                let insertion = after
                    .as_ref()
                    .and_then(|anchor| resolve_target(anchor, &blocks, &bindings))
                    .map_or(0, |index| index.saturating_add(1));
                blocks.insert(insertion, block);
            }
        }
    }

    Ok(PresentedNotesDocument {
        artifact_version,
        blocks,
    })
}

fn validate_operation(operation: &NotesOverlayOperation) -> Result<(), NotesOverlayError> {
    match operation {
        NotesOverlayOperation::Add {
            user_block_id,
            text,
            ..
        } => {
            if user_block_id.trim().is_empty() {
                return Err(NotesOverlayError::EmptyUserBlockId);
            }
            if text.trim().is_empty() {
                return Err(NotesOverlayError::EmptyText);
            }
        }
        NotesOverlayOperation::Reword { text, .. } if text.trim().is_empty() => {
            return Err(NotesOverlayError::EmptyText);
        }
        NotesOverlayOperation::Reword { .. }
        | NotesOverlayOperation::SetChecked { .. }
        | NotesOverlayOperation::Hide { .. }
        | NotesOverlayOperation::Reorder { .. } => {}
    }
    Ok(())
}

fn presented_generated(
    section: RecordingNotesSectionKind,
    block: &RecordingNotesBlock,
) -> PresentedNotesBlock {
    match block {
        RecordingNotesBlock::Claim {
            id,
            text,
            meeting_citations,
            external_citations,
        } => PresentedNotesBlock {
            id: PresentedNotesBlockId::Generated(id.clone()),
            section,
            text: text.clone(),
            action: false,
            owner: None,
            due_date: None,
            checked: false,
            provenance: NotesBlockProvenance::Generated,
            orphaned: false,
            meeting_citations: meeting_citations.clone(),
            external_citations: external_citations.clone(),
        },
        RecordingNotesBlock::Action {
            id,
            text,
            owner,
            due_date,
            ..
        } => PresentedNotesBlock {
            id: PresentedNotesBlockId::Generated(id.clone()),
            section,
            text: text.clone(),
            action: true,
            owner: owner.clone(),
            due_date: due_date.clone(),
            checked: false,
            provenance: NotesBlockProvenance::Generated,
            orphaned: false,
            meeting_citations: block_meeting_citations(block),
            external_citations: block_external_citations(block),
        },
    }
}

fn resolve_target(
    target: &OverlayTarget,
    blocks: &[PresentedNotesBlock],
    bindings: &BTreeMap<String, PresentedNotesBlockId>,
) -> Option<usize> {
    if let Some(bound) = bindings.get(&target.block_id)
        && let Some(index) = blocks.iter().position(|block| &block.id == bound)
    {
        return Some(index);
    }
    if let Some(index) = blocks.iter().position(|block| match &block.id {
        PresentedNotesBlockId::Generated(id) => id.as_str() == target.block_id,
        PresentedNotesBlockId::User(id) => id == &target.block_id,
    }) {
        return Some(index);
    }
    blocks
        .iter()
        .enumerate()
        .filter(|(_, block)| {
            block.section == target.section
                && block.action == target.action
                && matches!(block.id, PresentedNotesBlockId::Generated(_))
        })
        .filter_map(|(index, block)| {
            let score =
                overlap(&target.meeting_citations, &block.meeting_citations).saturating_add(
                    overlap(&target.external_citations, &block.external_citations),
                );
            (score > 0).then_some((score, index, block.id.clone()))
        })
        .max_by(|left, right| left.0.cmp(&right.0).then_with(|| right.2.cmp(&left.2)))
        .map(|(_, index, _)| index)
}

fn overlap<T: Ord>(left: &[T], right: &[T]) -> usize {
    left.iter().filter(|item| right.contains(item)).count()
}

fn block_meeting_citations(block: &RecordingNotesBlock) -> Vec<EventId> {
    match block {
        RecordingNotesBlock::Claim {
            meeting_citations, ..
        } => meeting_citations.clone(),
        RecordingNotesBlock::Action {
            meeting_citations,
            owner_meeting_citations,
            due_date_meeting_citations,
            ..
        } => meeting_citations
            .iter()
            .chain(owner_meeting_citations)
            .chain(due_date_meeting_citations)
            .copied()
            .collect(),
    }
}

fn block_external_citations(block: &RecordingNotesBlock) -> Vec<EvidenceId> {
    match block {
        RecordingNotesBlock::Claim {
            external_citations, ..
        } => external_citations.clone(),
        RecordingNotesBlock::Action {
            external_citations,
            owner_external_citations,
            due_date_external_citations,
            ..
        } => external_citations
            .iter()
            .chain(owner_external_citations)
            .chain(due_date_external_citations)
            .cloned()
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn notes(citation: u64, text: &str, action: bool) -> Result<RecordingNotes, serde_json::Error> {
        let kind = if action {
            RecordingNotesSectionKind::ActionItems
        } else {
            RecordingNotesSectionKind::Findings
        };
        let value = if action {
            json!({"sections":[{"kind":"action_items","blocks":[{"type":"action","id":format!("block-{citation}"),"text":text,"meeting_citations":[citation],"owner":null,"due_date":null}]}]})
        } else {
            json!({"sections":[{"kind":"findings","blocks":[{"type":"claim","id":format!("block-{citation}"),"text":text,"meeting_citations":[citation]}]}]})
        };
        let mut notes: RecordingNotes = serde_json::from_value(value)?;
        // Tests exercise overlay identity rather than schema-id validation, so the compact fixture
        // supplies a deliberately readable id.
        notes.sections[0].kind = kind;
        Ok(notes)
    }

    fn applied(sequence: u64, operation: NotesOverlayOperation) -> AppliedNotesOverlayOperation {
        AppliedNotesOverlayOperation {
            sequence,
            artifact_version: RecordingNotesVersion("old".to_owned()),
            operation,
        }
    }

    #[test]
    fn matched_reword_and_check_survive_regeneration_verbatim()
    -> Result<(), Box<dyn std::error::Error>> {
        let old = notes(10, "old draft", true)?;
        let target = OverlayTarget::from_block(old.sections[0].kind, &old.sections[0].blocks[0]);
        let operations = vec![
            applied(
                1,
                NotesOverlayOperation::Reword {
                    target: target.clone(),
                    text: "  User's exact words — kept.  ".to_owned(),
                    owner: Some("A. Person".to_owned()),
                    due_date: Some("Friday".to_owned()),
                },
            ),
            applied(
                2,
                NotesOverlayOperation::SetChecked {
                    target,
                    checked: true,
                },
            ),
        ];
        let regenerated = notes(10, "different generated wording", true)?;
        let original = serde_json::to_vec(&regenerated)?;
        let document = compose_notes_document(&regenerated, &operations)?;
        assert_eq!(document.blocks[0].text, "  User's exact words — kept.  ");
        assert_eq!(
            document.blocks[0].provenance,
            NotesBlockProvenance::EditedFromDraft
        );
        assert!(document.blocks[0].checked);
        assert_eq!(document.blocks[0].owner.as_deref(), Some("A. Person"));
        assert_eq!(document.blocks[0].due_date.as_deref(), Some("Friday"));
        assert_eq!(serde_json::to_vec(&regenerated)?, original);
        Ok(())
    }

    #[tokio::test]
    async fn typed_overlay_reopens_into_the_identical_composed_document()
    -> Result<(), Box<dyn std::error::Error>> {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("notes.sqlite");
        let entry_id = EntryId::new(71);
        let artifact = notes(10, "draft", false)?;
        let operation = NotesOverlayOperation::Add {
            user_block_id: "written-once".to_owned(),
            section: RecordingNotesSectionKind::Findings,
            text: " punctuation, spacing, and Unicode — byte-identical ".to_owned(),
            action: false,
            owner: None,
            due_date: None,
        };
        let before_restart = {
            let store = rag::Store::open(&path).await?;
            store
                .create_entry(&sotto_core::Entry::new(entry_id, 1, None))
                .await?;
            append_notes_overlay_operation(&store, entry_id, &artifact, &operation, 2).await?;
            load_presented_notes_document(&store, entry_id, &artifact).await?
        };
        let reopened = rag::Store::open(&path).await?;
        let after_restart = load_presented_notes_document(&reopened, entry_id, &artifact).await?;
        assert_eq!(after_restart, before_restart);
        assert_eq!(
            after_restart.blocks[1].text,
            " punctuation, spacing, and Unicode — byte-identical "
        );
        Ok(())
    }

    #[test]
    fn orphaned_reword_is_kept_as_uncited_user_block() -> Result<(), Box<dyn std::error::Error>> {
        let old = notes(10, "draft", false)?;
        let target = OverlayTarget::from_block(old.sections[0].kind, &old.sections[0].blocks[0]);
        let regenerated = notes(99, "unrelated", false)?;
        let document = compose_notes_document(
            &regenerated,
            &[applied(
                1,
                NotesOverlayOperation::Reword {
                    target,
                    text: "mine".to_owned(),
                    owner: None,
                    due_date: None,
                },
            )],
        )?;
        let orphan = document
            .blocks
            .iter()
            .find(|block| block.orphaned)
            .ok_or("missing orphan")?;
        assert_eq!(orphan.text, "mine");
        assert_eq!(orphan.provenance, NotesBlockProvenance::UserAuthored);
        assert!(orphan.meeting_citations.is_empty());
        assert!(orphan.external_citations.is_empty());
        Ok(())
    }

    #[test]
    fn hidden_match_stays_hidden_and_user_add_never_has_citations()
    -> Result<(), Box<dyn std::error::Error>> {
        let old = notes(10, "draft", false)?;
        let target = OverlayTarget::from_block(old.sections[0].kind, &old.sections[0].blocks[0]);
        let document = compose_notes_document(
            &old,
            &[
                applied(1, NotesOverlayOperation::Hide { target }),
                applied(
                    2,
                    NotesOverlayOperation::Add {
                        user_block_id: "mine".to_owned(),
                        section: RecordingNotesSectionKind::Findings,
                        text: "personal".to_owned(),
                        action: false,
                        owner: None,
                        due_date: None,
                    },
                ),
                applied(
                    3,
                    NotesOverlayOperation::Reword {
                        target: OverlayTarget {
                            block_id: "mine".to_owned(),
                            section: RecordingNotesSectionKind::Findings,
                            action: false,
                            meeting_citations: Vec::new(),
                            external_citations: Vec::new(),
                        },
                        text: "personal, revised".to_owned(),
                        owner: None,
                        due_date: None,
                    },
                ),
            ],
        )?;
        assert_eq!(document.blocks.len(), 1);
        assert_eq!(document.blocks[0].text, "personal, revised");
        assert_eq!(
            document.blocks[0].provenance,
            NotesBlockProvenance::UserAuthored
        );
        assert!(document.blocks[0].meeting_citations.is_empty());
        Ok(())
    }
}
