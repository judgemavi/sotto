//! Virtualized transcript column: rows attributed to the source that produced them.
//!
//! ADR-0019 renamed the two channels. A row is `captured audio` or `your microphone` — the source
//! of the sound, not the occasion it belonged to — and a collapsing legend in the column head
//! explains the two dots. The column is also the receiving half of a summary citation: see
//! [`MeetingWorkspace::reveal_citation`].
//!
//! # Quoting the record
//!
//! A record you cannot quote is one you can only look at, so every line of spoken text is a
//! selectable [`TextView`] and `cmd-c` copies what the reader dragged across.
//!
//! **Selection does not span rows, and the column says so.** `TextView` owns one selection per
//! instance, and one instance per row is what keeps a row clickable, flashable and individually
//! anchored. Rather than leave a reader to discover that a drag stops at the row boundary, the
//! column offers the multi-row unit explicitly: `Copy` in the head takes the whole transcript, and
//! shift-clicking a second row takes the range from the note anchor to it. Both go through
//! [`copy_text`], which prefixes every line with its media time and its source — a quoted
//! transcript that says neither is unattributable, and pasting three rows into a ticket is exactly
//! the case where when-and-who is the point.
//!
//! **The click still belongs to the note anchor.** GPUI fires `on_click` on mouse-up regardless of
//! how far the pointer travelled, and `TextView` installs its selection handlers on the window
//! without stopping propagation, so a drag inside a row both selects text and anchors that row.
//! That is the deliberate resolution: the two gestures address the same row and the anchor writes
//! nothing to the record, so selection is additive and never swallows the anchor.

use std::{
    collections::{BTreeMap, HashMap},
    sync::Arc,
    time::Duration,
};

use gpui::{
    AnyElement, App, ClipboardItem, Context, Div, ElementId, Global, IntoElement, ListState,
    Pixels, Rgba, SharedString, Stateful, Timer, WeakEntity, Window, div, list, prelude::*, px,
    rems,
};
use gpui_component::{
    scroll::{Scrollbar, ScrollbarShow},
    text::{TextView, TextViewStyle},
};
use sotto_core::{EventId, EventPayload, Source, SpeechState, TimelineEvent, replay_lenient};

use crate::reasoning::inspection::ScreenConsultation;

use super::{
    Button, MeetingWorkspace, StageTab,
    ask::AskSelection,
    control_row::{ControlRole, ControlRow},
    notes,
    notes::AnnotationView,
    tokens::{Space, TypeScale, WorkspaceTokens},
};

/// How long a row stays visibly marked after a citation lands on it.
const CITATION_FLASH: Duration = Duration::from_millis(1_400);

const MISSING_EVIDENCE: &str = "That evidence is not present in this transcript.";

/// Said once, on the row and again in anything copied from it, so the qualifier cannot be lost.
const UNFINALIZED_NOTICE: &str = "Not finalized before capture stopped";

/// What a live hypothesis is, carried *inside* the selectable text rather than beside it.
///
/// The provisional strip is the one surface whose text changes under the reader, so the warning
/// has to survive the clipboard. A marker rendered as a neighbouring label would not.
const PROVISIONAL_MARKER: &str = "still being revised";

/// A live hypothesis rendered so that the warning cannot be separated from the words.
fn provisional_line(text: &str) -> String {
    format!("{PROVISIONAL_MARKER} · {text}")
}

/// Escapes text for [`TextView::html`].
///
/// `TextView` renders Markdown or HTML, and transcript text is neither. HTML is the honest carrier
/// of the two: four substitutions round-trip exactly through html5ever's entity decoding, whereas
/// Markdown would silently eat a speaker's `*emphasis*` and `[brackets]` on the way to the
/// clipboard.
fn escape_html(text: &str) -> String {
    let mut escaped = String::with_capacity(text.len());
    for value in text.chars() {
        match value {
            '&' => escaped.push_str("&amp;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            '"' => escaped.push_str("&quot;"),
            _ => escaped.push(value),
        }
    }
    escaped
}

/// One line of transcript text the reader can drag across and copy.
///
/// It exists as a `RenderOnce` because `TextView` needs a `&mut Window` its call sites do not have:
/// the transcript column is rendered from `layout.rs` through [`render`], whose signature belongs
/// to another task in this wave.
#[derive(IntoElement)]
struct SelectableLine {
    id: ElementId,
    debug: SharedString,
    text: SharedString,
}

impl SelectableLine {
    /// `kind` names the line's role; the event id makes both the element id and the debug selector
    /// address exactly one row, which is what lets a test click one and copy from another.
    fn new(kind: &'static str, event_id: EventId, text: impl Into<SharedString>) -> Self {
        Self {
            id: (kind, event_id.get()).into(),
            debug: SharedString::from(format!("{kind}-{}", event_id.get())),
            text: text.into(),
        }
    }
}

impl RenderOnce for SelectableLine {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let debug = self.debug;
        div().debug_selector(move || debug.to_string()).child(
            TextView::html(self.id, escape_html(&self.text), window, cx)
                .selectable(true)
                // One transcript row is one paragraph; the inter-paragraph rhythm of a document
                // would open a gap the row layout never asked for.
                .style(TextViewStyle::default().paragraph_gap(rems(0.0))),
        )
    }
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct TranscriptRow {
    pub(crate) event_id: EventId,
    pub(crate) source: Source,
    pub(crate) start: Duration,
    pub(crate) text: String,
    pub(crate) prosody: Vec<sotto_core::Annotation>,
    pub(crate) unfinalized: bool,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct TranscriptProjection {
    pub(crate) committed: Vec<TranscriptRow>,
    pub(crate) unstable: Vec<TranscriptRow>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct FrameProjection {
    pub(crate) transcript: TranscriptProjection,
    pub(crate) annotations: Vec<AnnotationView>,
    pub(crate) annotations_by_anchor: BTreeMap<EventId, Vec<AnnotationView>>,
    pub(crate) latest_anchor: Option<EventId>,
    pub(crate) resolved_anchors: BTreeMap<EventId, EventId>,
}

/// Projects committed and unstable registers without allowing silence-time
/// hypotheses into the visible provisional strip.
pub(crate) fn project_transcript(events: &[TimelineEvent]) -> TranscriptProjection {
    project_frame(events).transcript
}

/// Projects a stopped session, preserving its trailing active hypothesis as an
/// explicitly unfinalized transcript row rather than a live provisional strip.
pub(crate) fn project_completed_transcript(events: &[TimelineEvent]) -> TranscriptProjection {
    project_completed_frame(events).transcript
}

/// Replaces only the visible utterance text with a retained-media projection. Existing event ids
/// are reused in speaker order so meeting citations, annotations, and screen coordinates continue
/// to point into the immutable captured timeline.
pub(crate) fn project_completed_derived_transcript(
    events: &[TimelineEvent],
    utterances: &[sotto_core::Utterance],
) -> TranscriptProjection {
    let captured = project_completed_transcript(events).committed;
    let mut anchors = HashMap::<Source, std::collections::VecDeque<EventId>>::new();
    for row in &captured {
        anchors
            .entry(row.source)
            .or_default()
            .push_back(row.event_id);
    }
    let mut next_id = events
        .iter()
        .map(TimelineEvent::id)
        .map(EventId::get)
        .max()
        .unwrap_or(0)
        .saturating_add(1);
    let mut committed = utterances
        .iter()
        .filter_map(|utterance| {
            let text = utterance.text.trim();
            if text.is_empty() || is_silence_marker(text) {
                return None;
            }
            let event_id = anchors
                .entry(utterance.source)
                .or_default()
                .pop_front()
                .unwrap_or_else(|| {
                    let id = EventId::new(next_id);
                    next_id = next_id.saturating_add(1);
                    id
                });
            Some(TranscriptRow {
                event_id,
                source: utterance.source,
                start: utterance.start,
                text: text.to_owned(),
                prosody: utterance.annotations.clone(),
                unfinalized: false,
            })
        })
        .collect::<Vec<_>>();
    committed.sort_by_key(|row| (row.start, row.event_id));
    TranscriptProjection {
        committed,
        unstable: Vec::new(),
    }
}

/// Builds every per-frame transcript/annotation view from one replay of the append-only log.
pub(crate) fn project_frame(events: &[TimelineEvent]) -> FrameProjection {
    project_frame_for_mode(events, None, false)
}

pub(crate) fn project_completed_frame(events: &[TimelineEvent]) -> FrameProjection {
    project_frame_for_mode(events, None, true)
}

pub(crate) fn project_frame_for(
    events: &[TimelineEvent],
    session: Option<sotto_core::SessionId>,
) -> FrameProjection {
    project_frame_for_mode(events, session, false)
}

/// Narrows the shared timeline buffer to one recording **before** anything replays it.
///
/// [`crate::TimelineState`] is append-only for the life of the process, so the buffer handed to a
/// live frame already holds every earlier recording of this app run. [`replay_lenient`] pins its
/// session to `events.first()` and requires strictly increasing ids, and each session restarts ids
/// at 1 — so replaying the concatenated buffer rejects the *current* recording wholesale as
/// `ForeignSession`/`NonMonotonicId`. Filtering afterwards cannot recover events replay already
/// dropped, which is why the scope is applied here rather than while walking the active set: the
/// second capture of a run showed no partials, surfaced no typed note, and offered no anchor at
/// all, leaving its note composer disabled with "waiting for the first transcript row".
///
/// Borrows in the common case — one recording's own log, and every completed-session caller — and
/// copies only when foreign events are actually present.
pub(crate) fn scope_to_session(
    events: &[TimelineEvent],
    session: Option<sotto_core::SessionId>,
) -> std::borrow::Cow<'_, [TimelineEvent]> {
    let Some(id) = session else {
        return std::borrow::Cow::Borrowed(events);
    };
    if events.iter().all(|event| event.session_id() == id) {
        return std::borrow::Cow::Borrowed(events);
    }
    std::borrow::Cow::Owned(
        events
            .iter()
            .filter(|event| event.session_id() == id)
            .cloned()
            .collect(),
    )
}

fn project_frame_for_mode(
    events: &[TimelineEvent],
    session: Option<sotto_core::SessionId>,
    completed: bool,
) -> FrameProjection {
    let scoped = scope_to_session(events, session);
    let events: &[TimelineEvent] = &scoped;
    let replayed = replay_lenient(events);
    let mut committed = Vec::new();
    let mut latest_final = HashMap::<Source, EventId>::new();
    let mut latest_partial = HashMap::<Source, TranscriptRow>::new();
    let mut speaking = HashMap::<Source, bool>::new();
    let mut annotations = Vec::new();
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
    let successors = events
        .iter()
        .filter(|event| transcript_ids.contains(&event.id()))
        .filter_map(|event| event.supersedes().map(|target| (target, event.id())))
        .collect::<BTreeMap<_, _>>();
    let active_ids = replayed
        .state()
        .active()
        .keys()
        .copied()
        .collect::<std::collections::BTreeSet<_>>();

    for event in replayed.state().active().values() {
        match event.payload() {
            EventPayload::Vad(segment) => {
                speaking.insert(segment.source, segment.kind == SpeechState::SpeechStart);
            }
            EventPayload::UtteranceFinal(utterance) => {
                let Some(row) = transcript_row(event, utterance) else {
                    continue;
                };
                latest_final
                    .entry(utterance.source)
                    .and_modify(|id| *id = (*id).max(event.id()))
                    .or_insert_with(|| event.id());
                committed.push(row);
            }
            EventPayload::UtterancePartial(utterance) => {
                if let Some(row) = transcript_row(event, utterance) {
                    latest_partial.insert(utterance.source, row);
                }
            }
            EventPayload::UserAnnotation(annotation) => annotations.push(AnnotationView {
                event_id: event.id(),
                anchor: annotation.anchor,
                text: annotation.text.clone(),
                mark: annotation.mark,
            }),
            // Screen snapshots carry a frame reference, never pixels. The default review path
            // never touches one: a frame is decoded only by an explicit later inspection.
            _ => {}
        }
    }

    committed.sort_by_key(|row| (row.start, row.event_id));
    let mut unstable = latest_partial
        .into_iter()
        .filter_map(|(source, row)| {
            let newer_than_final = latest_final
                .get(&source)
                .is_none_or(|final_id| row.event_id > *final_id);
            // A provisional strip is a live monitor of what is being said. A non-speech annotation
            // is by definition not that, so it never flickers there in either mode.
            let visible = (!completed || !is_silence_marker(&row.text))
                && !is_non_speech_annotation(&row.text);
            (newer_than_final
                && visible
                && (completed || speaking.get(&source).copied().unwrap_or(false)))
            .then_some(row)
        })
        .collect::<Vec<_>>();
    unstable.sort_by_key(|row| (row.start, row.event_id));
    if completed {
        for row in &mut unstable {
            row.unfinalized = true;
        }
        committed.append(&mut unstable);
        committed.sort_by_key(|row| (row.start, row.event_id));
    }

    let latest_anchor = committed
        .iter()
        .filter(|row| !completed || !row.unfinalized)
        .chain((!completed).then_some(&unstable).into_iter().flatten())
        .map(|row| row.event_id)
        .max();
    let mut annotations_by_anchor = BTreeMap::<EventId, Vec<AnnotationView>>::new();
    for annotation in &annotations {
        if let Some(anchor) = notes::resolve_anchor_with_index(
            annotation.anchor,
            &transcript_ids,
            &successors,
            &active_ids,
        ) {
            annotations_by_anchor
                .entry(anchor)
                .or_default()
                .push(annotation.clone());
        }
    }
    let resolved_anchors = transcript_ids
        .iter()
        .filter_map(|anchor| {
            notes::resolve_anchor_with_index(*anchor, &transcript_ids, &successors, &active_ids)
                .map(|resolved| (*anchor, resolved))
        })
        .collect();
    FrameProjection {
        transcript: TranscriptProjection {
            committed,
            unstable,
        },
        annotations,
        annotations_by_anchor,
        latest_anchor,
        resolved_anchors,
    }
}

fn is_silence_marker(text: &str) -> bool {
    matches!(
        text.trim().to_ascii_lowercase().as_str(),
        "[ silence ]" | "[silence]" | "[ pause ]" | "[pause]" | "[blank_audio]" | "[blank audio]"
    )
}

/// Whether the whole utterance is one of Whisper's own non-speech annotations.
///
/// Whisper reports what it heard when nobody spoke — `(soft music)`, `[people chattering]`, `♪` —
/// as ordinary utterance text. Only a *complete* annotation counts: `(laughs) yeah, agreed` is a
/// spoken row that happens to start with one, and stays a spoken row.
fn is_non_speech_annotation(text: &str) -> bool {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return false;
    }
    if trimmed
        .chars()
        .all(|value| matches!(value, '♪' | '*' | '~') || value.is_whitespace())
    {
        return true;
    }
    let mut chars = trimmed.chars();
    let (Some(open), Some(close)) = (chars.next(), chars.next_back()) else {
        return false;
    };
    let expected = match open {
        '(' => ')',
        '[' => ']',
        '{' => '}',
        '<' => '>',
        '*' => '*',
        '♪' => '♪',
        _ => return false,
    };
    let inner = chars.as_str();
    close == expected
        && !inner.trim().is_empty()
        && !inner
            .chars()
            .any(|value| matches!(value, '(' | ')' | '[' | ']' | '{' | '}' | '<' | '>'))
}

fn transcript_row(
    event: &TimelineEvent,
    utterance: &sotto_core::Utterance,
) -> Option<TranscriptRow> {
    let text = utterance.text.trim();
    (!text.is_empty() && !is_silence_marker(text)).then(|| TranscriptRow {
        event_id: event.id(),
        source: utterance.source,
        start: utterance.start,
        text: text.to_owned(),
        prosody: utterance.annotations.clone(),
        unfinalized: false,
    })
}

/// How one committed row is presented once its neighbours are known.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RowRole {
    /// Something was said. Renders as a full attributed row.
    Speech,
    /// The first non-speech annotation in an unbroken stretch on one source. Renders as a single
    /// quiet aside that also discloses how many further moments it stands for.
    NonSpeechLeader { moments: usize, through: Duration },
    /// A later moment inside a stretch its leader already announced. Renders as nothing, keeping
    /// its list slot so committed row indices stay identical to the projection's.
    NonSpeechFolded,
}

/// Folds each unbroken run of non-speech annotations on one source down to its first row.
///
/// A run is broken by *speech on the same source*, not by speech on the other one — the noise
/// being suppressed is exactly the case where one channel is quiet while the other carries the
/// conversation, so the two are interleaved in time.
fn classify_rows(rows: &[TranscriptRow]) -> Vec<RowRole> {
    let mut roles = vec![RowRole::Speech; rows.len()];
    let mut open = HashMap::<Source, usize>::new();
    for (index, row) in rows.iter().enumerate() {
        if !is_non_speech_annotation(&row.text) {
            open.remove(&row.source);
            continue;
        }
        if let Some(leader) = open.get(&row.source).copied() {
            roles[index] = RowRole::NonSpeechFolded;
            if let RowRole::NonSpeechLeader { moments, through } = &mut roles[leader] {
                *moments = moments.saturating_add(1);
                *through = row.start;
            }
        } else {
            roles[index] = RowRole::NonSpeechLeader {
                moments: 1,
                through: row.start,
            };
            open.insert(row.source, index);
        }
    }
    roles
}

/// Redirects a citation that lands inside a folded run to the row that actually renders it.
fn presented_event(rows: &[TranscriptRow], event_id: EventId) -> EventId {
    let Some(index) = rows.iter().position(|row| row.event_id == event_id) else {
        return event_id;
    };
    if !is_non_speech_annotation(&rows[index].text) {
        return event_id;
    }
    let source = rows[index].source;
    rows[..=index]
        .iter()
        .rev()
        .take_while(|row| row.source != source || is_non_speech_annotation(&row.text))
        .filter(|row| row.source == source)
        .last()
        .map_or(event_id, |row| row.event_id)
}

/// Renders a contiguous run of committed rows the way a reader expects to paste them.
///
/// Returns the text and how many lines it holds. Two decisions are load-bearing:
///
/// - **Media time and source travel with the text.** A transcript excerpt in a ticket that names
///   neither when it was said nor which side said it cannot be checked against the recording, and
///   checking the record is the whole claim.
/// - **Runs are classified over the copied range, not the whole column.** A range that begins
///   inside a folded stretch of non-speech would otherwise open with rows that render as nothing
///   and copy as nothing — a silent hole. Re-classifying makes the first non-speech row in the
///   range its own leader, so the count and the span it stands for are stated for exactly what was
///   copied.
fn copy_text(rows: &[TranscriptRow]) -> (String, usize) {
    let roles = classify_rows(rows);
    let lines = rows
        .iter()
        .zip(roles)
        .filter_map(|(row, role)| copy_line(row, role))
        .collect::<Vec<_>>();
    (lines.join("\n"), lines.len())
}

/// One copied line, or `None` for a moment its leader already accounted for.
fn copy_line(row: &TranscriptRow, role: RowRole) -> Option<String> {
    let body = match role {
        // Folding is a presentation choice, and the leader states the count and the span it covers,
        // so dropping the followers loses nothing a reader could have read on screen either.
        RowRole::NonSpeechFolded => return None,
        RowRole::NonSpeechLeader { moments, through } => non_speech_summary(row, moments, through),
        RowRole::Speech => row.text.clone(),
    };
    let mut qualifiers = Vec::new();
    if !row.prosody.is_empty() {
        qualifiers.push(
            row.prosody
                .iter()
                .map(sotto_core::Annotation::render_inline)
                .collect::<Vec<_>>()
                .join(", "),
        );
    }
    if row.unfinalized {
        qualifiers.push(UNFINALIZED_NOTICE.to_owned());
    }
    let qualifier = if qualifiers.is_empty() {
        String::new()
    } else {
        format!(" ({})", qualifiers.join("; "))
    };
    Some(format!(
        "[{}] {}{qualifier}: {body}",
        format_time(row.start),
        source_label(row.source),
    ))
}

/// What the reader is told after a copy, so an empty clipboard is never mistaken for a full one.
fn copy_report(lines: usize) -> String {
    if lines == 0 {
        "There is nothing in this transcript to copy yet.".to_owned()
    } else if lines == 1 {
        "Copied 1 transcript row, with its timecode and source, to the clipboard.".to_owned()
    } else {
        format!("Copied {lines} transcript rows, with timecodes and sources, to the clipboard.")
    }
}

/// Builds the Ask scope represented by the same committed rows a range-copy gesture addresses.
///
/// A stopped recording can retain one explicitly unfinalized tail row. It remains visible and
/// copyable, but selection Ask excludes it because the reasoning contract accepts final utterances
/// only. A live provisional row never enters `TranscriptPacer`, so it is excluded by construction.
fn ask_selection(
    session_id: sotto_core::SessionId,
    rows: &[TranscriptRow],
) -> Option<AskSelection> {
    let finals = rows
        .iter()
        .filter(|row| !row.unfinalized)
        .collect::<Vec<_>>();
    let first = finals.first()?;
    let last = finals.last()?;
    let count = finals.len();
    let label = if count == 1 {
        format!(
            "[{}] {} · 1 finalized row",
            format_time(first.start),
            source_label(first.source)
        )
    } else {
        format!(
            "[{}] {} → [{}] {} · {count} finalized rows",
            format_time(first.start),
            source_label(first.source),
            format_time(last.start),
            source_label(last.source),
        )
    };
    Some(AskSelection {
        session_id,
        event_ids: finals.into_iter().map(|row| row.event_id).collect(),
        label,
    })
}

/// Keeps measured variable-height rows intact when finals are appended.
pub(crate) fn sync_list_state(state: &ListState, row_count: usize) {
    let old_count = state.item_count();
    if row_count > old_count {
        state.splice(old_count..old_count, row_count - old_count);
    } else if row_count < old_count {
        state.reset(row_count);
    }
}

/// Transient emphasis for the row a citation just landed on.
///
/// This is view state, never record state: it decays on a timer and touches no timeline event. It
/// is held in a GPUI global rather than on `MeetingWorkspace` because that struct belongs to
/// another task in this wave, and because there is exactly one transcript column per app.
#[derive(Clone, Copy, Debug, Default)]
struct CitationFlash {
    row: Option<EventId>,
    generation: u64,
}

impl Global for CitationFlash {}

fn flashing_row(cx: &App) -> Option<EventId> {
    cx.try_global::<CitationFlash>().and_then(|flash| flash.row)
}

fn start_citation_flash(row: EventId, cx: &mut Context<MeetingWorkspace>) {
    let generation = cx
        .try_global::<CitationFlash>()
        .map_or(0, |flash| flash.generation)
        .wrapping_add(1);
    cx.set_global(CitationFlash {
        row: Some(row),
        generation,
    });
    cx.spawn(async move |workspace: WeakEntity<MeetingWorkspace>, cx| {
        Timer::after(CITATION_FLASH).await;
        let _ = workspace.update(cx, |_, cx| {
            // A later citation owns the flash now; only the one that set it may clear it.
            let ours = cx
                .try_global::<CitationFlash>()
                .is_some_and(|flash| flash.generation == generation);
            if ours {
                cx.set_global(CitationFlash::default());
                cx.notify();
            }
        });
    })
    .detach();
}

#[expect(
    clippy::too_many_arguments,
    reason = "transcript projection state remains explicit"
)]
pub(crate) fn render(
    committed: Vec<TranscriptRow>,
    unstable: Vec<TranscriptRow>,
    annotations: BTreeMap<EventId, Vec<AnnotationView>>,
    live: bool,
    meeting_selected: bool,
    following: bool,
    focused_event: Option<EventId>,
    selected: &[EventId],
    list_state: &ListState,
    available_width: Pixels,
    cx: &mut Context<MeetingWorkspace>,
) -> AnyElement {
    let tokens = WorkspaceTokens::resolve(cx);
    let flashing = flashing_row(cx);
    let empty = empty_message(live, meeting_selected);

    // The list is indexed by committed row, folded rows included, so a citation resolved through
    // `TranscriptPacer::citation_index` still addresses the slot it always did.
    sync_list_state(list_state, committed.len());
    let roles = Arc::new(classify_rows(&committed));
    let presented = roles
        .iter()
        .filter(|role| **role != RowRole::NonSpeechFolded)
        .count();
    let rows = Arc::new(committed);
    let rendered_rows = Arc::clone(&rows);
    let rendered_roles = Arc::clone(&roles);
    let annotations = Arc::new(annotations);
    let committed_annotations = Arc::clone(&annotations);
    let workspace = cx.entity().downgrade();
    let unstable_workspace = workspace.clone();
    let selected = Arc::<[EventId]>::from(selected);
    let rendered_selected = Arc::clone(&selected);
    let transcript_list = list(list_state.clone(), move |index, _, _| {
        let row = &rendered_rows[index];
        let pinned = committed_annotations
            .get(&row.event_id)
            .cloned()
            .unwrap_or_default();
        match rendered_roles[index] {
            RowRole::Speech => render_committed_row(
                row,
                index == 0 || rendered_rows[index - 1].source != row.source,
                pinned,
                RowMarks {
                    anchor: focused_event,
                    range: &rendered_selected,
                    flashing,
                },
                tokens,
                workspace.clone(),
            ),
            RowRole::NonSpeechLeader { moments, through } => render_non_speech_aside(
                row,
                moments,
                through,
                pinned,
                RowMarks {
                    anchor: focused_event,
                    range: &rendered_selected,
                    flashing,
                },
                tokens,
                workspace.clone(),
            ),
            // Already accounted for on its leader's line; the slot stays so indices do not move.
            RowRole::NonSpeechFolded => div().into_any_element(),
        }
    })
    .size_full();

    div()
        .size_full()
        .flex()
        .flex_col()
        .child(render_column_head(
            live,
            presented,
            following,
            available_width,
            tokens,
            cx,
        ))
        .child(
            div()
                .id("virtual-transcript")
                .debug_selector(|| "virtual-transcript".into())
                .flex_1()
                .min_h_0()
                .on_scroll_wheel(cx.listener(|this, _, _, cx| this.pause_follow_live(cx)))
                .when(rows.is_empty(), |container| {
                    container.child(div().px_5().py_4().text_color(tokens.muted).child(empty))
                })
                .when(!rows.is_empty(), |container| {
                    container.relative().child(transcript_list).child(
                        Scrollbar::vertical(list_state)
                            .id("transcript-scrollbar")
                            .scrollbar_show(ScrollbarShow::Always),
                    )
                }),
        )
        .when(!unstable.is_empty(), |column| {
            column.child(render_unstable_strip(
                unstable,
                annotations,
                focused_event,
                tokens,
                unstable_workspace,
            ))
        })
        .into_any_element()
}

/// The column head: label and row count survive, the legend gives up its width first.
fn column_head(
    live: bool,
    rows: usize,
    available_width: Pixels,
    tokens: WorkspaceTokens,
) -> ControlRow {
    ControlRow::for_width(available_width)
        .child(
            ControlRole::Essential,
            div()
                .debug_selector(|| "transcript-head-label".into())
                .text_size(TypeScale::BODY)
                .child(if live {
                    "Live transcript"
                } else {
                    "Transcript"
                }),
        )
        .child(
            ControlRole::Essential,
            div()
                .pl(Space::SM)
                .text_size(TypeScale::META)
                .text_color(tokens.faint)
                .child(format!("{rows} rows")),
        )
        .child(ControlRole::Ellipsizing, div())
        .child(ControlRole::Expendable, render_legend(tokens))
}

fn render_column_head(
    live: bool,
    rows: usize,
    following: bool,
    available_width: Pixels,
    tokens: WorkspaceTokens,
    cx: &mut Context<MeetingWorkspace>,
) -> AnyElement {
    let head_width = column_head_available_width(available_width);
    let compact = head_width <= ControlRow::COLLAPSE_WIDTH;
    // Selection stops at a row boundary, so the whole-transcript unit needs a visible control
    // rather than a gesture the reader has to be told about.
    let head = column_head(live, rows, head_width, tokens)
        .child_when(rows > 0 && !compact, ControlRole::Essential, || {
            copy_control(cx)
        })
        .child_when(live && !compact, ControlRole::Essential, || {
            follow_control(following, cx)
        });
    div()
        .flex()
        .flex_col()
        .px(Space::MD)
        .py(Space::SM)
        .border_b_1()
        .border_color(tokens.line_soft)
        .child(head.finish())
        .when(compact && (rows > 0 || live), |header| {
            // At the same width where the legend leaves, keep the actions rather than clipping
            // them: metadata remains on the first row and essential actions receive a second.
            let actions = ControlRow::for_width(head_width)
                .child(ControlRole::Ellipsizing, div())
                .child_when(rows > 0, ControlRole::Essential, || copy_control(cx))
                .child_when(live, ControlRole::Essential, || {
                    follow_control(following, cx)
                });
            header.child(div().pt(Space::XS).child(actions.finish()))
        })
        .into_any_element()
}

fn copy_control(cx: &mut Context<MeetingWorkspace>) -> AnyElement {
    let tokens = WorkspaceTokens::resolve(cx);
    div()
        .pl(Space::SM)
        .debug_selector(|| "transcript-copy-control".into())
        .child(
            Button::new("copy-transcript", tokens)
                .label("Copy")
                .on_click(cx.listener(|this, _, _, cx| this.copy_transcript(cx))),
        )
        .into_any_element()
}

fn follow_control(following: bool, cx: &mut Context<MeetingWorkspace>) -> AnyElement {
    let tokens = WorkspaceTokens::resolve(cx);
    div()
        .pl(Space::SM)
        .debug_selector(|| "transcript-follow-control".into())
        .child(
            Button::new("follow-transcript", tokens)
                .label(if following {
                    "Following live"
                } else {
                    "Follow live"
                })
                .on_click(cx.listener(|this, _, _, cx| this.follow_live(cx))),
        )
        .into_any_element()
}

/// Explains the two dots. It is the first thing to go when the column narrows.
fn render_legend(tokens: WorkspaceTokens) -> Div {
    div()
        .debug_selector(|| "transcript-legend".into())
        .flex()
        .items_center()
        .gap(Space::MD)
        .whitespace_nowrap()
        .child(legend_entry(Source::System, tokens))
        .child(legend_entry(Source::Mic, tokens))
}

fn legend_entry(source: Source, tokens: WorkspaceTokens) -> Div {
    div()
        .flex()
        .items_center()
        .gap(Space::XS)
        .text_size(TypeScale::META)
        .text_color(tokens.faint)
        .child(source_dot(source, px(7.0), tokens))
        .child(source_label(source))
}

fn source_dot(source: Source, diameter: gpui::Pixels, tokens: WorkspaceTokens) -> Div {
    div()
        .w(diameter)
        .h(diameter)
        .flex_none()
        .rounded(diameter / 2.0)
        .bg(source_color(source, tokens))
}

const fn source_color(source: Source, tokens: WorkspaceTokens) -> Rgba {
    match source {
        Source::Mic => tokens.warn,
        Source::System => tokens.accent,
    }
}

/// The control row sits inside the head's horizontal padding, so its honest width is the column
/// width less both insets. Clamp pathological layout transients rather than passing a negative
/// width into the shared collapse decision.
fn column_head_available_width(column_width: Pixels) -> Pixels {
    let available = column_width - Space::MD * 2;
    if available > px(0.0) {
        available
    } else {
        px(0.0)
    }
}

const fn empty_message(live: bool, meeting_selected: bool) -> &'static str {
    if live {
        "Listening for speech…"
    } else if meeting_selected {
        "No transcript was captured for this recording."
    } else {
        "Nothing is being captured. Start a session and choose what you want on the record."
    }
}

/// `names_source` is true only where the source *changes* from the row above.
///
/// The column head carries a legend mapping each dot to its source, so repeating "captured audio"
/// on every consecutive row spends horizontal space to say what the previous row already said. The
/// dot stays on every row — it is the thing the legend explains — and the words return the moment
/// the speaker changes, which is exactly where a reader needs them.
fn render_committed_row(
    row: &TranscriptRow,
    names_source: bool,
    annotations: Vec<AnnotationView>,
    marks: RowMarks<'_>,
    tokens: WorkspaceTokens,
    workspace: WeakEntity<MeetingWorkspace>,
) -> AnyElement {
    let body_selector = format!("transcript-row-body-{}", row.event_id.get());
    row_shell(row, marks, tokens, workspace)
        .child(
            div()
                .debug_selector(move || body_selector)
                .w_full()
                .flex()
                .items_start()
                .gap(Space::MD)
                .text_size(TypeScale::BODY)
                .child(timestamp(row.start, tokens))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .items_start()
                        .gap(Space::SM)
                        .child(
                            div()
                                .flex_none()
                                .flex()
                                .items_center()
                                .gap(Space::XS)
                                .font_weight(gpui::FontWeight::SEMIBOLD)
                                .text_size(TypeScale::META)
                                .text_color(tokens.ink_2)
                                .child(source_dot(row.source, px(6.0), tokens))
                                .when(names_source, |chip| chip.child(source_label(row.source))),
                        )
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .whitespace_normal()
                                .when(row.unfinalized, |text| {
                                    text.child(
                                        div()
                                            .mb_1()
                                            .text_size(TypeScale::META)
                                            .text_color(tokens.warn)
                                            .child(UNFINALIZED_NOTICE),
                                    )
                                })
                                .when(!row.prosody.is_empty(), |text| {
                                    text.child(
                                        div()
                                            .text_size(TypeScale::META)
                                            .text_color(tokens.faint)
                                            .child(format!(
                                                "[{}]",
                                                row.prosody
                                                    .iter()
                                                    .map(sotto_core::Annotation::render_inline)
                                                    .collect::<Vec<_>>()
                                                    .join(", ")
                                            )),
                                    )
                                })
                                .child(SelectableLine::new(
                                    "transcript-row-text",
                                    row.event_id,
                                    row.text.clone(),
                                )),
                        ),
                ),
        )
        .children(
            annotations.into_iter().map(move |annotation| {
                notes::render_pinned_annotation_with_tokens(annotation, tokens)
            }),
        )
        .into_any_element()
}

/// One quiet line standing for a stretch of non-speech annotations on one source.
fn render_non_speech_aside(
    row: &TranscriptRow,
    moments: usize,
    through: Duration,
    annotations: Vec<AnnotationView>,
    marks: RowMarks<'_>,
    tokens: WorkspaceTokens,
    workspace: WeakEntity<MeetingWorkspace>,
) -> AnyElement {
    row_shell(row, marks, tokens, workspace)
        .child(
            div()
                .w_full()
                .flex()
                .items_start()
                .gap(Space::MD)
                .text_size(TypeScale::META)
                .text_color(tokens.faint)
                .child(timestamp(row.start, tokens))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .italic()
                        .whitespace_normal()
                        // The aside is selectable for the same reason it discloses a count: a
                        // reader copying this stretch must carry away what it stands for.
                        .child(SelectableLine::new(
                            "transcript-aside-text",
                            row.event_id,
                            non_speech_summary(row, moments, through),
                        )),
                ),
        )
        .children(
            annotations.into_iter().map(move |annotation| {
                notes::render_pinned_annotation_with_tokens(annotation, tokens)
            }),
        )
        .into_any_element()
}

/// Says what was heard, on which source, and how many further moments this line stands for.
fn non_speech_summary(row: &TranscriptRow, moments: usize, through: Duration) -> String {
    let label = source_label(row.source);
    if moments > 1 {
        format!(
            "{} · {moments} non-speech moments on {label}, through {}",
            row.text,
            format_time(through)
        )
    } else {
        format!("{} · non-speech on {label}", row.text)
    }
}

/// What the reader has marked on the transcript, as a row needs it to draw itself.
#[derive(Clone, Copy)]
struct RowMarks<'a> {
    /// The note anchor, and the origin of any range.
    anchor: Option<EventId>,
    /// The rows a shift-click covered, which is also what Ask is scoped to.
    range: &'a [EventId],
    /// The row a citation is transiently revealing.
    flashing: Option<EventId>,
}

impl RowMarks<'_> {
    fn of(self, event_id: EventId) -> RowMark {
        row_mark(
            self.anchor == Some(event_id),
            self.range.contains(&event_id),
            self.flashing == Some(event_id),
        )
    }
}

/// What a row shows about the reader's own marks on the transcript.
///
/// Shift-click has three effects — the clipboard, the Ask scope, and a status line — and until the
/// range was drawn, two of them were invisible: the reader could not see how far back the range
/// they had just copied and scoped a question to actually reached.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RowMark {
    Plain,
    /// Inside the marked range: what a shift-click copied and what Ask is scoped to.
    InRange,
    /// The range's origin, and the row a typed note attaches to.
    Anchor,
    /// Transiently revealed by following a citation; outranks the others because it is the answer
    /// to a question the reader asked a moment ago.
    Flashed,
}

impl RowMark {
    /// The left rule this mark draws, if any. A range shows as a wash alone: ruling every row of
    /// it would compete with the anchor for the same edge.
    const fn rule(self, tokens: WorkspaceTokens) -> Option<Rgba> {
        match self {
            Self::Flashed => Some(tokens.accent),
            Self::Anchor => Some(tokens.accent_line),
            Self::InRange | Self::Plain => None,
        }
    }
}

const fn row_mark(focused: bool, in_range: bool, flashed: bool) -> RowMark {
    if flashed {
        RowMark::Flashed
    } else if focused {
        RowMark::Anchor
    } else if in_range {
        RowMark::InRange
    } else {
        RowMark::Plain
    }
}

fn row_shell(
    row: &TranscriptRow,
    marks: RowMarks<'_>,
    tokens: WorkspaceTokens,
    workspace: WeakEntity<MeetingWorkspace>,
) -> Stateful<Div> {
    let event_id = row.event_id;
    let mark = marks.of(event_id);
    div()
        // The id has to name *this* row. GPUI keys per-element click state by the element id path,
        // and `gpui::list` does not scope its items, so one shared id gave every row one shared
        // `pending_mouse_down`: the first row's listener runs first in the mouse-up capture phase,
        // sees the pending press was not over itself, and clears it before the row the reader
        // actually clicked is ever reached. Only the topmost row could be anchored.
        .id(("transcript-row", event_id.get()))
        .debug_selector(|| "transcript-row".into())
        .px(Space::MD)
        .py(Space::XS)
        .border_t_1()
        .border_color(tokens.line_soft)
        .when(mark != RowMark::Plain, |line| line.bg(tokens.accent_wash))
        // The rule takes its two pixels back out of the row's own padding. A left border that
        // widens the box reflows the text under the reader mid-gesture: a drag that ends by
        // anchoring its row moved the words 2px right on mouse-up and dropped the selection the
        // drag had just made. `a_reader_can_select_a_row_anchor_it_and_copy_a_range_of_it` fails
        // without this compensation.
        .when(mark.rule(tokens).is_some(), |line| {
            line.pl(Space::MD - px(2.0))
        })
        .when_some(mark.rule(tokens), |line, rule| {
            line.border_l_2().border_color(rule)
        })
        // Selecting a row is the note anchor gesture; it appends nothing and rewrites nothing. A
        // drag that selected text inside this row still lands here, and still anchors: the two
        // gestures address the same row, so selection is additive rather than competing.
        //
        // Shift is the one modifier that means something else. It reaches back to the anchor and
        // copies the range, which is the only way a reader gets several rows at once — `TextView`
        // holds one selection per row and cannot be dragged across a row boundary.
        .on_click(move |event, _, cx| {
            let extend = event.modifiers().shift;
            let _ = workspace.update(cx, |workspace, cx| {
                if extend {
                    workspace.copy_transcript_range(event_id, cx);
                } else {
                    workspace.select_annotation_anchor(event_id, cx);
                }
            });
        })
}

fn timestamp(start: Duration, tokens: WorkspaceTokens) -> Div {
    div()
        .w(px(46.0))
        .flex_none()
        .text_right()
        .font_family("Menlo")
        .text_size(TypeScale::META)
        .text_color(tokens.faint)
        .child(format_time(start))
}

fn render_unstable_strip(
    rows: Vec<TranscriptRow>,
    annotations: Arc<BTreeMap<EventId, Vec<AnnotationView>>>,
    focused_event: Option<EventId>,
    tokens: WorkspaceTokens,
    workspace: WeakEntity<MeetingWorkspace>,
) -> AnyElement {
    div()
        .id("unstable-transcript-strip")
        .flex_none()
        .border_t_1()
        .border_color(tokens.line)
        .bg(tokens.surface_2)
        .px_5()
        .py_3()
        .children(rows.into_iter().map(|row| {
            let pinned = annotations.get(&row.event_id).cloned().unwrap_or_default();
            let event_id = row.event_id;
            let row_workspace = workspace.clone();
            div()
                .id(("unstable-transcript-row", event_id.get()))
                .when(row.source == Source::System, |line| line.mt_2())
                .when(focused_event == Some(row.event_id), |line| {
                    line.bg(tokens.accent_wash)
                })
                .on_click(move |_, _, cx| {
                    let _ = row_workspace.update(cx, |workspace, cx| {
                        workspace.select_annotation_anchor(event_id, cx)
                    });
                })
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(Space::SM)
                        .text_size(TypeScale::META)
                        .text_color(tokens.faint)
                        .child(source_dot(row.source, px(6.0), tokens))
                        .child(source_label(row.source))
                        .child("Listening…"),
                )
                .child(
                    div()
                        .mt_1()
                        .text_color(tokens.muted)
                        .whitespace_normal()
                        // A hypothesis is selectable, but it never leaves the app as bare text: the
                        // clipboard has no room for the surrounding strip, so the warning that this
                        // wording is about to change is part of the line itself.
                        .child(SelectableLine::new(
                            "unstable-transcript-text",
                            event_id,
                            provisional_line(&row.text),
                        )),
                )
                .children(pinned.into_iter().map(move |annotation| {
                    notes::render_pinned_annotation_with_tokens(annotation, tokens)
                }))
        }))
        .into_any_element()
}

impl MeetingWorkspace {
    /// Where a citation from the summary lands — the receiving half of the citation gesture.
    ///
    /// T075's citation chip calls this with the cited `EventId`, and calls nothing else: this
    /// selects the Transcript tab, resolves the citation through the append-only supersede chain,
    /// scrolls the row that actually renders that evidence into view, selects it as the note
    /// anchor, and flashes it for [`CITATION_FLASH`].
    ///
    /// Returns whether the citation landed. `false` means the evidence is not in the transcript
    /// currently on screen, and the workspace message says so; the caller should not also claim a
    /// successful jump.
    pub fn reveal_citation(&mut self, event_id: EventId, cx: &mut Context<Self>) -> bool {
        // The evidence layer has to be the one on screen before anything is scrolled into it.
        self.select_stage_tab(StageTab::Transcript, cx);
        let frame = if self.transcript_live {
            project_frame_for(self.timeline.read(cx).events(), self.transcript_session)
        } else {
            project_completed_frame(&self.transcript_events)
        };
        let Some(resolved) = frame.resolved_anchors.get(&event_id).copied() else {
            self.message = Some(MISSING_EVIDENCE.to_owned());
            cx.notify();
            return false;
        };
        let target = presented_event(self.transcript_pacer.rows(), resolved);
        if let Some(index) = self.transcript_pacer.citation_index(target) {
            self.transcript_list.scroll_to_reveal_item(index);
        } else if !frame
            .transcript
            .unstable
            .iter()
            .any(|row| row.event_id == resolved)
        {
            self.message = Some(MISSING_EVIDENCE.to_owned());
            cx.notify();
            return false;
        }
        self.follow_transcript = false;
        self.focused_event = Some(target);
        let selection = self.transcript_session.and_then(|session_id| {
            self.transcript_pacer
                .rows()
                .iter()
                .find(|row| row.event_id == target)
                .and_then(|row| ask_selection(session_id, std::slice::from_ref(row)))
        });
        self.set_ask_selection(selection, cx);
        self.message = None;
        start_citation_flash(target, cx);
        cx.notify();
        true
    }

    /// Copies every row the column currently presents.
    ///
    /// This is the head's `Copy` control. Committed rows come from the pacer; while capture is
    /// live, the current provisional registers are re-projected from the timeline and appended
    /// with their warning so the visible unstable strip is represented too.
    fn copy_transcript(&mut self, cx: &mut Context<Self>) {
        let mut rows = self.transcript_pacer.rows().to_vec();
        if self.transcript_live {
            let frame = project_frame_for(self.timeline.read(cx).events(), self.transcript_session);
            rows.extend(frame.transcript.unstable.into_iter().map(|mut row| {
                row.text = provisional_line(&row.text);
                row
            }));
        }
        let (text, lines) = copy_text(&rows);
        if lines > 0 {
            cx.write_to_clipboard(ClipboardItem::new_string(text));
        }
        self.message = Some(copy_report(lines));
        cx.notify();
    }

    /// Copies from the note anchor through `to`, inclusive, in the order the rows are presented.
    ///
    /// Shift-clicking with no anchor set copies the clicked row alone rather than guessing at a
    /// range the reader never marked.
    fn copy_transcript_range(&mut self, to: EventId, cx: &mut Context<Self>) {
        let rows = self.transcript_pacer.rows();
        let Some(target) = rows.iter().position(|row| row.event_id == to) else {
            return;
        };
        let anchor = self
            .focused_event
            .and_then(|anchor| rows.iter().position(|row| row.event_id == anchor))
            .unwrap_or(target);
        let (first, last) = (anchor.min(target), anchor.max(target));
        let (text, lines) = copy_text(&rows[first..=last]);
        let selection = self
            .transcript_session
            .and_then(|session_id| ask_selection(session_id, &rows[first..=last]));
        if lines > 0 {
            cx.write_to_clipboard(ClipboardItem::new_string(text));
        }
        self.set_ask_selection(selection, cx);
        self.message = Some(copy_report(lines));
        cx.notify();
    }

    fn select_annotation_anchor(&mut self, event_id: EventId, cx: &mut Context<Self>) {
        self.focused_event = Some(event_id);
        self.follow_transcript = false;
        self.editing_annotation = None;
        let selection = self.transcript_session.and_then(|session_id| {
            self.transcript_pacer
                .rows()
                .iter()
                .find(|row| row.event_id == event_id)
                .and_then(|row| ask_selection(session_id, std::slice::from_ref(row)))
        });
        self.set_ask_selection(selection, cx);
        // Selecting a row is where the record answers questions about itself, so it is also where
        // the app admits that a reasoning run looked at the screen behind this recording.
        let disclosure = screen_consultation_disclosure(self.notes.read(cx).screen_consultations());
        let mut message = format!(
            "Transcript row {} selected as the typed-note anchor. Shift-click another row to copy \
             the range.",
            event_id.get()
        );
        if self.ask_selection.is_none() {
            message.push_str(
                " Ask about this selection excludes provisional words until they finalize.",
            );
        }
        if let Some(disclosure) = disclosure {
            message.push(' ');
            message.push_str(&disclosure);
        }
        self.message = Some(message);
        cx.notify();
    }
}

/// Renders what the last notes run saw of the screen, or nothing when it looked at nothing.
///
/// The default path produces `None`: a run that needs no visual context inspects nothing, and the
/// record must not imply otherwise.
fn screen_consultation_disclosure(consultations: &[ScreenConsultation]) -> Option<String> {
    (!consultations.is_empty()).then(|| {
        consultations
            .iter()
            .map(ScreenConsultation::describe)
            .collect::<Vec<_>>()
            .join(" ")
    })
}

/// ADR-0019: a row names the source of the sound, not the occasion it belonged to.
const fn source_label(source: Source) -> &'static str {
    match source {
        Source::Mic => "your microphone",
        Source::System => "captured audio",
    }
}

fn format_time(timestamp: Duration) -> String {
    let seconds = timestamp.as_secs();
    format!("{:02}:{:02}", seconds / 60, seconds % 60)
}

#[cfg(test)]
fn citation_index(rows: &[TranscriptRow], event_id: EventId) -> Option<usize> {
    rows.iter().position(|row| row.event_id == event_id)
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use sotto_core::{
        CaptureTarget, EventId, EventPayload, FrameRef, ScreenSnapshot, Session, SessionId, Source,
        SpeechState, TargetKind, TimelineBuilder, Utterance, VadSegment,
    };

    use crate::reasoning::inspection::{
        ConsultationOutcome, ConsultedEvidence, ConsultedMoment, ConsultedPrecision,
        ScreenConsultation,
    };

    use super::{
        PROVISIONAL_MARKER, RowMark, RowRole, TranscriptRow, citation_index, classify_rows,
        copy_report, copy_text, empty_message, escape_html, is_non_speech_annotation,
        non_speech_summary, presented_event, project_completed_derived_transcript,
        project_completed_transcript, project_frame, project_transcript, provisional_line,
        row_mark, screen_consultation_disclosure, source_label,
    };

    fn timeline() -> TimelineBuilder {
        TimelineBuilder::new(Session::new(
            SessionId::new(9),
            CaptureTarget {
                bundle_id: None,
                display_name: "Meeting".into(),
                window_title: None,
                kind: TargetKind::Application,
                audio_scoped: true,
            },
            0,
        ))
    }

    fn utterance(text: &str) -> Utterance {
        Utterance {
            source: Source::Mic,
            start: Duration::ZERO,
            end: Duration::from_secs(1),
            text: text.into(),
            avg_logprob: -0.1,
            annotations: vec![],
        }
    }

    fn vad(kind: SpeechState) -> EventPayload {
        EventPayload::Vad(VadSegment {
            source: Source::Mic,
            start: Duration::ZERO,
            end: None,
            kind,
        })
    }

    fn row(id: u64, source: Source, start: u64, text: &str) -> TranscriptRow {
        TranscriptRow {
            event_id: EventId::new(id),
            source,
            start: Duration::from_secs(start),
            text: text.to_owned(),
            prosody: vec![],
            unfinalized: false,
        }
    }

    /// A second recording in the same app run must render, and must offer a note anchor.
    ///
    /// `TimelineState` is append-only for the life of the process, so by the time a person starts
    /// their second capture the shared buffer already holds the first one's log. `replay_lenient`
    /// pins its session to `events.first()` and requires strictly increasing ids, and each session
    /// restarts ids at 1 — so replaying the concatenated buffer rejects every event of the second
    /// recording before the per-session filter downstream ever sees it.
    #[test]
    fn a_second_live_recording_projects_over_the_first_ones_retained_events()
    -> Result<(), Box<dyn std::error::Error>> {
        let mut first = timeline();
        first.append(
            Duration::from_secs(1),
            EventPayload::UtteranceFinal(utterance("first recording")),
        );
        let second_id = SessionId::new(10);
        let mut second = TimelineBuilder::new(Session::new(
            second_id,
            CaptureTarget {
                bundle_id: None,
                display_name: "Meeting".into(),
                window_title: None,
                kind: TargetKind::Application,
                audio_scoped: true,
            },
            0,
        ));
        second.append(
            Duration::from_secs(1),
            EventPayload::UtteranceFinal(utterance("second recording")),
        );
        let mut retained = first.events().to_vec();
        retained.extend_from_slice(second.events());

        let frame = super::project_frame_for(&retained, Some(second_id));
        assert_eq!(
            frame
                .transcript
                .committed
                .iter()
                .map(|row| row.text.as_str())
                .collect::<Vec<_>>(),
            vec!["second recording"],
            "the live recording must render over an earlier recording's retained events"
        );
        assert!(
            frame.latest_anchor.is_some(),
            "the live recording must offer a typed-note anchor"
        );
        Ok(())
    }

    #[test]
    fn only_latest_live_partial_survives_while_speaking() -> Result<(), Box<dyn std::error::Error>>
    {
        let mut timeline = timeline();
        timeline.append(Duration::ZERO, vad(SpeechState::SpeechStart));
        let stale = timeline.append(
            Duration::ZERO,
            EventPayload::UtterancePartial(utterance("old")),
        );
        timeline.supersede(
            Duration::from_secs(1),
            EventPayload::UtterancePartial(utterance("new")),
            &stale,
        )?;

        let projection = project_transcript(timeline.events());
        assert_eq!(
            projection.unstable[0].text, "new",
            "the newest hypothesis wins"
        );
        assert!(
            !projection.unstable[0].unfinalized,
            "a live hypothesis is not an unfinalized final"
        );
        assert!(projection.committed.is_empty(), "nothing is committed yet");
        Ok(())
    }

    #[test]
    fn completed_projection_preserves_trailing_partial_without_a_live_strip()
    -> Result<(), Box<dyn std::error::Error>> {
        let mut timeline = timeline();
        timeline.append(
            Duration::ZERO,
            EventPayload::UtteranceFinal(utterance("first persisted final")),
        );
        timeline.append(
            Duration::ZERO,
            EventPayload::UtteranceFinal(utterance("second persisted final extends the first")),
        );
        timeline.append(Duration::from_secs(1), vad(SpeechState::SpeechStart));
        let stale = timeline.append(
            Duration::from_secs(1),
            EventPayload::UtterancePartial(utterance("trailing draft")),
        );
        timeline.supersede(
            Duration::from_secs(2),
            EventPayload::UtterancePartial(utterance("the last thing anyone said")),
            &stale,
        )?;
        timeline.append(Duration::from_secs(3), vad(SpeechState::SpeechEnd));

        let projection = project_completed_transcript(timeline.events());

        assert!(
            projection.unstable.is_empty(),
            "a stopped session has no live strip"
        );
        assert_eq!(projection.committed.len(), 3, "three rows survive");
        assert_eq!(
            projection.committed[0].text, "first persisted final",
            "the first final is preserved"
        );
        assert_eq!(
            projection.committed[1].text, "second persisted final extends the first",
            "the second final is preserved"
        );
        assert_eq!(
            projection.committed[2].text, "the last thing anyone said",
            "the trailing hypothesis is preserved"
        );
        assert!(
            projection.committed[2].unfinalized,
            "the trailing hypothesis is marked unfinalized"
        );
        Ok(())
    }

    #[test]
    fn completed_projection_suppresses_a_trailing_silence_marker() {
        let mut timeline = timeline();
        timeline.append(
            Duration::ZERO,
            EventPayload::UtteranceFinal(utterance("the last thing actually said")),
        );
        timeline.append(
            Duration::from_secs(1),
            EventPayload::UtterancePartial(utterance("[ Silence ]")),
        );

        let projection = project_completed_transcript(timeline.events());

        assert_eq!(projection.committed.len(), 1, "only real speech remains");
        assert_eq!(
            projection.committed[0].text, "the last thing actually said",
            "the spoken row survives"
        );
        assert!(
            projection.unstable.is_empty(),
            "silence never reaches the strip"
        );
    }

    #[test]
    fn committed_silence_and_pause_markers_are_not_transcript_rows() {
        let mut timeline = timeline();
        timeline.append(
            Duration::ZERO,
            EventPayload::UtteranceFinal(utterance("[ Silence ]")),
        );
        timeline.append(
            Duration::from_secs(1),
            EventPayload::UtteranceFinal(utterance("[ Pause ]")),
        );
        timeline.append(
            Duration::from_secs(2),
            EventPayload::UtteranceFinal(utterance("actual speech")),
        );

        let projection = project_completed_transcript(timeline.events());

        assert_eq!(projection.committed.len(), 1, "markers are not rows");
        assert_eq!(
            projection.committed[0].text, "actual speech",
            "the spoken row survives"
        );
    }

    #[test]
    fn derived_projection_replaces_text_but_keeps_captured_anchors() {
        let mut timeline = timeline();
        let first = timeline.append(
            Duration::ZERO,
            EventPayload::UtteranceFinal(utterance("a play")),
        );
        let mut corrected = utterance("labeled");
        corrected.start = Duration::from_millis(20);

        let projection = project_completed_derived_transcript(timeline.events(), &[corrected]);

        assert_eq!(projection.committed.len(), 1, "one row is projected");
        assert_eq!(
            projection.committed[0].text, "labeled",
            "the derived text is shown"
        );
        assert_eq!(
            projection.committed[0].event_id,
            first.id(),
            "the captured anchor is reused"
        );
        assert!(
            matches!(
                timeline.events()[0].payload(),
                EventPayload::UtteranceFinal(value) if value.text == "a play"
            ),
            "the captured event is never rewritten"
        );
    }

    #[test]
    fn silence_gates_unstable_text() {
        let mut timeline = timeline();
        timeline.append(Duration::ZERO, vad(SpeechState::SpeechStart));
        timeline.append(
            Duration::ZERO,
            EventPayload::UtterancePartial(utterance("phantom words")),
        );
        timeline.append(Duration::from_secs(1), vad(SpeechState::SpeechEnd));

        assert!(
            project_transcript(timeline.events()).unstable.is_empty(),
            "silence must close the provisional strip"
        );
    }

    /// Every row of a shift-clicked range is marked, not only the two the reader clicked.
    ///
    /// The gesture's extent was invisible: the anchor was washed and the rows it reached were not,
    /// so a reader had no way to see what they had copied or what Ask had been scoped to.
    #[test]
    fn a_marked_range_shows_on_every_row_it_covers_and_still_names_its_anchor() {
        assert_eq!(
            row_mark(true, true, false),
            RowMark::Anchor,
            "the range's origin stays distinguishable from the rows it reached"
        );
        assert_eq!(
            row_mark(false, true, false),
            RowMark::InRange,
            "a row inside the range must show that it is inside the range"
        );
        assert_eq!(
            row_mark(false, false, false),
            RowMark::Plain,
            "an unmarked row claims nothing"
        );
        assert_eq!(
            row_mark(true, true, true),
            RowMark::Flashed,
            "a citation the reader just followed outranks a standing mark"
        );
        assert_eq!(
            row_mark(true, false, false),
            RowMark::Anchor,
            "an anchor with no range behind it is still the anchor"
        );
    }

    #[test]
    fn citation_index_uses_committed_rows_only() {
        let mut timeline = timeline();
        let first = timeline.append(
            Duration::ZERO,
            EventPayload::UtteranceFinal(utterance("first")),
        );
        let second = timeline.append(
            Duration::from_secs(1),
            EventPayload::UtteranceFinal(utterance("second")),
        );
        let rows = project_transcript(timeline.events()).committed;

        assert_eq!(
            citation_index(&rows, first.id()),
            Some(0),
            "the first row is addressable"
        );
        assert_eq!(
            citation_index(&rows, second.id()),
            Some(1),
            "the second row is addressable"
        );
        assert_eq!(
            citation_index(&rows, EventId::new(99)),
            None,
            "an unknown event addresses nothing"
        );
    }

    #[test]
    fn empty_copy_distinguishes_idle_from_an_empty_persisted_recording() {
        assert_eq!(
            empty_message(false, true),
            "No transcript was captured for this recording.",
            "a reopened recording says so in ADR-0019's vocabulary"
        );
        assert!(
            !empty_message(false, false).contains("microphone"),
            "the idle state must not imply a source"
        );
    }

    #[test]
    fn a_run_that_consulted_the_screen_says_so_with_its_moment_and_precision() {
        assert_eq!(
            screen_consultation_disclosure(&[]),
            None,
            "a run that looked at nothing must claim nothing"
        );

        let disclosure = screen_consultation_disclosure(&[ScreenConsultation {
            session_id: SessionId::new(59),
            requested: ConsultedMoment::Timestamp(Duration::from_secs(68)),
            evidence: ConsultedEvidence::LocalOcr,
            reason: "read the number on the cited slide".to_owned(),
            outcome: ConsultationOutcome::Frame {
                requested_media_time: Duration::from_secs(68),
                decoded_media_time: Duration::from_millis(68_025),
                precision: ConsultedPrecision::DecodedVideoFrame,
                ocr_characters: Some(7),
            },
        }])
        .unwrap_or_default();

        assert!(disclosure.contains("01:08"), "{disclosure}");
        assert!(
            disclosure.contains("decoded video frame at 01:08.025"),
            "{disclosure}"
        );
        assert!(
            disclosure.contains("no image was sent to the reasoning backend"),
            "{disclosure}"
        );
    }

    #[test]
    fn one_frame_projection_resolves_every_register_without_event_clones()
    -> Result<(), Box<dyn std::error::Error>> {
        let mut timeline = timeline();
        timeline.append(Duration::ZERO, vad(SpeechState::SpeechStart));
        let partial = timeline.append(
            Duration::ZERO,
            EventPayload::UtterancePartial(utterance("draft")),
        );
        timeline.append_user_annotation(
            Duration::from_millis(100),
            partial.id(),
            "Pin this",
            sotto_core::MarkKind::Note,
        )?;
        let final_row = timeline.supersede(
            Duration::from_secs(1),
            EventPayload::UtteranceFinal(utterance("settled")),
            &partial,
        )?;

        let frame = project_frame(timeline.events());
        assert_eq!(
            frame.transcript.committed.len(),
            1,
            "one committed row survives"
        );
        assert_eq!(frame.annotations.len(), 1, "the typed note is projected");
        assert_eq!(
            frame.resolved_anchors[&partial.id()],
            final_row.id(),
            "the anchor follows the supersede chain"
        );
        assert_eq!(
            frame.annotations_by_anchor[&final_row.id()][0].text,
            "Pin this",
            "the note hangs off the surviving row"
        );
        assert_eq!(
            frame.latest_anchor,
            Some(final_row.id()),
            "the newest row is the default anchor"
        );
        Ok(())
    }

    #[test]
    fn rows_name_their_source_in_adr_0019_vocabulary() {
        assert_eq!(
            source_label(Source::System),
            "captured audio",
            "the captured target is named by its source"
        );
        assert_eq!(
            source_label(Source::Mic),
            "your microphone",
            "the local participant is named by their source"
        );
    }

    /// A run of rows from one source names it once, at the change.
    ///
    /// The column head's legend already maps each dot to its source, so repeating the words on
    /// every consecutive row spends the width the transcript needs for the transcript. The rule the
    /// renderer applies is this one, so it is asserted here rather than through a mounted frame.
    #[test]
    fn a_run_of_rows_from_one_source_names_it_only_where_it_changes() {
        let sources = [
            Source::System,
            Source::System,
            Source::System,
            Source::Mic,
            Source::Mic,
            Source::System,
        ];
        let names: Vec<bool> = sources
            .iter()
            .enumerate()
            .map(|(index, source)| index == 0 || sources[index - 1] != *source)
            .collect();
        assert_eq!(
            names,
            [true, false, false, true, false, true],
            "the words return exactly where the speaker changes, and the first row always names \
             its own source"
        );
    }

    #[test]
    fn whole_non_speech_annotations_are_recognised_and_partial_ones_are_not() {
        for text in [
            "(soft music)",
            "  [people chattering] ",
            "(SOFT MUSIC)",
            "♪",
            "♪♪",
            "*coughs*",
            "[Music]",
        ] {
            assert!(
                is_non_speech_annotation(text),
                "{text} is a whole non-speech annotation"
            );
        }
        for text in [
            "(laughs) yeah, agreed",
            "we shipped it (finally)",
            "the retry path (see the ticket) is keyed wrong",
            "()",
            "sure, sounds fine",
        ] {
            assert!(
                !is_non_speech_annotation(text),
                "{text} carries speech and must stay a spoken row"
            );
        }
    }

    #[test]
    fn a_quiet_channel_folds_to_one_disclosed_aside_while_the_other_keeps_speaking() {
        let rows = [
            row(1, Source::System, 0, "so where did the retry land"),
            row(2, Source::Mic, 1, "(soft music)"),
            row(3, Source::System, 2, "on the order id, apparently"),
            row(4, Source::Mic, 3, "(people chattering)"),
            row(5, Source::System, 4, "which is the bug"),
            row(6, Source::Mic, 5, "(soft music)"),
            row(7, Source::Mic, 6, "sorry, I was on mute"),
            row(8, Source::Mic, 7, "(soft music)"),
        ];

        let roles = classify_rows(&rows);

        assert_eq!(roles[0], RowRole::Speech, "captured speech is never folded");
        assert_eq!(
            roles[1],
            RowRole::NonSpeechLeader {
                moments: 3,
                through: Duration::from_secs(5)
            },
            "one leader stands for the whole quiet stretch, interleaved speech notwithstanding"
        );
        assert_eq!(roles[3], RowRole::NonSpeechFolded, "later moments fold in");
        assert_eq!(roles[5], RowRole::NonSpeechFolded, "later moments fold in");
        assert_eq!(
            roles[6],
            RowRole::Speech,
            "speech on the quiet source ends the stretch"
        );
        assert_eq!(
            roles[7],
            RowRole::NonSpeechLeader {
                moments: 1,
                through: Duration::from_secs(7)
            },
            "a new stretch starts after the source speaks again"
        );
        assert_eq!(
            roles.len(),
            rows.len(),
            "folding never removes a row from the projection"
        );
    }

    #[test]
    fn a_folded_stretch_discloses_what_it_stands_for() {
        let leader = row(2, Source::Mic, 1, "(soft music)");

        assert_eq!(
            non_speech_summary(&leader, 3, Duration::from_secs(305)),
            "(soft music) · 3 non-speech moments on your microphone, through 05:05",
            "nothing is deleted silently: the count and the span are stated"
        );
        assert_eq!(
            non_speech_summary(&leader, 1, Duration::from_secs(1)),
            "(soft music) · non-speech on your microphone",
            "a lone annotation is demoted, not summarized away"
        );
    }

    #[test]
    fn a_citation_into_a_folded_stretch_lands_on_the_line_that_renders_it() {
        let rows = [
            row(1, Source::System, 0, "so where did the retry land"),
            row(2, Source::Mic, 1, "(soft music)"),
            row(3, Source::System, 2, "on the order id"),
            row(4, Source::Mic, 3, "(people chattering)"),
        ];

        assert_eq!(
            presented_event(&rows, EventId::new(4)),
            EventId::new(2),
            "a folded moment redirects to its leader"
        );
        assert_eq!(
            presented_event(&rows, EventId::new(2)),
            EventId::new(2),
            "a leader is its own presentation"
        );
        assert_eq!(
            presented_event(&rows, EventId::new(3)),
            EventId::new(3),
            "a spoken row is never redirected"
        );
        assert_eq!(
            presented_event(&rows, EventId::new(99)),
            EventId::new(99),
            "an unknown event is returned untouched for the caller to reject"
        );
    }

    #[test]
    fn typed_notes_append_beside_captured_rows_without_rewriting_them()
    -> Result<(), Box<dyn std::error::Error>> {
        let mut timeline = timeline();
        let captured = timeline.append(
            Duration::ZERO,
            EventPayload::UtteranceFinal(utterance("we key the retry on the order id")),
        );
        let before = timeline.events().to_vec();

        timeline.append_user_annotation(
            Duration::from_secs(30),
            captured.id(),
            "this is the bug",
            sotto_core::MarkKind::Important,
        )?;

        let frame = project_frame(timeline.events());
        assert_eq!(
            frame.transcript.committed.len(),
            1,
            "a typed note is not a transcript row"
        );
        assert_eq!(
            frame.transcript.committed[0].text, "we key the retry on the order id",
            "the captured text is untouched by the note"
        );
        assert_eq!(
            frame.annotations_by_anchor[&captured.id()][0].text,
            "this is the bug",
            "the note hangs off the row it anchors to"
        );
        assert_eq!(
            &timeline.events()[..before.len()],
            before.as_slice(),
            "appending a note rewrites no earlier event"
        );
        assert_eq!(
            timeline.events().len(),
            before.len() + 1,
            "the note is appended, not merged"
        );
        Ok(())
    }

    #[test]
    fn copied_rows_carry_their_media_time_and_their_source() {
        let mut hesitant = row(1, Source::System, 12, "sure, sounds fine");
        hesitant.prosody = vec![
            sotto_core::Annotation::Hesitant,
            sotto_core::Annotation::Pause(Duration::from_millis(2_500)),
        ];
        let mut trailing = row(2, Source::Mic, 305, "the last thing anyone said");
        trailing.unfinalized = true;
        let rows = [hesitant, trailing];

        let (text, lines) = copy_text(&rows);

        assert_eq!(lines, 2, "both rows are copied");
        let copied = text.lines().collect::<Vec<_>>();
        assert_eq!(
            copied[0], "[00:12] captured audio (hesitant, 2.5s pause): sure, sounds fine",
            "a copied row leads with its media time, its source, and how it was said"
        );
        assert_eq!(
            copied[1],
            "[05:05] your microphone (Not finalized before capture stopped): the last thing anyone \
             said",
            "an unfinalized row says so in the clipboard, not only on screen"
        );
    }

    #[test]
    fn a_copied_fold_states_what_it_stands_for_instead_of_leaving_a_hole() {
        let rows = [
            row(1, Source::System, 0, "so where did the retry land"),
            row(2, Source::Mic, 1, "(soft music)"),
            row(3, Source::Mic, 2, "(people chattering)"),
            row(4, Source::Mic, 3, "(soft music)"),
            row(5, Source::System, 4, "on the order id"),
        ];

        let (text, lines) = copy_text(&rows);

        assert_eq!(
            lines, 3,
            "three moments fold into the one line that renders them: {text}"
        );
        assert!(
            text.contains("3 non-speech moments on your microphone, through 00:03"),
            "the copied fold discloses the count and the span it covers: {text}"
        );
        assert!(
            !text.contains("(people chattering)"),
            "a folded moment is accounted for, not repeated: {text}"
        );
    }

    #[test]
    fn a_range_that_opens_mid_fold_still_discloses_what_it_copied() {
        let rows = [
            row(1, Source::Mic, 1, "(soft music)"),
            row(2, Source::Mic, 2, "(people chattering)"),
            row(3, Source::Mic, 3, "(soft music)"),
        ];

        // The reader's range begins on a row that renders as nothing on screen, because its leader
        // is above the range. Classified over the whole column it would copy as nothing at all.
        let (text, lines) = copy_text(&rows[1..]);

        assert_eq!(lines, 1, "the range is not empty: {text}");
        assert_eq!(
            text,
            "[00:02] your microphone: (people chattering) · 2 non-speech moments on your \
             microphone, through 00:03",
            "the first non-speech row inside the range leads it and states the range's own count"
        );
    }

    #[test]
    fn copying_nothing_says_so_rather_than_claiming_a_full_clipboard() {
        let (text, lines) = copy_text(&[]);
        assert!(text.is_empty(), "an empty transcript copies no text");
        assert_eq!(lines, 0, "and reports no lines");
        assert_eq!(
            copy_report(0),
            "There is nothing in this transcript to copy yet.",
            "an empty copy is never reported as a successful one"
        );
        assert_eq!(
            copy_report(1),
            "Copied 1 transcript row, with its timecode and source, to the clipboard.",
            "a single row is reported in the singular"
        );
        assert!(
            copy_report(3).starts_with("Copied 3 transcript rows"),
            "a range reports how much it took"
        );
    }

    #[test]
    fn a_live_hypothesis_cannot_be_copied_without_its_warning() {
        let line = provisional_line("we key the retry on the ord");
        assert!(
            line.starts_with(PROVISIONAL_MARKER),
            "the warning leads the text it qualifies: {line}"
        );
        assert!(
            line.ends_with("we key the retry on the ord"),
            "and the words themselves survive: {line}"
        );
    }

    #[test]
    fn speech_reaches_the_renderer_as_written_rather_than_as_markup() {
        assert_eq!(
            escape_html("we shipped it (finally) & *fixed* <the retry>"),
            "we shipped it (finally) &amp; *fixed* &lt;the retry&gt;",
            "punctuation a speaker used is escaped, never interpreted"
        );
        assert_eq!(
            escape_html("she said \"ship it\""),
            "she said &quot;ship it&quot;",
            "quotes survive into the rendered line"
        );
    }

    #[test]
    fn the_default_transcript_path_reads_no_screen_frame() {
        let mut timeline = timeline();
        timeline.append(
            Duration::ZERO,
            EventPayload::UtteranceFinal(utterance("look at the dashboard")),
        );
        timeline.append(
            Duration::from_secs(1),
            EventPayload::ScreenSnapshot(ScreenSnapshot {
                frame_ref: FrameRef::new("recording://session/9#00:01"),
                ocr_text: String::new(),
                active_app: Some("Zoom".to_owned()),
                window_title: Some("Sprint 41".to_owned()),
                visible_from: Duration::from_secs(1),
                visible_to: None,
            }),
        );

        let frame = project_frame(timeline.events());

        assert_eq!(
            frame.transcript.committed.len(),
            1,
            "a screen snapshot is not a transcript row"
        );
        assert!(
            frame.annotations.is_empty(),
            "a screen snapshot produces no annotation"
        );
        assert_eq!(
            screen_consultation_disclosure(&[]),
            None,
            "the default path claims no inspection because it performed none"
        );
    }
}

/// Evidence that the selectable column mounts, measures, and answers the pointer.
///
/// A projection test proves the strings are right and nothing about whether a `TextView` survives
/// inside a virtualized list. These build the real shell over a real store and drive it with real
/// clicks, because a transcript that lays out to nothing would pass every test above.
#[cfg(test)]
mod mounted_tests {
    use std::{ops::Deref as _, sync::Arc, time::Duration};

    use gpui::{
        AppContext as _, Bounds, Modifiers, TestAppContext, VisualTestContext, WindowBounds,
        WindowOptions, point, px, size,
    };
    use rag::Store;
    use secrecy::SecretString;
    use sotto_core::{
        CaptureTarget, EventId, EventPayload, Session, SessionId, Source, TargetKind,
        TimelineBuilder, Utterance,
    };

    use super::{PROVISIONAL_MARKER, TranscriptRow, provisional_line};
    use crate::workspace::{MeetingWorkspace, StageTab};
    use crate::{mcp, reasoning, session};

    const WORKSPACE_WIDTH: gpui::Pixels = px(1400.0);

    const FIXTURE_SESSION: SessionId = SessionId::new(1_786_625_633_040_598_000);

    struct NoOpenAiCredentials;

    impl reasoning::OpenAiCredentialStore for NoOpenAiCredentials {
        fn store(&self, _: &SecretString) -> Result<(), sotto_core::ProviderError> {
            Ok(())
        }

        fn load(&self) -> Result<Option<SecretString>, sotto_core::ProviderError> {
            Ok(None)
        }

        fn delete(&self) -> Result<(), sotto_core::ProviderError> {
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

    struct MountedShell {
        workspace: gpui::Entity<MeetingWorkspace>,
        ingress: crate::devwindow::TimelineIngress,
        visual: &'static mut VisualTestContext,
    }

    /// `debug_bounds` takes a `&'static str`, and these selectors name one row each.
    fn selector(kind: &str, event_id: EventId) -> &'static str {
        String::leak(format!("{kind}-{}", event_id.get()))
    }

    /// Persists a stopped recording of four spoken rows and returns the row event ids in order.
    async fn persist_stopped_session(
        database: &std::path::Path,
    ) -> Result<Vec<EventId>, Box<dyn std::error::Error>> {
        let session_id = FIXTURE_SESSION;
        let mut record = Session::new(
            session_id,
            CaptureTarget {
                bundle_id: Some("com.example.meeting".into()),
                display_name: "Meeting app".into(),
                window_title: Some("This is a persisted recording".into()),
                kind: TargetKind::Window,
                audio_scoped: true,
            },
            1_786_625_633_040,
        );
        record.end(1_786_625_700_000);
        let store = Store::open(database).await?;
        store.save_session(&record).await?;
        let mut timeline = TimelineBuilder::new(record);
        let spoken = [
            (Source::System, "so where did the retry land"),
            (Source::Mic, "on the order id, apparently"),
            (Source::System, "which is the bug"),
            (
                Source::Mic,
                "and the ticket says \"ship it\" & nothing else",
            ),
        ];
        let ids = spoken
            .into_iter()
            .enumerate()
            .map(|(index, (source, text))| {
                let start = Duration::from_secs(index as u64 * 3);
                timeline
                    .append(
                        Duration::from_secs(100 + index as u64),
                        EventPayload::UtteranceFinal(Utterance {
                            source,
                            start,
                            end: start + Duration::from_secs(2),
                            text: text.to_owned(),
                            avg_logprob: 0.0,
                            annotations: vec![],
                        }),
                    )
                    .id()
            })
            .collect::<Vec<_>>();
        store.append_events(timeline.events()).await?;
        Ok(ids)
    }

    fn mount(
        cx: &mut TestAppContext,
        directory: &std::path::Path,
    ) -> Result<MountedShell, Box<dyn std::error::Error>> {
        mount_at(cx, directory, WORKSPACE_WIDTH)
    }

    fn mount_at(
        cx: &mut TestAppContext,
        directory: &std::path::Path,
        width: gpui::Pixels,
    ) -> Result<MountedShell, Box<dyn std::error::Error>> {
        let database = directory.join("sotto.sqlite3");
        let (ingress, timeline) = cx.update(|cx| crate::devwindow::attach_ingress(cx, 16));
        let session = cx.new(|_| session::SessionController::new(ingress.clone()));
        let reasoning = cx.new(|_| {
            reasoning::ReasoningController::load(
                directory.join("reasoning.json"),
                Arc::new(NoOpenAiCredentials),
            )
        });
        let mcp = cx.new(|_| {
            mcp::McpController::load(Some(directory.join("mcp.json")), Arc::new(NoMcpCredentials))
        });
        let handle = cx.update(|cx| {
            cx.open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(Bounds {
                        origin: point(px(0.0), px(0.0)),
                        size: size(width, px(720.0)),
                    })),
                    ..WindowOptions::default()
                },
                |window, cx| {
                    cx.new(|cx| {
                        MeetingWorkspace::new(
                            database, timeline, reasoning, session, mcp, window, cx,
                        )
                    })
                },
            )
        })?;
        let workspace = handle.root(cx)?;
        let visual = VisualTestContext::from_window(*handle.deref(), cx).into_mut();
        visual.run_until_parked();
        Ok(MountedShell {
            workspace,
            ingress,
            visual,
        })
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_reader_can_select_a_row_anchor_it_and_copy_a_range_of_it()
    -> Result<(), Box<dyn std::error::Error>> {
        let mut cx = TestAppContext::single();
        cx.update(gpui_component::init);
        let dir = tempfile::tempdir()?;
        let ids = persist_stopped_session(&dir.path().join("sotto.sqlite3")).await?;
        let shell = mount(&mut cx, dir.path())?;
        let workspace = shell.workspace;
        let visual = shell.visual;
        visual.update(|_, cx| {
            workspace.update(cx, |workspace, cx| {
                workspace.select_meeting(FIXTURE_SESSION, cx);
                workspace.select_stage_tab(StageTab::Transcript, cx);
            });
        });
        visual.refresh()?;
        visual.run_until_parked();
        // The text view parses on a background task; without draining it the row would measure
        // against an empty parse and this test would assert over nothing.
        visual.refresh()?;
        visual.run_until_parked();

        let first = ids
            .first()
            .copied()
            .ok_or_else(|| std::io::Error::other("the fixture must persist rows"))?;
        let third = ids
            .get(2)
            .copied()
            .ok_or_else(|| std::io::Error::other("the fixture must persist three rows"))?;

        let text_bounds = visual
            .debug_bounds(selector("transcript-row-text", first))
            .ok_or_else(|| std::io::Error::other("row text must render as a selectable line"))?;
        assert!(
            text_bounds.size.width > px(0.0) && text_bounds.size.height > px(0.0),
            "the selectable line must take real space, not collapse to nothing: {text_bounds:?}"
        );

        // Click the row body — the same surface a drag-selection starts on. The anchor gesture must
        // survive selection being wired onto it.
        let body = visual
            .debug_bounds(selector("transcript-row-body", first))
            .ok_or_else(|| std::io::Error::other("the row body must render"))?;
        visual.simulate_click(body.center(), Modifiers::none());
        visual.refresh()?;
        visual.run_until_parked();
        assert_eq!(
            visual.update(|_, cx| workspace.read(cx).focused_event),
            Some(first),
            "clicking selectable row text must still set the note anchor"
        );
        assert_eq!(
            visual.update(|_, cx| {
                workspace
                    .read(cx)
                    .ask_selection
                    .as_ref()
                    .map(|selection| selection.event_ids.clone())
            }),
            Some(vec![first]),
            "anchoring one finalized row offers exactly that row to Ask without changing scope"
        );

        // Shift-click a later row: the range from the anchor lands on the clipboard.
        let later = visual
            .debug_bounds(selector("transcript-row-body", third))
            .ok_or_else(|| std::io::Error::other("a later row body must render"))?;
        visual.simulate_click(later.center(), Modifiers::shift());
        visual.refresh()?;
        visual.run_until_parked();

        let copied = visual
            .update(|_, cx| cx.read_from_clipboard())
            .and_then(|item| item.text())
            .ok_or_else(|| std::io::Error::other("a shift-click must put text on the clipboard"))?;
        assert_eq!(
            copied,
            "[00:00] captured audio: so where did the retry land\n[00:03] your microphone: on the \
             order id, apparently\n[00:06] captured audio: which is the bug",
            "the copied range spans the anchor through the shift-clicked row, timecoded and \
             attributed"
        );
        assert_eq!(
            visual.update(|_, cx| workspace.read(cx).focused_event),
            Some(first),
            "extending the copy range must not move the note anchor"
        );
        assert_eq!(
            visual.update(|_, cx| {
                workspace
                    .read(cx)
                    .ask_selection
                    .as_ref()
                    .map(|selection| selection.event_ids.clone())
            }),
            Some(ids[..=2].to_vec()),
            "the Ask selection must be the exact anchor-through-row range"
        );

        // The head control takes the whole column, and speech punctuation survives the round trip
        // through the renderer's escaping.
        visual.update(|_, cx| {
            workspace.update(cx, |workspace, cx| workspace.copy_transcript(cx));
        });
        let whole = visual
            .update(|_, cx| cx.read_from_clipboard())
            .and_then(|item| item.text())
            .ok_or_else(|| {
                std::io::Error::other("Copy must put the transcript on the clipboard")
            })?;
        assert_eq!(whole.lines().count(), 4, "every presented row is copied");
        assert!(
            whole.ends_with("and the ticket says \"ship it\" & nothing else"),
            "quotes and ampersands reach the clipboard as the speaker said them: {whole}"
        );
        assert!(
            visual
                .debug_bounds("transcript-copy-control")
                .is_some_and(|bounds| bounds.size.width > px(0.0)),
            "the whole-transcript copy must be a visible control, not a hidden gesture"
        );

        // Finally, the gesture the maintainer asked for: drag across a row and press cmd-c.
        let last = ids
            .last()
            .copied()
            .ok_or_else(|| std::io::Error::other("the fixture must persist rows"))?;
        let quoted = visual
            .debug_bounds(selector("transcript-row-text", last))
            .ok_or_else(|| std::io::Error::other("the last row must render its text"))?;
        visual.simulate_mouse_down(
            gpui::point(quoted.left() + px(2.0), quoted.top() + px(4.0)),
            gpui::MouseButton::Left,
            Modifiers::none(),
        );
        visual.simulate_mouse_move(
            gpui::point(quoted.right() - px(2.0), quoted.bottom() - px(4.0)),
            gpui::MouseButton::Left,
            Modifiers::none(),
        );
        visual.simulate_mouse_up(
            gpui::point(quoted.right() - px(2.0), quoted.bottom() - px(4.0)),
            gpui::MouseButton::Left,
            Modifiers::none(),
        );
        visual.run_until_parked();
        visual.simulate_keystrokes("cmd-c");
        visual.run_until_parked();

        let selected = visual
            .update(|_, cx| cx.read_from_clipboard())
            .and_then(|item| item.text())
            .ok_or_else(|| std::io::Error::other("a drag then cmd-c must copy the selection"))?;
        assert_eq!(
            selected, "and the ticket says \"ship it\" & nothing else",
            "dragging across a row copies exactly the words in it, punctuation intact"
        );
        assert_eq!(
            visual.update(|_, cx| workspace.read(cx).focused_event),
            Some(last),
            "a drag that selected text still leaves that row as the note anchor"
        );
        Ok(())
    }

    /// The copy control is the only way to take a multi-row unit, so it may not be the thing that
    /// falls off the head when the window is at its documented minimum.
    #[tokio::test(flavor = "multi_thread")]
    async fn the_copy_control_survives_the_narrowest_supported_window()
    -> Result<(), Box<dyn std::error::Error>> {
        const MIN_WORKSPACE_WIDTH: gpui::Pixels = px(680.0);

        let mut cx = TestAppContext::single();
        cx.update(gpui_component::init);
        let dir = tempfile::tempdir()?;
        persist_stopped_session(&dir.path().join("sotto.sqlite3")).await?;
        let shell = mount_at(&mut cx, dir.path(), MIN_WORKSPACE_WIDTH)?;
        let workspace = shell.workspace;
        let visual = shell.visual;
        visual.update(|_, cx| {
            workspace.update(cx, |workspace, cx| {
                workspace.select_meeting(FIXTURE_SESSION, cx);
                workspace.select_stage_tab(StageTab::Transcript, cx);
            });
        });
        visual.refresh()?;
        visual.run_until_parked();

        let copy = visual
            .debug_bounds("transcript-copy-control")
            .ok_or_else(|| std::io::Error::other("Copy must survive the narrowest width"))?;
        assert!(
            copy.size.width > px(0.0),
            "Copy must keep real width rather than paint as a sliver: {copy:?}"
        );
        assert!(
            copy.right() <= MIN_WORKSPACE_WIDTH,
            "Copy must stay wholly inside the window: {copy:?}"
        );
        Ok(())
    }

    /// Recording is the tightest composition: the visible rail leaves 470 px at the minimum
    /// viewport, and the transcript receives only 57.5% of that stage. Every essential head
    /// control must still live wholly inside its actual column while only the legend disappears.
    #[test]
    fn the_live_head_keeps_every_essential_inside_its_narrow_column()
    -> Result<(), Box<dyn std::error::Error>> {
        const MIN_WORKSPACE_WIDTH: gpui::Pixels = px(680.0);

        let mut cx = TestAppContext::single();
        cx.update(gpui_component::init);
        let dir = tempfile::tempdir()?;
        let shell = mount_at(&mut cx, dir.path(), MIN_WORKSPACE_WIDTH)?;
        let workspace = shell.workspace;
        let visual = shell.visual;
        let live = SessionId::new(1_786_625_999_000_000_001);
        visual.update(|_, cx| {
            workspace.update(cx, |workspace, cx| {
                workspace.show_live_transcript(live, cx);
                workspace.transcript_pacer.replace(vec![TranscriptRow {
                    event_id: EventId::new(91),
                    source: Source::Mic,
                    start: Duration::ZERO,
                    text: "one settled row".to_owned(),
                    prosody: vec![],
                    unfinalized: false,
                }]);
                cx.notify();
            });
        });
        visual.refresh()?;
        visual.run_until_parked();

        let stage = visual
            .debug_bounds("stage-transcript")
            .ok_or_else(|| std::io::Error::other("the live transcript stage must render"))?;
        for selector in [
            "transcript-head-label",
            "transcript-copy-control",
            "transcript-follow-control",
        ] {
            let control = visual.debug_bounds(selector).ok_or_else(|| {
                std::io::Error::other(format!("{selector} must survive the live narrow head"))
            })?;
            assert!(
                control.size.width > px(0.0)
                    && control.left() >= stage.left()
                    && control.right() <= stage.right(),
                "{selector} must stay wholly inside the transcript column: {control:?} in {stage:?}"
            );
        }
        assert!(
            visual.debug_bounds("transcript-legend").is_none(),
            "the legend, and only the legend, must collapse in the narrow live head"
        );
        Ok(())
    }

    /// The stated limit, held to by a test rather than left for a reader to discover.
    ///
    /// A drag that leaves the row it started in copies only that row. That is why `Copy` and the
    /// shift-click range exist, and why the module documents the boundary instead of implying a
    /// selection that spans the column.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_drag_past_the_row_boundary_copies_only_the_row_it_started_in()
    -> Result<(), Box<dyn std::error::Error>> {
        let mut cx = TestAppContext::single();
        cx.update(gpui_component::init);
        let dir = tempfile::tempdir()?;
        let ids = persist_stopped_session(&dir.path().join("sotto.sqlite3")).await?;
        let shell = mount(&mut cx, dir.path())?;
        let workspace = shell.workspace;
        let visual = shell.visual;
        visual.update(|_, cx| {
            workspace.update(cx, |workspace, cx| {
                workspace.select_meeting(FIXTURE_SESSION, cx);
                workspace.select_stage_tab(StageTab::Transcript, cx);
            });
        });
        visual.refresh()?;
        visual.run_until_parked();

        let (first, second) = (
            ids.first()
                .copied()
                .ok_or_else(|| std::io::Error::other("the fixture must persist rows"))?,
            ids.get(1)
                .copied()
                .ok_or_else(|| std::io::Error::other("the fixture must persist two rows"))?,
        );
        let start = visual
            .debug_bounds(selector("transcript-row-text", first))
            .ok_or_else(|| std::io::Error::other("the first row must render its text"))?;
        let beyond = visual
            .debug_bounds(selector("transcript-row-text", second))
            .ok_or_else(|| std::io::Error::other("the second row must render its text"))?;

        visual.simulate_mouse_down(
            gpui::point(start.left() + px(2.0), start.top() + px(4.0)),
            gpui::MouseButton::Left,
            Modifiers::none(),
        );
        let end = gpui::point(beyond.right() - px(2.0), beyond.bottom() - px(4.0));
        visual.simulate_mouse_move(end, gpui::MouseButton::Left, Modifiers::none());
        visual.simulate_mouse_up(end, gpui::MouseButton::Left, Modifiers::none());
        visual.run_until_parked();
        visual.simulate_keystrokes("cmd-c");
        visual.run_until_parked();

        let copied = visual
            .update(|_, cx| cx.read_from_clipboard())
            .and_then(|item| item.text())
            .unwrap_or_default();
        assert_eq!(
            copied, "so where did the retry land",
            "a selection stops at the row it began in; the range gesture is the multi-row unit"
        );
        Ok(())
    }

    /// Every row must be clickable, not only the first.
    ///
    /// Rows shared one element id until T077, and GPUI keys click state by that id, so the first
    /// row's mouse-up listener cleared the pending press before the clicked row saw it. The column
    /// looked interactive and answered exactly one row.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_row_below_the_first_can_still_be_made_the_note_anchor()
    -> Result<(), Box<dyn std::error::Error>> {
        let mut cx = TestAppContext::single();
        cx.update(gpui_component::init);
        let dir = tempfile::tempdir()?;
        let ids = persist_stopped_session(&dir.path().join("sotto.sqlite3")).await?;
        let shell = mount(&mut cx, dir.path())?;
        let workspace = shell.workspace;
        let visual = shell.visual;
        visual.update(|_, cx| {
            workspace.update(cx, |workspace, cx| {
                workspace.select_meeting(FIXTURE_SESSION, cx);
                workspace.select_stage_tab(StageTab::Transcript, cx);
            });
        });
        visual.refresh()?;
        visual.run_until_parked();

        for expected in ids.iter().copied().rev() {
            let body = visual
                .debug_bounds(selector("transcript-row-body", expected))
                .ok_or_else(|| std::io::Error::other("every row must render its body"))?;
            visual.simulate_click(body.center(), Modifiers::none());
            visual.refresh()?;
            visual.run_until_parked();
            assert_eq!(
                visual.update(|_, cx| workspace.read(cx).focused_event),
                Some(expected),
                "clicking a row must anchor that row, whichever one it is"
            );
        }
        Ok(())
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_citation_still_scrolls_and_flashes_after_the_rows_became_selectable()
    -> Result<(), Box<dyn std::error::Error>> {
        let mut cx = TestAppContext::single();
        cx.update(gpui_component::init);
        let dir = tempfile::tempdir()?;
        let ids = persist_stopped_session(&dir.path().join("sotto.sqlite3")).await?;
        let shell = mount(&mut cx, dir.path())?;
        let workspace = shell.workspace;
        let visual = shell.visual;
        visual.update(|_, cx| {
            workspace.update(cx, |workspace, cx| {
                workspace.select_meeting(FIXTURE_SESSION, cx);
            });
        });
        visual.refresh()?;
        visual.run_until_parked();

        let cited = ids
            .get(2)
            .copied()
            .ok_or_else(|| std::io::Error::other("the fixture must persist three rows"))?;
        let landed = visual.update(|_, cx| {
            workspace.update(cx, |workspace, cx| workspace.reveal_citation(cited, cx))
        });
        visual.refresh()?;
        visual.run_until_parked();

        assert!(
            landed,
            "a cited row present in the transcript must be found"
        );
        assert_eq!(
            visual.update(|_, cx| workspace.read(cx).stage_tab),
            StageTab::Transcript,
            "a citation must bring the transcript layer forward"
        );
        assert_eq!(
            visual.update(|_, cx| workspace.read(cx).focused_event),
            Some(cited),
            "a citation must land on the row it addressed"
        );
        assert!(
            !visual.update(|_, cx| {
                workspace.update(cx, |workspace, cx| {
                    workspace.reveal_citation(EventId::new(999_999), cx)
                })
            }),
            "evidence outside this transcript must still be refused rather than faked"
        );
        Ok(())
    }

    /// The live path, where the text under the reader is still moving.
    ///
    /// A hypothesis is selectable like anything else, but it must never leave the app looking
    /// settled, so the warning is inside the selectable line rather than beside it.
    #[test]
    fn a_live_hypothesis_renders_as_a_selectable_line_that_admits_it_is_one()
    -> Result<(), Box<dyn std::error::Error>> {
        let mut cx = TestAppContext::single();
        cx.update(gpui_component::init);
        let dir = tempfile::tempdir()?;
        let shell = mount(&mut cx, dir.path())?;
        let workspace = shell.workspace;
        let ingress = shell.ingress;
        let visual = shell.visual;

        let live = SessionId::new(1_786_625_999_000_000_000);
        let mut timeline = TimelineBuilder::new(Session::new(
            live,
            CaptureTarget::microphone_only(),
            1_786_625_999_000,
        ));
        timeline.append(
            Duration::ZERO,
            EventPayload::Vad(sotto_core::VadSegment {
                source: Source::Mic,
                start: Duration::ZERO,
                end: None,
                kind: sotto_core::SpeechState::SpeechStart,
            }),
        );
        let hypothesis = timeline.append(
            Duration::ZERO,
            EventPayload::UtterancePartial(Utterance {
                source: Source::Mic,
                start: Duration::ZERO,
                end: Duration::from_secs(1),
                text: "we key the retry on the ord".to_owned(),
                avg_logprob: 0.0,
                annotations: vec![],
            }),
        );
        for event in timeline.events() {
            visual
                .background_executor
                .clone()
                .block(ingress.send(event.clone()))
                .map_err(|_| std::io::Error::other("the ingress seam must accept the event"))?;
        }
        // The seam drains on a `smol::Timer`, which follows the wall clock rather than the test
        // executor's, so this waits for real elapsed time rather than advancing a virtual one.
        let mut delivered = 0;
        for _ in 0..100 {
            visual.run_until_parked();
            delivered = visual.update(|_, cx| workspace.read(cx).timeline.read(cx).events().len());
            if delivered == 2 {
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        assert_eq!(
            delivered, 2,
            "the ingress seam must deliver the live events before anything is asserted"
        );
        visual.update(|_, cx| {
            workspace.update(cx, |workspace, cx| {
                workspace.show_live_transcript(live, cx);
                workspace.select_stage_tab(StageTab::Transcript, cx);
            });
        });
        visual.refresh()?;
        visual.run_until_parked();
        visual.refresh()?;
        visual.run_until_parked();

        let line = visual
            .debug_bounds(selector("unstable-transcript-text", hypothesis.id()))
            .ok_or_else(|| {
                std::io::Error::other("a live hypothesis must render as a selectable line")
            })?;
        assert!(
            line.size.width > px(0.0) && line.size.height > px(0.0),
            "the provisional line must take real space: {line:?}"
        );
        assert!(
            provisional_line("we key the retry on the ord").starts_with(PROVISIONAL_MARKER),
            "and what it renders is the warning followed by the words, so a copy carries both"
        );
        visual.simulate_click(line.center(), Modifiers::none());
        visual.run_until_parked();
        assert!(
            visual.update(|_, cx| workspace.read(cx).ask_selection.is_none()),
            "a live provisional row remains selectable text but is never offered as finalized Ask evidence"
        );
        assert!(
            visual
                .update(|_, cx| workspace.read(cx).message.clone())
                .is_some_and(|message| message.contains("excludes provisional words")),
            "the provisional-tail exclusion must be stated where the selection is made"
        );
        visual.update(|_, cx| {
            workspace.update(cx, |workspace, cx| workspace.copy_transcript(cx));
        });
        let copied = visual
            .update(|_, cx| cx.read_from_clipboard())
            .and_then(|item| item.text())
            .ok_or_else(|| std::io::Error::other("Copy must include the visible live row"))?;
        assert!(
            copied.contains(PROVISIONAL_MARKER) && copied.contains("we key the retry on the ord"),
            "whole-transcript Copy must include the visible hypothesis and its warning: {copied}"
        );
        Ok(())
    }

    #[test]
    fn a_provisional_line_carries_its_warning_into_whatever_is_copied() {
        let line = provisional_line("we key the retry on the ord");
        assert!(
            line.contains(PROVISIONAL_MARKER),
            "a hypothesis never leaves the app as bare settled text: {line}"
        );
    }
}

#[cfg(test)]
mod layout_tests {
    use gpui::{Context, IntoElement, Render, TestAppContext, Window, div, prelude::*, px, size};

    use super::{ControlRow, Space, WorkspaceTokens, column_head, column_head_available_width};

    type MeasuredHead = (
        gpui::Bounds<gpui::Pixels>,
        Option<gpui::Bounds<gpui::Pixels>>,
    );

    struct HeadHarness {
        column_width: gpui::Pixels,
    }

    impl Render for HeadHarness {
        fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            let tokens = WorkspaceTokens::resolve(cx);
            let available_width = column_head_available_width(self.column_width);
            div().size_full().child(
                div()
                    .px(Space::MD)
                    .py(Space::SM)
                    .child(column_head(false, 128, available_width, tokens).finish()),
            )
        }
    }

    #[test]
    fn the_legend_collapses_at_the_shared_threshold_while_the_label_remains()
    -> Result<(), Box<dyn std::error::Error>> {
        let measure = |width| -> Result<MeasuredHead, Box<dyn std::error::Error>> {
            // Debug bounds are retained by a visual context between frames. A fresh harness for
            // each width proves absence rather than mistaking the previous wide frame for the
            // current narrow one.
            let mut cx = TestAppContext::single();
            cx.update(gpui_component::init);
            let (_view, visual) = cx.add_window_view(move |_, _| HeadHarness {
                column_width: width,
            });
            visual.simulate_resize(size(px(900.0), px(400.0)));
            visual.refresh()?;
            visual.run_until_parked();
            let label = visual
                .debug_bounds("transcript-head-label")
                .ok_or_else(|| std::io::Error::other("the column label must stay rendered"))?;
            let legend = visual.debug_bounds("transcript-legend");
            Ok((label, legend))
        };

        let head_insets = Space::MD * 2;
        let (wide_label, wide_legend) =
            measure(ControlRow::COLLAPSE_WIDTH + px(1.0) + head_insets)?;
        let (threshold_label, threshold_legend) =
            measure(ControlRow::COLLAPSE_WIDTH + head_insets)?;
        let (narrow_label, narrow_legend) =
            measure(ControlRow::COLLAPSE_WIDTH - px(1.0) + head_insets)?;

        assert!(
            wide_legend.is_some_and(|bounds| bounds.size.width > px(0.0)),
            "the legend explains the dots when there is room"
        );
        assert!(
            threshold_legend.is_none() && narrow_legend.is_none(),
            "at or below the shared threshold the legend must be absent, not merely narrower"
        );
        assert_eq!(
            (threshold_label.size.width, narrow_label.size.width),
            (wide_label.size.width, wide_label.size.width),
            "the column label remains at every width"
        );
        Ok(())
    }
}
