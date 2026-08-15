use std::{collections::HashSet, fmt::Write as _};

use mcp::EvidenceId;
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use sotto_core::{EventId, SessionId};

use super::{MeetingNotesError, validate_cited_claim, validate_cited_optional_claim};

/// Stable identity of one generated summary block.
///
/// The model never supplies this value. Sotto derives it from the recording, section kind, and
/// cited anchors after parsing the provider response.
#[derive(Clone, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(transparent)]
pub struct RecordingNotesBlockId(String);

impl RecordingNotesBlockId {
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Recording-supported section kinds. A result contains only the sections its content supports.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RecordingNotesSectionKind {
    Overview,
    Topics,
    Explanations,
    Findings,
    Decisions,
    ActionItems,
    OpenQuestions,
    Risks,
    FollowUps,
}

impl RecordingNotesSectionKind {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Overview => "overview",
            Self::Topics => "topics",
            Self::Explanations => "explanations",
            Self::Findings => "findings",
            Self::Decisions => "decisions",
            Self::ActionItems => "action_items",
            Self::OpenQuestions => "open_questions",
            Self::Risks => "risks",
            Self::FollowUps => "follow_ups",
        }
    }

    const fn expects_actions(self) -> bool {
        matches!(self, Self::ActionItems | Self::FollowUps)
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum RecordingNotesBlock {
    Claim {
        id: RecordingNotesBlockId,
        text: String,
        #[serde(default)]
        meeting_citations: Vec<EventId>,
        #[serde(default)]
        external_citations: Vec<EvidenceId>,
    },
    Action {
        id: RecordingNotesBlockId,
        text: String,
        #[serde(default)]
        meeting_citations: Vec<EventId>,
        #[serde(default)]
        external_citations: Vec<EvidenceId>,
        owner: Option<String>,
        #[serde(default)]
        owner_meeting_citations: Vec<EventId>,
        #[serde(default)]
        owner_external_citations: Vec<EvidenceId>,
        due_date: Option<String>,
        #[serde(default)]
        due_date_meeting_citations: Vec<EventId>,
        #[serde(default)]
        due_date_external_citations: Vec<EvidenceId>,
    },
}

impl RecordingNotesBlock {
    #[must_use]
    pub const fn id(&self) -> &RecordingNotesBlockId {
        match self {
            Self::Claim { id, .. } | Self::Action { id, .. } => id,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RecordingNotesSection {
    pub kind: RecordingNotesSectionKind,
    pub blocks: Vec<RecordingNotesBlock>,
}

/// The superseding, adaptive recording-summary artifact.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RecordingNotes {
    pub sections: Vec<RecordingNotesSection>,
}

/// Citation lists default to empty because absent and empty carry the same meaning: no evidence of
/// that kind was cited.
///
/// This is not a weakening. `validate_cited_claim` rejects both an unsupported claim (no citations
/// at all) and any citation naming evidence that was not supplied. An omitted list therefore fails
/// exactly where an explicitly empty one would.
///
/// It matters because backends that cannot guarantee structured output — Codex among them — omit
/// empty arrays rather than emitting one per citation field. Demanding the field present turned a
/// well-formed set of notes into `missing field 'external_citations'`, which describes our schema
/// rather than anything wrong with the model's answer.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
enum DraftBlock {
    Claim {
        text: String,
        #[serde(default)]
        meeting_citations: Vec<EventId>,
        #[serde(default)]
        external_citations: Vec<EvidenceId>,
    },
    Action {
        text: String,
        #[serde(default)]
        meeting_citations: Vec<EventId>,
        #[serde(default)]
        external_citations: Vec<EvidenceId>,
        #[serde(default)]
        owner: Option<String>,
        #[serde(default)]
        owner_meeting_citations: Vec<EventId>,
        #[serde(default)]
        owner_external_citations: Vec<EvidenceId>,
        #[serde(default)]
        due_date: Option<String>,
        #[serde(default)]
        due_date_meeting_citations: Vec<EventId>,
        #[serde(default)]
        due_date_external_citations: Vec<EvidenceId>,
    },
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct RecordingNotesDraftSection {
    kind: RecordingNotesSectionKind,
    blocks: Vec<DraftBlock>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct RecordingNotesDraft {
    sections: Vec<RecordingNotesDraftSection>,
}

impl From<&RecordingNotes> for RecordingNotesDraft {
    fn from(notes: &RecordingNotes) -> Self {
        Self {
            sections: notes
                .sections
                .iter()
                .map(|section| RecordingNotesDraftSection {
                    kind: section.kind,
                    blocks: section
                        .blocks
                        .iter()
                        .map(|block| match block {
                            RecordingNotesBlock::Claim {
                                text,
                                meeting_citations,
                                external_citations,
                                ..
                            } => DraftBlock::Claim {
                                text: text.clone(),
                                meeting_citations: meeting_citations.clone(),
                                external_citations: external_citations.clone(),
                            },
                            RecordingNotesBlock::Action {
                                text,
                                meeting_citations,
                                external_citations,
                                owner,
                                owner_meeting_citations,
                                owner_external_citations,
                                due_date,
                                due_date_meeting_citations,
                                due_date_external_citations,
                                ..
                            } => DraftBlock::Action {
                                text: text.clone(),
                                meeting_citations: meeting_citations.clone(),
                                external_citations: external_citations.clone(),
                                owner: owner.clone(),
                                owner_meeting_citations: owner_meeting_citations.clone(),
                                owner_external_citations: owner_external_citations.clone(),
                                due_date: due_date.clone(),
                                due_date_meeting_citations: due_date_meeting_citations.clone(),
                                due_date_external_citations: due_date_external_citations.clone(),
                            },
                        })
                        .collect(),
                })
                .collect(),
        }
    }
}

impl RecordingNotesDraft {
    pub(super) fn finalize(self, session_id: SessionId) -> RecordingNotes {
        RecordingNotes {
            sections: self
                .sections
                .into_iter()
                .map(|section| {
                    let blocks = section
                        .blocks
                        .into_iter()
                        .map(|block| block.finalize(session_id, section.kind))
                        .collect();
                    RecordingNotesSection {
                        kind: section.kind,
                        blocks: consolidate_blocks(blocks),
                    }
                })
                .collect(),
        }
    }
}

fn consolidate_blocks(blocks: Vec<RecordingNotesBlock>) -> Vec<RecordingNotesBlock> {
    let mut consolidated: Vec<RecordingNotesBlock> = Vec::new();
    for block in blocks {
        if let Some(existing) = consolidated.iter_mut().find(|existing| {
            existing.id() == block.id()
                && std::mem::discriminant(*existing) == std::mem::discriminant(&block)
        }) {
            merge_same_identity(existing, block);
        } else {
            consolidated.push(block);
        }
    }
    consolidated
}

fn merge_same_identity(existing: &mut RecordingNotesBlock, incoming: RecordingNotesBlock) {
    match (existing, incoming) {
        (
            RecordingNotesBlock::Claim { text, .. },
            RecordingNotesBlock::Claim {
                text: incoming_text,
                ..
            },
        ) => merge_text(text, incoming_text),
        (
            RecordingNotesBlock::Action {
                text,
                owner,
                owner_meeting_citations,
                owner_external_citations,
                due_date,
                due_date_meeting_citations,
                due_date_external_citations,
                ..
            },
            RecordingNotesBlock::Action {
                text: incoming_text,
                owner: incoming_owner,
                due_date: incoming_due_date,
                ..
            },
        ) => {
            merge_text(text, incoming_text);
            if *owner != incoming_owner {
                *owner = None;
                owner_meeting_citations.clear();
                owner_external_citations.clear();
            }
            if *due_date != incoming_due_date {
                *due_date = None;
                due_date_meeting_citations.clear();
                due_date_external_citations.clear();
            }
        }
        // `consolidate_blocks` compares discriminants before calling this helper.
        _ => {}
    }
}

fn merge_text(existing: &mut String, incoming: String) {
    if existing != &incoming {
        existing.push_str("; ");
        existing.push_str(&incoming);
    }
}

impl DraftBlock {
    fn finalize(
        self,
        session_id: SessionId,
        section_kind: RecordingNotesSectionKind,
    ) -> RecordingNotesBlock {
        match self {
            Self::Claim {
                text,
                meeting_citations,
                external_citations,
            } => RecordingNotesBlock::Claim {
                id: block_id(
                    session_id,
                    section_kind,
                    &meeting_citations,
                    &external_citations,
                ),
                text,
                meeting_citations,
                external_citations,
            },
            Self::Action {
                text,
                meeting_citations,
                external_citations,
                owner,
                owner_meeting_citations,
                owner_external_citations,
                due_date,
                due_date_meeting_citations,
                due_date_external_citations,
            } => RecordingNotesBlock::Action {
                id: action_block_id(
                    session_id,
                    section_kind,
                    &meeting_citations,
                    &external_citations,
                    &owner_meeting_citations,
                    &owner_external_citations,
                    &due_date_meeting_citations,
                    &due_date_external_citations,
                ),
                text,
                meeting_citations,
                external_citations,
                owner,
                owner_meeting_citations,
                owner_external_citations,
                due_date,
                due_date_meeting_citations,
                due_date_external_citations,
            },
        }
    }
}

fn block_id(
    session_id: SessionId,
    section_kind: RecordingNotesSectionKind,
    meeting: &[EventId],
    external: &[EvidenceId],
) -> RecordingNotesBlockId {
    let mut hash = block_identity_hash(session_id, section_kind);
    hash_citations(&mut hash, "", meeting, external);
    finalize_block_id(hash)
}

/// An action's owner and due-date citations join its base citations as block identity, not just
/// its base citations alone. Two adaptive actions that cite the same single event but name
/// different owners are different facts; folding owner and due-date evidence in here is what keeps
/// `consolidate_blocks` from treating them as one block and nulling both owners out.
#[expect(
    clippy::too_many_arguments,
    reason = "mirrors validate_action_id's identity inputs"
)]
fn action_block_id(
    session_id: SessionId,
    section_kind: RecordingNotesSectionKind,
    meeting: &[EventId],
    external: &[EvidenceId],
    owner_meeting: &[EventId],
    owner_external: &[EvidenceId],
    due_date_meeting: &[EventId],
    due_date_external: &[EvidenceId],
) -> RecordingNotesBlockId {
    let mut hash = block_identity_hash(session_id, section_kind);
    hash_citations(&mut hash, "", meeting, external);
    hash_citations(&mut hash, "owner-", owner_meeting, owner_external);
    hash_citations(&mut hash, "due_date-", due_date_meeting, due_date_external);
    finalize_block_id(hash)
}

fn block_identity_hash(session_id: SessionId, section_kind: RecordingNotesSectionKind) -> Sha256 {
    let mut hash = Sha256::new();
    hash.update(b"recording-notes-block/v1\0");
    hash.update(session_id.get().to_string().as_bytes());
    hash.update(b"\0");
    hash.update(section_kind.as_str().as_bytes());
    hash
}

fn hash_citations(hash: &mut Sha256, prefix: &str, meeting: &[EventId], external: &[EvidenceId]) {
    let mut meeting = meeting.to_vec();
    meeting.sort_unstable();
    meeting.dedup();
    let mut external: Vec<_> = external.iter().map(EvidenceId::as_str).collect();
    external.sort_unstable();
    external.dedup();
    for event in meeting {
        hash.update(format!("\0{prefix}event:").as_bytes());
        hash.update(event.get().to_string().as_bytes());
    }
    for evidence in external {
        hash.update(format!("\0{prefix}external:").as_bytes());
        hash.update(evidence.as_bytes());
    }
}

fn finalize_block_id(hash: Sha256) -> RecordingNotesBlockId {
    let digest = hash.finalize();
    let mut value = String::with_capacity(51);
    value.push_str("recording-block-v1-");
    for byte in &digest[..16] {
        let _ = write!(value, "{byte:02x}");
    }
    RecordingNotesBlockId(value)
}

impl RecordingNotes {
    pub(super) fn validate(
        &self,
        session_id: SessionId,
        meeting_ids: &HashSet<EventId>,
        external_ids: &HashSet<EvidenceId>,
    ) -> Result<(), MeetingNotesError> {
        let mut section_kinds = HashSet::new();
        let mut block_ids = HashSet::new();
        for section in &self.sections {
            let field = section.kind.as_str();
            if section.blocks.is_empty() {
                return Err(MeetingNotesError::EmptySection { field });
            }
            if !section_kinds.insert(section.kind) {
                return Err(MeetingNotesError::DuplicateSection { field });
            }
            for block in &section.blocks {
                if !block_ids.insert(block.id()) {
                    return Err(MeetingNotesError::DuplicateBlockId {
                        block_id: block.id().as_str().to_owned(),
                    });
                }
                match block {
                    RecordingNotesBlock::Claim {
                        id,
                        text,
                        meeting_citations,
                        external_citations,
                    } => {
                        if section.kind.expects_actions() {
                            return Err(MeetingNotesError::WrongBlockKind { field });
                        }
                        validate_id(
                            id,
                            session_id,
                            section.kind,
                            meeting_citations,
                            external_citations,
                        )?;
                        validate_cited_claim(
                            field,
                            text,
                            meeting_citations,
                            external_citations,
                            meeting_ids,
                            external_ids,
                        )?;
                    }
                    RecordingNotesBlock::Action {
                        id,
                        text,
                        meeting_citations,
                        external_citations,
                        owner,
                        owner_meeting_citations,
                        owner_external_citations,
                        due_date,
                        due_date_meeting_citations,
                        due_date_external_citations,
                        ..
                    } => {
                        if !section.kind.expects_actions() {
                            return Err(MeetingNotesError::WrongBlockKind { field });
                        }
                        validate_action_id(
                            id,
                            session_id,
                            section.kind,
                            meeting_citations,
                            external_citations,
                            owner_meeting_citations,
                            owner_external_citations,
                            due_date_meeting_citations,
                            due_date_external_citations,
                        )?;
                        validate_cited_claim(
                            field,
                            text,
                            meeting_citations,
                            external_citations,
                            meeting_ids,
                            external_ids,
                        )?;
                        validate_cited_optional_claim(
                            field,
                            owner.as_deref(),
                            owner_meeting_citations,
                            owner_external_citations,
                            meeting_ids,
                            external_ids,
                        )?;
                        validate_cited_optional_claim(
                            field,
                            due_date.as_deref(),
                            due_date_meeting_citations,
                            due_date_external_citations,
                            meeting_ids,
                            external_ids,
                        )?;
                    }
                }
            }
        }
        Ok(())
    }
}

fn validate_id(
    id: &RecordingNotesBlockId,
    session_id: SessionId,
    kind: RecordingNotesSectionKind,
    meeting: &[EventId],
    external: &[EvidenceId],
) -> Result<(), MeetingNotesError> {
    let expected = block_id(session_id, kind, meeting, external);
    check_block_id(id, &expected)
}

/// Mirrors `action_block_id` exactly. A previous attempt updated only the id derivation and left
/// this validator checking base citations alone, which failed every action carrying owner or
/// due-date evidence with `InvalidBlockId` — the two must agree on what identity is made of.
#[expect(
    clippy::too_many_arguments,
    reason = "mirrors action_block_id's identity inputs"
)]
fn validate_action_id(
    id: &RecordingNotesBlockId,
    session_id: SessionId,
    kind: RecordingNotesSectionKind,
    meeting: &[EventId],
    external: &[EvidenceId],
    owner_meeting: &[EventId],
    owner_external: &[EvidenceId],
    due_date_meeting: &[EventId],
    due_date_external: &[EvidenceId],
) -> Result<(), MeetingNotesError> {
    let expected = action_block_id(
        session_id,
        kind,
        meeting,
        external,
        owner_meeting,
        owner_external,
        due_date_meeting,
        due_date_external,
    );
    check_block_id(id, &expected)
}

fn check_block_id(
    id: &RecordingNotesBlockId,
    expected: &RecordingNotesBlockId,
) -> Result<(), MeetingNotesError> {
    if id != expected {
        return Err(MeetingNotesError::InvalidBlockId {
            block_id: id.as_str().to_owned(),
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn omitted_and_explicit_empty_citations_parse_identically() -> Result<(), serde_json::Error> {
        let omitted: RecordingNotesDraft = serde_json::from_str(
            r#"{"sections":[{"kind":"findings","blocks":[{"type":"claim","text":"Per the spec","meeting_citations":[1]}]}]}"#,
        )?;
        let explicit: RecordingNotesDraft = serde_json::from_str(
            r#"{"sections":[{"kind":"findings","blocks":[{"type":"claim","text":"Per the spec","meeting_citations":[1],"external_citations":[]}]}]}"#,
        )?;
        assert_eq!(
            omitted.finalize(SessionId::new(7)),
            explicit.finalize(SessionId::new(7)),
            "an omitted citations array must parse identically to an explicit empty one"
        );
        Ok(())
    }

    #[test]
    fn ids_are_stable_across_text_changes_and_citation_order() -> Result<(), serde_json::Error> {
        let first: RecordingNotesDraft = serde_json::from_str(
            r#"{"sections":[{"kind":"findings","blocks":[{"type":"claim","text":"First wording","meeting_citations":[2,1]}]}]}"#,
        )?;
        let second: RecordingNotesDraft = serde_json::from_str(
            r#"{"sections":[{"kind":"findings","blocks":[{"type":"claim","text":"Rewritten","meeting_citations":[1,2]}]}]}"#,
        )?;
        assert_eq!(
            first.finalize(SessionId::new(7)).sections[0].blocks[0].id(),
            second.finalize(SessionId::new(7)).sections[0].blocks[0].id()
        );
        Ok(())
    }

    #[test]
    fn new_wire_format_has_ids_and_no_basis() -> Result<(), serde_json::Error> {
        let draft: RecordingNotesDraft = serde_json::from_str(
            r#"{"sections":[{"kind":"action_items","blocks":[{"type":"action","text":"Ship it","meeting_citations":[1],"owner":"Morgan","owner_meeting_citations":[1],"due_date":null}]}]}"#,
        )?;
        let value = serde_json::to_value(draft.finalize(SessionId::new(7)))?;
        assert!(value["sections"][0]["blocks"][0]["id"].is_string());
        assert!(value.to_string().contains("recording-block-v1-"));
        assert!(!value.to_string().contains("basis"));
        Ok(())
    }

    #[test]
    fn section_and_recording_are_part_of_block_identity() -> Result<(), serde_json::Error> {
        let findings: RecordingNotesDraft = serde_json::from_str(
            r#"{"sections":[{"kind":"findings","blocks":[{"type":"claim","text":"Fact","meeting_citations":[1]}]}]}"#,
        )?;
        let topics: RecordingNotesDraft = serde_json::from_str(
            r#"{"sections":[{"kind":"topics","blocks":[{"type":"claim","text":"Fact","meeting_citations":[1]}]}]}"#,
        )?;
        let first = findings.clone().finalize(SessionId::new(7));
        let other_recording = findings.finalize(SessionId::new(8));
        let other_section = topics.finalize(SessionId::new(7));
        assert_ne!(
            first.sections[0].blocks[0].id(),
            other_recording.sections[0].blocks[0].id()
        );
        assert_ne!(
            first.sections[0].blocks[0].id(),
            other_section.sections[0].blocks[0].id()
        );
        Ok(())
    }

    #[test]
    fn same_anchor_claims_consolidate_into_one_addressable_block() -> Result<(), serde_json::Error>
    {
        let draft: RecordingNotesDraft = serde_json::from_str(
            r#"{"sections":[{"kind":"findings","blocks":[{"type":"claim","text":"First finding","meeting_citations":[1]},{"type":"claim","text":"Second finding","meeting_citations":[1]}]}]}"#,
        )?;
        let artifact = draft.finalize(SessionId::new(7));
        assert_eq!(artifact.sections[0].blocks.len(), 1);
        let text = match &artifact.sections[0].blocks[0] {
            RecordingNotesBlock::Claim { text, .. } => text,
            RecordingNotesBlock::Action { .. } => {
                return Err(serde_json::Error::io(std::io::Error::other(
                    "findings contain claim blocks",
                )));
            }
        };
        assert_eq!(text, "First finding; Second finding");
        artifact
            .validate(
                SessionId::new(7),
                &HashSet::from([EventId::new(1)]),
                &HashSet::new(),
            )
            .map_err(|error| serde_json::Error::io(std::io::Error::other(error)))?;
        Ok(())
    }

    #[test]
    fn unsupported_claims_and_empty_sections_fail_closed() -> Result<(), serde_json::Error> {
        let unsupported: RecordingNotesDraft = serde_json::from_str(
            r#"{"sections":[{"kind":"findings","blocks":[{"type":"claim","text":"Unsupported"}]}]}"#,
        )?;
        let ids = HashSet::from([EventId::new(1)]);
        assert!(matches!(
            unsupported.finalize(SessionId::new(7)).validate(
                SessionId::new(7),
                &ids,
                &HashSet::new()
            ),
            Err(MeetingNotesError::InvalidEvidenceBasis { field: "findings" })
        ));

        let empty: RecordingNotesDraft =
            serde_json::from_str(r#"{"sections":[{"kind":"findings","blocks":[]}]}"#)?;
        assert!(matches!(
            empty
                .finalize(SessionId::new(7))
                .validate(SessionId::new(7), &ids, &HashSet::new()),
            Err(MeetingNotesError::EmptySection { field: "findings" })
        ));
        Ok(())
    }

    #[test]
    fn persisted_block_id_is_verified_not_trusted() -> Result<(), serde_json::Error> {
        let draft: RecordingNotesDraft = serde_json::from_str(
            r#"{"sections":[{"kind":"findings","blocks":[{"type":"claim","text":"Fact","meeting_citations":[1]}]}]}"#,
        )?;
        let mut value = serde_json::to_value(draft.finalize(SessionId::new(7)))?;
        value["sections"][0]["blocks"][0]["id"] = serde_json::json!("model-invented");
        let artifact: RecordingNotes = serde_json::from_value(value)?;
        assert!(matches!(
            artifact.validate(
                SessionId::new(7),
                &HashSet::from([EventId::new(1)]),
                &HashSet::new()
            ),
            Err(MeetingNotesError::InvalidBlockId { .. })
        ));
        Ok(())
    }

    #[test]
    fn same_anchor_actions_with_different_owners_stay_distinct_and_keep_their_owners()
    -> Result<(), serde_json::Error> {
        // Both actions cite the same single event and would collide under base-citation-only
        // identity, which used to merge them into one block with both owners nulled out by
        // `merge_same_identity`. Folding owner citations into the id keeps them apart instead.
        let draft: RecordingNotesDraft = serde_json::from_str(
            r#"{"sections":[{"kind":"action_items","blocks":[
                {"type":"action","text":"Ship it","meeting_citations":[1],"owner":"Morgan","owner_meeting_citations":[2]},
                {"type":"action","text":"Ship it","meeting_citations":[1],"owner":"Riley","owner_meeting_citations":[3]}
            ]}]}"#,
        )?;
        let artifact = draft.finalize(SessionId::new(7));

        assert_eq!(artifact.sections[0].blocks.len(), 2);
        let owners: Vec<Option<String>> = artifact.sections[0]
            .blocks
            .iter()
            .map(|block| match block {
                RecordingNotesBlock::Action { owner, .. } => owner.clone(),
                RecordingNotesBlock::Claim { .. } => None,
            })
            .collect();
        assert!(owners.contains(&Some("Morgan".to_owned())));
        assert!(owners.contains(&Some("Riley".to_owned())));
        artifact
            .validate(
                SessionId::new(7),
                &HashSet::from([EventId::new(1), EventId::new(2), EventId::new(3)]),
                &HashSet::new(),
            )
            .map_err(|error| serde_json::Error::io(std::io::Error::other(error)))?;
        Ok(())
    }

    #[test]
    fn reduce_projection_contains_no_sotto_owned_ids() -> Result<(), serde_json::Error> {
        let draft: RecordingNotesDraft = serde_json::from_str(
            r#"{"sections":[{"kind":"findings","blocks":[{"type":"claim","text":"Fact","meeting_citations":[1]}]}]}"#,
        )?;
        let artifact = draft.finalize(SessionId::new(7));
        let reduce_value = serde_json::to_value(RecordingNotesDraft::from(&artifact))?;

        assert!(!reduce_value.to_string().contains("\"id\""));
        assert_eq!(reduce_value["sections"][0]["blocks"][0]["text"], "Fact");
        Ok(())
    }
}
