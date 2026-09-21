//! Translate a notes markdown surface into T087 overlay operations.
//!
//! The overlay model is read-only here. This module only *authors* `Add`, `Reword`, `Hide`,
//! `Reorder`, and `SetChecked` against the presented document the person just saw.

use std::collections::{BTreeSet, HashSet};

use insight::{
    NotesOverlayOperation, OverlayTarget, PresentedNotesBlock, PresentedNotesBlockId,
    RecordingNotesSectionKind,
};

/// Why a full-document save did not produce overlay operations.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum DocumentEditError {
    /// More than one honest explanation exists; guessing would reattribute a claim.
    Ambiguous(String),
    /// The edited text is not the markdown this surface round-trips.
    InvalidMarkdown(String),
}

impl std::fmt::Display for DocumentEditError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Ambiguous(detail) => {
                write!(f, "This edit is ambiguous and was not saved. {detail}")
            }
            Self::InvalidMarkdown(detail) => write!(f, "{detail}"),
        }
    }
}

impl std::error::Error for DocumentEditError {}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ParsedItem {
    section: RecordingNotesSectionKind,
    text: String,
    action: bool,
    checked: bool,
    owner: Option<String>,
    due_date: Option<String>,
}

/// Section title used in both the read column and the markdown surface.
#[must_use]
pub(crate) const fn section_title(kind: RecordingNotesSectionKind) -> &'static str {
    match kind {
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

#[must_use]
pub(crate) const fn section_is_prose(kind: RecordingNotesSectionKind) -> bool {
    matches!(kind, RecordingNotesSectionKind::Overview)
}

fn section_from_title(title: &str) -> Option<RecordingNotesSectionKind> {
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
    .find(|kind| section_title(*kind) == title)
}

/// Render the composed document as the markdown the editor loads.
#[must_use]
pub(crate) fn render_notes_markdown(blocks: &[PresentedNotesBlock]) -> String {
    let mut rendered = String::new();
    let mut last_section = None;
    for block in blocks {
        if last_section != Some(block.section) {
            if !rendered.is_empty() {
                rendered.push('\n');
            }
            rendered.push_str("## ");
            rendered.push_str(section_title(block.section));
            rendered.push_str("\n\n");
            last_section = Some(block.section);
        }
        rendered.push_str(&render_item(block));
        rendered.push_str("\n\n");
    }
    rendered
}

fn render_item(block: &PresentedNotesBlock) -> String {
    let fields = render_fields(block.owner.as_deref(), block.due_date.as_deref());
    if block.action {
        let mark = if block.checked { "x" } else { " " };
        format!("- [{mark}] {}{fields}", block.text)
    } else if section_is_prose(block.section) {
        format!("{}{fields}", block.text)
    } else {
        format!("- {}{fields}", block.text)
    }
}

fn render_fields(owner: Option<&str>, due_date: Option<&str>) -> String {
    let mut suffix = String::new();
    if let Some(owner) = owner.filter(|value| !value.is_empty()) {
        suffix.push_str(" | owner: ");
        suffix.push_str(owner);
    }
    if let Some(due) = due_date.filter(|value| !value.is_empty()) {
        suffix.push_str(" | due: ");
        suffix.push_str(due);
    }
    suffix
}

/// Diff edited markdown against the presented blocks and emit overlay operations.
///
/// `user_id_seed` prefixes new user block ids so a save does not collide with earlier adds.
pub(crate) fn diff_notes_document(
    blocks: &[PresentedNotesBlock],
    edited: &str,
    user_id_seed: u64,
) -> Result<Vec<NotesOverlayOperation>, DocumentEditError> {
    let parsed = parse_notes_markdown(edited)?;
    let assignment = assign_items(blocks, &parsed)?;
    Ok(operations_from_assignment(
        blocks,
        &parsed,
        &assignment,
        user_id_seed,
    ))
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum Binding {
    Kept(usize),
    Added,
}

fn assign_items(
    blocks: &[PresentedNotesBlock],
    parsed: &[ParsedItem],
) -> Result<Vec<Binding>, DocumentEditError> {
    let mut binding = vec![Binding::Added; parsed.len()];
    let sections = parsed
        .iter()
        .map(|item| item.section)
        .chain(blocks.iter().map(|block| block.section))
        .collect::<HashSet<_>>();

    for section in sections {
        let baseline: Vec<usize> = blocks
            .iter()
            .enumerate()
            .filter(|(_, block)| block.section == section)
            .map(|(index, _)| index)
            .collect();
        let edited: Vec<usize> = parsed
            .iter()
            .enumerate()
            .filter(|(_, item)| item.section == section)
            .map(|(index, _)| index)
            .collect();
        let matched = match_section(
            &baseline
                .iter()
                .map(|index| &blocks[*index])
                .collect::<Vec<_>>(),
            &edited
                .iter()
                .map(|index| &parsed[*index])
                .collect::<Vec<_>>(),
        )?;
        for (edited_local, baseline_local) in matched {
            let parsed_index = edited[edited_local];
            let block_index = baseline[baseline_local];
            binding[parsed_index] = Binding::Kept(block_index);
        }
    }
    Ok(binding)
}

fn match_section(
    baseline: &[&PresentedNotesBlock],
    edited: &[&ParsedItem],
) -> Result<Vec<(usize, usize)>, DocumentEditError> {
    let exact = exact_lcs(baseline, edited);
    let mut used_baseline = vec![false; baseline.len()];
    let mut used_edited = vec![false; edited.len()];
    let mut pairs = Vec::new();
    for (edited_index, baseline_index) in exact {
        used_edited[edited_index] = true;
        used_baseline[baseline_index] = true;
        pairs.push((edited_index, baseline_index));
    }

    let leftover_edited: Vec<usize> = used_edited
        .iter()
        .enumerate()
        .filter_map(|(index, used)| (!*used).then_some(index))
        .collect();
    let leftover_baseline: Vec<usize> = used_baseline
        .iter()
        .enumerate()
        .filter_map(|(index, used)| (!*used).then_some(index))
        .collect();

    let zipped = leftover_edited.len().min(leftover_baseline.len());
    for index in 0..zipped {
        let edited_index = leftover_edited[index];
        let baseline_index = leftover_baseline[index];
        let positional = token_overlap(&baseline[baseline_index].text, &edited[edited_index].text);
        if positional == 0 {
            continue;
        }
        for (other_pos, other_baseline) in leftover_baseline.iter().copied().enumerate() {
            if other_pos == index {
                continue;
            }
            let cross = token_overlap(&baseline[other_baseline].text, &edited[edited_index].text);
            if cross > 0 && cross >= positional {
                return Err(DocumentEditError::Ambiguous(format!(
                    "“{}” could continue “{}” or “{}”. Edit one of them, or delete and re-add, then save again.",
                    truncate_for_message(&edited[edited_index].text),
                    truncate_for_message(&baseline[baseline_index].text),
                    truncate_for_message(&baseline[other_baseline].text),
                )));
            }
        }
        for (other_pos, other_edited) in leftover_edited.iter().copied().enumerate() {
            if other_pos == index {
                continue;
            }
            let cross = token_overlap(&baseline[baseline_index].text, &edited[other_edited].text);
            if cross > 0 && cross >= positional {
                return Err(DocumentEditError::Ambiguous(format!(
                    "“{}” could continue as “{}” or “{}”. Edit one of them, or delete and re-add, then save again.",
                    truncate_for_message(&baseline[baseline_index].text),
                    truncate_for_message(&edited[edited_index].text),
                    truncate_for_message(&edited[other_edited].text),
                )));
            }
        }
        pairs.push((edited_index, baseline_index));
    }
    Ok(pairs)
}

fn exact_lcs(baseline: &[&PresentedNotesBlock], edited: &[&ParsedItem]) -> Vec<(usize, usize)> {
    let rows = edited.len();
    let cols = baseline.len();
    let mut table = vec![vec![0_usize; cols.saturating_add(1)]; rows.saturating_add(1)];
    for row in 0..rows {
        for col in 0..cols {
            let equal = edited[row].text == baseline[col].text
                && edited[row].action == baseline[col].action;
            table[row + 1][col + 1] = if equal {
                table[row][col].saturating_add(1)
            } else {
                table[row][col + 1].max(table[row + 1][col])
            };
        }
    }
    let mut pairs = Vec::new();
    let mut row = rows;
    let mut col = cols;
    while row > 0 && col > 0 {
        let equal = edited[row - 1].text == baseline[col - 1].text
            && edited[row - 1].action == baseline[col - 1].action;
        if equal {
            pairs.push((row - 1, col - 1));
            row -= 1;
            col -= 1;
        } else if table[row - 1][col] >= table[row][col - 1] {
            row -= 1;
        } else {
            col -= 1;
        }
    }
    pairs.reverse();
    pairs
}

fn token_overlap(left: &str, right: &str) -> usize {
    let left_tokens = tokens(left);
    tokens(right).intersection(&left_tokens).count()
}

fn tokens(text: &str) -> BTreeSet<String> {
    let mut current = String::new();
    let mut found = BTreeSet::new();
    for character in text.chars() {
        if character.is_alphanumeric() {
            current.extend(character.to_lowercase());
        } else if !current.is_empty() {
            if current.chars().count() >= 2 {
                found.insert(std::mem::take(&mut current));
            } else {
                current.clear();
            }
        }
    }
    if current.chars().count() >= 2 {
        found.insert(current);
    }
    found
}

fn truncate_for_message(text: &str) -> String {
    const LIMIT: usize = 48;
    let mut chars = text.chars();
    let taken: String = chars.by_ref().take(LIMIT).collect();
    if chars.next().is_some() {
        format!("{taken}…")
    } else {
        taken
    }
}

fn operations_from_assignment(
    blocks: &[PresentedNotesBlock],
    parsed: &[ParsedItem],
    binding: &[Binding],
    user_id_seed: u64,
) -> Vec<NotesOverlayOperation> {
    let mut operations = Vec::new();
    let mut kept = BTreeSet::new();
    let mut edited_targets = Vec::with_capacity(parsed.len());
    let mut add_count = 0_u64;

    for (parsed_item, bound) in parsed.iter().zip(binding.iter()) {
        match bound {
            Binding::Kept(index) => {
                let block = &blocks[*index];
                let target = overlay_target(block);
                kept.insert(*index);
                if block.text != parsed_item.text
                    || block.owner != parsed_item.owner
                    || block.due_date != parsed_item.due_date
                {
                    operations.push(NotesOverlayOperation::Reword {
                        target: target.clone(),
                        text: parsed_item.text.clone(),
                        owner: parsed_item.owner.clone(),
                        due_date: parsed_item.due_date.clone(),
                    });
                }
                if block.action && block.checked != parsed_item.checked {
                    operations.push(NotesOverlayOperation::SetChecked {
                        target: target.clone(),
                        checked: parsed_item.checked,
                    });
                }
                edited_targets.push(target);
            }
            Binding::Added => {
                let user_block_id = format!("user-{user_id_seed}-{add_count}");
                add_count = add_count.saturating_add(1);
                operations.push(NotesOverlayOperation::Add {
                    user_block_id: user_block_id.clone(),
                    section: parsed_item.section,
                    text: parsed_item.text.clone(),
                    action: parsed_item.action,
                    owner: parsed_item.owner.clone(),
                    due_date: parsed_item.due_date.clone(),
                });
                edited_targets.push(OverlayTarget {
                    block_id: user_block_id,
                    section: parsed_item.section,
                    action: parsed_item.action,
                    meeting_citations: Vec::new(),
                    external_citations: Vec::new(),
                });
            }
        }
    }

    for (index, block) in blocks.iter().enumerate() {
        if !kept.contains(&index) {
            operations.push(NotesOverlayOperation::Hide {
                target: overlay_target(block),
            });
        }
    }

    // compose() applies Hide then Add, so surviving original blocks keep their relative order
    // and new blocks land at the end before Reorder places them.
    let mut current: Vec<OverlayTarget> = blocks
        .iter()
        .enumerate()
        .filter(|(index, _)| kept.contains(index))
        .map(|(_, block)| overlay_target(block))
        .collect();
    for target in &edited_targets {
        if current
            .iter()
            .all(|existing| existing.block_id != target.block_id)
        {
            current.push(target.clone());
        }
    }
    operations.extend(reorder_operations(&mut current, &edited_targets));
    operations
}

fn reorder_operations(
    current: &mut Vec<OverlayTarget>,
    desired: &[OverlayTarget],
) -> Vec<NotesOverlayOperation> {
    let mut operations = Vec::new();
    if current.len() != desired.len() {
        return operations;
    }
    for (index, wanted) in desired.iter().enumerate() {
        if current.get(index).map(|target| target.block_id.as_str())
            == Some(wanted.block_id.as_str())
        {
            continue;
        }
        let Some(from) = current
            .iter()
            .position(|target| target.block_id == wanted.block_id)
        else {
            continue;
        };
        let moved = current.remove(from);
        current.insert(index, moved);
        let after = index
            .checked_sub(1)
            .and_then(|previous| current.get(previous).cloned());
        operations.push(NotesOverlayOperation::Reorder {
            target: wanted.clone(),
            after,
        });
    }
    operations
}

pub(crate) fn overlay_target(block: &PresentedNotesBlock) -> OverlayTarget {
    OverlayTarget {
        block_id: match &block.id {
            PresentedNotesBlockId::Generated(id) => id.as_str().to_owned(),
            PresentedNotesBlockId::User(id) => id.clone(),
        },
        section: block.section,
        action: block.action,
        meeting_citations: block.meeting_citations.clone(),
        external_citations: block.external_citations.clone(),
    }
}

fn parse_notes_markdown(edited: &str) -> Result<Vec<ParsedItem>, DocumentEditError> {
    let mut items = Vec::new();
    let mut section = None;
    let mut prose = Vec::new();
    for raw in edited.lines() {
        let line = raw.trim_end();
        let trimmed = line.trim();
        if let Some(title) = trimmed.strip_prefix("## ") {
            flush_prose(section, &mut prose, &mut items)?;
            let title = title.trim();
            section = Some(section_from_title(title).ok_or_else(|| {
                DocumentEditError::InvalidMarkdown(format!(
                    "Unknown heading “{title}”. Use the section titles already in the note."
                ))
            })?);
            continue;
        }
        if trimmed.starts_with('#') {
            return Err(DocumentEditError::InvalidMarkdown(
                "Headings in the note must use ## and a known section title.".to_owned(),
            ));
        }
        if trimmed.is_empty() {
            flush_prose(section, &mut prose, &mut items)?;
            continue;
        }
        if let Some(item) = parse_list_item(trimmed) {
            flush_prose(section, &mut prose, &mut items)?;
            let Some(section) = section else {
                return Err(DocumentEditError::InvalidMarkdown(
                    "Start the note with a ## section heading before adding a paragraph."
                        .to_owned(),
                ));
            };
            items.push(ParsedItem {
                section,
                text: item.text,
                action: item.action,
                checked: item.checked,
                owner: item.owner,
                due_date: item.due_date,
            });
            continue;
        }
        if section.is_none() {
            return Err(DocumentEditError::InvalidMarkdown(
                "Start the note with a ## section heading before adding a paragraph.".to_owned(),
            ));
        }
        prose.push(trimmed.to_owned());
    }
    flush_prose(section, &mut prose, &mut items)?;
    Ok(items)
}

struct ParsedFields {
    text: String,
    action: bool,
    checked: bool,
    owner: Option<String>,
    due_date: Option<String>,
}

fn parse_list_item(line: &str) -> Option<ParsedFields> {
    let rest = line.strip_prefix("- ")?;
    let (action, checked, body) = if let Some(body) = rest.strip_prefix("[ ] ") {
        (true, false, body)
    } else if let Some(body) = rest
        .strip_prefix("[x] ")
        .or_else(|| rest.strip_prefix("[X] "))
    {
        (true, true, body)
    } else {
        (false, false, rest)
    };
    let (text, owner, due_date) = split_fields(body);
    Some(ParsedFields {
        text,
        action,
        checked,
        owner,
        due_date,
    })
}

fn split_fields(body: &str) -> (String, Option<String>, Option<String>) {
    let mut owner = None;
    let mut due_date = None;
    let mut text = body;
    loop {
        if let Some((head, value)) = text.rsplit_once(" | due: ") {
            due_date = Some(value.trim().to_owned()).filter(|value| !value.is_empty());
            text = head;
            continue;
        }
        if let Some((head, value)) = text.rsplit_once(" | owner: ") {
            owner = Some(value.trim().to_owned()).filter(|value| !value.is_empty());
            text = head;
            continue;
        }
        break;
    }
    (text.trim().to_owned(), owner, due_date)
}

fn flush_prose(
    section: Option<RecordingNotesSectionKind>,
    prose: &mut Vec<String>,
    items: &mut Vec<ParsedItem>,
) -> Result<(), DocumentEditError> {
    if prose.is_empty() {
        return Ok(());
    }
    let text = prose.join("\n");
    prose.clear();
    if text.trim().is_empty() {
        return Ok(());
    }
    let Some(section) = section else {
        return Err(DocumentEditError::InvalidMarkdown(
            "Start the note with a ## section heading before adding a paragraph.".to_owned(),
        ));
    };
    let (text, owner, due_date) = split_fields(&text);
    items.push(ParsedItem {
        section,
        text,
        action: false,
        checked: false,
        owner,
        due_date,
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use insight::NotesBlockProvenance;

    use super::*;

    fn user_block(
        id: &str,
        section: RecordingNotesSectionKind,
        text: &str,
        action: bool,
    ) -> PresentedNotesBlock {
        PresentedNotesBlock {
            id: PresentedNotesBlockId::User(id.to_owned()),
            section,
            text: text.to_owned(),
            action,
            owner: None,
            due_date: None,
            checked: false,
            provenance: NotesBlockProvenance::UserAuthored,
            orphaned: false,
            meeting_citations: Vec::new(),
            external_citations: Vec::new(),
        }
    }

    #[test]
    fn round_trip_emits_no_operations() -> Result<(), Box<dyn std::error::Error>> {
        let blocks = vec![
            user_block(
                "overview",
                RecordingNotesSectionKind::Overview,
                "Sprint 41 is scoped to payment retries.",
                false,
            ),
            user_block(
                "decision",
                RecordingNotesSectionKind::Decisions,
                "The search rewrite is deferred.",
                false,
            ),
            {
                let mut action = user_block(
                    "action",
                    RecordingNotesSectionKind::ActionItems,
                    "Retry rollout checklist.",
                    true,
                );
                action.owner = Some("Dana".to_owned());
                action.due_date = Some("Thursday".to_owned());
                action
            },
        ];
        let markdown = render_notes_markdown(&blocks);
        let operations = diff_notes_document(&blocks, &markdown, 1)?;
        assert!(
            operations.is_empty(),
            "an untouched render must not author overlay operations, got {operations:?}"
        );
        Ok(())
    }

    #[test]
    fn reword_add_hide_and_reorder_emit_the_overlay_ops() -> Result<(), Box<dyn std::error::Error>>
    {
        let blocks = vec![
            user_block(
                "a",
                RecordingNotesSectionKind::Overview,
                "First generated sentence about retries.",
                false,
            ),
            user_block(
                "b",
                RecordingNotesSectionKind::Overview,
                "Second generated sentence about audits.",
                false,
            ),
            user_block(
                "c",
                RecordingNotesSectionKind::Overview,
                "Third generated sentence about rollback.",
                false,
            ),
        ];
        let edited = "\
## Overview

Second generated sentence about audits.

First generated sentence about retries, restated.

A paragraph the user typed.

";
        let operations = diff_notes_document(&blocks, edited, 9)?;
        assert!(
            operations.iter().any(|operation| matches!(
                operation,
                NotesOverlayOperation::Reword { target, text, .. }
                    if target.block_id == "a"
                        && text == "First generated sentence about retries, restated."
            )),
            "rewording a kept paragraph must emit Reword on that block, got {operations:?}"
        );
        assert!(
            operations.iter().any(|operation| matches!(
                operation,
                NotesOverlayOperation::Hide { target } if target.block_id == "c"
            )),
            "a removed paragraph must emit Hide, got {operations:?}"
        );
        assert!(
            operations.iter().any(|operation| matches!(
                operation,
                NotesOverlayOperation::Add { text, .. }
                    if text == "A paragraph the user typed."
            )),
            "a new paragraph must emit Add, got {operations:?}"
        );
        assert!(
            operations.iter().any(|operation| matches!(
                operation,
                NotesOverlayOperation::Reorder { target, .. } if target.block_id == "b"
            )) || operations.iter().any(|operation| matches!(
                operation,
                NotesOverlayOperation::Reorder { target, .. } if target.block_id == "a"
            )),
            "moving remaining paragraphs must emit Reorder, got {operations:?}"
        );
        Ok(())
    }

    #[test]
    fn similar_leftovers_refuse_rather_than_guess() {
        let blocks = vec![
            user_block(
                "demo",
                RecordingNotesSectionKind::Overview,
                "Alice presents the demo today.",
                false,
            ),
            user_block(
                "plan",
                RecordingNotesSectionKind::Overview,
                "Alice presents the plan today.",
                false,
            ),
        ];
        let edited = "\
## Overview

Alice presents the plan tomorrow.

Alice presents the demo tomorrow.
";
        let result = diff_notes_document(&blocks, edited, 1);
        assert!(
            matches!(result, Err(DocumentEditError::Ambiguous(_))),
            "swap-and-reword of similar claims must refuse, got {result:?}"
        );
    }

    #[test]
    fn unrelated_replacement_is_hide_and_add() -> Result<(), Box<dyn std::error::Error>> {
        let blocks = vec![user_block(
            "generated",
            RecordingNotesSectionKind::Overview,
            "The generated claim about retries.",
            false,
        )];
        let edited = "\
## Overview

A wholly different user paragraph.
";
        let operations = diff_notes_document(&blocks, edited, 4)?;
        assert!(
            operations.iter().any(|operation| matches!(
                operation,
                NotesOverlayOperation::Hide { target } if target.block_id == "generated"
            )),
            "replacing a claim with unrelated text must Hide the original, got {operations:?}"
        );
        assert!(
            operations.iter().any(|operation| matches!(
                operation,
                NotesOverlayOperation::Add { text, .. }
                    if text == "A wholly different user paragraph."
            )),
            "replacing a claim with unrelated text must Add the new paragraph, got {operations:?}"
        );
        assert!(
            operations
                .iter()
                .all(|operation| !matches!(operation, NotesOverlayOperation::Reword { .. })),
            "unrelated replacement must not reword the original block, got {operations:?}"
        );
        Ok(())
    }
}
