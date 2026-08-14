//! The recording library rail, Home, and the three equally weighted ways a session begins.
//!
//! ADR-0019 makes a session a *recording of something*, so the rail is a **Library** rather than a
//! list of meetings, and starting one is not a single button. Capturing an app, recording only the
//! microphone, and importing a file are three first-class beginnings; the home surface must not
//! rank them by promoting one to a button and demoting the others to links.
//!
//! T078 makes **Home the rail's first entry** rather than a separate navigation surface. The rail
//! is already the only navigator in the shell, so the way back to the entry points belongs in it,
//! sitting above the recordings it deselects.
//!
//! T083 makes the rail **collapsible** and takes its search field out. Both follow from the same
//! observation: the rail is a quarter of a 1000 px window, and anything a person still needs while
//! it is hidden cannot live inside it. So the search field is in the toolbar and this module only
//! consumes the query it holds; `layout.rs` decides whether the rail is rendered at all.

use std::{
    collections::BTreeMap,
    path::Path,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use chrono::Datelike as _;
use gpui::{
    AnyElement, Context, Entity, FontWeight, Global, MouseButton, Pixels, Rgba, Subscription,
    Window, div, prelude::*, px,
};
use gpui_component::{
    Sizable as _,
    button::{Button, ButtonVariants as _},
    input::{Input, InputEvent, InputState},
    scroll::ScrollableElement,
    tooltip::Tooltip,
};
use insight::load_latest_grounded_notes;
use rag::{SessionSummary, Store};
use sotto_core::{EventPayload, SessionId, types::RecordingTitle};

use super::{
    MeetingWorkspace,
    control_row::{ControlRole, ControlRow},
    icons,
    layout::format_bytes,
    tokens::{Space, TypeScale, WorkspaceTokens},
};
use crate::session::SessionController;

/// The rail's nominal width, matching the shell's default left panel. It is well below
/// [`ControlRow::COLLAPSE_WIDTH`], which is the honest answer for every row in here: the rail is
/// narrow, so anything expendable in one of its rows should be dropped rather than squeezed.
const RAIL_WIDTH: Pixels = px(240.0);

// The rail's markers are content, not controls: they say what a row *is* — a captured app, a
// microphone-only recording, an imported file, Home — beside the words that already name it, and
// nothing here is clickable on its own.
//
// T082 converted them from Unicode to vendored SVGs anyway, and the reason is the same defect that
// produced the emoji trash bin: a codepoint is a request, not a picture. `▣` and `⇥` have no
// settled meaning for "captured application" and "imported file" — `⇥` is a tab key — and which
// face CoreText picks for each is the font's decision, not Sotto's. Drawn at the ambient text size
// and colour they still read as typography rather than as buttons, so being content markers is an
// argument about *weight*, not about who owns the drawing.

/// What the library actually occupies on this Mac.
///
/// Both numbers are measured, never estimated: the count is the persisted session catalogue, and
/// the size is the sum of the retained media the store still has on disk. Nothing else is claimed —
/// a home surface that invented a dashboard would be the failure this task exists to avoid.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct LibraryFootprint {
    recordings: usize,
    retained_bytes: u64,
}

impl LibraryFootprint {
    /// Sums the retained media the store reports for the sessions in the catalogue.
    ///
    /// A recording the store has marked deleted or pruned contributes zero, so the size never
    /// counts bytes that are no longer on the disk.
    pub(crate) fn measure(database: &Path, meetings: &[SessionSummary]) -> Self {
        let retained_bytes = Store::open(database)
            .and_then(|store| store.list_recordings())
            .map(|recordings| {
                recordings.iter().fold(0_u64, |total, recording| {
                    total.saturating_add(recording.byte_size())
                })
            })
            .unwrap_or_default();
        Self {
            recordings: meetings.len(),
            retained_bytes,
        }
    }

    fn count(self) -> String {
        if self.recordings == 1 {
            "1 recording".to_owned()
        } else {
            format!("{} recordings", self.recordings)
        }
    }

    /// The Home rail entry's second line. Short, because the rail is narrow.
    fn meta(self) -> String {
        if self.recordings == 0 {
            return "nothing recorded yet".to_owned();
        }
        if self.retained_bytes == 0 {
            return format!("{} · no retained media", self.count());
        }
        format!("{} · {}", self.count(), format_bytes(self.retained_bytes))
    }

    /// The one storage claim the home surface makes, in ADR-0019's vocabulary.
    pub(crate) fn claim(self) -> String {
        if self.recordings == 0 {
            return "No recordings yet — Sotto is storing nothing on this Mac.".to_owned();
        }
        if self.retained_bytes == 0 {
            return format!("{} · no retained media — all on this Mac.", self.count());
        }
        format!(
            "{} · {} retained — all on this Mac.",
            self.count(),
            format_bytes(self.retained_bytes)
        )
    }
}

/// Builds the rail's search text, and refreshes the names the rail displays.
///
/// Both names are indexed, deliberately. Someone who renames a recording to `Standup` must still
/// find it by the `Chat | BTU Daily Standup | Microsoft Teams` they remember, so the chosen name
/// never replaces the captured one in the index — it is added to it.
pub(crate) fn search_index(
    database: &Path,
    meetings: &[SessionSummary],
) -> BTreeMap<SessionId, String> {
    let Ok(store) = Store::open(database) else {
        return BTreeMap::new();
    };
    meetings
        .iter()
        .map(|meeting| {
            let mut text = target_title(meeting).to_lowercase();
            text.push(' ');
            text.push_str(&meeting.capture_target.display_name.to_lowercase());
            if let Some(title) = &meeting.title {
                text.push(' ');
                text.push_str(&title.as_str().to_lowercase());
            }
            if let Ok(events) = store.load_session(meeting.id) {
                for event in events {
                    match event.payload() {
                        EventPayload::UtteranceFinal(value)
                        | EventPayload::UtterancePartial(value) => {
                            text.push(' ');
                            text.push_str(&value.text.to_lowercase());
                        }
                        EventPayload::UserAnnotation(value) => {
                            text.push(' ');
                            text.push_str(&value.text.to_lowercase());
                        }
                        _ => {}
                    }
                }
            }
            if let Ok(Some(derived)) = store.load_derived_transcript(meeting.id) {
                for utterance in derived.utterances {
                    text.push(' ');
                    text.push_str(&utterance.text.to_lowercase());
                }
            }
            if let Ok(Some(report)) = load_latest_grounded_notes(&store, meeting.id) {
                text.push(' ');
                text.push_str(&format!("{:?}", report.notes).to_lowercase());
            }
            (meeting.id, text)
        })
        .collect()
}

/// One presented library entry, resolved before any element is built so grouping, labelling, and
/// filtering are testable without a window.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct RailRow {
    pub(crate) id: SessionId,
    pub(crate) title: String,
    /// The vendored asset path for this row's marker.
    pub(crate) icon: &'static str,
    pub(crate) meta: String,
    pub(crate) live: bool,
    pub(crate) selected: bool,
}

/// Groups the catalogue the way the mock does: a running recording leads under `Now`, then
/// calendar-day groups newest first. Filtering matches the title as well as the indexed body so a
/// recording that started seconds ago — and is therefore not yet indexed — still finds itself.
///
/// Rows are named by [`recording_name`], so a renamed recording is grouped and labelled by the
/// name its owner chose while still being findable by the one it was captured under.
pub(crate) fn group_rail(
    meetings: &[SessionSummary],
    selected: Option<SessionId>,
    live: Option<SessionId>,
    query: &str,
    index: &BTreeMap<SessionId, String>,
    now_unix_ms: u64,
) -> Vec<(String, Vec<RailRow>)> {
    let needle = query.trim().to_lowercase();
    let mut ordered = meetings.iter().collect::<Vec<_>>();
    ordered.sort_by(|left, right| {
        let live_first = (live == Some(right.id)).cmp(&(live == Some(left.id)));
        live_first
            .then_with(|| right.started_at_unix_ms.cmp(&left.started_at_unix_ms))
            .then_with(|| right.id.get().cmp(&left.id.get()))
    });

    let mut groups: Vec<(String, Vec<RailRow>)> = Vec::new();
    for meeting in ordered {
        let title = recording_name(meeting);
        // A renamed recording is still findable by what was captured: the chosen name is matched
        // here, and the captured one lives in the index this also searches.
        let matched = needle.is_empty()
            || title.to_lowercase().contains(&needle)
            || target_title(meeting).to_lowercase().contains(&needle)
            || index
                .get(&meeting.id)
                .is_some_and(|text| text.contains(&needle));
        if !matched {
            continue;
        }
        let is_live = live == Some(meeting.id);
        let label = if is_live {
            "Now".to_owned()
        } else {
            day_label(meeting.started_at_unix_ms, now_unix_ms)
        };
        let row = RailRow {
            id: meeting.id,
            title,
            icon: if meeting.capture_target.is_microphone_only() {
                icons::MICROPHONE
            } else {
                icons::CAPTURED
            },
            meta: row_meta(meeting, is_live),
            live: is_live,
            selected: selected == Some(meeting.id),
        };
        match groups.last_mut() {
            Some((existing, rows)) if *existing == label => rows.push(row),
            _ => groups.push((label, vec![row])),
        }
    }
    groups
}

/// Renders the rail over the catalogue, narrowed by whatever the **toolbar's** search holds.
///
/// The search field itself is not in here any more. T083 moved it to the toolbar for one reason:
/// it used to live in this rail, so collapsing the rail would have taken the search with it — and a
/// control that disappears with the thing it filters is not a control a person can use to find
/// something. `query` arrives already read from that field; the matching rule is unchanged and
/// still spans titles *and* indexed bodies, which is [`group_rail`]'s job.
pub(crate) fn render(
    meetings: Vec<SessionSummary>,
    selected: Option<SessionId>,
    live: Option<SessionId>,
    query: &str,
    index: &BTreeMap<SessionId, String>,
    footprint: LibraryFootprint,
    cx: &mut Context<MeetingWorkspace>,
) -> AnyElement {
    let tokens = WorkspaceTokens::resolve(cx);
    let total = meetings.len();
    let groups = group_rail(&meetings, selected, live, query, index, now_unix_ms());
    let filtering = !query.trim().is_empty();
    let renaming = renaming_session(cx);

    div()
        .debug_selector(|| "library-rail".into())
        .size_full()
        .min_w_0()
        .overflow_hidden()
        .flex()
        .flex_col()
        .border_r_1()
        .border_color(tokens.line)
        .bg(tokens.ground)
        .child(render_head(total, tokens))
        .child(render_home_row(selected.is_none(), footprint, tokens, cx))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .min_h_0()
                .px(px(6.0))
                .pb(Space::MD)
                .overflow_hidden()
                .overflow_y_scrollbar()
                .when(groups.is_empty(), |view| {
                    view.child(
                        div()
                            .px(Space::SM)
                            .py(Space::SM)
                            .text_size(px(12.0))
                            .text_color(tokens.muted)
                            .child(if filtering {
                                "Nothing here matches that search."
                            } else {
                                "No recordings yet."
                            }),
                    )
                })
                .children(groups.into_iter().flat_map(|(label, rows)| {
                    let heading = div()
                        .px(Space::SM)
                        .pt(px(10.0))
                        .pb(Space::XS)
                        .text_size(TypeScale::META)
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(tokens.faint)
                        .child(label.to_uppercase())
                        .into_any_element();
                    std::iter::once(heading)
                        .chain(rows.into_iter().map(|row| {
                            if renaming == Some(row.id) {
                                render_rename_row(&row, tokens, cx)
                            } else {
                                render_row(row, tokens, cx)
                            }
                        }))
                        .collect::<Vec<_>>()
                })),
        )
        .into_any_element()
}

fn render_head(total: usize, tokens: WorkspaceTokens) -> AnyElement {
    ControlRow::new()
        .child(
            ControlRole::Essential,
            div()
                .text_size(TypeScale::META)
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(tokens.faint)
                .child("LIBRARY"),
        )
        .spacer()
        .child(
            ControlRole::Essential,
            div()
                .debug_selector(|| "library-count".into())
                .font_family("Menlo")
                .text_size(TypeScale::META)
                .text_color(tokens.faint)
                .child(total.to_string()),
        )
        .finish()
        .gap(Space::SM)
        .px(Space::MD)
        .pt(Space::MD)
        .pb(Space::SM)
        .into_any_element()
}

/// What one rail entry shows. A struct rather than a parameter list because the trailing control
/// is optional and a seven-argument face would be read by nobody.
struct RailFace {
    icon: &'static str,
    title: String,
    title_selector: &'static str,
    meta: String,
    meta_color: Rgba,
    selected: bool,
    /// The entry's own control, rendered after the text. Only the open recording has one.
    trailing: Option<AnyElement>,
}

/// The shared face of every rail entry — Home and each recording alike — so the rail's one
/// destination that is not a recording still reads as part of the same list.
fn rail_face(face: RailFace, tokens: WorkspaceTokens) -> impl IntoElement {
    let RailFace {
        icon,
        title,
        title_selector,
        meta,
        meta_color,
        selected,
        trailing,
    } = face;
    let row = ControlRow::for_width(RAIL_WIDTH)
        .child(
            ControlRole::Essential,
            div()
                .size(px(26.0))
                .rounded(px(7.0))
                .flex()
                .items_center()
                .justify_center()
                .border_1()
                .border_color(tokens.line_soft)
                .bg(if selected {
                    tokens.surface
                } else {
                    tokens.sunken
                })
                .text_size(px(13.0))
                .text_color(tokens.muted)
                .child(icons::marker(icon)),
        )
        .child(
            ControlRole::Ellipsizing,
            div()
                .min_w_0()
                .overflow_hidden()
                .flex()
                .flex_col()
                .child(
                    div()
                        .min_w_0()
                        .overflow_hidden()
                        .whitespace_nowrap()
                        .text_ellipsis()
                        .debug_selector(move || title_selector.into())
                        .text_size(px(12.5))
                        .font_weight(FontWeight::SEMIBOLD)
                        .child(title),
                )
                .child(
                    div()
                        .min_w_0()
                        .overflow_hidden()
                        .whitespace_nowrap()
                        .text_ellipsis()
                        .font_family("Menlo")
                        .text_size(TypeScale::META)
                        .text_color(meta_color)
                        .child(meta),
                ),
        );
    match trailing {
        Some(control) => row.child(ControlRole::Essential, control),
        None => row,
    }
    .finish()
    .gap(px(9.0))
}

/// The rail's Home entry: the way back out of a recording, and the only entry selected when
/// nothing is open. It deselects; it never stops, starts, or discards anything.
fn render_home_row(
    selected: bool,
    footprint: LibraryFootprint,
    tokens: WorkspaceTokens,
    cx: &mut Context<MeetingWorkspace>,
) -> AnyElement {
    div()
        .px(px(6.0))
        .pb(Space::SM)
        .child(
            div()
                .debug_selector(|| "library-home".into())
                .id("library-home")
                .w_full()
                .min_w_0()
                .overflow_hidden()
                .px(Space::SM)
                .py(px(7.0))
                .rounded(px(8.0))
                .cursor_pointer()
                .when(selected, |view| view.bg(tokens.accent_wash))
                .when(!selected, |view| {
                    view.hover(move |view| view.bg(tokens.surface_2))
                })
                .tooltip(|window, cx| {
                    Tooltip::new("Home — the ways a recording begins").build(window, cx)
                })
                .on_click(cx.listener(|this, _, _, cx| this.show_home(cx)))
                .child(rail_face(
                    RailFace {
                        icon: icons::HOME,
                        title: "Home".to_owned(),
                        title_selector: "library-home-title",
                        meta: footprint.meta(),
                        meta_color: tokens.faint,
                        selected,
                        trailing: None,
                    },
                    tokens,
                )),
        )
        .into_any_element()
}

fn render_row(
    row: RailRow,
    tokens: WorkspaceTokens,
    cx: &mut Context<MeetingWorkspace>,
) -> AnyElement {
    let id = row.id;
    let tooltip = row.title.clone();
    let key = row_key(id);
    let selected = row.selected;
    let content = rail_face(
        RailFace {
            icon: row.icon,
            title: row.title.clone(),
            title_selector: "session-rail-title",
            meta: row.meta,
            meta_color: if row.live { tokens.live } else { tokens.faint },
            selected,
            trailing: selected.then(|| rename_control(id, key, row.title, cx)),
        },
        tokens,
    );
    div()
        .debug_selector(|| "library-row".into())
        .id(("library-row", key))
        .w_full()
        .min_w_0()
        .overflow_hidden()
        .px(Space::SM)
        .py(px(7.0))
        .rounded(px(8.0))
        .cursor_pointer()
        .when(selected, |view| view.bg(tokens.accent_wash))
        .when(!selected, |view| {
            view.hover(move |view| view.bg(tokens.surface_2))
        })
        .tooltip(move |window, cx| Tooltip::new(tooltip.clone()).build(window, cx))
        .on_click(cx.listener(move |this, _, _, cx| this.select_meeting(id, cx)))
        .child(content)
        .into_any_element()
}

/// A stable element key for one session. Element ids take a `u64`; session ids are `u128`
/// nanosecond stamps, so the low half is taken rather than the value truncated arbitrarily.
fn row_key(id: SessionId) -> u64 {
    u64::try_from(id.get() & u128::from(u64::MAX)).unwrap_or(u64::MAX)
}

/// The rename gesture, on the entry a person is looking at.
///
/// It appears on the **open** recording only, and that is the whole of the design argument: the
/// rail is the product's navigation and visual calm is a hard requirement, so a control on all
/// forty rows would be forty controls nobody asked for. The row a person has selected is the one
/// they are reading the name of when they decide it is wrong.
fn rename_control(
    id: SessionId,
    key: u64,
    current: String,
    cx: &mut Context<MeetingWorkspace>,
) -> AnyElement {
    div()
        // The row underneath opens the recording. Renaming it must not also re-open it.
        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .child(
            Button::new(("rename-recording", key))
                .label("Rename")
                .ghost()
                .xsmall()
                .tooltip("Rename this recording")
                .debug_selector(|| "rail-rename".into())
                .on_click(cx.listener(move |_, _, window, cx| {
                    begin_rename(id, current.clone(), window, cx);
                })),
        )
        .into_any_element()
}

/// The rail's open rename, if one is open.
///
/// The editor is a [`Global`] because the rail is rendered from a catalogue and a `Context`, and
/// has no field of its own to hold a live text input in. It holds the subscription too, so
/// finishing a rename drops the input, its listener, and the editor in one move.
struct RenameEditor {
    session: SessionId,
    input: Entity<InputState>,
    _subscription: Subscription,
}

#[derive(Default)]
struct RailRename(Option<RenameEditor>);

impl Global for RailRename {}

fn renaming_session(cx: &Context<MeetingWorkspace>) -> Option<SessionId> {
    cx.try_global::<RailRename>()
        .and_then(|state| state.0.as_ref())
        .map(|editor| editor.session)
}

fn rename_input(cx: &Context<MeetingWorkspace>) -> Option<Entity<InputState>> {
    cx.try_global::<RailRename>()
        .and_then(|state| state.0.as_ref())
        .map(|editor| editor.input.clone())
}

/// Opens the editor on one row, seeded with the name that row is showing.
///
/// Seeding with the *displayed* name — not with an empty field — means a person editing a captured
/// window title starts from it rather than retyping it, and re-opening a rename shows what they
/// chose last time.
fn begin_rename(
    id: SessionId,
    current: String,
    window: &mut Window,
    cx: &mut Context<MeetingWorkspace>,
) {
    if renaming_session(cx) == Some(id) {
        cancel_rename(cx);
        return;
    }
    let input = cx.new(|cx| InputState::new(window, cx).placeholder("Name this recording"));
    input.update(cx, |state, cx| {
        state.set_value(current, window, cx);
        state.focus(window, cx);
    });
    let subscription = cx.subscribe_in(
        &input,
        window,
        |this: &mut MeetingWorkspace, _, event: &InputEvent, _, cx| {
            if matches!(event, InputEvent::PressEnter { .. }) {
                commit_rename(this, cx);
            }
        },
    );
    cx.set_global(RailRename(Some(RenameEditor {
        session: id,
        input,
        _subscription: subscription,
    })));
    cx.notify();
}

/// Closes the editor without writing anything.
fn cancel_rename(cx: &mut Context<MeetingWorkspace>) {
    cx.set_global(RailRename::default());
    cx.notify();
}

/// Writes the chosen name, or clears it, and closes the editor either way.
///
/// A blank field is not an error and not a blank name: it removes the chosen title, and the
/// recording goes back to being named by what was captured. That is the only way back to the
/// default, and it is the same rule the store enforces — a blank title is never persisted.
///
/// Nothing about the capture target is written here. `set_session_title` touches one row in one
/// table, and it is not the row that records what was captured.
fn commit_rename(workspace: &mut MeetingWorkspace, cx: &mut Context<MeetingWorkspace>) {
    let Some((id, value)) = cx
        .try_global::<RailRename>()
        .and_then(|state| state.0.as_ref())
        .map(|editor| (editor.session, editor.input.read(cx).value().to_string()))
    else {
        return;
    };
    let chosen = RecordingTitle::new(&value);
    let written = Store::open(&workspace.database)
        .and_then(|store| store.set_session_title(id, chosen.as_ref()));
    cx.set_global(RailRename::default());
    match written {
        Ok(()) => {
            // The catalogue carries the name, so it is re-read rather than patched in place, and
            // the search index is rebuilt over both names so the rename is findable immediately.
            workspace.message = workspace
                .notes
                .update(cx, |notes, _| notes.refresh_catalogue())
                .err();
            if workspace.message.is_none() {
                workspace.rebuild_library_index(cx);
            }
            let ready = workspace
                .reasoning_backend(cx)
                .is_ok_and(|backend| backend.is_some());
            workspace.sync_ask_scope(ready, cx);
        }
        Err(error) => {
            workspace.message = Some(format!("Could not rename this recording: {error}"));
        }
    }
    cx.notify();
}

/// The open rename, rendered in the row's own place so the name is edited where it is read.
fn render_rename_row(
    row: &RailRow,
    tokens: WorkspaceTokens,
    cx: &mut Context<MeetingWorkspace>,
) -> AnyElement {
    let Some(input) = rename_input(cx) else {
        return render_row(row.clone(), tokens, cx);
    };
    let key = row_key(row.id);
    div()
        .debug_selector(|| "library-row".into())
        .id(("library-rename-row", key))
        .w_full()
        .min_w_0()
        .overflow_hidden()
        .px(Space::SM)
        .py(px(7.0))
        .rounded(px(8.0))
        .bg(tokens.accent_wash)
        // Editing the name must not re-open the recording underneath.
        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .on_key_down(cx.listener(|_, event: &gpui::KeyDownEvent, _, cx| {
            if event.keystroke.key.as_str() == "escape" {
                cancel_rename(cx);
            }
        }))
        .child(
            div()
                .debug_selector(|| "rail-rename-input".into())
                .child(Input::new(&input).xsmall()),
        )
        .child(
            div()
                .mt(px(3.0))
                .text_size(TypeScale::META)
                .text_color(tokens.faint)
                .child("Return saves · Escape cancels · empty restores the captured name"),
        )
        .into_any_element()
}

/// One of the three equally weighted ways a session begins.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum StartChoice {
    /// Screen and audio from one picked application. Wired today.
    CaptureApp,
    /// Microphone only: no picker, no screen, no application audio. Wired by T050.
    Microphone,
    /// A file the user already has becomes a session. T071; not built.
    Import,
}

/// Presentation order. All three carry identical weight — same card, same chrome, same type.
pub(crate) const START_CHOICES: [StartChoice; 3] = [
    StartChoice::CaptureApp,
    StartChoice::Microphone,
    StartChoice::Import,
];

impl StartChoice {
    pub(crate) const fn icon(self) -> &'static str {
        match self {
            Self::CaptureApp => icons::CAPTURED,
            Self::Microphone => icons::MICROPHONE,
            Self::Import => icons::IMPORT,
        }
    }

    pub(crate) const fn title(self) -> &'static str {
        match self {
            Self::CaptureApp => "Capture an app",
            Self::Microphone => "Record just your microphone",
            Self::Import => "Import audio or video",
        }
    }

    pub(crate) const fn description(self) -> &'static str {
        match self {
            Self::CaptureApp => "Screen and audio from one app — nothing else is heard.",
            Self::Microphone => "A voice note, an interview in the room, thinking out loud.",
            Self::Import => {
                "A file becomes a session — same transcript, same notes, same retention."
            }
        }
    }

    /// The plain sentence an entry point says about itself when it cannot do its job. `None` means
    /// the capability exists and the card acts.
    pub(crate) const fn unavailability(self) -> Option<&'static str> {
        match self {
            Self::CaptureApp | Self::Microphone => None,
            Self::Import => Some(
                "Not built yet — Sotto cannot turn a file into a session, so this does nothing.",
            ),
        }
    }

    const fn element_id(self) -> &'static str {
        match self {
            Self::CaptureApp => "start-choice-capture",
            Self::Microphone => "start-choice-microphone",
            Self::Import => "start-choice-import",
        }
    }

    fn begin(self, workspace: &mut MeetingWorkspace, cx: &mut Context<MeetingWorkspace>) {
        match self {
            Self::CaptureApp => workspace.start_scoped_session(cx),
            Self::Microphone => workspace
                .session
                .update(cx, SessionController::start_microphone_only),
            // Unreachable: `render_start_choices` attaches no click handler to an unavailable
            // choice. Kept total rather than panicking so a future wiring mistake is inert.
            Self::Import => {}
        }
    }
}

/// The home surface: the product thesis, three equal beginnings, and one true storage claim.
///
/// It renders every choice, and a choice that cannot run states its own unavailability in place
/// rather than being hidden, greyed into meaninglessness, or wired to a stub session.
///
/// `recording_in_flight` is the *lifecycle* answer, not the stage's: a person can stand on Home
/// while a capture runs, so the surface says so and points at the two things that still matter —
/// Stop in the bar above, and the running recording in the rail under `Now`.
pub(crate) fn render_start_choices(
    can_start: bool,
    recording_in_flight: bool,
    footprint: LibraryFootprint,
    cx: &mut Context<MeetingWorkspace>,
) -> AnyElement {
    let tokens = WorkspaceTokens::resolve(cx);
    div()
        .debug_selector(|| "start-choices".into())
        .flex_1()
        .min_h_0()
        .min_w_0()
        .overflow_hidden()
        .flex()
        .items_center()
        .justify_center()
        .p(px(24.0))
        .child(
            div()
                .w_full()
                .max_w(px(430.0))
                .min_w_0()
                .flex()
                .flex_col()
                .child(
                    div()
                        .text_size(px(19.0))
                        .font_weight(FontWeight::BOLD)
                        .text_color(tokens.ink)
                        .child("Record anything you can hear."),
                )
                .child(
                    div()
                        .mt(Space::SM)
                        .mb(px(20.0))
                        .text_size(px(13.0))
                        .text_color(tokens.muted)
                        .child(
                            "An app on screen, just your voice, or a file you already have. Sotto \
                             keeps the recording on this Mac, transcribes it here, and writes a \
                             summary you can check line by line.",
                        ),
                )
                .when(recording_in_flight, |view| {
                    view.child(
                        div()
                            .debug_selector(|| "home-live-note".into())
                            .mb(px(16.0))
                            .px(px(12.0))
                            .py(px(10.0))
                            .rounded(px(10.0))
                            .border_1()
                            .border_color(tokens.live_line)
                            .bg(tokens.live_wash)
                            .text_size(px(12.0))
                            .text_color(tokens.live_ink)
                            .child(
                                "A recording is running. Stop is in the bar above, and the \
                                 recording itself is in the Library under Now.",
                            ),
                    )
                })
                .children(
                    START_CHOICES
                        .into_iter()
                        .map(|choice| render_start_card(choice, can_start, tokens, cx)),
                )
                .child(
                    div()
                        .debug_selector(|| "home-footprint".into())
                        .mt(px(14.0))
                        .text_size(px(11.5))
                        .text_color(tokens.faint)
                        .child(footprint.claim()),
                )
                .child(
                    div()
                        .mt(px(2.0))
                        .text_size(px(11.5))
                        .text_color(tokens.faint)
                        .child("Recordings are transcribed on this Mac and never uploaded."),
                ),
        )
        .into_any_element()
}

fn render_start_card(
    choice: StartChoice,
    can_start: bool,
    tokens: WorkspaceTokens,
    cx: &mut Context<MeetingWorkspace>,
) -> AnyElement {
    let blocked = choice.unavailability().or(if can_start {
        None
    } else {
        Some("A recording is already running — stop it first.")
    });
    let card = div()
        .debug_selector(move || choice.element_id().into())
        .id(choice.element_id())
        .w_full()
        .flex()
        .items_center()
        .gap(Space::MD)
        .px(px(14.0))
        .py(px(13.0))
        .mb(Space::SM)
        .rounded(px(11.0))
        .border_1()
        .border_color(tokens.line)
        .bg(tokens.surface)
        .child(
            div()
                .size(px(34.0))
                .flex_none()
                .rounded(px(9.0))
                .flex()
                .items_center()
                .justify_center()
                .border_1()
                .border_color(tokens.accent_line)
                .bg(tokens.accent_wash)
                .text_size(px(16.0))
                .text_color(tokens.accent)
                .child(icons::marker(choice.icon())),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .overflow_hidden()
                .flex()
                .flex_col()
                .child(
                    div()
                        .text_size(px(13.0))
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(tokens.ink)
                        .child(choice.title()),
                )
                .child(
                    div()
                        .mt(px(1.0))
                        .text_size(px(12.0))
                        .text_color(tokens.muted)
                        .child(choice.description()),
                )
                .when_some(blocked, |view, note| {
                    view.child(
                        div()
                            .mt(Space::XS)
                            .text_size(px(11.5))
                            .text_color(tokens.warn)
                            .child(note),
                    )
                }),
        );
    if blocked.is_some() {
        return card.cursor_default().into_any_element();
    }
    card.cursor_pointer()
        .hover(move |view| view.bg(tokens.surface_2).border_color(tokens.accent_line))
        .on_click(cx.listener(move |this, _, _, cx| choice.begin(this, cx)))
        .into_any_element()
}

/// The name every surface falls back to when nothing else names a recording.
///
/// It is never reached by a capture, whose target always names something. It exists for the
/// recording that has no capture target at all — an import (T071) — and as the last guard against
/// a blank row.
pub(crate) const UNTITLED_RECORDING: &str = "Untitled recording";

/// The name a recording has when nobody has renamed it: exactly today's behaviour.
///
/// This is derived from the capture target and stays derived from it. A rename never rewrites what
/// this reads, so this remains the recording's original identity for as long as it exists.
pub(crate) fn target_title(meeting: &SessionSummary) -> String {
    let derived = meeting
        .capture_target
        .window_title
        .clone()
        .unwrap_or_else(|| meeting.capture_target.display_name.clone());
    if derived.trim().is_empty() {
        return UNTITLED_RECORDING.to_owned();
    }
    derived
}

/// What the recording is called: the chosen name if there is one, otherwise [`target_title`].
///
/// Resolution happens here, at the point of display, rather than by writing a default title into
/// the store. A recording nobody renames therefore has *no* title at all, which is what makes
/// "titled exactly as it is today" true by construction rather than by copying today's string into
/// a row that would then go stale.
///
/// **This is the function every surface that names a recording should call.** [`target_title`]
/// answers a different question — what was captured — and is the fallback, not the name.
pub(crate) fn recording_name(meeting: &SessionSummary) -> String {
    meeting
        .title
        .as_ref()
        .map_or_else(|| target_title(meeting), RecordingTitle::to_string)
}

fn row_meta(meeting: &SessionSummary, live: bool) -> String {
    if live {
        return "recording".to_owned();
    }
    let source = if meeting.capture_target.is_microphone_only() {
        "microphone"
    } else {
        "captured"
    };
    meeting.ended_at_unix_ms.map_or_else(
        || format!("not finalized · {source}"),
        |ended| {
            format!(
                "{} · {source}",
                format_duration(meeting.started_at_unix_ms, ended)
            )
        },
    )
}

fn format_duration(started_at_unix_ms: u64, ended_at_unix_ms: u64) -> String {
    let seconds = ended_at_unix_ms.saturating_sub(started_at_unix_ms) / 1_000;
    let (hours, minutes, remainder) = (seconds / 3_600, (seconds % 3_600) / 60, seconds % 60);
    if hours > 0 {
        format!("{hours}:{minutes:02}:{remainder:02}")
    } else {
        format!("{minutes}:{remainder:02}")
    }
}

fn day_label(started_at_unix_ms: u64, now_unix_ms: u64) -> String {
    let started = local_date(started_at_unix_ms);
    let today = local_date(now_unix_ms);
    match (today - started).num_days() {
        0 => "Today".to_owned(),
        1 => "Yesterday".to_owned(),
        2..=6 => started.format("%A").to_string(),
        _ if started.year() == today.year() => started.format("%b %-d").to_string(),
        _ => started.format("%b %-d, %Y").to_string(),
    }
}

fn local_date(unix_ms: u64) -> chrono::NaiveDate {
    let stamp = UNIX_EPOCH + Duration::from_millis(unix_ms);
    chrono::DateTime::<chrono::Local>::from(stamp).date_naive()
}

fn now_unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |value| {
            u64::try_from(value.as_millis()).unwrap_or(u64::MAX)
        })
}

#[cfg(test)]
mod tests {
    use std::{collections::BTreeMap, ops::Deref as _, sync::Arc};

    use gpui::{
        AppContext as _, Bounds, Modifiers, TestAppContext, VisualTestContext, WindowBounds,
        WindowOptions, point, px, size,
    };
    use gpui_component::Root;
    use rag::{SessionSummary, Store};
    use secrecy::SecretString;
    use sotto_core::{CaptureTarget, Session, SessionId, TargetKind, types::RecordingTitle};

    use super::{
        LibraryFootprint, START_CHOICES, StartChoice, day_label, format_duration, group_rail,
        icons, recording_name, target_title,
    };
    use crate::{mcp, reasoning, session};

    const DAY_MS: u64 = 86_400_000;

    /// Narrowest supported workspace viewport, matching the shell's own acceptance width. The
    /// rail's title truncation is pinned here so a layout change that reintroduces clipping fails a
    /// test rather than reaching a maintainer.
    const MIN_RAIL_ACCEPTANCE_WIDTH: gpui::Pixels = px(680.0);

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

    fn captured(id: u128, title: &str, started: u64, ended: Option<u64>) -> SessionSummary {
        SessionSummary {
            id: SessionId::new(id),
            started_at_unix_ms: started,
            ended_at_unix_ms: ended,
            title: None,
            capture_target: CaptureTarget {
                bundle_id: Some("com.example.app".into()),
                display_name: "Example".into(),
                window_title: Some(title.into()),
                kind: TargetKind::Window,
                audio_scoped: true,
            },
        }
    }

    fn microphone(id: u128, started: u64, ended: Option<u64>) -> SessionSummary {
        SessionSummary {
            id: SessionId::new(id),
            started_at_unix_ms: started,
            ended_at_unix_ms: ended,
            title: None,
            capture_target: CaptureTarget::microphone_only(),
        }
    }

    #[test]
    fn window_title_is_the_catalogue_label() {
        let summary = captured(1, "Planning", 0, None);
        assert_eq!(
            target_title(&summary),
            "Planning",
            "a captured window's own title is the library label"
        );
        assert_eq!(
            recording_name(&summary),
            "Planning",
            "a recording nobody renamed is named exactly as it is today"
        );
    }

    #[test]
    fn a_chosen_name_replaces_the_label_without_touching_what_was_captured() {
        let mut summary = captured(1, "Chat | BTU Daily Standup | Microsoft Teams", 0, None);
        let captured_target = summary.capture_target.clone();
        summary.title = RecordingTitle::new("  Standup  ");

        assert_eq!(
            recording_name(&summary),
            "Standup",
            "the chosen name is what the recording is called"
        );
        assert_eq!(
            target_title(&summary),
            "Chat | BTU Daily Standup | Microsoft Teams",
            "the captured window title survives the rename as the recording's origin"
        );
        assert_eq!(
            summary.capture_target, captured_target,
            "a rename must leave every claim about what was captured untouched"
        );
    }

    #[test]
    fn a_recording_with_nothing_to_name_it_never_renders_blank() {
        let mut summary = captured(1, "  ", 0, None);
        summary.capture_target.window_title = Some("   ".to_owned());
        summary.capture_target.display_name = String::new();
        assert_eq!(
            recording_name(&summary),
            super::UNTITLED_RECORDING,
            "a recording with no usable capture-time name still names itself"
        );
        assert!(
            RecordingTitle::new("   ").is_none(),
            "a whitespace-only rename is refused rather than stored"
        );
    }

    #[test]
    fn a_microphone_only_recording_is_labelled_by_its_scope() {
        let summary = microphone(2, 0, None);
        assert_eq!(
            target_title(&summary),
            "Microphone only",
            "a microphone-only session has no window to name it"
        );
        assert_eq!(
            super::row_meta(&summary, false),
            "not finalized · microphone",
            "the rail must name the source of a microphone-only recording"
        );
    }

    #[test]
    fn durations_read_as_a_recording_length() {
        assert_eq!(format_duration(0, 92_000), "1:32", "under an hour is m:ss");
        assert_eq!(
            format_duration(0, 4_360_000),
            "1:12:40",
            "an hour or more carries the hour field"
        );
        assert_eq!(
            format_duration(500, 400),
            "0:00",
            "an impossible end must not underflow"
        );
    }

    #[test]
    fn day_labels_follow_the_users_calendar() {
        let now = 1_786_625_633_040;
        assert_eq!(day_label(now, now), "Today", "same day reads as Today");
        assert_eq!(
            day_label(now - DAY_MS, now),
            "Yesterday",
            "one day back reads as Yesterday"
        );
        let earlier = day_label(now - 3 * DAY_MS, now);
        assert!(
            !earlier.is_empty() && earlier != "Today" && earlier != "Yesterday",
            "within the week falls back to a weekday name, got {earlier}"
        );
        let old = day_label(now - 400 * DAY_MS, now);
        assert!(
            old.contains(char::is_numeric),
            "a recording from another year must carry a dated label, got {old}"
        );
    }

    #[test]
    fn the_rail_lists_a_running_recording_and_keeps_it_after_it_ends()
    -> Result<(), Box<dyn std::error::Error>> {
        let now = 1_786_625_633_040;
        let live_id = SessionId::new(9);
        let running = vec![
            captured(
                7,
                "Yesterday's call",
                now - DAY_MS,
                Some(now - DAY_MS + 60_000),
            ),
            captured(9, "Live capture", now - 5_000, None),
        ];
        let groups = group_rail(&running, None, Some(live_id), "", &BTreeMap::new(), now);
        let (label, rows) = groups
            .first()
            .ok_or("a running recording must be grouped")?;
        assert_eq!(label, "Now", "a running recording leads the rail under Now");
        let row = rows.first().ok_or("the Now group must carry the row")?;
        assert!(row.live, "the running row must be marked live");
        assert_eq!(
            row.meta, "recording",
            "a running recording reports itself as recording, not a duration"
        );

        let stopped = vec![
            captured(
                7,
                "Yesterday's call",
                now - DAY_MS,
                Some(now - DAY_MS + 60_000),
            ),
            captured(9, "Live capture", now - 5_000, Some(now)),
        ];
        let groups = group_rail(&stopped, Some(live_id), None, "", &BTreeMap::new(), now);
        let (label, rows) = groups.first().ok_or("the stopped recording must remain")?;
        assert_eq!(label, "Today", "once stopped it joins its calendar day");
        let row = rows.first().ok_or("the Today group must carry the row")?;
        assert!(!row.live, "a stopped recording is no longer live");
        assert!(row.selected, "the stopped recording stays selected");
        assert_eq!(
            row.meta, "0:05 · captured",
            "a stopped recording reports its length and its source"
        );
        Ok(())
    }

    #[test]
    fn search_matches_titles_and_indexed_bodies() -> Result<(), Box<dyn std::error::Error>> {
        let now = 1_786_625_633_040;
        let meetings = vec![
            captured(1, "Sprint planning", now, Some(now + 1_000)),
            captured(2, "CS231n lecture", now, Some(now + 1_000)),
        ];
        let mut index = BTreeMap::new();
        index.insert(SessionId::new(2), "batch normalization dropout".to_owned());

        let by_title = group_rail(&meetings, None, None, "sprint", &index, now);
        let titles = by_title
            .iter()
            .flat_map(|(_, rows)| rows.iter().map(|row| row.title.clone()))
            .collect::<Vec<_>>();
        assert_eq!(
            titles,
            vec!["Sprint planning".to_owned()],
            "a title search must narrow the rail"
        );

        let by_body = group_rail(&meetings, None, None, "dropout", &index, now);
        let titles = by_body
            .iter()
            .flat_map(|(_, rows)| rows.iter().map(|row| row.title.clone()))
            .collect::<Vec<_>>();
        assert_eq!(
            titles,
            vec!["CS231n lecture".to_owned()],
            "searching what was said and written must reach the indexed body"
        );

        let unmatched = group_rail(&meetings, None, None, "nothing here", &index, now);
        assert!(
            unmatched.is_empty(),
            "a search with no match must show no rows rather than everything"
        );
        Ok(())
    }

    /// The rename must not cost someone the name they already know the recording by.
    #[test]
    fn a_renamed_recording_is_found_by_both_names() -> Result<(), Box<dyn std::error::Error>> {
        let now = 1_786_625_633_040;
        let mut renamed = captured(
            1,
            "Chat | BTU Daily Standup | Microsoft Teams",
            now,
            Some(now + 1_000),
        );
        renamed.title = RecordingTitle::new("Standup");
        let meetings = vec![
            renamed,
            captured(2, "CS231n lecture", now, Some(now + 1_000)),
        ];
        let index = BTreeMap::new();

        for needle in ["standup", "microsoft teams", "btu daily"] {
            let rows = group_rail(&meetings, None, None, needle, &index, now)
                .into_iter()
                .flat_map(|(_, rows)| rows)
                .map(|row| row.title)
                .collect::<Vec<_>>();
            assert_eq!(
                rows,
                vec!["Standup".to_owned()],
                "{needle:?} must still reach the renamed recording, under its chosen name"
            );
        }
        Ok(())
    }

    /// The rail's search text is what the toolbar query is matched against for anything the row
    /// itself does not carry, so both names have to be in it.
    #[test]
    fn the_search_index_carries_the_chosen_and_the_captured_name()
    -> Result<(), Box<dyn std::error::Error>> {
        let directory = tempfile::tempdir()?;
        let database = directory.path().join("sotto.sqlite3");
        let session_id = SessionId::new(1_786_625_633_040_598_000);
        let mut record = Session::new(
            session_id,
            CaptureTarget {
                bundle_id: Some("com.microsoft.teams2".into()),
                display_name: "Microsoft Teams".into(),
                window_title: Some("Chat | BTU Daily Standup | Microsoft Teams".into()),
                kind: TargetKind::Window,
                audio_scoped: true,
            },
            1_786_625_633_040,
        );
        record.end(1_786_625_700_000);
        let store = Store::open(&database)?;
        store.save_session(&record)?;
        let title = RecordingTitle::new("Standup").ok_or("Standup is a title")?;
        store.set_session_title(session_id, Some(&title))?;
        drop(store);

        let meetings = Store::open(&database)?.list_sessions()?;
        let index = super::search_index(&database, &meetings);
        let text = index
            .get(&session_id)
            .ok_or("the session must be indexed")?;
        assert!(
            text.contains("standup") && text.contains("microsoft teams"),
            "the index must carry the chosen name and the captured one, got {text}"
        );
        Ok(())
    }

    #[test]
    fn three_equal_beginnings_are_offered_and_each_says_what_it_is() {
        assert_eq!(
            START_CHOICES.len(),
            3,
            "the idle surface offers exactly three ways to begin"
        );
        for choice in START_CHOICES {
            assert!(
                !choice.title().is_empty() && !choice.description().is_empty(),
                "{choice:?} must carry its own name and explanation"
            );
            // An unresolvable path renders as nothing at all, silently. Prove each beginning's
            // marker is really a vendored asset rather than a plausible-looking string.
            assert!(
                gpui::AssetSource::load(&icons::Assets, choice.icon())
                    .is_ok_and(|bytes| bytes.is_some()),
                "{choice:?} must carry a marker Sotto actually ships"
            );
        }
        let ids = START_CHOICES.map(StartChoice::element_id);
        assert_eq!(
            ids.iter().collect::<std::collections::BTreeSet<_>>().len(),
            3,
            "each beginning needs its own stable element identity"
        );
    }

    #[test]
    fn home_states_the_measured_footprint_and_nothing_else() {
        assert_eq!(
            LibraryFootprint::default().claim(),
            "No recordings yet — Sotto is storing nothing on this Mac.",
            "an empty library says it is storing nothing rather than showing 0 B"
        );
        let transcripts_only = LibraryFootprint {
            recordings: 3,
            retained_bytes: 0,
        };
        assert_eq!(
            transcripts_only.claim(),
            "3 recordings · no retained media — all on this Mac.",
            "recordings whose media is gone must say so, not report zero bytes"
        );
        assert_eq!(
            transcripts_only.meta(),
            "3 recordings · no retained media",
            "the rail entry carries the same truth in fewer words"
        );
        let retained = LibraryFootprint {
            recordings: 1,
            retained_bytes: 2_100_000_000,
        };
        assert_eq!(
            retained.claim(),
            "1 recording · 2.1 GB retained — all on this Mac.",
            "one recording is singular and its size is the shell's own byte formatting"
        );
        assert_eq!(
            retained.meta(),
            "1 recording · 2.1 GB",
            "the rail entry drops the claim's tail, never its numbers"
        );
    }

    #[test]
    fn an_unbuilt_beginning_states_its_own_unavailability() {
        assert_eq!(
            StartChoice::CaptureApp.unavailability(),
            None,
            "capturing an app is wired today"
        );
        assert_eq!(
            StartChoice::Microphone.unavailability(),
            None,
            "microphone-only capture is wired through SessionController::start_microphone_only"
        );
        let import = StartChoice::Import
            .unavailability()
            .unwrap_or("MISSING SENTENCE");
        assert!(
            import.contains("Not built yet"),
            "import must say plainly that it does not exist, got {import}"
        );
        assert!(
            import.contains("does nothing"),
            "import must say plainly that clicking it achieves nothing, got {import}"
        );
    }

    /// Mounts the shell the way `main.rs` does — under `gpui_component::Root`, in an activated
    /// window.
    ///
    /// The rename editor is a real `gpui_component` text input, and that widget reads the window's
    /// `Root` while it paints. A test that put `MeetingWorkspace` straight at the window root
    /// would panic inside the widget rather than exercise the gesture.
    fn mount_shell(
        cx: &mut TestAppContext,
        directory: &std::path::Path,
        database: std::path::PathBuf,
    ) -> Result<&'static mut VisualTestContext, Box<dyn std::error::Error>> {
        let reasoning_path = directory.join("reasoning.json");
        let mcp_path = directory.join("mcp.json");
        let (ingress, timeline) = cx.update(|cx| crate::devwindow::attach_ingress(cx, 16));
        let session = cx.new(|_| session::SessionController::new(ingress));
        let reasoning = cx.new(|_| {
            reasoning::ReasoningController::load(reasoning_path, Arc::new(NoOpenAiCredentials))
        });
        let mcp = cx.new(|_| mcp::McpController::load(Some(mcp_path), Arc::new(NoMcpCredentials)));
        let handle = cx.update(|cx| {
            cx.open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(Bounds {
                        origin: point(px(0.0), px(0.0)),
                        size: size(px(1_000.0), px(680.0)),
                    })),
                    ..WindowOptions::default()
                },
                move |window, cx| {
                    let view = cx.new(|cx| {
                        super::MeetingWorkspace::new(
                            database, timeline, reasoning, session, mcp, window, cx,
                        )
                    });
                    cx.new(|cx| Root::new(view, window, cx))
                },
            )
        })?;
        let visual = VisualTestContext::from_window(*handle.deref(), cx).into_mut();
        visual.update(|window, _| window.activate_window());
        visual.run_until_parked();
        Ok(visual)
    }

    /// Which recording, if any, the rail is currently renaming.
    ///
    /// Asserted through the editor's own state rather than through a missing element: GPUI's
    /// `debug_bounds` keeps the last bounds recorded under a selector, so "no input is drawn" is
    /// not a question a *rendered frame* can answer.
    fn open_editor(visual: &mut VisualTestContext) -> Option<SessionId> {
        visual.update(|_, cx| {
            cx.try_global::<super::RailRename>()
                .and_then(|state| state.0.as_ref())
                .map(|editor| editor.session)
        })
    }

    /// The whole gesture, through the controls a person uses: select the recording, rename it in
    /// the rail, and prove the new name is durable while the captured scope is not.
    #[test]
    fn renaming_from_the_rail_persists_the_name_and_never_the_capture_target()
    -> Result<(), Box<dyn std::error::Error>> {
        let mut cx = TestAppContext::single();
        cx.update(gpui_component::init);
        let dir = tempfile::tempdir()?;
        let database = dir.path().join("sotto.sqlite3");
        let session_id = SessionId::new(1_786_625_633_040_598_000);
        let target = CaptureTarget {
            bundle_id: Some("com.microsoft.teams2".into()),
            display_name: "Microsoft Teams".into(),
            window_title: Some("Chat | BTU Daily Standup | Microsoft Teams".into()),
            kind: TargetKind::Window,
            audio_scoped: true,
        };
        let mut record = Session::new(session_id, target.clone(), 1_786_625_633_040);
        record.end(1_786_625_700_000);
        Store::open(&database)?.save_session(&record)?;

        let visual = mount_shell(&mut cx, dir.path(), database.clone())?;

        // Nothing is open at launch, so no rename control is offered yet: the gesture belongs to
        // the entry a person is looking at.
        assert!(
            visual.debug_bounds("rail-rename").is_none(),
            "an unselected rail must offer no rename control"
        );
        let row = visual
            .debug_bounds("library-row")
            .ok_or_else(|| std::io::Error::other("the persisted recording must render a row"))?;
        visual.simulate_click(row.center(), Modifiers::none());
        visual.run_until_parked();

        let rename = visual.debug_bounds("rail-rename").ok_or_else(|| {
            std::io::Error::other("the open recording's row must offer a rename control")
        })?;
        visual.simulate_click(rename.center(), Modifiers::none());
        visual.run_until_parked();
        assert!(
            visual.debug_bounds("rail-rename-input").is_some(),
            "the rename must be edited in the row it names"
        );
        assert_eq!(
            open_editor(visual),
            Some(session_id),
            "the editor must be open on the row whose name is wrong"
        );

        visual.simulate_keystrokes("cmd-a s t a n d u p enter");
        visual.run_until_parked();

        // Reopening the store is the relaunch: nothing in memory answers these.
        let reopened = Store::open(&database)?;
        assert_eq!(
            reopened
                .load_session_title(session_id)?
                .map(|title| title.as_str().to_owned()),
            Some("standup".to_owned()),
            "the chosen name must outlive the process that chose it"
        );
        assert_eq!(
            reopened.load_session_record(session_id)?.capture_target(),
            &target,
            "renaming must leave the recorded capture scope exactly as it was captured"
        );
        let summaries = reopened.list_sessions()?;
        let summary = summaries
            .first()
            .ok_or_else(|| std::io::Error::other("the catalogue must still list the recording"))?;
        assert_eq!(
            recording_name(summary),
            "standup",
            "the rail names the recording by the name it was given"
        );
        assert_eq!(
            target_title(summary),
            "Chat | BTU Daily Standup | Microsoft Teams",
            "the captured window title remains the recording's origin after the rename"
        );
        assert_eq!(
            open_editor(visual),
            None,
            "Return must close the editor rather than leave the row in edit"
        );
        Ok(())
    }

    /// A blank name is not a name. Clearing the field is the way back to the captured one.
    #[test]
    fn clearing_the_name_restores_the_captured_one_rather_than_blanking_the_row()
    -> Result<(), Box<dyn std::error::Error>> {
        let mut cx = TestAppContext::single();
        cx.update(gpui_component::init);
        let dir = tempfile::tempdir()?;
        let database = dir.path().join("sotto.sqlite3");
        let session_id = SessionId::new(1_786_625_633_040_598_000);
        let mut record = Session::new(
            session_id,
            CaptureTarget {
                bundle_id: Some("com.example.app".into()),
                display_name: "Example".into(),
                window_title: Some("Weekly sync".into()),
                kind: TargetKind::Window,
                audio_scoped: true,
            },
            1_786_625_633_040,
        );
        record.end(1_786_625_700_000);
        let store = Store::open(&database)?;
        store.save_session(&record)?;
        let chosen = RecordingTitle::new("Standup").ok_or("Standup is a title")?;
        store.set_session_title(session_id, Some(&chosen))?;
        drop(store);

        let visual = mount_shell(&mut cx, dir.path(), database.clone())?;

        let row = visual
            .debug_bounds("library-row")
            .ok_or_else(|| std::io::Error::other("the persisted recording must render a row"))?;
        visual.simulate_click(row.center(), Modifiers::none());
        visual.run_until_parked();
        let rename = visual
            .debug_bounds("rail-rename")
            .ok_or_else(|| std::io::Error::other("the open recording must offer a rename"))?;
        visual.simulate_click(rename.center(), Modifiers::none());
        visual.run_until_parked();

        visual.simulate_keystrokes("cmd-a space enter");
        visual.run_until_parked();

        let reopened = Store::open(&database)?;
        assert_eq!(
            reopened.load_session_title(session_id)?,
            None,
            "a whitespace-only name must clear the title rather than persist a blank one"
        );
        let summaries = reopened.list_sessions()?;
        let summary = summaries
            .first()
            .ok_or_else(|| std::io::Error::other("the catalogue must still list the recording"))?;
        assert_eq!(
            recording_name(summary),
            "Weekly sync",
            "clearing the name returns the recording to the one it was captured under"
        );
        Ok(())
    }

    /// Escape is the way out of an editor a person opened by mistake.
    #[test]
    fn escaping_the_editor_writes_nothing() -> Result<(), Box<dyn std::error::Error>> {
        let mut cx = TestAppContext::single();
        cx.update(gpui_component::init);
        let dir = tempfile::tempdir()?;
        let database = dir.path().join("sotto.sqlite3");
        let session_id = SessionId::new(1_786_625_633_040_598_000);
        let mut record = Session::new(
            session_id,
            CaptureTarget {
                bundle_id: Some("com.example.app".into()),
                display_name: "Example".into(),
                window_title: Some("Weekly sync".into()),
                kind: TargetKind::Window,
                audio_scoped: true,
            },
            1_786_625_633_040,
        );
        record.end(1_786_625_700_000);
        Store::open(&database)?.save_session(&record)?;

        let visual = mount_shell(&mut cx, dir.path(), database.clone())?;
        let row = visual
            .debug_bounds("library-row")
            .ok_or_else(|| std::io::Error::other("the persisted recording must render a row"))?;
        visual.simulate_click(row.center(), Modifiers::none());
        visual.run_until_parked();
        let rename = visual
            .debug_bounds("rail-rename")
            .ok_or_else(|| std::io::Error::other("the open recording must offer a rename"))?;
        visual.simulate_click(rename.center(), Modifiers::none());
        visual.run_until_parked();

        visual.simulate_keystrokes("cmd-a n o p e escape");
        visual.run_until_parked();

        assert_eq!(open_editor(visual), None, "Escape must close the editor");
        assert_eq!(
            Store::open(&database)?.load_session_title(session_id)?,
            None,
            "an abandoned rename must write nothing at all"
        );
        Ok(())
    }

    #[test]
    fn a_long_rail_title_keeps_its_beginning_inside_the_rail_at_minimum_width()
    -> Result<(), Box<dyn std::error::Error>> {
        let mut cx = TestAppContext::single();
        cx.update(gpui_component::init);
        let dir = tempfile::tempdir()?;
        let database = dir.path().join("sotto.sqlite3");
        let session_id = SessionId::new(1_786_625_633_040_598_000);
        let mut record = Session::new(
            session_id,
            CaptureTarget {
                bundle_id: Some("com.example.meeting".into()),
                display_name: "Example".into(),
                window_title: Some(
                    "Quarterly platform architecture review with the whole extended team \
                     and every invited guest"
                        .into(),
                ),
                kind: TargetKind::Window,
                audio_scoped: true,
            },
            1_786_625_633_040,
        );
        record.end(1_786_625_700_000);
        Store::open(&database)?.save_session(&record)?;

        let reasoning_path = dir.path().join("reasoning.json");
        let mcp_path = dir.path().join("mcp.json");
        let (ingress, timeline) = cx.update(|cx| crate::devwindow::attach_ingress(cx, 16));
        let session = cx.new(|_| session::SessionController::new(ingress));
        let reasoning = cx.new(|_| {
            reasoning::ReasoningController::load(reasoning_path, Arc::new(NoOpenAiCredentials))
        });
        let mcp = cx.new(|_| mcp::McpController::load(Some(mcp_path), Arc::new(NoMcpCredentials)));
        let session_for_assertion = session.clone();
        let (_workspace, visual) = cx.add_window_view(move |window, cx| {
            super::MeetingWorkspace::new(database, timeline, reasoning, session, mcp, window, cx)
        });

        visual.simulate_resize(size(MIN_RAIL_ACCEPTANCE_WIDTH, px(620.0)));
        visual.refresh()?;
        visual.run_until_parked();

        let rail = visual
            .debug_bounds("library-rail")
            .ok_or_else(|| std::io::Error::other("the library rail must render"))?;
        let title = visual.debug_bounds("session-rail-title").ok_or_else(|| {
            std::io::Error::other("the persisted recording must render its title")
        })?;
        assert!(
            title.size.width > px(0.0),
            "the title must keep visible width rather than collapsing"
        );
        assert!(
            title.left() >= rail.left(),
            "the title must start at the rail's beginning, not scroll out of it"
        );
        assert!(
            title.right() <= rail.right(),
            "the title must truncate inside the rail rather than clip past it"
        );
        assert!(
            rail.right() <= MIN_RAIL_ACCEPTANCE_WIDTH,
            "the rail itself must fit the stated minimum width"
        );

        // The rail offers exactly one way to begin: Home. `New recording` and `Import…` were a
        // second and third, duplicating the three start cards Home already shows — and Home is
        // where the unbuilt Import control states its own unavailability.
        for absent in [
            "library-new-recording",
            "library-import",
            "library-import-note",
        ] {
            assert!(
                visual.debug_bounds(absent).is_none(),
                "{absent} must not exist: Home is the rail's only entry point"
            );
        }
        let home = visual
            .debug_bounds("library-home")
            .ok_or_else(|| std::io::Error::other("the rail must render Home"))?;
        visual.simulate_click(home.center(), Modifiers::none());
        visual.run_until_parked();
        assert!(
            visual.update(|_, cx| matches!(
                session_for_assertion.read(cx).lifecycle(),
                session::SessionLifecycle::Idle { .. }
            )),
            "reaching Home must not begin any session"
        );
        Ok(())
    }
}
