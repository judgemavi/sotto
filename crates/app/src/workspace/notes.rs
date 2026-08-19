//! The summary column: your notes first, then whatever the recording supports.
//!
//! Two rules shape everything below.
//!
//! **The summary is adaptive.** A section exists because the taxonomy returned content for it. A
//! lecture yields topics, a debugging session yields findings, a planning call yields decisions.
//! Only [`summary_sections`] names a section; every renderer below it walks a `Vec` it was handed,
//! so changing the taxonomy is a change to one function rather than to the column.
//!
//! **Every claim carries a citation chip.** The chip jumps to the transcript row that supports the
//! claim. That is the product's central promise made operable, so it is a control on the claim, not
//! decoration beside it.
//!
//! **Evidence is mandatory in the data and quiet on the page.** A claim without a citation is
//! rejected upstream in `insight` and never reaches this column; nothing here relaxes that. What
//! [`EvidenceDisclosure`] decides is only when the chips are *spent vertical space*. One claim in
//! the maintainer's first real summary carried ten of them, which cost more height than the
//! sentence they supported, so the chips are hidden until a reader asks — for one claim, or for
//! the whole summary.

use std::{
    collections::BTreeMap,
    hash::{Hash as _, Hasher as _},
    time::Duration,
};

use gpui::{App, Context, ElementId, Entity, Rgba, WeakEntity, Window, div, prelude::*};
use gpui_component::{
    Disableable, Selectable as _, Sizable as _,
    button::ButtonVariants as _,
    input::{Input, InputState},
    scroll::ScrollableElement,
    text::TextView,
};
use insight::{
    NotesBlockProvenance, NotesOverlayOperation, OverlayTarget, PresentedNotesBlock,
    PresentedNotesBlockId, PresentedNotesDocument, RecordingNotesSectionKind, ScreenConsultation,
    SourceStatus,
};
#[cfg(test)]
use insight::{RecordingNotes, RecordingNotesBlock, RecordingNotesSection};
use providers::{
    CODEX_CLI_BACKEND_ID, OPENAI_RESPONSES_BACKEND_ID, ReasoningSurface,
    backend::{ObservedRequestNormalization, SamplingControl},
};
use sotto_core::{EventId, EventPayload, MarkKind, TimelineEvent, replay_lenient};

use crate::{
    mcp::{ConfiguredServer, GrantReceiptState, SessionGrantView},
    notes::NotesState,
};

use super::{
    Button, MeetingWorkspace,
    control_row::{ControlRole, ControlRow},
    motion,
    tokens::{TypeScale, WorkspaceTokens},
};

/// Media time of each transcript row a claim may cite, used to label citation chips.
///
/// The mock labels every chip with the moment it lands on. The projection that knows those moments
/// lives in the transcript column, so this column takes them as input rather than re-deriving them:
/// see [`render_with_citation_times`].
pub(crate) type CitationTimes = BTreeMap<EventId, Duration>;

/// Provider picker state for the notes column head. Computed on the workspace entity
/// before render so the column never re-reads that entity while GPUI is updating it.
pub(crate) struct NotesProviderPicker {
    pub ready: bool,
    pub openai_ok: bool,
    pub codex_ok: bool,
    pub selected: Option<String>,
}

/// Renders the column with transcript moments available for citation and anchor labels.
#[expect(
    clippy::too_many_arguments,
    reason = "source context is passed in rather than read back off the rendering entity"
)]
pub(crate) fn render_with_citation_times(
    state: NotesState,
    live: bool,
    generation_running: bool,
    annotations: &[AnnotationView],
    latest_anchor: Option<EventId>,
    selected_anchor: Option<EventId>,
    annotation_input: &Entity<InputState>,
    servers: Vec<ConfiguredServer>,
    selected_grant: Option<SessionGrantView>,
    citation_times: &CitationTimes,
    picker: NotesProviderPicker,
    cx: &mut Context<MeetingWorkspace>,
) -> gpui::AnyElement {
    let tokens = WorkspaceTokens::resolve(cx);
    let summary = SummaryView::resolve(state, live);
    div()
        .size_full()
        .min_w_0()
        .flex()
        .flex_col()
        .debug_selector(|| "notes-column".into())
        .child(render_head(&summary, live, generation_running, picker, cx))
        .child(
            div()
                .flex_1()
                .min_h_0()
                .min_w_0()
                .overflow_y_scrollbar()
                .px_4()
                .py_3()
                .children(live.then(|| render_your_notes(annotations, citation_times, cx)))
                .children(
                    (!live).then(|| render_completed_annotations(annotations, citation_times, cx)),
                )
                .child(SummaryBody {
                    summary,
                    citation_times: citation_times.clone(),
                    workspace: cx.weak_entity(),
                })
                .child(render_source_context(servers, selected_grant, cx)),
        )
        .child(
            div()
                .px_4()
                .py_3()
                .border_t_1()
                .border_color(tokens.line_soft)
                .debug_selector(|| "notes-composer".into())
                .child(div().mb_1().text_sm().text_color(tokens.faint).child(
                    composer_anchor_label(selected_anchor, latest_anchor, citation_times),
                ))
                .child(
                    ControlRow::new()
                        .child(
                            ControlRole::Ellipsizing,
                            Input::new(annotation_input).disabled(
                                live && selected_anchor.is_none() && latest_anchor.is_none(),
                            ),
                        )
                        .child(
                            ControlRole::Essential,
                            Button::new("append-note", tokens)
                                .label(if live { "Add" } else { "Save block" })
                                .small()
                                .disabled(
                                    live && selected_anchor.is_none() && latest_anchor.is_none(),
                                )
                                .debug_selector(|| "append-note-control".into())
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.submit_annotation(window, cx);
                                })),
                        )
                        .finish()
                        .gap_2(),
                )
                .children(
                    annotation_composer_reason(
                        live,
                        selected_anchor.is_some() || latest_anchor.is_some(),
                    )
                    .map(|reason| {
                        div()
                            .mt_1()
                            .text_sm()
                            .text_color(tokens.faint)
                            .child(reason)
                    }),
                ),
        )
        .into_any_element()
}

fn render_completed_annotations(
    annotations: &[AnnotationView],
    citation_times: &CitationTimes,
    cx: &mut Context<MeetingWorkspace>,
) -> gpui::AnyElement {
    let tokens = WorkspaceTokens::resolve(cx);
    div()
        .children((!annotations.is_empty()).then(|| {
            div()
                .mt_3()
                .pb_1()
                .mb_2()
                .border_b_1()
                .border_color(tokens.line_soft)
                .child("Your notes")
        }))
        .children(annotations.iter().map(|annotation| {
            let anchor = annotation.anchor;
            div()
                .mb_2()
                .child(SelectableText {
                    id: ("completed-user-note", annotation.event_id.get()).into(),
                    text: annotation.text.clone(),
                    color: tokens.ink_2,
                })
                .child(
                    Button::new(
                        ("completed-user-note-anchor", annotation.event_id.get()),
                        tokens,
                    )
                    .label(moment_label(anchor, citation_times))
                    .ghost()
                    .xsmall()
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.open_citation(anchor, cx);
                    })),
                )
                .child(div().text_xs().text_color(tokens.faint).child("Your words"))
        }))
        .into_any_element()
}

fn render_head(
    summary: &SummaryView,
    live: bool,
    generation_running: bool,
    picker: NotesProviderPicker,
    cx: &mut Context<MeetingWorkspace>,
) -> gpui::AnyElement {
    let tokens = WorkspaceTokens::resolve(cx);
    let notes_ready = picker.ready;
    let selected = picker.selected;
    let openai_ok = picker.openai_ok;
    let codex_ok = picker.codex_ok;
    let mut row = ControlRow::new()
        .child(
            ControlRole::Essential,
            div().text_color(tokens.ink).child("Notes"),
        )
        .child(
            ControlRole::Ellipsizing,
            div()
                .text_sm()
                .text_color(tokens.faint)
                .child(summary.meta.clone()),
        );
    if openai_ok {
        let selected_openai = selected.as_deref() == Some(OPENAI_RESPONSES_BACKEND_ID);
        row = row.child(
            ControlRole::Essential,
            Button::new("notes-provider-openai", tokens)
                .label("OpenAI")
                .small()
                .selected(selected_openai)
                .disabled(live || generation_running)
                .on_click(cx.listener(|this, _, _, cx| {
                    this.select_provider(
                        ReasoningSurface::Notes,
                        Some(OPENAI_RESPONSES_BACKEND_ID),
                        cx,
                    );
                })),
        );
    }
    if codex_ok {
        let selected_codex = selected.as_deref() == Some(CODEX_CLI_BACKEND_ID);
        row = row.child(
            ControlRole::Essential,
            Button::new("notes-provider-codex", tokens)
                .label("Codex")
                .small()
                .selected(selected_codex)
                .disabled(live || generation_running)
                .on_click(cx.listener(|this, _, _, cx| {
                    this.select_provider(ReasoningSurface::Notes, Some(CODEX_CLI_BACKEND_ID), cx);
                })),
        );
    }
    row = row.child(
        ControlRole::Essential,
        Button::new("summarize", tokens)
            .label(if summary.sections.is_empty() {
                "Write notes"
            } else {
                "Write notes again"
            })
            .small()
            .disabled(live || generation_running || !notes_ready)
            .debug_selector(|| "summarize-control".into())
            .on_click(cx.listener(|this, _, _, cx| this.generate_notes(cx))),
    );
    div()
        .px_4()
        .py_2()
        .border_b_1()
        .border_color(tokens.line_soft)
        .child(
            row.finish()
                .gap_2()
                .debug_selector(|| "notes-head-row".into()),
        )
        .into_any_element()
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct AnnotationView {
    pub(crate) event_id: EventId,
    pub(crate) anchor: EventId,
    pub(crate) text: String,
    pub(crate) mark: MarkKind,
}

#[derive(Clone, Debug)]
pub(crate) struct EditingNotesBlock {
    target: OverlayTarget,
    action: bool,
}

/// Returns the active append-only annotation versions in event order.
#[cfg(test)]
pub(crate) fn active_annotations(events: &[TimelineEvent]) -> Vec<AnnotationView> {
    replay_lenient(events)
        .state()
        .active()
        .values()
        .filter_map(|event| {
            let EventPayload::UserAnnotation(annotation) = event.payload() else {
                return None;
            };
            Some(AnnotationView {
                event_id: event.id(),
                anchor: annotation.anchor,
                text: annotation.text.clone(),
                mark: annotation.mark,
            })
        })
        .collect()
}

/// Projection consumed by the transcript column to pin annotations under their anchor.
#[cfg(test)]
pub(crate) fn annotations_by_anchor(
    events: &[TimelineEvent],
) -> BTreeMap<EventId, Vec<AnnotationView>> {
    let mut grouped = BTreeMap::<EventId, Vec<AnnotationView>>::new();
    for annotation in active_annotations(events) {
        let Some(visible_anchor) = resolve_transcript_anchor(events, annotation.anchor) else {
            continue;
        };
        grouped.entry(visible_anchor).or_default().push(annotation);
    }
    grouped
}

/// Resolves a durable annotation/citation anchor to the active utterance that replaced it.
///
/// Rolling partial ids are intentionally transient. The annotation retains its original id for
/// audit, while presentation follows the append-only supersession chain into the current partial
/// and eventually the settled final.
#[must_use]
#[cfg(test)]
pub(crate) fn resolve_transcript_anchor(
    events: &[TimelineEvent],
    anchor: EventId,
) -> Option<EventId> {
    let transcript_ids = events
        .iter()
        .filter(|event| {
            matches!(
                event.payload(),
                EventPayload::UtteranceFinal(_) | EventPayload::UtterancePartial(_)
            )
        })
        .map(TimelineEvent::id)
        .collect::<std::collections::BTreeSet<_>>();
    if !transcript_ids.contains(&anchor) {
        return None;
    }

    let successors = events
        .iter()
        .filter(|event| transcript_ids.contains(&event.id()))
        .filter_map(|event| event.supersedes().map(|target| (target, event.id())))
        .collect::<BTreeMap<_, _>>();
    let active = replay_lenient(events)
        .state()
        .active()
        .keys()
        .copied()
        .collect::<std::collections::BTreeSet<_>>();
    resolve_anchor_with_index(anchor, &transcript_ids, &successors, &active)
}

pub(crate) fn resolve_anchor_with_index(
    anchor: EventId,
    transcript_ids: &std::collections::BTreeSet<EventId>,
    successors: &BTreeMap<EventId, EventId>,
    active: &std::collections::BTreeSet<EventId>,
) -> Option<EventId> {
    if !transcript_ids.contains(&anchor) {
        return None;
    }
    let mut resolved = anchor;
    while let Some(next) = successors.get(&resolved).copied() {
        if next <= resolved {
            return None;
        }
        resolved = next;
    }
    active.contains(&resolved).then_some(resolved)
}

#[must_use]
pub(crate) fn latest_anchor(events: &[TimelineEvent]) -> Option<EventId> {
    replay_lenient(events)
        .state()
        .active()
        .values()
        .filter(|event| {
            matches!(
                event.payload(),
                EventPayload::UtteranceFinal(_) | EventPayload::UtterancePartial(_)
            )
        })
        .map(TimelineEvent::id)
        .max()
}

pub(crate) fn render_pinned_annotation_with_tokens(
    annotation: AnnotationView,
    tokens: WorkspaceTokens,
) -> gpui::AnyElement {
    div()
        .mt_2()
        .ml_4()
        .pl_3()
        .border_l_2()
        .border_color(tokens.accent)
        .text_sm()
        .child(
            div()
                .text_color(tokens.accent)
                .child(format!("Your {}", mark_label(annotation.mark))),
        )
        .child(SelectableText {
            id: ("pinned-note-text", annotation.event_id.get()).into(),
            text: annotation.text,
            color: tokens.ink_2,
        })
        .into_any_element()
}

/// Your notes come first, and say what typing will do rather than reporting emptiness.
fn render_your_notes(
    annotations: &[AnnotationView],
    citation_times: &CitationTimes,
    cx: &mut Context<MeetingWorkspace>,
) -> gpui::AnyElement {
    let tokens = WorkspaceTokens::resolve(cx);
    div()
        .mb_4()
        .rounded_lg()
        .border_1()
        .border_color(tokens.line)
        .bg(tokens.ground)
        .debug_selector(|| "your-notes-block".into())
        .child(
            div().px_3().pt_2().child(
                ControlRow::new()
                    .child(
                        ControlRole::Essential,
                        div().text_color(tokens.ink).child("Your notes"),
                    )
                    .child(ControlRole::Ellipsizing, div())
                    .finish()
                    .gap_2(),
            ),
        )
        .when(annotations.is_empty(), |card| {
            card.child(
                div()
                    .px_3()
                    .py_2()
                    .text_sm()
                    .text_color(tokens.faint)
                    .child("Anything you type lands in the transcript at the moment you typed it."),
            )
        })
        .children(annotations.iter().map(|annotation| {
            let anchor = annotation.anchor;
            let event_id = annotation.event_id;
            let edit = annotation.clone();
            div()
                .px_3()
                .py_2()
                .child(
                    ControlRow::new()
                        .child(
                            ControlRole::Essential,
                            div()
                                .text_sm()
                                .text_color(tokens.faint)
                                .child(moment_label(anchor, citation_times)),
                        )
                        .child(
                            ControlRole::Ellipsizing,
                            div()
                                .text_sm()
                                .text_color(tokens.accent)
                                .child(mark_label(annotation.mark)),
                        )
                        .child(
                            ControlRole::Essential,
                            Button::new(("edit-typed-note", event_id.get()), tokens)
                                .label("Edit")
                                .ghost()
                                .xsmall()
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    this.begin_annotation_edit(edit.clone(), window, cx);
                                })),
                        )
                        .child(
                            ControlRole::Essential,
                            Button::new(("typed-note-anchor", event_id.get()), tokens)
                                .label("Show")
                                .ghost()
                                .xsmall()
                                .tooltip("Jump to the transcript row this note is anchored to")
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.open_citation(anchor, cx);
                                })),
                        )
                        .finish()
                        .gap_2(),
                )
                .child(
                    div()
                        .min_w_0()
                        .debug_selector(move || format!("typed-note-text-{}", event_id.get()))
                        .child(SelectableText {
                            id: ("typed-note-text", event_id.get()).into(),
                            text: annotation.text.clone(),
                            color: tokens.ink_2,
                        }),
                )
        }))
        .into_any_element()
}

const fn mark_label(mark: MarkKind) -> &'static str {
    match mark {
        MarkKind::Note => "note",
        MarkKind::Important => "important mark",
        MarkKind::FollowUp => "follow-up mark",
    }
}

/// Only the case a person cannot act on earns a line. The other two explained a text field to
/// someone already typing in it.
const fn annotation_composer_reason(_live: bool, has_anchor: bool) -> Option<&'static str> {
    if has_anchor {
        None
    } else {
        Some("Waiting for the first transcript row so this note has a moment to attach to.")
    }
}

/// Labels the composer with the moment the note will attach to.
fn composer_anchor_label(
    selected_anchor: Option<EventId>,
    latest_anchor: Option<EventId>,
    citation_times: &CitationTimes,
) -> String {
    selected_anchor.map_or_else(
        || {
            latest_anchor.map_or_else(
                || "Attaches once this recording has its first transcript row".to_owned(),
                |anchor| format!("Attaches at {}", moment_label(anchor, citation_times)),
            )
        },
        |anchor| format!("Attaches at {}", moment_label(anchor, citation_times)),
    )
}

/// A transcript moment as the user reads it: its timecode when known, its record id otherwise.
fn moment_label(event_id: EventId, citation_times: &CitationTimes) -> String {
    citation_times
        .get(&event_id)
        .map_or_else(|| format!("#{}", event_id.get()), |time| timecode(*time))
}

fn timecode(time: Duration) -> String {
    let seconds = time.as_secs();
    let (hours, minutes, seconds) = (seconds / 3_600, (seconds % 3_600) / 60, seconds % 60);
    if hours > 0 {
        format!("{hours}:{minutes:02}:{seconds:02}")
    } else {
        format!("{minutes:02}:{seconds:02}")
    }
}

/// Record text the reader can select, copy, and reach with the keyboard.
///
/// A note-taking product whose notes cannot be copied is failing at its own job, and until now no
/// text in this workspace was selectable. [`TextView`] is `gpui-component`'s only selectable text
/// primitive, and it needs a `Window` that the column's plain render functions never receive — so
/// the leaf is a [`RenderOnce`] component, which is handed one at draw time. It also registers a
/// focus handle as a tab stop, so the text is reachable without a mouse.
#[derive(IntoElement)]
struct SelectableText {
    /// Identifies the text's place in the document; hashed with the text itself to key
    /// `TextView`'s parse state, because a stable id alone would show a stale sentence.
    id: ElementId,
    text: String,
    color: Rgba,
}

impl RenderOnce for SelectableText {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let mut identity = std::collections::hash_map::DefaultHasher::new();
        self.id.hash(&mut identity);
        self.text.hash(&mut identity);
        TextView::markdown(
            ("selectable-text", identity.finish()),
            as_literal_markdown(&self.text),
            window,
            cx,
        )
        .selectable(true)
        .text_color(self.color)
    }
}

/// Presents record text as text rather than as markup.
///
/// [`TextView`] parses its input as markdown. Summary claims and typed notes are prose that nobody
/// wrote as markup, so a claim mentioning `*` or a note beginning `- ` must not silently restyle
/// itself. CommonMark defines a backslash before any ASCII punctuation character as that literal
/// character, and selection copies the rendered text, so the escape never reaches the clipboard.
fn as_literal_markdown(text: &str) -> String {
    let mut escaped = String::with_capacity(text.len().saturating_mul(2));
    for character in text.chars() {
        if character.is_ascii_punctuation() {
            escaped.push('\\');
        }
        escaped.push(character);
    }
    escaped
}

impl MeetingWorkspace {
    fn begin_annotation_edit(
        &mut self,
        annotation: AnnotationView,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.focused_event = Some(annotation.anchor);
        self.annotation_input.update(cx, |input, cx| {
            input.set_value(annotation.text.clone(), window, cx)
        });
        self.editing_annotation = Some(annotation);
        self.message = Some("Editing typed note; Return appends a new version.".to_owned());
        cx.notify();
    }

    /// Whether the shared composer is editing the notes document rather than typing a note.
    ///
    /// One composer serves both, and Return has to mean different things in each: a block's
    /// verbatim text and an action's Owner and Due lines are multi-line, so Return must insert a
    /// newline there, while a typed note is a single line that Return has always submitted. The
    /// predicate is shared with [`Self::submit_annotation`] so the key and the button cannot drift
    /// into disagreeing about which of the two the composer is holding.
    pub(crate) fn composer_edits_notes_document(&self, cx: &App) -> bool {
        !self.transcript_live
            && matches!(
                self.notes.read(cx).snapshot().state,
                NotesState::Ready { .. } | NotesState::Stale { .. }
            )
    }

    pub(crate) fn submit_annotation(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.composer_edits_notes_document(cx) {
            self.submit_notes_block(window, cx);
            return;
        }
        let Some(id) = self.transcript_session else {
            self.message = Some("No recording is selected for this typed note.".to_owned());
            cx.notify();
            return;
        };
        let events = if self.transcript_live {
            self.timeline
                .read(cx)
                .events()
                .iter()
                .filter(|event| event.session_id() == id)
                .cloned()
                .collect::<Vec<_>>()
        } else {
            self.transcript_events.clone()
        };
        let selected = self.focused_event.filter(|candidate| {
            events.iter().any(|event| {
                event.id() == *candidate
                    && matches!(
                        event.payload(),
                        EventPayload::UtteranceFinal(_) | EventPayload::UtterancePartial(_)
                    )
            })
        });
        let default_anchor = if self.transcript_live {
            latest_anchor(&events)
        } else {
            last_final_anchor(&events)
        };
        let Some(anchor) = self
            .editing_annotation
            .as_ref()
            .map(|editing| editing.anchor)
            .or(selected)
            .or(default_anchor)
        else {
            self.message =
                Some("This recording has no transcript row to anchor your note to.".to_owned());
            cx.notify();
            return;
        };
        let text = self.annotation_input.read(cx).value().to_string();
        if text.trim().is_empty() {
            self.message = Some("Type a note before pressing Return.".to_owned());
            cx.notify();
            return;
        }
        let result = if self.transcript_live {
            if let Some(editing) = &self.editing_annotation {
                self.session
                    .read(cx)
                    .supersede_user_annotation(
                        editing.event_id,
                        text.trim().to_owned(),
                        editing.mark,
                    )
                    .map(|()| None)
            } else {
                self.session
                    .read(cx)
                    .append_user_annotation(anchor, text.trim().to_owned(), MarkKind::Note)
                    .map(|()| None)
            }
        } else {
            crate::persistence_runtime::block_on(async {
                let store = rag::Store::open(&self.database).await?;
                store
                    .append_completed_annotation(
                        id,
                        anchor,
                        text.trim(),
                        self.editing_annotation
                            .as_ref()
                            .map_or(MarkKind::Note, |editing| editing.mark),
                        self.editing_annotation
                            .as_ref()
                            .map(|editing| editing.event_id),
                    )
                    .await?;
                Ok::<_, sotto_core::RagError>(
                    store
                        .refresh_searchable_prior_meeting(id)
                        .await
                        .err()
                        .map(|error| error.to_string()),
                )
            })
            .map_err(|error| error.to_string())
        };
        match result {
            Ok(reingestion_error) => {
                self.annotation_input
                    .update(cx, |input, cx| input.set_value("", window, cx));
                self.editing_annotation = None;
                if self.transcript_live {
                    self.message = None;
                } else {
                    self.load_transcript(id);
                    let enabled = self.notes_ready(cx);
                    self.notes.update(cx, |notes, _| {
                        let _ = notes.select(id, enabled);
                    });
                    self.message = Some(reingestion_error.map_or_else(
                        || "Typed note appended. What was captured is unchanged; any existing summary is now marked stale.".to_owned(),
                        |error| format!("Typed note appended and any existing summary marked stale, but cross-session search could not be refreshed: {error}"),
                    ));
                }
            }
            Err(error) => self.message = Some(error),
        }
        cx.notify();
    }

    fn begin_notes_block_edit(
        &mut self,
        target: OverlayTarget,
        text: String,
        owner: Option<String>,
        due_date: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let action = target.action;
        let editable = if action {
            format!(
                "{text}\nOwner: {}\nDue: {}",
                owner.unwrap_or_default(),
                due_date.unwrap_or_default()
            )
        } else {
            text
        };
        self.annotation_input
            .update(cx, |input, cx| input.set_value(editable, window, cx));
        self.editing_notes_block = Some(EditingNotesBlock { target, action });
        self.message = Some(if action {
            "Editing block. Its Owner and Due lines are part of this verbatim edit.".to_owned()
        } else {
            "Editing block; save keeps your wording verbatim.".to_owned()
        });
        cx.notify();
    }

    fn apply_notes_operation(&mut self, operation: NotesOverlayOperation, cx: &mut Context<Self>) {
        let created_at = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| u64::try_from(duration.as_millis()).unwrap_or(u64::MAX));
        let result = created_at
            .map_err(|error| format!("The system clock cannot date this edit: {error}"))
            .and_then(|created_at| {
                self.notes.update(cx, |notes, _| {
                    notes.append_overlay_operation(&operation, created_at)
                })
            });
        self.message = Some(match result {
            Ok(()) => "Notes document saved. The generated artifact is unchanged.".to_owned(),
            Err(error) => error,
        });
        cx.notify();
    }

    fn submit_notes_block(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let input = self.annotation_input.read(cx).value().to_string();
        if input.trim().is_empty() {
            self.message = Some("Type a block before saving it.".to_owned());
            cx.notify();
            return;
        }
        let operation = if let Some(editing) = self.editing_notes_block.take() {
            let (text, owner, due_date) = parse_editable_block(&input, editing.action);
            NotesOverlayOperation::Reword {
                target: editing.target,
                text,
                owner,
                due_date,
            }
        } else {
            let id = format!(
                "user-{}",
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map_or(0, |duration| duration.as_nanos())
            );
            NotesOverlayOperation::Add {
                user_block_id: id,
                section: RecordingNotesSectionKind::Overview,
                text: input,
                action: false,
                owner: None,
                due_date: None,
            }
        };
        self.apply_notes_operation(operation, cx);
        self.annotation_input
            .update(cx, |input, cx| input.set_value("", window, cx));
    }
}

fn parse_editable_block(input: &str, action: bool) -> (String, Option<String>, Option<String>) {
    if !action {
        return (input.to_owned(), None, None);
    }
    let mut lines = input.lines().collect::<Vec<_>>();
    let due_date = lines
        .last()
        .and_then(|line| line.strip_prefix("Due:"))
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned);
    if lines.last().is_some_and(|line| line.starts_with("Due:")) {
        lines.pop();
    }
    let owner = lines
        .last()
        .and_then(|line| line.strip_prefix("Owner:"))
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned);
    if lines.last().is_some_and(|line| line.starts_with("Owner:")) {
        lines.pop();
    }
    (lines.join("\n"), owner, due_date)
}

#[must_use]
fn last_final_anchor(events: &[TimelineEvent]) -> Option<EventId> {
    replay_lenient(events)
        .state()
        .active()
        .values()
        .filter(|event| matches!(event.payload(), EventPayload::UtteranceFinal(_)))
        .map(TimelineEvent::id)
        .max()
}

/// What the sources block says, decided before anything is drawn.
///
/// The block used to render a heading, the read-only policy, the retrieval receipt and a note that
/// nothing was configured — four lines of disclosure about a capability nobody in that state is
/// using. Burying a disclosure in a permanent banner that applies to no one is how disclosures stop
/// being read, so the policy now appears exactly where a reader can act on it: beside the controls
/// that select resources and grant query disclosure.
#[derive(Clone, Debug, Eq, PartialEq)]
enum SourceContext {
    /// Nothing is configured. One quiet line, no heading and no policy.
    Quiet(&'static str),
    /// Sources exist, so the policy governs live controls and is stated over them.
    Configured {
        policy: &'static str,
        receipt: String,
    },
}

impl SourceContext {
    fn resolve(configured: bool, selected: Option<&SessionGrantView>) -> Self {
        if !configured {
            return Self::Quiet(
                "No outside sources configured; a summary is written from this recording alone. Add one in Settings.",
            );
        }
        Self::Configured {
            policy: "Default off. Only the exact read-only resources you select can be used by a requested summary.",
            receipt: selected.map_or_else(
                || "Select a recording to choose source context.".to_owned(),
                |view| match &view.receipts {
                    GrantReceiptState::NotRetrieved => "Not retrieved. A future summary may retrieve bounded evidence; a transcript-only summary remains available if it cannot.".to_owned(),
                },
            ),
        }
    }
}

/// Renders from values supplied by the caller.
///
/// This must never reach back through `cx.entity()`: it runs inside
/// `MeetingWorkspace::render`, which already holds that entity, and reading it again is a
/// double lease that aborts the process at launch rather than failing the frame.
fn render_source_context(
    servers: Vec<ConfiguredServer>,
    selected: Option<SessionGrantView>,
    cx: &mut Context<MeetingWorkspace>,
) -> gpui::AnyElement {
    let tokens = WorkspaceTokens::resolve(cx);
    let (policy, receipt) = match SourceContext::resolve(!servers.is_empty(), selected.as_ref()) {
        SourceContext::Quiet(line) => {
            return div()
                .mt_4()
                .text_sm()
                .text_color(tokens.faint)
                .debug_selector(|| "source-context-quiet".into())
                .child(line)
                .into_any_element();
        }
        SourceContext::Configured { policy, receipt } => (policy, receipt),
    };
    let selected_resources = selected
        .as_ref()
        .map(|view| view.grant.selected_resources())
        .unwrap_or_default();
    div()
        .mt_4()
        .p_3()
        .rounded_lg()
        .bg(tokens.sunken)
        .debug_selector(|| "source-context-block".into())
        .child(div().text_color(tokens.ink).child("Sources for this recording"))
        .child(div().text_sm().text_color(tokens.muted).child(policy))
        .child(div().text_sm().text_color(tokens.muted).child(receipt))
        .children(servers.into_iter().enumerate().map(|(server_index, server)| {
            let disclosed = selected
                .as_ref()
                .is_some_and(|view| view.grant.query_disclosure(&server.id) == mcp::MeetingQueryDisclosure::Redacted);
            let disclosure_id = server.id.clone();
            div()
                .mt_2()
                .child(
                    ControlRow::new()
                        .child(
                            ControlRole::Ellipsizing,
                            div().text_sm().child(format!(
                                "{} — remote host {}",
                                server.display_name,
                                server.endpoint.host()
                            )),
                        )
                        .child(
                            ControlRole::Essential,
                            Button::new(("query-disclosure", server_index), tokens)
                                .label(if disclosed { "Disclosure: on" } else { "Disclosure: off" })
                                .ghost()
                                .xsmall()
                                .tooltip("Whether a summary may disclose a redacted recording-derived query to this source")
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.toggle_query_disclosure(disclosure_id.clone(), !disclosed, cx);
                                })),
                        )
                        .finish()
                        .gap_2(),
                )
                .children(server.resources.into_iter().enumerate().map(|(resource_index, resource)| {
                    let chosen = selected_resources.iter().any(|selection| {
                        selection.server_id == resource.server_id && selection.uri == resource.uri
                    });
                    let server_id = resource.server_id.clone();
                    let uri = resource.uri.clone();
                    let label = resource.title.clone().unwrap_or(resource.name);
                    ControlRow::new()
                        .child(
                            ControlRole::Ellipsizing,
                            div().text_sm().text_color(tokens.muted).child(label),
                        )
                        .child(
                            ControlRole::Essential,
                            Button::new((
                                "source-resource",
                                server_index.saturating_mul(10_000).saturating_add(resource_index),
                            ), tokens)
                            .label(if chosen { "Selected" } else { "Use" })
                            .ghost()
                            .xsmall()
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.toggle_source_resource(server_id.clone(), uri.clone(), cx);
                            })),
                        )
                        .finish()
                        .gap_2()
                }))
        }))
        .into_any_element()
}

/// One claim, with the evidence that makes it sayable.
#[derive(Clone, Debug, Eq, PartialEq)]
struct Claim {
    /// Who owns the claim, when the taxonomy attributes one.
    lead: Option<String>,
    text: String,
    /// Qualifiers the taxonomy attached to the claim, such as a due date.
    detail: Option<String>,
    meeting: Vec<EventId>,
    external: Vec<mcp::EvidenceId>,
    provenance: NotesBlockProvenance,
    checked: bool,
    orphaned: bool,
    target: Option<OverlayTarget>,
    action: bool,
    owner: Option<String>,
    due_date: Option<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SectionShape {
    /// Continuous text; the mock renders it as a paragraph.
    Prose,
    /// Discrete claims; the mock renders them as bullets.
    Points,
}

/// One section of the adaptive summary. It exists only because it has claims.
#[derive(Clone, Debug, Eq, PartialEq)]
struct SummarySection {
    heading: String,
    shape: SectionShape,
    claims: Vec<Claim>,
}

/// The single seam between the notes taxonomy and this column.
///
/// This is the only place a section is named. When the taxonomy changes shape, this function
/// changes and nothing else does — the renderers below walk whatever `Vec` they are handed. An
/// empty section never reaches them, so a heading with nothing under it cannot be drawn.
///
/// `RecordingNotes` is expected to already exclude empty sections — its validation rejects one —
/// but the filter below does not lean on that alone: a heading with nothing under it must never be
/// producible from this function no matter what the artifact contains.
#[cfg(test)]
fn summary_sections(notes: RecordingNotes) -> Vec<SummarySection> {
    notes
        .sections
        .into_iter()
        .filter(|section| !section.blocks.is_empty())
        .map(summary_section)
        .collect()
}

fn document_sections(document: PresentedNotesDocument) -> Vec<SummarySection> {
    let mut sections = Vec::<SummarySection>::new();
    for block in document.blocks {
        let (heading, shape) = section_heading(block.section);
        if sections
            .last()
            .is_none_or(|section| section.heading != heading)
        {
            sections.push(SummarySection {
                heading: heading.to_owned(),
                shape,
                claims: Vec::new(),
            });
        }
        if let Some(section) = sections.last_mut() {
            section.claims.push(presented_claim(block));
        }
    }
    sections
}

fn presented_claim(block: PresentedNotesBlock) -> Claim {
    let target = Some(OverlayTarget {
        block_id: match &block.id {
            PresentedNotesBlockId::Generated(block_id) => block_id.as_str().to_owned(),
            PresentedNotesBlockId::User(block_id) => block_id.clone(),
        },
        section: block.section,
        action: block.action,
        meeting_citations: block.meeting_citations.clone(),
        external_citations: block.external_citations.clone(),
    });
    Claim {
        lead: block.owner.clone(),
        text: block.text,
        detail: block.due_date.clone().map(|date| format!("due {date}")),
        meeting: block.meeting_citations,
        external: block.external_citations,
        provenance: block.provenance,
        checked: block.checked,
        orphaned: block.orphaned,
        target,
        action: block.action,
        owner: block.owner,
        due_date: block.due_date,
    }
}

#[cfg(test)]
fn summary_section(section: RecordingNotesSection) -> SummarySection {
    let (heading, shape) = section_heading(section.kind);
    SummarySection {
        heading: heading.to_owned(),
        shape,
        claims: section.blocks.into_iter().map(block_claim).collect(),
    }
}

/// Where each recording-supported section kind lands in the column: its heading, in the voice of
/// `docs/design/workspace-v2-mock.html`, and whether it reads as prose or discrete points.
const fn section_heading(kind: RecordingNotesSectionKind) -> (&'static str, SectionShape) {
    match kind {
        RecordingNotesSectionKind::Overview => ("Overview", SectionShape::Prose),
        RecordingNotesSectionKind::Topics => ("Topics", SectionShape::Points),
        RecordingNotesSectionKind::Explanations => ("Explanations", SectionShape::Points),
        RecordingNotesSectionKind::Findings => ("Findings", SectionShape::Points),
        RecordingNotesSectionKind::Decisions => ("Decisions", SectionShape::Points),
        RecordingNotesSectionKind::ActionItems => ("Action items", SectionShape::Points),
        RecordingNotesSectionKind::OpenQuestions => ("Open questions", SectionShape::Points),
        RecordingNotesSectionKind::Risks => ("Risks", SectionShape::Points),
        RecordingNotesSectionKind::FollowUps => ("Follow-ups", SectionShape::Points),
    }
}

#[cfg(test)]
fn block_claim(block: RecordingNotesBlock) -> Claim {
    match block {
        RecordingNotesBlock::Claim {
            text,
            meeting_citations,
            external_citations,
            ..
        } => Claim {
            lead: None,
            text,
            detail: None,
            meeting: meeting_citations,
            external: external_citations,
            provenance: NotesBlockProvenance::Generated,
            checked: false,
            orphaned: false,
            target: None,
            action: false,
            owner: None,
            due_date: None,
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
        } => {
            let mut meeting = meeting_citations;
            meeting.extend(owner_meeting_citations);
            meeting.extend(due_date_meeting_citations);
            meeting.sort_unstable();
            meeting.dedup();
            let mut external = external_citations;
            external.extend(owner_external_citations);
            external.extend(due_date_external_citations);
            external.sort();
            external.dedup();
            Claim {
                lead: owner,
                text,
                detail: due_date.map(|date| format!("due {date}")),
                meeting,
                external,
                provenance: NotesBlockProvenance::Generated,
                checked: false,
                orphaned: false,
                target: None,
                action: true,
                owner: None,
                due_date: None,
            }
        }
    }
}

/// Everything the column needs to say about the summary, resolved from one state.
struct SummaryView {
    /// The head meta line: "N sections · every claim cited", or why there is no summary yet.
    meta: String,
    /// Provenance the user is owed before they trust the sections: model, origin, sources.
    provenance: Vec<String>,
    /// A warning band, shown when the summary is stale or the run failed.
    caution: Option<String>,
    /// What is happening or will happen, shown when there are no sections to read.
    pending: Option<String>,
    sections: Vec<SummarySection>,
    bundle: Option<mcp::ContextBundle>,
    screen_consultations: Vec<ScreenConsultation>,
    /// True while a notes pass is in flight. Drives the indeterminate bar, not copy.
    writing: bool,
}

impl SummaryView {
    fn resolve(state: NotesState, live: bool) -> Self {
        if live {
            return Self::pending(
                "Notes after you stop",
                "Notes are written after you stop — from the recording, so every claim can point at a moment. Anything you type below is kept at the moment you typed it.",
            );
        }
        match state {
            NotesState::NoMeeting => Self::pending(
                "nothing recorded yet",
                "No recording is open. Start or import one, and the summary is written from its transcript after it stops.",
            ),
            NotesState::Disabled => Self::pending(
                "no notes yet",
                "No notes yet. Writing them reads the transcript and keeps only what this recording supports — with a moment on every claim. Add a writing source in Settings, then pick it here.",
            ),
            NotesState::Generating => {
                let mut view = Self::pending(
                    "writing notes…",
                    "Reading the transcript and writing only what it supports. You can still read while this runs.",
                );
                view.writing = true;
                view
            }
            NotesState::Failed(error) => {
                let mut view = Self::pending(
                    "no notes",
                    "The transcript is unchanged and still complete. Write notes again once the cause above is addressed.",
                );
                view.caution = Some(format!("Writing notes failed: {error}"));
                view
            }
            NotesState::Ready {
                document,
                bundle,
                source_status,
                cached,
                model,
                normalizations,
                screen_consultations,
                ..
            } => Self::ready(
                *document,
                bundle,
                source_status,
                format!("{} · {model}", ready_origin(cached)),
                None,
                &normalizations,
                screen_consultations,
            ),
            NotesState::Stale {
                document,
                bundle,
                source_status,
                model,
                normalizations,
                screen_consultations,
                ..
            } => Self::ready(
                *document,
                bundle,
                source_status,
                format!("Saved notes · {model}"),
                Some(
                    "This write-up is from before your latest typed note. Write notes again when you want it to include that note."
                        .to_owned(),
                ),
                &normalizations,
                screen_consultations,
            ),
        }
    }

    fn pending(meta: &str, pending: &str) -> Self {
        Self {
            meta: meta.to_owned(),
            provenance: Vec::new(),
            caution: None,
            pending: Some(pending.to_owned()),
            sections: Vec::new(),
            bundle: None,
            screen_consultations: Vec::new(),
            writing: false,
        }
    }

    fn ready(
        document: PresentedNotesDocument,
        bundle: mcp::ContextBundle,
        source_status: SourceStatus,
        heading: String,
        caution: Option<String>,
        normalizations: &[ObservedRequestNormalization],
        screen_consultations: Vec<ScreenConsultation>,
    ) -> Self {
        let sections = document_sections(document);
        let provenance = std::iter::once(heading)
            .chain(source_line(source_status).map(ToOwned::to_owned))
            .chain(normalization_line(normalizations))
            .collect();
        // A summary with no section at all is not a summary; say so rather than drawing a heading
        // count of zero over an empty column.
        if sections.is_empty() {
            let mut view = Self::pending(
                "no sections supported",
                "The transcript did not support a single section. Nothing was written rather than something unevidenced.",
            );
            view.provenance = provenance;
            view.caution = caution;
            view.screen_consultations = screen_consultations;
            return view;
        }
        Self {
            meta: sections_meta(sections.len()),
            provenance,
            caution,
            pending: None,
            sections,
            bundle: Some(bundle),
            screen_consultations,
            writing: false,
        }
    }
}

fn sections_meta(count: usize) -> String {
    if count == 1 {
        "1 section".to_owned()
    } else {
        format!("{count} sections")
    }
}

const fn ready_origin(cached: bool) -> &'static str {
    if cached {
        "Saved notes"
    } else {
        "Written just now"
    }
}

/// Names each unsupported control once even when a multi-dispatch notes run observed the same
/// normalization during more than one map/reduce request.
///
/// This is provenance, not a failure. Codex has none of these knobs by design; Insight still
/// parses the reply and rejects uncited claims. Putting the same sentence in the warning wash
/// made a successful summary look like it had failed.
pub(super) fn normalization_line(
    normalizations: &[ObservedRequestNormalization],
) -> Option<String> {
    let names = lost_control_names(normalizations);
    (!names.is_empty()).then(|| {
        format!(
            "This backend cannot set {}. The summary was still written from the reply.",
            join_control_names(&names)
        )
    })
}

fn lost_control_names(normalizations: &[ObservedRequestNormalization]) -> Vec<&'static str> {
    let mut json_object = false;
    let mut max_tokens = false;
    let mut temperature = false;
    for item in normalizations {
        match item.normalization.control {
            SamplingControl::JsonObjectOutput => json_object = true,
            SamplingControl::MaxTokens => max_tokens = true,
            SamplingControl::Temperature => temperature = true,
        }
    }
    let mut names = Vec::new();
    if json_object {
        names.push("JSON mode");
    }
    if max_tokens {
        names.push("a token cap");
    }
    if temperature {
        names.push("temperature");
    }
    names
}

fn join_control_names(names: &[&str]) -> String {
    match names {
        [] => String::new(),
        [one] => (*one).to_owned(),
        [first, second] => format!("{first} or {second}"),
        [rest @ .., last] => format!("{}, or {last}", rest.join(", ")),
    }
}

/// Sources are worth a line only when they changed the answer, or when they were meant to and
/// could not. "None selected" is the default state of a feature the reader is not using.
const fn source_line(status: SourceStatus) -> Option<&'static str> {
    match status {
        SourceStatus::NotSelected => None,
        SourceStatus::Available => Some("Sources: retrieved and saved with this summary."),
        SourceStatus::Unavailable => {
            Some("Sources: unavailable; the summary fell back to this recording alone.")
        }
    }
}

/// Which claims are currently showing the evidence that made them sayable.
///
/// This is display state and nothing else. `insight` still rejects an uncited claim before it can
/// reach this column, and every chip drawn below still resolves to a real transcript row. What is
/// decided here is only whether the chips are worth their vertical space right now.
///
/// It lives in window element state rather than on the workspace on purpose: reading a summary is
/// not an edit, so revealing evidence must not touch the session, the record, or anything
/// persisted.
#[derive(Debug, Default, Eq, PartialEq)]
struct EvidenceDisclosure {
    /// Identifies the summary these choices were made about.
    summary: u64,
    /// Whether the whole-summary toggle currently shows every claim's chips.
    all: bool,
    screen: bool,
}

impl EvidenceDisclosure {
    /// The choices that apply to `summary`. A different summary reads quiet again.
    fn revealed(&self, summary: u64) -> Revealed {
        if self.summary == summary {
            Revealed {
                all: self.all,
                screen: self.screen,
            }
        } else {
            Revealed::default()
        }
    }

    /// Re-points at `summary`, discarding the choice made about a different one: a re-summarize
    /// reads quiet again rather than inheriting the last summary's revealed state.
    fn rebind(&mut self, summary: u64) {
        if self.summary != summary {
            self.summary = summary;
            self.all = false;
            self.screen = false;
        }
    }

    fn toggle_all(&mut self, summary: u64) {
        self.rebind(summary);
        self.all = !self.all;
    }

    fn toggle_screen(&mut self, summary: u64) {
        self.rebind(summary);
        self.screen = !self.screen;
    }
}

/// The disclosure choice in force for the summary being drawn.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct Revealed {
    all: bool,
    screen: bool,
}

/// Identifies a rendered summary, so evidence choices never carry over to a different one.
fn summary_fingerprint(sections: &[SummarySection]) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    for section in sections {
        section.heading.hash(&mut hasher);
        section.claims.len().hash(&mut hasher);
        for claim in &section.claims {
            claim.text.hash(&mut hasher);
            claim.meeting.len().hash(&mut hasher);
            claim.external.len().hash(&mut hasher);
        }
    }
    hasher.finish()
}

fn consultation_fingerprint(summary: u64, consultations: &[ScreenConsultation]) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    summary.hash(&mut hasher);
    for consultation in consultations {
        consultation.describe().hash(&mut hasher);
    }
    hasher.finish()
}

/// The summary itself: provenance, the evidence control, and the sections.
///
/// A [`RenderOnce`] component rather than a plain function because the column's callers hand it no
/// `Window`, and both selectable text and the per-window disclosure state need one. It carries a
/// weak workspace handle instead of a `Context`, so nothing here can re-lease the entity that
/// `MeetingWorkspace::render` already holds.
#[derive(IntoElement)]
struct SummaryBody {
    summary: SummaryView,
    citation_times: CitationTimes,
    workspace: WeakEntity<MeetingWorkspace>,
}

/// What every claim in one summary shares, so no renderer needs eight parameters.
struct ClaimContext<'a> {
    summary: u64,
    disclosure: Entity<EvidenceDisclosure>,
    revealed: Revealed,
    bundle: mcp::ContextBundle,
    citation_times: &'a CitationTimes,
    workspace: WeakEntity<MeetingWorkspace>,
    tokens: WorkspaceTokens,
}

impl RenderOnce for SummaryBody {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let tokens = WorkspaceTokens::resolve(cx);
        let summary = self.summary;
        let screen_consultations = summary.screen_consultations.clone();
        let fingerprint = consultation_fingerprint(
            summary_fingerprint(&summary.sections),
            &screen_consultations,
        );
        let disclosure = window.use_keyed_state("summary-evidence-disclosure", cx, |_, _| {
            EvidenceDisclosure::default()
        });
        let revealed = disclosure.read(cx).revealed(fingerprint);
        let context = ClaimContext {
            summary: fingerprint,
            disclosure,
            revealed,
            bundle: summary.bundle.unwrap_or_else(mcp::ContextBundle::empty),
            citation_times: &self.citation_times,
            workspace: self.workspace,
            tokens,
        };
        let mut ordinal = 0_usize;
        let writing = summary.writing;
        div()
            .debug_selector(|| "summary-area".into())
            .children(summary.caution.map(|caution| {
                div()
                    .mb_3()
                    .p_3()
                    .rounded_lg()
                    .bg(tokens.warn_wash)
                    .text_sm()
                    .text_color(tokens.warn)
                    .child(caution)
            }))
            .children(
                summary
                    .provenance
                    .into_iter()
                    .map(|line| div().mb_1().text_sm().text_color(tokens.faint).child(line)),
            )
            .children(summary.pending.map(|pending| {
                div()
                    .mt_2()
                    .p_3()
                    .rounded_lg()
                    .text_sm()
                    .text_color(tokens.muted)
                    .debug_selector(|| "summary-pending".into())
                    .child(pending)
                    .when(writing, |view| {
                        view.child(
                            div()
                                .mt_3()
                                .child(motion::writing_progress("notes-writing", tokens)),
                        )
                    })
            }))
            .children((!screen_consultations.is_empty()).then(|| {
                render_screen_receipt(
                    &screen_consultations,
                    fingerprint,
                    context.disclosure.clone(),
                    revealed.screen,
                    tokens,
                )
            }))
            .children((!summary.sections.is_empty()).then(|| render_evidence_control(&context)))
            .children(
                summary
                    .sections
                    .into_iter()
                    .map(|section| render_section(section, &context, &mut ordinal)),
            )
    }
}

fn render_screen_receipt(
    consultations: &[ScreenConsultation],
    summary: u64,
    disclosure: Entity<EvidenceDisclosure>,
    revealed: bool,
    tokens: WorkspaceTokens,
) -> gpui::AnyElement {
    let count = consultations.len();
    div()
        .mt_2()
        .debug_selector(|| "summary-screen-receipt".into())
        .child(
            Button::new("summary-screen-receipt-toggle", tokens)
                .label(if revealed {
                    "Hide screen consultation receipt".to_owned()
                } else {
                    format!("Screen requests: {count} · Show receipt")
                })
                .ghost()
                .small()
                .on_click(move |_, _, cx| {
                    disclosure.update(cx, |state, cx| {
                        state.toggle_screen(summary);
                        cx.notify();
                    });
                }),
        )
        .children(revealed.then(|| {
            div().children(consultations.iter().map(|entry| {
                div()
                    .mt_1()
                    .text_sm()
                    .text_color(tokens.faint)
                    .child(entry.describe())
            }))
        }))
        .into_any_element()
}

/// The one control that reveals or hides every claim's evidence at once.
fn render_evidence_control(context: &ClaimContext<'_>) -> gpui::AnyElement {
    let shown = context.revealed.all;
    let summary = context.summary;
    let disclosure = context.disclosure.clone();
    ControlRow::new()
        .child(ControlRole::Ellipsizing, div())
        .child(
            ControlRole::Essential,
            evidence_control(
                "summary-evidence-toggle".into(),
                "summary-evidence-toggle".to_owned(),
                if shown {
                    "Hide timecodes".to_owned()
                } else {
                    "Show timecodes".to_owned()
                },
                "Show or hide the transcript timecodes behind every claim in this summary",
                context.tokens,
                move |cx| {
                    disclosure.update(cx, |state, cx| {
                        state.toggle_all(summary);
                        cx.notify();
                    });
                },
            ),
        )
        .finish()
        .mt_2()
        .mb_1()
        .gap_2()
        .into_any_element()
}

fn render_section(
    section: SummarySection,
    context: &ClaimContext<'_>,
    ordinal: &mut usize,
) -> gpui::AnyElement {
    let tokens = context.tokens;
    let count = section.claims.len().to_string();
    let prose = section.shape == SectionShape::Prose;
    div()
        .mt_3()
        .child(
            div().pb_1().mb_2().child(
                ControlRow::new()
                    .child(
                        ControlRole::Ellipsizing,
                        div()
                            .font_family(TypeScale::READING)
                            .text_size(TypeScale::TITLE)
                            .text_color(tokens.ink)
                            .child(section.heading),
                    )
                    .child(
                        ControlRole::Essential,
                        div().text_sm().text_color(tokens.faint).child(count),
                    )
                    .finish()
                    .gap_2(),
            ),
        )
        .children(section.claims.into_iter().map(|claim| {
            let index = *ordinal;
            *ordinal = ordinal.saturating_add(1);
            render_claim(claim, index, prose, context)
        }))
        .into_any_element()
}

fn render_claim(
    claim: Claim,
    ordinal: usize,
    prose: bool,
    context: &ClaimContext<'_>,
) -> gpui::AnyElement {
    let tokens = context.tokens;
    let raw_text = claim.text.clone();
    let owner = claim.owner.clone();
    let due_date = claim.due_date.clone();
    let checked = claim.checked;
    let target = claim.target.clone();
    let mut text = claim.text;
    if let Some(lead) = claim.lead {
        text = format!("{lead} — {text}");
    }
    if let Some(detail) = claim.detail {
        text = format!("{text} ({detail})");
    }
    if claim.checked {
        text = format!("✓ {text}");
    }
    let provenance = match claim.provenance {
        NotesBlockProvenance::Generated => "Generated",
        NotesBlockProvenance::EditedFromDraft => "Edited from draft",
        NotesBlockProvenance::UserAuthored => "Your words",
    };
    let provenance_line = div()
        .text_xs()
        .text_color(if claim.orphaned {
            tokens.warn
        } else {
            tokens.faint
        })
        .child(if claim.orphaned {
            format!(
                "{provenance} · the regenerated summary no longer contains the block you edited"
            )
        } else {
            provenance.to_owned()
        });
    #[cfg(test)]
    let provenance_line = provenance_line.debug_selector(move || {
        if claim.orphaned {
            format!("summary-provenance-orphaned-{ordinal}")
        } else {
            format!(
                "summary-provenance-{}-{ordinal}",
                match claim.provenance {
                    NotesBlockProvenance::Generated => "generated",
                    NotesBlockProvenance::EditedFromDraft => "edited",
                    NotesBlockProvenance::UserAuthored => "user",
                }
            )
        }
    });
    let hover_group = format!("notes-block-{ordinal}");
    div()
        .group(hover_group.clone())
        .mb_2()
        .min_w_0()
        .when(!prose, |item| {
            item.pl_3().border_l_2().border_color(tokens.line)
        })
        .child(
            div()
                .min_w_0()
                .debug_selector(move || format!("summary-claim-{ordinal}"))
                .child(SelectableText {
                    id: ("summary-claim", ordinal).into(),
                    text,
                    color: tokens.ink_2,
                }),
        )
        .children(target.map(|target| {
            let check_target = target.clone();
            let edit_target = target.clone();
            let hide_target = target;
            let edit_text = raw_text.clone();
            let edit_owner = owner.clone();
            let edit_due_date = due_date.clone();
            let check_workspace = context.workspace.clone();
            let edit_workspace = context.workspace.clone();
            let hide_workspace = context.workspace.clone();
            ControlRow::new()
                .child(ControlRole::Ellipsizing, div())
                .child_when(claim.action, ControlRole::Essential, || {
                    let label = if checked { "Uncheck" } else { "Check" };
                    let button = Button::new(("notes-check", ordinal), tokens)
                        .label(label)
                        .ghost()
                        .xsmall()
                        .on_click(move |_, _, cx| {
                            let operation = NotesOverlayOperation::SetChecked {
                                target: check_target.clone(),
                                checked: !checked,
                            };
                            let _ = check_workspace.update(cx, |this, cx| {
                                this.apply_notes_operation(operation, cx);
                            });
                        });
                    #[cfg(test)]
                    let button = button.debug_selector(move || format!("notes-check-{ordinal}"));
                    button.into_any_element()
                })
                .child(ControlRole::Essential, {
                    let button = Button::new(("notes-edit", ordinal), tokens)
                        .label("Edit")
                        .ghost()
                        .xsmall()
                        .on_click(move |_, window, cx| {
                            let _ = edit_workspace.update(cx, |this, cx| {
                                this.begin_notes_block_edit(
                                    edit_target.clone(),
                                    edit_text.clone(),
                                    edit_owner.clone(),
                                    edit_due_date.clone(),
                                    window,
                                    cx,
                                );
                            });
                        });
                    #[cfg(test)]
                    let button = button.debug_selector(move || format!("notes-edit-{ordinal}"));
                    button
                })
                .child(ControlRole::Essential, {
                    let button = Button::new(("notes-hide", ordinal), tokens)
                        .label("Hide")
                        .ghost()
                        .xsmall()
                        .on_click(move |_, _, cx| {
                            let operation = NotesOverlayOperation::Hide {
                                target: hide_target.clone(),
                            };
                            let _ = hide_workspace.update(cx, |this, cx| {
                                this.apply_notes_operation(operation, cx);
                            });
                        });
                    #[cfg(test)]
                    let button = button.debug_selector(move || format!("notes-hide-{ordinal}"));
                    button
                })
                .finish()
                .gap_1()
                .opacity(0.0)
                .group_hover(hover_group, |style| style.opacity(1.0))
        }))
        .child(provenance_line)
        .child(render_evidence(
            claim.meeting,
            claim.external,
            ordinal,
            context,
        ))
        .into_any_element()
}

/// A claim's evidence chips, drawn only while the whole summary is showing its timecodes.
///
/// There is deliberately no per-claim control. One existed briefly, and it worked against the
/// task it belonged to: a compact affordance under every claim is a line of chrome under every
/// claim, and a summary meant to read as prose was reading as a form. Evidence is now one choice
/// for the whole summary — the chips are the promise, and `Show timecodes` is when they are spent.
fn render_evidence(
    meeting: Vec<EventId>,
    external: Vec<mcp::EvidenceId>,
    ordinal: usize,
    context: &ClaimContext<'_>,
) -> gpui::AnyElement {
    div()
        .mt_1()
        .min_w_0()
        .children(
            context
                .revealed
                .all
                .then(|| render_citations(meeting, external, ordinal, context)),
        )
        .into_any_element()
}

/// One evidence control: a focusable button that answers the pointer and the keyboard alike.
///
/// `gpui-component`'s button registers a tab stop but binds no key activation, so a control that
/// only answered a click would put the evidence out of a keyboard reader's reach. Hiding the chips
/// is a decision about vertical space; it may not become a decision about who can see the evidence.
/// The key listener sits on the wrapper because key events dispatch up from the focused button
/// through its ancestors.
///
/// **The key path is NOT covered by an automated test.** The mounted-render harness builds a window
/// whose root view is [`MeetingWorkspace`] rather than `gpui_component::Root`, so a simulated
/// keystroke panics inside `gpui-component`'s root lookup before reaching any listener, and mouse
/// events in that harness never move focus onto a button. Both are properties of how the window is
/// mounted, not of this control. The pointer path below is covered.
pub(super) fn evidence_control(
    id: ElementId,
    selector: String,
    label: String,
    tooltip: &'static str,
    tokens: WorkspaceTokens,
    activate: impl Fn(&mut App) + 'static,
) -> gpui::AnyElement {
    let activate = std::rc::Rc::new(activate);
    let by_key = std::rc::Rc::clone(&activate);
    div()
        .flex_none()
        .min_w_0()
        .on_key_down(move |event, _, cx| {
            if matches!(event.keystroke.key.as_str(), "enter" | "space") {
                by_key(cx);
            }
        })
        .child(
            Button::new(id, tokens)
                .label(label)
                .ghost()
                .xsmall()
                .tooltip(tooltip)
                .debug_selector(move || selector)
                .on_click(move |_, _, cx| activate(cx)),
        )
        .into_any_element()
}

/// Citation chips wrap rather than compete for one line.
///
/// This is the fifth instance of the same clipping defect in this project, and the shipped column
/// was the worst of them: three "Open transcript evidence" buttons on one non-wrapping row, the
/// third sliced through a word. A chip list has no fixed arity, so no shrink priority can save it —
/// a fourth citation would always clip whichever child ranked last. The fix is the mock's own: the
/// chip is a short moment label, and the row wraps. Fixed-arity control rows in this column go
/// through [`ControlRow`], which is what shrink priority is actually for.
fn render_citations(
    meeting: Vec<EventId>,
    external: Vec<mcp::EvidenceId>,
    ordinal: usize,
    context: &ClaimContext<'_>,
) -> gpui::AnyElement {
    let tokens = context.tokens;
    let citation_times = context.citation_times;
    let bundle = &context.bundle;
    div()
        .mt_1()
        .min_w_0()
        .child(div().flex().flex_wrap().gap_1().min_w_0().children(
            meeting.into_iter().enumerate().map(|(index, event_id)| {
                let workspace = context.workspace.clone();
                Button::new(
                    (
                        "summary-citation",
                        ordinal.saturating_mul(1_000).saturating_add(index),
                    ),
                    tokens,
                )
                .label(moment_label(event_id, citation_times))
                .outline()
                .xsmall()
                .tooltip("Jump to the transcript row that supports this claim")
                .debug_selector(move || format!("summary-citation-{ordinal}-{index}"))
                .on_click(move |_, _, cx| {
                    let _ = workspace.update(cx, |this, cx| this.open_citation(event_id, cx));
                })
            }),
        ))
        .children(external.into_iter().filter_map(|id| {
            bundle
                .excerpts()
                .iter()
                .find(|excerpt| excerpt.evidence_id == id)
                .map(|excerpt| {
                    div()
                        .mt_1()
                        .text_sm()
                        .text_color(tokens.faint)
                        .child(format!(
                            "External evidence · {} · {} · {} · SHA-256 {}{}",
                            excerpt.title,
                            excerpt.receipt.server_id.as_str(),
                            excerpt.receipt.resource_uri.as_str(),
                            excerpt.receipt.content_sha256,
                            if excerpt.receipt.truncated {
                                " · truncated"
                            } else {
                                ""
                            }
                        ))
                })
        }))
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use insight::RecordingNotes;
    use providers::{
        BackendId,
        backend::{ObservedRequestNormalization, RequestNormalization, SamplingControl},
    };
    use sotto_core::{
        CaptureTarget, EventId, EventPayload, MarkKind, Session, SessionId, Source, SpeechState,
        TargetKind, TimelineBuilder, Utterance, VadSegment,
    };

    use super::{
        CitationTimes, EvidenceDisclosure, SectionShape, SourceContext, SummaryView,
        active_annotations, annotations_by_anchor, as_literal_markdown, composer_anchor_label,
        last_final_anchor, latest_anchor, moment_label, resolve_transcript_anchor, sections_meta,
        summary_fingerprint, summary_sections, timecode,
    };
    use crate::notes::NotesState;

    /// A claim block, cited to `citation` alone. Block ids are Sotto-derived in the real
    /// pipeline; a fixture only needs one that is unique within the `RecordingNotes` it builds,
    /// since these tests call `summary_sections` directly rather than `RecordingNotes::validate`.
    fn note(text: &str, citation: u64) -> serde_json::Value {
        serde_json::json!({
            "type": "claim",
            "id": format!("test-claim-{citation}"),
            "text": text,
            "meeting_citations": [citation],
            "external_citations": [],
        })
    }

    fn document(
        notes: RecordingNotes,
    ) -> Result<insight::PresentedNotesDocument, insight::NotesOverlayError> {
        insight::compose_notes_document(&notes, &[])
    }

    fn action(text: &str, citation: u64) -> serde_json::Value {
        serde_json::json!({
            "type": "action",
            "id": format!("test-action-{citation}"),
            "text": text,
            "meeting_citations": [citation],
            "external_citations": [],
            "owner": "Dana",
            "owner_meeting_citations": [citation],
            "owner_external_citations": [],
            "due_date": "Thursday",
            "due_date_meeting_citations": [citation],
            "due_date_external_citations": [],
        })
    }

    fn section(kind: &str, blocks: Vec<serde_json::Value>) -> serde_json::Value {
        serde_json::json!({ "kind": kind, "blocks": blocks })
    }

    /// Assembles a `RecordingNotes` fixture from section fragments built by [`section`].
    /// `RecordingNotesBlockId`'s inner field is private, so a fixture goes through JSON the way a
    /// real artifact would rather than constructing blocks as Rust struct literals.
    #[expect(
        clippy::panic,
        reason = "a fixture that fails to parse is a test bug; stop immediately"
    )]
    fn recording_notes(sections: Vec<serde_json::Value>) -> RecordingNotes {
        serde_json::from_value(serde_json::json!({ "sections": sections }))
            .unwrap_or_else(|error| panic!("test fixture must parse as RecordingNotes: {error}"))
    }

    fn target() -> CaptureTarget {
        CaptureTarget {
            bundle_id: None,
            display_name: "Recording".to_owned(),
            window_title: None,
            kind: TargetKind::Application,
            audio_scoped: true,
        }
    }

    #[test]
    fn a_debugging_session_renders_only_the_sections_its_content_supports() {
        let notes = recording_notes(vec![
            section(
                "overview",
                vec![note("Traced the double charge to a lock-key mismatch.", 1)],
            ),
            section("topics", vec![note("Lock is keyed on order id.", 2)]),
            section(
                "follow_ups",
                vec![action("Key both paths on payment intent id.", 3)],
            ),
        ]);

        let sections = summary_sections(notes);

        assert_eq!(
            sections
                .iter()
                .map(|section| section.heading.as_str())
                .collect::<Vec<_>>(),
            vec!["Overview", "Topics", "Follow-ups"],
            "only sections with content may be rendered"
        );
        assert_eq!(
            sections[0].shape,
            SectionShape::Prose,
            "an overview reads as prose"
        );
        assert_eq!(
            sections[2].shape,
            SectionShape::Points,
            "follow-ups read as discrete claims"
        );
    }

    #[test]
    fn a_recording_whose_content_supports_every_section_renders_all_seven() {
        let notes = recording_notes(vec![
            section("overview", vec![note("Sprint 41 planning.", 1)]),
            section("topics", vec![note("Capacity.", 2)]),
            section("decisions", vec![note("Search rewrite deferred.", 3)]),
            section("action_items", vec![action("Retry rollout checklist.", 4)]),
            section(
                "open_questions",
                vec![note("Does the exporter live on?", 5)],
            ),
            section("risks", vec![note("Staging is still fragile.", 6)]),
            section("follow_ups", vec![action("Book the security review.", 7)]),
        ]);

        let sections = summary_sections(notes);

        assert_eq!(sections.len(), 7, "seven supported sections render seven");
        assert_eq!(
            sections_meta(sections.len()),
            "7 sections",
            "the meta line counts what was rendered"
        );
        assert_eq!(
            sections_meta(1),
            "1 section",
            "a single section is not reported in the plural"
        );
    }

    #[test]
    fn no_section_is_ever_rendered_empty() {
        let sections = summary_sections(recording_notes(vec![section(
            "decisions",
            vec![note("Ship on Friday.", 9)],
        )]));

        assert_eq!(sections.len(), 1, "one supported section renders one");
        assert!(
            sections.iter().all(|section| !section.claims.is_empty()),
            "a heading with nothing under it must never be produced"
        );
    }

    #[test]
    fn every_claim_carries_the_evidence_a_chip_is_built_from() {
        let sections = summary_sections(recording_notes(vec![
            section("overview", vec![note("Planning.", 1)]),
            section("action_items", vec![action("Checklist.", 2)]),
        ]));

        for section in &sections {
            for claim in &section.claims {
                assert!(
                    !claim.meeting.is_empty() || !claim.external.is_empty(),
                    "every claim must carry evidence to cite: {}",
                    claim.text
                );
            }
        }
        let action_claim = &sections[1].claims[0];
        assert_eq!(
            action_claim.lead.as_deref(),
            Some("Dana"),
            "an attributed action keeps its owner"
        );
        assert_eq!(
            action_claim.meeting.len(),
            1,
            "citations repeated across text, owner and due date collapse to one chip"
        );
    }

    #[test]
    fn a_citation_chip_reads_as_the_moment_it_lands_on() {
        let mut times = CitationTimes::new();
        times.insert(EventId::new(7), Duration::from_secs(761));
        times.insert(EventId::new(8), Duration::from_secs(4_360));

        assert_eq!(
            moment_label(EventId::new(7), &times),
            "12:41",
            "a known moment reads as its timecode"
        );
        assert_eq!(
            moment_label(EventId::new(8), &times),
            "1:12:40",
            "a long recording keeps its hour"
        );
        assert_eq!(
            moment_label(EventId::new(9), &times),
            "#9",
            "an unknown moment still names the row it jumps to"
        );
        assert_eq!(
            timecode(Duration::ZERO),
            "00:00",
            "the start of a recording is a valid moment"
        );
    }

    #[test]
    fn evidence_is_hidden_until_a_reader_asks_for_all_of_it() {
        let sections = summary_sections(recording_notes(vec![
            section("overview", vec![note("Sprint 41 planning.", 1)]),
            section("decisions", vec![note("Search rewrite deferred.", 2)]),
        ]));
        let summary = summary_fingerprint(&sections);
        let mut disclosure = EvidenceDisclosure::default();

        assert!(
            !disclosure.revealed(summary).all,
            "a summary reads as prose before a reader asks for anything"
        );

        disclosure.toggle_all(summary);
        assert!(
            disclosure.revealed(summary).all,
            "one control reveals every claim's evidence at once"
        );

        disclosure.toggle_all(summary);
        assert!(
            !disclosure.revealed(summary).all,
            "toggling back returns the whole summary to prose"
        );
    }

    #[test]
    fn evidence_choices_do_not_carry_over_to_a_different_summary() {
        let first = summary_fingerprint(&summary_sections(recording_notes(vec![section(
            "overview",
            vec![note("Sprint 41 planning.", 1)],
        )])));
        let second = summary_fingerprint(&summary_sections(recording_notes(vec![section(
            "overview",
            vec![note("A lecture on training dynamics.", 1)],
        )])));
        assert_ne!(
            first, second,
            "two different summaries must not share one identity"
        );

        let mut disclosure = EvidenceDisclosure::default();
        disclosure.toggle_all(first);
        assert!(
            disclosure.revealed(first).all,
            "the summary the reader opened stays open"
        );
        assert!(
            !disclosure.revealed(second).all,
            "a re-summarize starts quiet rather than inheriting the last summary's choice"
        );
    }

    #[test]
    fn record_text_is_rendered_as_text_rather_than_as_markup() {
        assert_eq!(
            as_literal_markdown("- ship *now*"),
            r"\- ship \*now\*",
            "prose that happens to look like markup keeps its own characters"
        );
        assert_eq!(
            as_literal_markdown("Sprint 41 planning"),
            "Sprint 41 planning",
            "ordinary prose is passed through untouched"
        );
    }

    #[test]
    fn the_sources_block_is_one_quiet_line_until_a_source_exists()
    -> Result<(), Box<dyn std::error::Error>> {
        let empty = SourceContext::resolve(false, None);
        let SourceContext::Quiet(line) = empty else {
            return Err(std::io::Error::other(
                "with nothing configured the block must be one quiet line, not a policy essay",
            )
            .into());
        };
        assert!(
            !line.contains("read-only"),
            "a capability nobody is using is not explained at length: {line}"
        );
        assert!(
            line.contains("Settings"),
            "the quiet line still says where a source would be added: {line}"
        );

        let configured = SourceContext::resolve(true, None);
        let SourceContext::Configured { policy, receipt } = configured else {
            return Err(std::io::Error::other(
                "with a source configured the policy governs live controls and must be stated",
            )
            .into());
        };
        assert!(
            policy.contains("read-only"),
            "the disclosure appears where the reader can act on it: {policy}"
        );
        assert!(
            receipt.contains("Select a recording"),
            "the retrieval receipt still reports its own state: {receipt}"
        );
        Ok(())
    }

    #[test]
    fn the_pending_state_says_what_summarizing_will_do() {
        let view = SummaryView::resolve(NotesState::Disabled, false);

        assert_eq!(view.meta, "no notes yet", "the head states the state");
        let pending = view
            .pending
            .unwrap_or_else(|| "no pending copy was produced".to_owned());
        assert!(
            pending.contains("only what this recording supports"),
            "the pending state explains adaptivity: {pending}"
        );
        assert!(
            pending.contains("moment on every claim"),
            "the pending state promises citations: {pending}"
        );
        assert!(
            view.sections.is_empty(),
            "nothing may be rendered as a summary before one exists"
        );
    }

    #[test]
    fn a_live_recording_is_told_the_summary_comes_after_it_stops() {
        let view = SummaryView::resolve(NotesState::NoMeeting, true);

        assert_eq!(view.meta, "Notes after you stop", "the head is live");
        assert!(
            view.pending
                .unwrap_or_default()
                .contains("written after you stop"),
            "a live recording must not be told a summary is missing"
        );
    }

    #[test]
    fn a_failed_summary_names_its_cause() {
        let view = SummaryView::resolve(
            NotesState::Failed("the Summarizer backend refused the request".to_owned()),
            false,
        );

        let caution = view
            .caution
            .unwrap_or_else(|| "no cause was named".to_owned());
        assert!(
            caution.contains("the Summarizer backend refused the request"),
            "the failure must name its cause: {caution}"
        );
        assert!(
            view.sections.is_empty(),
            "a failed run renders no sections at all"
        );
    }

    #[test]
    fn a_stale_summary_says_what_is_wrong_with_it_and_still_renders()
    -> Result<(), Box<dyn std::error::Error>> {
        let view = SummaryView::resolve(
            NotesState::Stale {
                notes: Box::new(recording_notes(vec![section(
                    "decisions",
                    vec![note("Ship on Friday.", 1)],
                )])),
                document: Box::new(document(recording_notes(vec![section(
                    "decisions",
                    vec![note("Ship on Friday.", 1)],
                )]))?),
                bundle: mcp::ContextBundle::empty(),
                source_status: insight::SourceStatus::NotSelected,
                model: "gpt-5.4-codex".to_owned(),
                normalizations: Vec::new(),
                screen_consultations: Vec::new(),
            },
            false,
        );

        assert_eq!(view.sections.len(), 1, "a stale summary is still readable");
        assert!(
            view.caution.unwrap_or_default().contains("from before"),
            "a stale summary must say why it is stale"
        );
        assert!(
            view.provenance
                .iter()
                .any(|line| line.contains("gpt-5.4-codex")),
            "the model that produced the summary is named: {:?}",
            view.provenance
        );
        Ok(())
    }

    #[test]
    fn a_fresh_summary_names_its_model_and_its_source_footing()
    -> Result<(), Box<dyn std::error::Error>> {
        let view = SummaryView::resolve(
            NotesState::Ready {
                screen_consultations: Vec::new(),
                notes: Box::new(recording_notes(vec![section(
                    "overview",
                    vec![note("Planning.", 1)],
                )])),
                document: Box::new(document(recording_notes(vec![section(
                    "overview",
                    vec![note("Planning.", 1)],
                )]))?),
                bundle: mcp::ContextBundle::empty(),
                source_status: insight::SourceStatus::Unavailable,
                cached: false,
                model: "gpt-5.4-codex".to_owned(),
                normalizations: Vec::new(),
            },
            false,
        );

        assert_eq!(
            view.provenance,
            vec![
                "Written just now · gpt-5.4-codex".to_owned(),
                "Sources: unavailable; the summary fell back to this recording alone.".to_owned(),
            ],
            "the column states who wrote the summary and on what footing"
        );
        Ok(())
    }

    #[test]
    fn a_fresh_summary_names_each_lost_control_once() -> Result<(), Box<dyn std::error::Error>> {
        let backend_id = BackendId::new("codex-cli")?;
        let observation = |dispatch_id, control| ObservedRequestNormalization {
            dispatch_id,
            normalization: RequestNormalization {
                backend_id: backend_id.clone(),
                control,
            },
        };

        let view = SummaryView::resolve(
            NotesState::Ready {
                screen_consultations: Vec::new(),
                notes: Box::new(recording_notes(vec![section(
                    "overview",
                    vec![note("Planning.", 1)],
                )])),
                document: Box::new(document(recording_notes(vec![section(
                    "overview",
                    vec![note("Planning.", 1)],
                )]))?),
                bundle: mcp::ContextBundle::empty(),
                source_status: insight::SourceStatus::NotSelected,
                cached: false,
                model: "gpt-5.4-codex".to_owned(),
                normalizations: vec![
                    observation(1, SamplingControl::Temperature),
                    observation(2, SamplingControl::MaxTokens),
                    observation(3, SamplingControl::Temperature),
                    observation(4, SamplingControl::JsonObjectOutput),
                ],
            },
            false,
        );

        assert_eq!(
            view.caution, None,
            "a successful downgrade is not a failure"
        );
        assert!(
            view.provenance.iter().any(|line| {
                line.contains("JSON mode, a token cap, or temperature")
                    && line.contains("still written from the reply")
            }),
            "map/reduce dispatches must not repeat the same lost control: {:?}",
            view.provenance
        );
        Ok(())
    }

    #[test]
    fn a_summary_with_no_supported_section_says_so_instead_of_counting_zero()
    -> Result<(), Box<dyn std::error::Error>> {
        let view = SummaryView::resolve(
            NotesState::Ready {
                screen_consultations: Vec::new(),
                notes: Box::new(RecordingNotes::default()),
                document: Box::new(document(RecordingNotes::default())?),
                bundle: mcp::ContextBundle::empty(),
                source_status: insight::SourceStatus::NotSelected,
                cached: false,
                model: "gpt-5.4-codex".to_owned(),
                normalizations: Vec::new(),
            },
            false,
        );

        assert_eq!(
            view.meta, "no sections supported",
            "an empty summary never reports a section count of zero"
        );
        assert!(
            view.provenance
                .iter()
                .any(|line| line.contains("gpt-5.4-codex")),
            "an empty result still names the model that produced it"
        );
        Ok(())
    }

    #[test]
    fn empty_notes_copy_is_honest() {
        let view = SummaryView::resolve(NotesState::Disabled, false);
        assert!(
            view.pending.unwrap_or_default().contains("No notes yet"),
            "the empty state must not imply a summary exists"
        );
    }

    #[test]
    fn annotation_projection_keeps_only_the_active_edit_at_its_anchor()
    -> Result<(), Box<dyn std::error::Error>> {
        let mut timeline = TimelineBuilder::new(Session::new(SessionId::new(51), target(), 0));
        let anchor = timeline.append(
            Duration::ZERO,
            EventPayload::UtteranceFinal(Utterance {
                source: Source::Mic,
                start: Duration::ZERO,
                end: Duration::from_secs(1),
                text: "Anchor".to_owned(),
                avg_logprob: -0.1,
                annotations: vec![],
            }),
        );
        let first = timeline.append_user_annotation(
            Duration::from_secs(1),
            anchor.id(),
            "First",
            MarkKind::Note,
        )?;
        let edit = timeline.supersede_user_annotation(
            Duration::from_secs(2),
            "Edited",
            MarkKind::Important,
            first.id(),
        )?;

        let annotations = active_annotations(timeline.events());
        assert_eq!(annotations.len(), 1, "one active version per typed note");
        assert_eq!(annotations[0].event_id, edit.id(), "the edit is active");
        assert_eq!(annotations[0].text, "Edited", "the edit's text is shown");
        assert_eq!(
            annotations_by_anchor(timeline.events())
                .get(&anchor.id())
                .map(Vec::len),
            Some(1),
            "the note stays pinned under its anchor"
        );
        assert_eq!(
            latest_anchor(timeline.events()),
            Some(anchor.id()),
            "the anchor is the latest transcript row"
        );
        Ok(())
    }

    #[test]
    fn composer_anchor_waits_for_a_transcript_row() {
        let mut timeline = TimelineBuilder::new(Session::new(SessionId::new(52), target(), 0));
        timeline.append(
            Duration::ZERO,
            EventPayload::Vad(VadSegment {
                source: Source::Mic,
                start: Duration::ZERO,
                end: None,
                kind: SpeechState::SpeechStart,
            }),
        );
        assert_eq!(
            latest_anchor(timeline.events()),
            None,
            "speech detection alone is not a transcript row"
        );
    }

    #[test]
    fn the_composer_is_labelled_with_the_moment_it_will_attach_to() {
        let mut times = CitationTimes::new();
        times.insert(EventId::new(4), Duration::from_secs(125));
        times.insert(EventId::new(9), Duration::from_secs(1_802));

        assert_eq!(
            composer_anchor_label(Some(EventId::new(4)), Some(EventId::new(9)), &times),
            "Attaches at 02:05"
        );
        assert_eq!(
            composer_anchor_label(None, Some(EventId::new(9)), &times),
            "Attaches at 30:02"
        );
        assert_eq!(
            composer_anchor_label(None, None, &times),
            "Attaches once this recording has its first transcript row"
        );
    }

    #[test]
    fn post_meeting_default_anchor_ignores_a_trailing_partial() {
        let mut timeline = TimelineBuilder::new(Session::new(SessionId::new(54), target(), 0));
        let final_row = timeline.append(
            Duration::ZERO,
            EventPayload::UtteranceFinal(Utterance {
                source: Source::Mic,
                start: Duration::ZERO,
                end: Duration::from_secs(1),
                text: "Final".to_owned(),
                avg_logprob: -0.1,
                annotations: vec![],
            }),
        );
        timeline.append(
            Duration::from_secs(1),
            EventPayload::UtterancePartial(Utterance {
                source: Source::Mic,
                start: Duration::from_secs(1),
                end: Duration::from_secs(2),
                text: "Draft".to_owned(),
                avg_logprob: -0.1,
                annotations: vec![],
            }),
        );
        assert_eq!(
            last_final_anchor(timeline.events()),
            Some(final_row.id()),
            "a stopped recording anchors on its last settled row"
        );
    }

    #[test]
    fn partial_annotation_follows_supersession_chain_into_final_row()
    -> Result<(), Box<dyn std::error::Error>> {
        let mut timeline = TimelineBuilder::new(Session::new(SessionId::new(53), target(), 0));
        let first_partial = timeline.append(
            Duration::from_millis(500),
            EventPayload::UtterancePartial(Utterance {
                source: Source::System,
                start: Duration::ZERO,
                end: Duration::from_millis(500),
                text: "we should".to_owned(),
                avg_logprob: -0.1,
                annotations: vec![],
            }),
        );
        let annotation = timeline.append_user_annotation(
            Duration::from_millis(600),
            first_partial.id(),
            "Remember this",
            MarkKind::Important,
        )?;
        let second_partial = timeline.supersede(
            Duration::from_secs(1),
            EventPayload::UtterancePartial(Utterance {
                source: Source::System,
                start: Duration::ZERO,
                end: Duration::from_secs(1),
                text: "we should ship".to_owned(),
                avg_logprob: -0.1,
                annotations: vec![],
            }),
            &first_partial,
        )?;

        assert_eq!(
            resolve_transcript_anchor(timeline.events(), first_partial.id()),
            Some(second_partial.id()),
            "the anchor follows the supersession chain"
        );
        assert_eq!(
            annotations_by_anchor(timeline.events())
                .get(&second_partial.id())
                .and_then(|values| values.first())
                .map(|value| (value.event_id, value.anchor)),
            Some((annotation.id(), first_partial.id())),
            "the note keeps its original anchor for audit"
        );

        let final_event = timeline.supersede(
            Duration::from_millis(1_500),
            EventPayload::UtteranceFinal(Utterance {
                source: Source::System,
                start: Duration::ZERO,
                end: Duration::from_millis(1_500),
                text: "we should ship Friday".to_owned(),
                avg_logprob: -0.1,
                annotations: vec![],
            }),
            &second_partial,
        )?;

        assert_eq!(
            resolve_transcript_anchor(timeline.events(), first_partial.id()),
            Some(final_event.id()),
            "the anchor settles on the final row"
        );
        assert_eq!(
            annotations_by_anchor(timeline.events())
                .get(&final_event.id())
                .and_then(|values| values.first())
                .map(|value| (value.event_id, value.anchor)),
            Some((annotation.id(), first_partial.id())),
            "the note is presented under the settled row"
        );
        Ok(())
    }

    /// The column must be built as a real render tree, not merely as strings.
    ///
    /// Five workspace tasks passed their suites while the app aborted on launch, because no test
    /// built one. This one persists a summary, opens the workspace over it, and reads back the
    /// bounds of every control the column draws.
    mod rendered {
        use std::{sync::Arc, time::Duration};

        use futures_util::stream;
        use gpui::{AppContext as _, Entity, Modifiers, TestAppContext, px, size};
        use gpui_component::input::InputEvent;
        use insight::{
            MeetingNotesGenerator, NotesBlockProvenance, NotesOverlayOperation, OverlayTarget,
            RecordingNotesSectionKind, append_notes_overlay_operation, load_latest_grounded_notes,
        };
        use providers::{
            AuthKind, AuthStatus, BackendCapabilities, BackendDescriptor, BackendFingerprint,
            BackendId,
        };
        use rag::Store;
        use secrecy::SecretString;
        use sotto_core::{
            BoxFuture, BoxStream, CancellationToken, CaptureTarget, CompletionProvider,
            CompletionRequest, Delta, EventId, EventPayload, ProviderError, Session, SessionId,
            Source, StopReason, TargetKind, TimelineBuilder, Usage, Utterance,
        };

        use crate::{mcp, notes::NotesState, reasoning, session};

        /// The width the shipped window refuses to go below.
        const MIN_WORKSPACE_WIDTH: gpui::Pixels = px(680.0);

        struct NoOpenAiCredentials;

        impl reasoning::OpenAiCredentialStore for NoOpenAiCredentials {
            fn store(&self, _: &SecretString) -> Result<(), ProviderError> {
                Ok(())
            }

            fn load(&self) -> Result<Option<SecretString>, ProviderError> {
                Ok(None)
            }

            fn delete(&self) -> Result<(), ProviderError> {
                Ok(())
            }
        }

        struct NoMcpCredentials;

        impl mcp::McpCredentialStore for NoMcpCredentials {
            fn store(
                &self,
                _: &::mcp::ServerId,
                _: &::mcp::HttpEndpoint,
                _: &SecretString,
            ) -> Result<(), mcp::McpUiError> {
                Ok(())
            }

            fn load(
                &self,
                _: &::mcp::ServerId,
                _: &::mcp::HttpEndpoint,
            ) -> Result<Option<SecretString>, mcp::McpUiError> {
                Ok(None)
            }

            fn delete(
                &self,
                _: &::mcp::ServerId,
                _: &::mcp::HttpEndpoint,
            ) -> Result<(), mcp::McpUiError> {
                Ok(())
            }
        }

        /// Replays one prepared summary so the persisted artifact is real, not hand-written JSON.
        struct ReplayProvider(String);

        impl CompletionProvider for ReplayProvider {
            fn stream(
                &self,
                _request: CompletionRequest,
                _cancellation: CancellationToken,
            ) -> BoxFuture<
                '_,
                Result<BoxStream<'static, Result<Delta, ProviderError>>, ProviderError>,
            > {
                let text = self.0.clone();
                Box::pin(async move {
                    Ok(Box::pin(stream::iter([Ok(Delta {
                        text,
                        is_final: true,
                        usage: Some(Usage::default()),
                        stop_reason: Some(StopReason::EndTurn),
                    })])) as BoxStream<'static, _>)
                })
            }

            fn model_id(&self) -> &str {
                "replay-model"
            }
        }

        fn fingerprint() -> Result<BackendFingerprint, Box<dyn std::error::Error>> {
            Ok(BackendDescriptor::new(
                BackendId::new("test.replay")?,
                "Replay",
                "replay-model",
                1,
                BackendCapabilities::reasoning_baseline(),
                AuthKind::None,
                AuthStatus::Ready,
            )?
            .fingerprint()
            .clone())
        }

        /// Persists a stopped recording with three transcript rows and no summary.
        async fn persist_recording(
            database: &std::path::Path,
        ) -> Result<Vec<EventId>, Box<dyn std::error::Error>> {
            let store = Store::open(database).await?;
            let session_id = SessionId::new(41);
            let mut record = Session::new(
                session_id,
                CaptureTarget {
                    bundle_id: Some("us.zoom.xos".to_owned()),
                    display_name: "Zoom".to_owned(),
                    window_title: Some("Sprint 41 planning".to_owned()),
                    kind: TargetKind::Window,
                    audio_scoped: true,
                },
                1,
            );
            record.end(2);
            store.save_session(&record).await?;
            let mut timeline = TimelineBuilder::new(record);
            let mut ids = Vec::new();
            for (index, text) in [
                "Carry-over first: the payments retry work slipped because staging was down.",
                "Then we're agreed — the search rewrite waits until 42.",
                "I'll own the retry rollout checklist and have it reviewed by Thursday.",
            ]
            .into_iter()
            .enumerate()
            {
                let step = u64::try_from(index)?.saturating_add(1);
                let offset = Duration::from_secs(120_u64.saturating_mul(step));
                ids.push(
                    timeline
                        .append(
                            offset,
                            EventPayload::UtteranceFinal(Utterance {
                                source: Source::System,
                                start: offset,
                                end: offset + Duration::from_secs(4),
                                text: text.to_owned(),
                                avg_logprob: -0.1,
                                annotations: Vec::new(),
                            }),
                        )
                        .id(),
                );
            }
            store.append_events(timeline.events()).await?;
            Ok(ids)
        }

        /// Persists a stopped recording whose summary has three sections and four citations.
        async fn recording_with_summary(
            database: &std::path::Path,
        ) -> Result<EventId, Box<dyn std::error::Error>> {
            let ids = persist_recording(database).await?;
            let store = Store::open(database).await?;
            let artifact = format!(
                r#"{{"sections":[{{"kind":"overview","blocks":[{{"type":"claim","text":"Sprint 41 is scoped to payment retries and audit fixes after the staging outage pushed the retry work into the following sprint.","meeting_citations":[{first},{second}],"external_citations":[]}}]}},{{"kind":"decisions","blocks":[{{"type":"claim","text":"The search rewrite is deferred to sprint 42.","meeting_citations":[{second}],"external_citations":[]}}]}},{{"kind":"action_items","blocks":[{{"type":"action","text":"Retry rollout checklist, reviewed by Thursday.","meeting_citations":[{third}],"external_citations":[],"owner":"Dana","owner_meeting_citations":[{third}],"owner_external_citations":[],"due_date":"Thursday","due_date_meeting_citations":[{third}],"due_date_external_citations":[]}}]}}]}}"#,
                first = ids[0].get(),
                second = ids[1].get(),
                third = ids[2].get(),
            );
            MeetingNotesGenerator::new(&store, Arc::new(ReplayProvider(artifact)))
                .with_backend_fingerprint(fingerprint()?)
                .generate_grounded_with_cancellation(
                    SessionId::new(41),
                    None,
                    CancellationToken::new(),
                )
                .await?;
            Ok(ids[1])
        }

        /// Opens the workspace over the persisted summary at the narrowest supported width.
        fn open_summarized_workspace<'window>(
            cx: &'window mut TestAppContext,
            dir: &std::path::Path,
            database: std::path::PathBuf,
        ) -> (
            Entity<crate::workspace::MeetingWorkspace>,
            &'window mut gpui::VisualTestContext,
        ) {
            cx.update(gpui_component::init);
            let reasoning_path = dir.join("reasoning.json");
            let mcp_path = dir.join("mcp.json");
            let (ingress, timeline) = cx.update(|cx| crate::devwindow::attach_ingress(cx, 16));
            let session = cx.new(|_| session::SessionController::new(ingress));
            let reasoning = cx.new(|_| {
                reasoning::ReasoningController::load(reasoning_path, Arc::new(NoOpenAiCredentials))
            });
            let mcp_controller =
                cx.new(|_| mcp::McpController::load(Some(mcp_path), Arc::new(NoMcpCredentials)));
            cx.add_window_view(move |window, cx| {
                crate::workspace::MeetingWorkspace::new(
                    database,
                    timeline,
                    reasoning,
                    session,
                    mcp_controller,
                    window,
                    cx,
                )
            })
        }

        /// Asserts every named control renders wholly inside the notes column.
        fn assert_in_column(
            visual: &mut gpui::VisualTestContext,
            selectors: &[&'static str],
        ) -> Result<(), Box<dyn std::error::Error>> {
            for selector in selectors {
                let column = visual
                    .debug_bounds("notes-column")
                    .ok_or_else(|| std::io::Error::other("the summary column must render"))?;
                let bounds = visual.debug_bounds(selector).ok_or_else(|| {
                    std::io::Error::other(format!("{selector} must render in the column"))
                })?;
                assert!(
                    bounds.size.width > px(0.0),
                    "{selector} must keep a visible width"
                );
                assert!(
                    bounds.left() >= column.left() && bounds.right() <= column.right(),
                    "{selector} must stay wholly inside the column at the minimum window width"
                );
            }
            Ok(())
        }

        fn click_control(
            visual: &mut gpui::VisualTestContext,
            selector: &'static str,
        ) -> Result<(), Box<dyn std::error::Error>> {
            let bounds = visual.debug_bounds(selector).ok_or_else(|| {
                std::io::Error::other(format!("{selector} must be reachable in the mounted tree"))
            })?;
            visual.simulate_click(bounds.center(), Modifiers::none());
            visual.run_until_parked();
            visual.refresh()?;
            visual.run_until_parked();
            Ok(())
        }

        fn replace_composer_text(
            visual: &mut gpui::VisualTestContext,
            workspace: &Entity<crate::workspace::MeetingWorkspace>,
            value: &str,
        ) {
            let value = value.to_owned();
            visual.update(|window, cx| {
                workspace.update(cx, |this, cx| {
                    this.annotation_input
                        .update(cx, |input, cx| input.set_value(value.clone(), window, cx));
                });
            });
        }

        fn copy_claim(
            visual: &mut gpui::VisualTestContext,
            selector: &'static str,
        ) -> Result<String, Box<dyn std::error::Error>> {
            visual.executor().advance_clock(Duration::from_millis(500));
            visual.run_until_parked();
            visual.refresh()?;
            visual.run_until_parked();
            let claim = visual.debug_bounds(selector).ok_or_else(|| {
                std::io::Error::other(format!("{selector} must render before it can be copied"))
            })?;
            let start = gpui::point(claim.left() + px(2.0), claim.top() + px(4.0));
            let end = gpui::point(claim.right() - px(2.0), claim.bottom() - px(4.0));
            visual.simulate_mouse_down(start, gpui::MouseButton::Left, Modifiers::none());
            visual.simulate_mouse_move(end, gpui::MouseButton::Left, Modifiers::none());
            visual.simulate_mouse_up(end, gpui::MouseButton::Left, Modifiers::none());
            visual.run_until_parked();
            visual.simulate_keystrokes("cmd-c");
            visual.run_until_parked();
            Ok(visual
                .update(|_, cx| cx.read_from_clipboard())
                .and_then(|item| item.text())
                .unwrap_or_default())
        }

        #[tokio::test(flavor = "multi_thread")]
        async fn the_summary_reads_as_prose_and_gives_up_its_evidence_only_when_asked()
        -> Result<(), Box<dyn std::error::Error>> {
            let dir = tempfile::tempdir()?;
            let database = dir.path().join("sotto.sqlite3");
            let cited = recording_with_summary(&database).await?;

            let mut cx = TestAppContext::single();
            let (workspace, visual) = open_summarized_workspace(&mut cx, dir.path(), database);
            visual.simulate_resize(size(MIN_WORKSPACE_WIDTH, px(720.0)));
            // A stopped session must be open for the review stage to mount the notes column.
            visual.update(|_, cx| {
                workspace.update(cx, |this, cx| this.select_meeting(SessionId::new(41), cx));
            });
            visual.refresh()?;
            visual.run_until_parked();

            // The default is prose. Three claims, four citations between them, no chip drawn.
            for hidden in [
                "summary-citation-0-0",
                "summary-citation-0-1",
                "summary-citation-1-0",
                "summary-citation-2-0",
            ] {
                assert!(
                    visual.debug_bounds(hidden).is_none(),
                    "{hidden} must stay hidden until a reader asks for it"
                );
            }
            assert_in_column(
                visual,
                &[
                    "notes-head-row",
                    "summarize-control",
                    "summary-area",
                    "notes-composer",
                    "append-note-control",
                    "summary-evidence-toggle",
                    "summary-claim-0",
                    "summary-claim-1",
                    "summary-claim-2",
                    // No MCP source is configured in this fixture, so the block is one quiet line.
                    "source-context-quiet",
                ],
            )?;
            assert!(
                visual.debug_bounds("your-notes-block").is_none(),
                "the retired post-close composer block must not sit above the notes document"
            );
            assert!(
                visual.debug_bounds("summary-pending").is_none(),
                "a summarized recording must not also render the pending state"
            );
            assert!(
                visual.debug_bounds("source-context-block").is_none(),
                "with nothing configured the sources policy must not be drawn over an empty list"
            );

            // There is deliberately no per-claim control. One existed briefly and was removed:
            // a compact affordance under every claim is a line of chrome under every claim, and a
            // summary meant to read as prose was reading as a form.
            for absent in [
                "summary-evidence-0",
                "summary-evidence-1",
                "summary-evidence-2",
            ] {
                assert!(
                    visual.debug_bounds(absent).is_none(),
                    "{absent} must not render: evidence is one choice for the whole summary"
                );
            }

            // The one control reveals every claim's evidence at once.
            let toggle = visual
                .debug_bounds("summary-evidence-toggle")
                .ok_or_else(|| std::io::Error::other("the summary evidence toggle must render"))?;
            visual.simulate_click(toggle.center(), Modifiers::none());
            visual.run_until_parked();
            assert_in_column(
                visual,
                &[
                    "summary-citation-0-0",
                    "summary-citation-0-1",
                    "summary-citation-1-0",
                    "summary-citation-2-0",
                ],
            )?;

            // Every revealed chip still resolves to the transcript row it cites.
            let chip = visual.debug_bounds("summary-citation-0-1").ok_or_else(|| {
                std::io::Error::other("the overview's second citation chip must render")
            })?;
            visual.simulate_click(chip.center(), Modifiers::none());
            visual.run_until_parked();
            assert_eq!(
                visual.update(|_, cx| workspace.read(cx).focused_event),
                Some(cited),
                "following a citation must land on the transcript row it cites"
            );
            Ok(())
        }

        #[tokio::test(flavor = "multi_thread")]
        async fn the_mounted_document_distinguishes_authors_and_its_reader_belief_controls_work()
        -> Result<(), Box<dyn std::error::Error>> {
            let dir = tempfile::tempdir()?;
            let database = dir.path().join("sotto.sqlite3");
            let _ = recording_with_summary(&database).await?;

            let mut cx = TestAppContext::single();
            let (workspace, visual) = open_summarized_workspace(&mut cx, dir.path(), database);
            visual.simulate_resize(size(px(900.0), px(820.0)));
            visual.update(|_, cx| {
                workspace.update(cx, |this, cx| this.select_meeting(SessionId::new(41), cx));
            });
            visual.refresh()?;
            visual.run_until_parked();

            let original_artifact = visual.update(|_, cx| {
                let notes = workspace.read(cx).notes.clone();
                let NotesState::Ready { notes, .. } = notes.read(cx).snapshot().state else {
                    return Err::<Vec<u8>, Box<dyn std::error::Error>>(
                        std::io::Error::other("the persisted summary must open ready").into(),
                    );
                };
                Ok(serde_json::to_vec(&notes)?)
            })?;

            // Reword a generated claim through Edit -> composer -> Save.
            let claim = visual
                .debug_bounds("summary-claim-0")
                .ok_or_else(|| std::io::Error::other("the generated overview must render"))?;
            visual.simulate_mouse_move(claim.center(), None, Modifiers::none());
            visual.run_until_parked();
            click_control(visual, "notes-edit-0")?;
            assert!(
                visual.update(|_, cx| workspace.read(cx).editing_notes_block.is_some()),
                "Edit must put the mounted composer into block-edit mode"
            );
            replace_composer_text(visual, &workspace, "User's exact overview wording.");
            click_control(visual, "append-note-control")?;

            let composed_reword = visual.update(|_, cx| {
                let notes = workspace.read(cx).notes.clone();
                let NotesState::Ready { document, .. } = notes.read(cx).snapshot().state else {
                    return Err::<String, Box<dyn std::error::Error>>(
                        std::io::Error::other("reworded notes must remain ready").into(),
                    );
                };
                Ok(document.blocks[0].text.clone())
            })?;
            assert_eq!(
                composed_reword, "User's exact overview wording.",
                "Save must apply the edit to the composed document before it renders"
            );
            let copied = copy_claim(visual, "summary-claim-0")?;
            assert!(
                copied.contains("User's exact overview wording."),
                "the reworded user's text must replace the generated text on screen, got {copied:?}"
            );

            // Check is its own mounted control and changes the presented action, not the artifact.
            click_control(visual, "notes-check-2")?;

            // With no edit active the same composer adds a user-authored overview block.
            replace_composer_text(visual, &workspace, "A note written entirely by the user.");
            click_control(visual, "append-note-control")?;

            assert_in_column(
                visual,
                &[
                    "summary-provenance-edited-0",
                    "summary-provenance-generated-1",
                    "summary-provenance-user-3",
                    "summary-claim-3",
                ],
            )?;
            let copied = copy_claim(visual, "summary-claim-3")?;
            assert!(
                copied.contains("A note written entirely by the user."),
                "the newly added user's words must reach the screen, got {copied:?}"
            );
            click_control(visual, "summary-evidence-toggle")?;
            assert!(
                visual.debug_bounds("summary-citation-3-0").is_none(),
                "the user block must never draw a citation chip or imply model evidence"
            );

            let state = visual.update(|_, cx| {
                let notes = workspace.read(cx).notes.clone();
                notes.read(cx).snapshot().state
            });
            let NotesState::Ready {
                notes, document, ..
            } = state
            else {
                return Err(std::io::Error::other("edited notes must remain ready").into());
            };
            assert_eq!(
                serde_json::to_vec(&notes)?,
                original_artifact,
                "overlay controls must leave the stored generated artifact byte-unchanged"
            );
            let edited = document
                .blocks
                .iter()
                .find(|block| block.text == "User's exact overview wording.")
                .ok_or_else(|| std::io::Error::other("the reworded claim must remain composed"))?;
            assert_eq!(edited.provenance, NotesBlockProvenance::EditedFromDraft);
            let action = document
                .blocks
                .iter()
                .find(|block| block.text == "Retry rollout checklist, reviewed by Thursday.")
                .ok_or_else(|| std::io::Error::other("the checked action must remain composed"))?;
            assert!(action.checked, "Check must check the mounted action");
            let added = document
                .blocks
                .iter()
                .find(|block| block.text == "A note written entirely by the user.")
                .ok_or_else(|| std::io::Error::other("the added block must remain composed"))?;
            assert_eq!(added.provenance, NotesBlockProvenance::UserAuthored);
            assert!(added.meeting_citations.is_empty());
            assert!(added.external_citations.is_empty());

            // Bounds persist across frames in this harness, so hiding is asserted against the
            // freshly composed state rather than through a misleading disappearance assertion.
            let decision = visual
                .debug_bounds("summary-claim-1")
                .ok_or_else(|| std::io::Error::other("the generated decision must render"))?;
            visual.simulate_mouse_move(decision.center(), None, Modifiers::none());
            visual.run_until_parked();
            click_control(visual, "notes-hide-1")?;
            let state = visual.update(|_, cx| {
                let notes = workspace.read(cx).notes.clone();
                notes.read(cx).snapshot().state
            });
            let NotesState::Ready { document, .. } = state else {
                return Err(std::io::Error::other("hidden notes must remain ready").into());
            };
            assert!(
                document
                    .blocks
                    .iter()
                    .all(|block| block.text != "The search rewrite is deferred to sprint 42."),
                "Hide must remove the selected generated decision from the composed document"
            );
            Ok(())
        }

        /// Return still submits a typed note where the composer is not editing the document.
        ///
        /// Making the composer multi-line gave Return a second meaning, and a newline is the wrong
        /// one on a stopped recording that has no summary: there is no block to edit, so the only
        /// thing the composer can be holding is a note. Pinned because nothing else would notice
        /// Return quietly turning into a line break on that surface.
        #[tokio::test(flavor = "multi_thread")]
        async fn return_still_appends_a_typed_note_when_no_summary_owns_the_composer()
        -> Result<(), Box<dyn std::error::Error>> {
            let dir = tempfile::tempdir()?;
            let database = dir.path().join("sotto.sqlite3");
            persist_recording(&database).await?;

            let mut cx = TestAppContext::single();
            let (workspace, visual) =
                open_summarized_workspace(&mut cx, dir.path(), database.clone());
            visual.simulate_resize(size(px(900.0), px(820.0)));
            visual.update(|_, cx| {
                workspace.update(cx, |this, cx| this.select_meeting(SessionId::new(41), cx));
            });
            visual.refresh()?;
            visual.run_until_parked();

            assert!(
                !visual.update(|_, cx| workspace.read(cx).composer_edits_notes_document(cx)),
                "a recording with no summary has no document for the composer to edit"
            );
            replace_composer_text(visual, &workspace, "Chase the staging outage postmortem.");
            // The composer's own key handling belongs to `gpui-component` and is unchanged; what
            // this pins is the subscription that decides what plain Return means here. Focusing the
            // input instead would not reach it: a focused `TextElement` paints through
            // `Root::read`, and this window's first layer is the workspace rather than a `Root`.
            visual.update(|_, cx| {
                let input = workspace.read(cx).annotation_input.clone();
                input.update(cx, |_, cx| {
                    cx.emit(InputEvent::PressEnter { secondary: false });
                });
            });
            visual.run_until_parked();

            // Reopening the store is the proof: an in-memory message would say the same thing
            // whether or not the note reached disk.
            let reopened = Store::open(&database).await?;
            let typed = reopened
                .load_session(SessionId::new(41))
                .await?
                .into_iter()
                .filter_map(|event| match event.payload() {
                    EventPayload::UserAnnotation(annotation) => Some(annotation.text.clone()),
                    _ => None,
                })
                .collect::<Vec<_>>();
            assert_eq!(
                typed,
                vec!["Chase the staging outage postmortem.".to_owned()],
                "Return must append the typed note, not insert a line break into it"
            );
            Ok(())
        }

        #[tokio::test(flavor = "multi_thread")]
        async fn editing_an_action_preserves_its_text_owner_and_due_date_without_panicking()
        -> Result<(), Box<dyn std::error::Error>> {
            let dir = tempfile::tempdir()?;
            let database = dir.path().join("sotto.sqlite3");
            let _ = recording_with_summary(&database).await?;

            let mut cx = TestAppContext::single();
            let (workspace, visual) = open_summarized_workspace(&mut cx, dir.path(), database);
            visual.simulate_resize(size(px(900.0), px(820.0)));
            visual.update(|_, cx| {
                workspace.update(cx, |this, cx| this.select_meeting(SessionId::new(41), cx));
            });
            visual.refresh()?;
            visual.run_until_parked();

            let action = visual
                .debug_bounds("summary-claim-2")
                .ok_or_else(|| std::io::Error::other("the generated action must render"))?;
            visual.simulate_mouse_move(action.center(), None, Modifiers::none());
            visual.run_until_parked();
            click_control(visual, "notes-edit-2")?;
            let editable = visual.update(|_, cx| {
                workspace
                    .read(cx)
                    .annotation_input
                    .read(cx)
                    .value()
                    .to_string()
            });
            assert!(
                editable.contains("\nOwner:") && editable.contains("\nDue:"),
                "an action edit must reach a newline-safe composer with owner and due fields"
            );

            replace_composer_text(
                visual,
                &workspace,
                "Ship the revised rollout checklist.\nOwner: Priya\nDue: Friday",
            );
            click_control(visual, "append-note-control")?;

            let state = visual.update(|_, cx| {
                let notes = workspace.read(cx).notes.clone();
                notes.read(cx).snapshot().state
            });
            let NotesState::Ready { document, .. } = state else {
                return Err(std::io::Error::other("edited notes must remain ready").into());
            };
            let action = document
                .blocks
                .iter()
                .find(|block| block.text == "Ship the revised rollout checklist.")
                .ok_or_else(|| std::io::Error::other("the edited action must remain composed"))?;
            assert_eq!(action.owner.as_deref(), Some("Priya"));
            assert_eq!(action.due_date.as_deref(), Some("Friday"));
            Ok(())
        }

        #[tokio::test(flavor = "multi_thread")]
        async fn an_orphaned_reword_reaches_the_mounted_document_with_an_explicit_warning()
        -> Result<(), Box<dyn std::error::Error>> {
            let dir = tempfile::tempdir()?;
            let database = dir.path().join("sotto.sqlite3");
            let _ = recording_with_summary(&database).await?;
            let store = Store::open(&database).await?;
            let artifact = load_latest_grounded_notes(&store, SessionId::new(41))
                .await?
                .ok_or_else(|| std::io::Error::other("the summary artifact must persist"))?;
            let entry_id = store.entry_for_session(SessionId::new(41)).await?;
            append_notes_overlay_operation(
                &store,
                entry_id,
                &artifact.artifact,
                &NotesOverlayOperation::Reword {
                    target: OverlayTarget {
                        block_id: "a-generated-block-that-no-longer-exists".to_owned(),
                        section: RecordingNotesSectionKind::Overview,
                        action: false,
                        meeting_citations: vec![EventId::new(9_999)],
                        external_citations: Vec::new(),
                    },
                    text: "The user's preserved orphaned wording.".to_owned(),
                    owner: None,
                    due_date: None,
                },
                7,
            )
            .await?;
            drop(store);

            let mut cx = TestAppContext::single();
            let (workspace, visual) = open_summarized_workspace(&mut cx, dir.path(), database);
            visual.simulate_resize(size(px(900.0), px(820.0)));
            visual.update(|_, cx| {
                workspace.update(cx, |this, cx| this.select_meeting(SessionId::new(41), cx));
            });
            visual.refresh()?;
            visual.run_until_parked();

            assert_in_column(
                visual,
                &["summary-claim-3", "summary-provenance-orphaned-3"],
            )?;
            let copied = copy_claim(visual, "summary-claim-3")?;
            assert!(
                copied.contains("The user's preserved orphaned wording."),
                "the orphaned user wording must remain readable, got {copied:?}"
            );
            click_control(visual, "summary-evidence-toggle")?;
            assert!(
                visual.debug_bounds("summary-citation-3-0").is_none(),
                "an orphaned edit is user-authored and must not retain its old citation affordance"
            );
            let state = visual.update(|_, cx| {
                let notes = workspace.read(cx).notes.clone();
                notes.read(cx).snapshot().state
            });
            let NotesState::Ready { document, .. } = state else {
                return Err(std::io::Error::other("orphaned notes must remain ready").into());
            };
            let orphan = document
                .blocks
                .iter()
                .find(|block| block.orphaned)
                .ok_or_else(|| std::io::Error::other("the orphan flag must survive mounting"))?;
            assert_eq!(orphan.provenance, NotesBlockProvenance::UserAuthored);
            assert!(orphan.meeting_citations.is_empty());
            assert!(orphan.external_citations.is_empty());
            Ok(())
        }

        /// With a source configured, the policy governs live controls and is stated over them.
        #[tokio::test(flavor = "multi_thread")]
        async fn a_configured_source_states_its_policy_where_the_controls_are()
        -> Result<(), Box<dyn std::error::Error>> {
            let dir = tempfile::tempdir()?;
            let database = dir.path().join("sotto.sqlite3");
            let _ = recording_with_summary(&database).await?;
            std::fs::write(
                dir.path().join("mcp.json"),
                r#"{"version":1,"servers":[{"id":"project-docs","display_name":"Project docs","endpoint":"https://sources.example/mcp"}],"grants":[]}"#,
            )?;

            let mut cx = TestAppContext::single();
            let (workspace, visual) = open_summarized_workspace(&mut cx, dir.path(), database);
            visual.simulate_resize(size(MIN_WORKSPACE_WIDTH, px(720.0)));
            visual.update(|_, cx| {
                workspace.update(cx, |this, cx| this.select_meeting(SessionId::new(41), cx));
            });
            visual.refresh()?;
            visual.run_until_parked();

            assert!(
                visual.debug_bounds("source-context-quiet").is_none(),
                "with a source configured the quiet line gives way to the policy it replaces"
            );
            assert_in_column(visual, &["source-context-block"])?;
            Ok(())
        }

        /// Summary and typed-note text can be selected with the mouse and copied to the clipboard.
        ///
        /// The maintainer's complaint was that a line could not be lifted out of a summary. This
        /// drags across a rendered claim and presses the copy binding, then reads the real
        /// clipboard — a selectable flag asserted in isolation would prove nothing about whether
        /// the text is reachable in the tree the column actually builds.
        #[tokio::test(flavor = "multi_thread")]
        async fn a_summary_claim_can_be_selected_and_copied()
        -> Result<(), Box<dyn std::error::Error>> {
            let dir = tempfile::tempdir()?;
            let database = dir.path().join("sotto.sqlite3");
            let _ = recording_with_summary(&database).await?;

            let mut cx = TestAppContext::single();
            let (workspace, visual) = open_summarized_workspace(&mut cx, dir.path(), database);
            visual.simulate_resize(size(px(900.0), px(720.0)));
            visual.update(|_, cx| {
                workspace.update(cx, |this, cx| this.select_meeting(SessionId::new(41), cx));
            });
            visual.refresh()?;
            visual.run_until_parked();
            // The markdown parse is debounced off the render thread; let it land.
            visual.executor().advance_clock(Duration::from_millis(500));
            visual.run_until_parked();
            visual.refresh()?;
            visual.run_until_parked();

            let claim = visual
                .debug_bounds("summary-claim-0")
                .ok_or_else(|| std::io::Error::other("the first claim must render"))?;
            let start = gpui::point(claim.left() + px(2.0), claim.top() + px(4.0));
            let end = gpui::point(claim.right() - px(2.0), claim.bottom() - px(4.0));
            visual.simulate_mouse_down(start, gpui::MouseButton::Left, Modifiers::none());
            visual.simulate_mouse_move(end, gpui::MouseButton::Left, Modifiers::none());
            visual.simulate_mouse_up(end, gpui::MouseButton::Left, Modifiers::none());
            visual.run_until_parked();
            visual.simulate_keystrokes("cmd-c");
            visual.run_until_parked();

            let copied = visual
                .update(|_, cx| cx.read_from_clipboard())
                .and_then(|item| item.text())
                .unwrap_or_default();
            assert!(
                copied.contains("Sprint 41"),
                "a reader must be able to copy a line out of the summary, got {copied:?}"
            );
            Ok(())
        }

        /// The display toggle is not a licence to relax the evidence contract.
        ///
        /// T076 hides chips by default, which would be a quiet disaster if it also softened what a
        /// claim must carry. This runs the real generator over the real store twice: once with a
        /// claim that cites nothing, and once with a claim citing a transcript row that does not
        /// exist. Neither may become notes the column could draw.
        #[tokio::test(flavor = "multi_thread")]
        async fn an_unsupported_claim_still_fails_closed() -> Result<(), Box<dyn std::error::Error>>
        {
            let dir = tempfile::tempdir()?;
            let database = dir.path().join("sotto.sqlite3");
            let ids = persist_recording(&database).await?;
            let store = Store::open(&database).await?;
            let backend_fingerprint = fingerprint()?;

            let generate = |artifact: String| async {
                MeetingNotesGenerator::new(&store, Arc::new(ReplayProvider(artifact)))
                    .with_backend_fingerprint(backend_fingerprint.clone())
                    .generate_grounded_with_cancellation(
                        SessionId::new(41),
                        None,
                        CancellationToken::new(),
                    )
                    .await
            };

            let uncited = generate(claim_artifact("The team agreed to ship on Friday.", "")).await;
            let Err(uncited) = uncited else {
                return Err(std::io::Error::other(
                    "a claim carrying no evidence must never become notes",
                )
                .into());
            };
            assert!(
                matches!(
                    uncited,
                    insight::MeetingNotesError::MissingCitation { .. }
                        | insight::MeetingNotesError::InvalidEvidenceBasis { .. }
                ),
                "an uncited claim must be rejected as unevidenced, got {uncited}"
            );

            let phantom = ids
                .iter()
                .map(|id| id.get())
                .max()
                .unwrap_or_default()
                .saturating_add(500);
            let unknown = generate(claim_artifact(
                "The team agreed to ship on Friday.",
                &phantom.to_string(),
            ))
            .await;
            let Err(unknown) = unknown else {
                return Err(std::io::Error::other(
                    "a claim citing a row that does not exist must never become notes",
                )
                .into());
            };
            assert!(
                matches!(unknown, insight::MeetingNotesError::UnknownCitation { .. }),
                "every citation must resolve to a real transcript row, got {unknown}"
            );
            Ok(())
        }

        /// One overview claim with exactly the citation list given.
        fn claim_artifact(text: &str, citations: &str) -> String {
            format!(
                r#"{{"sections":[{{"kind":"overview","blocks":[{{"type":"claim","text":"{text}","meeting_citations":[{citations}],"external_citations":[]}}]}}]}}"#
            )
        }
    }
}
