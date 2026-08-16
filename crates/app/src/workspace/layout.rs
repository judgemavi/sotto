//! Shell composition: the chrome bars, the state-dependent stage, and persisted window state.
//!
//! `docs/design/workspace-v2-mock.html` is normative here with one deliberate departure: the mock's
//! `.titlebar` is **not** drawn. It is a web mock, so it has to paint its own app name, Settings
//! and theme switch; a native macOS app already has all three in the menu bar and the window
//! frame, and T082 moved them there. Duplicating the operating system's chrome inside the window
//! cost a permanent row of the vertical space this workspace exists to spend on the record.
//!
//! The strip macOS needs for the traffic lights is still reserved, but since T083 it is the app's
//! **toolbar** rather than an empty band: the sidebar toggle, search, and the Ask toggle sit in it,
//! to the right of the lights. It is a row that already existed, so all three controls cost no
//! additional height, and the two things that used to reserve space permanently — the rail's search
//! field and the 42 px vertical Ask rail — stop doing so.
//!
//! So the shell has exactly two state bars — the capture bar while a recording runs, the view bar
//! while a stopped session is open — and the columns beneath them depend on which state it is in.
//!
//! The bars follow the **lifecycle**; the stage follows the **selection**. Keeping those two
//! separate is what lets T078's Home be a state a person can return to at any time: deselecting
//! swaps the stage to Home without touching a running capture or its Stop.
//!
//! Settings renders here too, as an overlay over the whole shell. It is not a second OS window:
//! the mock's `#setScrim` covers the workspace it configures, and a scrim-backed dialog floating
//! inside its own window frame is two competing containers.

use std::path::{Path, PathBuf};
use std::time::Duration;

use gpui::{
    AnyElement, Context, Entity, IntoElement, Pixels, Render, Window, div, prelude::*, px, relative,
};
use gpui_component::{
    Disableable, IconName, Root, Selectable as _, Sizable as _, Size,
    button::Button,
    button::ButtonVariants as _,
    input::{Input, InputState},
};
use rag::SessionSummary;
use sotto_core::TargetKind;

use crate::notes::NotesState;
use crate::session::SessionLifecycle;

use super::{
    MeetingWorkspace, OpenRecording, PersistedWorkspaceState, StageTab,
    control_row::{ControlRole, ControlRow},
    delete_icon_button, library, notes,
    tokens::{Space, TypeScale, WorkspaceTokens},
    transcript,
};

/// Smallest supported workspace viewport; control-row acceptance is pinned here.
const MIN_WORKSPACE_WIDTH: Pixels = px(680.0);

/// The mock's library rail width, and the narrower width it takes on a small window.
const LIBRARY_WIDTH: Pixels = px(248.0);
const LIBRARY_WIDTH_NARROW: Pixels = px(210.0);
const LIBRARY_NARROW_BELOW: Pixels = px(980.0);

/// What the Ask panel takes while it is open — and nothing at all while it is not.
///
/// Before T083 the right edge reserved 42 px for a vertical rail whose only job was to hold the
/// control that opened the panel. That control is in the toolbar now, so a closed Ask is closed:
/// the column is not rendered, and the stage gets the width.
const ASK_PANEL_WIDTH: Pixels = px(344.0);

/// The transcript's share of the stage while a recording runs; the summary takes the remainder.
///
/// Named because two places must agree on it and they are not adjacent: `render_stage` lays the
/// panes out with it, and `available_transcript_width` re-derives the width the transcript will
/// occupy so its head can decide whether the legend collapses. A literal in both would let someone
/// rebalance the panes and leave the derivation computing a width the column does not have — the
/// head would then collapse at the wrong moment, and nothing would fail, because a threshold
/// comparison against a wrong number is still a comparison.
const TRANSCRIPT_STAGE_SHARE: f32 = 0.575;

/// The width at the left of the toolbar that belongs to macOS, not to Sotto.
///
/// `main.rs` moves the traffic lights to [`super::TRAFFIC_LIGHT_INSET`] from the left edge. macOS
/// lays its three standard window buttons out 14 px wide with 20 px between origins, so the last
/// one ends at `13 + 2×20 + 14 = 67`, and the toolbar leaves another inset's worth of air before
/// its first control. That is 80 px; the normative mock reserves 84 px of left padding on the same
/// row, so this takes the mock's figure — it is the larger of the two, and the one that has been
/// looked at.
const TOOLBAR_LEADING_INSET: Pixels = px(84.0);

/// Which arrangement the stage takes. Derived from session state, never stored.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Stage {
    /// Nothing is open: the stage is Home — the three ways a recording begins, and what the
    /// library already holds. Reached at launch and from the rail's Home entry, which is the only
    /// thing that clears `transcript_session` outside teardown.
    Home,
    /// A recording runs: transcript and notes sit side by side.
    Recording,
    /// A stopped session is open: one tabbed stage, Notes leading.
    Review,
}

impl Render for MeetingWorkspace {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let width = window.viewport_size().width;
        let snapshot = self.notes.read(cx).snapshot();
        // A recording the controller has already finished is not live, whatever the cached flag says.
        //
        // `self.transcript_live` clears only when `refresh_after_session` reaches `load_transcript`,
        // and that path sits behind three early returns. When one of them took, a stopped recording
        // kept rendering as live and Summarize stayed disabled — until the person selected another
        // recording and came back, because selecting calls `load_transcript` directly. Reading the
        // flag against the controller's own `completed_session_id` makes the shell self-correct on
        // the next frame instead of depending on one refresh having run.
        let live = self.session.read(cx).active_session_id();
        let finished = self.session.read(cx).completed_session_id();
        let transcript_live =
            self.transcript_live && !(finished.is_some() && finished == self.transcript_session);
        let frame = if transcript_live {
            transcript::project_frame_for(self.timeline.read(cx).events(), self.transcript_session)
        } else {
            transcript::project_completed_frame(&self.transcript_events)
        };
        let open_session = snapshot
            .meetings
            .iter()
            .find(|session| Some(session.id) == self.transcript_session)
            .cloned();
        let lifecycle = self.session.read(cx).lifecycle().clone();
        let session = self.session.read(cx);
        let transcription_unavailability = session.transcription_unavailability();
        let transcription_model = session.transcription_model().clone();
        let can_start = lifecycle.can_start();
        let tokens = WorkspaceTokens::resolve(cx);
        let library_query = self.library_filter.read(cx).value().to_string();
        let stage = if transcript_live {
            Stage::Recording
        } else if open_session.is_some() {
            Stage::Review
        } else {
            Stage::Home
        };

        // Citation chips read as timecodes, not event ids: the shell holds the transcript rows, so
        // it is the one place that can hand the summary column each cited moment's media time.
        let citation_times = frame
            .transcript
            .committed
            .iter()
            .chain(frame.transcript.unstable.iter())
            .map(|row| (row.event_id, row.start))
            .collect::<notes::CitationTimes>();

        let start_choices = if stage == Stage::Home {
            library::render_start_choices(
                can_start,
                lifecycle.requires_visible_control(),
                self.library_footprint,
                transcription_model.selected(),
                transcription_model.availability().clone(),
                transcription_unavailability.clone(),
                cx,
            )
        } else {
            div().into_any_element()
        };
        let transcript_width =
            available_transcript_width(width, stage, self.library_collapsed, self.ask_open);
        let transcript_column = transcript::render(
            self.transcript_pacer.rows().to_vec(),
            frame.transcript.unstable,
            frame.annotations_by_anchor,
            transcript_live,
            self.transcript_session.is_some(),
            self.follow_transcript,
            self.focused_event,
            &self.transcript_list,
            transcript_width,
            cx,
        );
        let notes_column = notes::render_with_citation_times(
            snapshot.state.clone(),
            transcript_live,
            matches!(snapshot.state, NotesState::Generating),
            &frame.annotations,
            frame.latest_anchor,
            self.focused_event,
            &self.annotation_input,
            self.mcp.read(cx).servers(),
            self.mcp.read(cx).selected_grant(),
            &citation_times,
            cx,
        );

        let settings_sheet = self.settings.clone().filter(|_| self.settings_open);

        div()
            .size_full()
            .relative()
            .flex()
            .flex_col()
            .bg(tokens.ground)
            .text_color(tokens.ink)
            .text_size(TypeScale::BODY)
            // The window titlebar is transparent so this ground colour reaches the top of the
            // window and the appearance choice covers the whole frame. The traffic lights are drawn
            // by macOS over the left of that area, so the toolbar starts to the right of them and
            // nothing is ever placed underneath.
            .child(render_toolbar(
                width,
                self.library_collapsed,
                self.ask_open,
                &self.library_filter,
                tokens,
                cx,
            ))
            .when_some(self.message.clone(), |view, message| {
                view.child(
                    div()
                        .flex_none()
                        .px(Space::LG)
                        .py(Space::SM)
                        .bg(tokens.warn_wash)
                        .text_color(tokens.warn)
                        .text_size(TypeScale::CONTROL)
                        .debug_selector(|| "workspace-message".into())
                        .child(message),
                )
            })
            // The capture bar follows the *lifecycle*, not the stage: ADR-0015 requires Stop to
            // stay one visible action away even while the user reads a stopped session.
            .when(lifecycle.requires_visible_control(), |view| {
                view.child(render_capture_bar(
                    &lifecycle,
                    self.live_started_at,
                    width,
                    tokens,
                    cx,
                ))
            })
            .when_some(
                open_session.filter(|_| stage == Stage::Review),
                |view, session| {
                    view.child(render_view_bar(
                        &session,
                        self.open_recording.as_ref(),
                        self.stage_tab,
                        live.is_some(),
                        self.retranscription_running,
                        transcription_unavailability.clone(),
                        width,
                        tokens,
                        cx,
                    ))
                },
            )
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .flex()
                    // The rail is a column, not an overlay: collapsed, it is not rendered at all
                    // and the stage takes the width back. The way to bring it back is the toolbar
                    // control, which never moves.
                    .when(!self.library_collapsed, |view| {
                        view.child(
                            div()
                                .h_full()
                                .flex_none()
                                .w(if width < LIBRARY_NARROW_BELOW {
                                    LIBRARY_WIDTH_NARROW
                                } else {
                                    LIBRARY_WIDTH
                                })
                                .overflow_hidden()
                                .border_r_1()
                                .border_color(tokens.line)
                                .debug_selector(|| "library-rail".into())
                                .child(library::render(
                                    snapshot.meetings,
                                    self.transcript_session,
                                    live,
                                    &library_query,
                                    &self.library_index,
                                    self.library_footprint,
                                    cx,
                                )),
                        )
                    })
                    .child(render_stage(
                        stage,
                        self.stage_tab,
                        start_choices,
                        transcript_column,
                        notes_column,
                        tokens,
                    ))
                    // Ask stays on the right, where it has always opened. Only the *control* moved
                    // to the toolbar: relocating the panel as well would have moved two things at
                    // once for a change the maintainer asked for as one, and the left edge already
                    // belongs to the rail. Closed, it occupies nothing.
                    .when(self.ask_open, |view| {
                        view.child(
                            div()
                                .h_full()
                                .w(ASK_PANEL_WIDTH)
                                .flex_none()
                                .debug_selector(|| "ask-panel".into())
                                .border_l_1()
                                .border_color(tokens.line)
                                .child(self.ask_dock.clone()),
                        )
                    }),
            )
            // Settings, over the workspace it configures. `occlude` is what makes it modal: without
            // it the scrim would paint over the shell while every control underneath stayed live.
            .when_some(settings_sheet, |view, sheet| {
                view.child(
                    div()
                        .absolute()
                        .top_0()
                        .left_0()
                        .size_full()
                        .occlude()
                        .debug_selector(|| "settings-overlay".into())
                        .child(sheet),
                )
            })
            // Confirm dialogs, painted last so they sit over the shell *and* over the settings
            // sheet that raised them. `gpui_component::Root` stores the open dialogs but renders
            // nothing itself, so the view it wraps is the only place this layer can come from —
            // and there is exactly one such view, which is why it belongs here rather than in the
            // sheet. Off a `Root` — the layout tests that mount the shell bare — it yields nothing
            // rather than panicking.
            .children(Root::render_dialog_layer(window, cx))
    }
}

/// The stage receives the viewport remainder after its two optional siblings take their fixed
/// widths. During a recording the transcript then receives the same 57.5% share used by
/// [`render_stage`], so width-sensitive controls decide from the space they really occupy.
fn available_stage_width(width: Pixels, library_collapsed: bool, ask_open: bool) -> Pixels {
    let library_width = if library_collapsed {
        px(0.0)
    } else if width < LIBRARY_NARROW_BELOW {
        LIBRARY_WIDTH_NARROW
    } else {
        LIBRARY_WIDTH
    };
    let ask_width = if ask_open { ASK_PANEL_WIDTH } else { px(0.0) };
    let available = width - library_width - ask_width;
    if available > px(0.0) {
        available
    } else {
        px(0.0)
    }
}

fn available_transcript_width(
    width: Pixels,
    stage: Stage,
    library_collapsed: bool,
    ask_open: bool,
) -> Pixels {
    let stage_width = available_stage_width(width, library_collapsed, ask_open);
    if stage == Stage::Recording {
        stage_width * TRANSCRIPT_STAGE_SHARE
    } else {
        stage_width
    }
}

/// The toolbar: the band macOS reserves for the traffic lights, doing a job.
///
/// Three controls, in the order a person reads them: what the window shows on the left (the rail),
/// what to find in it (search), and what to ask about it (Ask). All three are *chrome* — none of
/// them is a domain verb — which is why they can share a 38 px row with the operating system's own
/// buttons without competing with Stop, which stays on the capture bar below.
///
/// Shrink roles: the two toggles are [`ControlRole::Essential`] and keep their intrinsic width at
/// every viewport; search is [`ControlRole::Ellipsizing`] and gives its width up first. Nothing
/// here is expendable, because a control that vanishes is a control a person cannot get back to —
/// and getting back is the whole point of the sidebar toggle.
fn render_toolbar(
    width: Pixels,
    library_collapsed: bool,
    ask_open: bool,
    search: &Entity<InputState>,
    tokens: WorkspaceTokens,
    cx: &mut Context<MeetingWorkspace>,
) -> AnyElement {
    ControlRow::for_width(width - TOOLBAR_LEADING_INSET)
        .child(
            ControlRole::Essential,
            div()
                .debug_selector(|| "toolbar-library-toggle".into())
                .child(
                    // One control, one place, both states — marked selected while the rail shows.
                    // The picture stays `panel-left` rather than flipping between an opening and a
                    // closing variant, so the control keeps its width and the search field beside
                    // it does not jump; only the tooltip changes.
                    super::icon_button(
                        "toggle-library",
                        IconName::PanelLeft,
                        if library_collapsed {
                            "Show the Library"
                        } else {
                            "Hide the Library"
                        },
                    )
                    .selected(!library_collapsed)
                    .on_click(cx.listener(|this, _, _, cx| this.toggle_library(cx))),
                ),
        )
        .child(
            ControlRole::Ellipsizing,
            div()
                .min_w_0()
                .max_w(px(360.0))
                .overflow_hidden()
                .debug_selector(|| "toolbar-search".into())
                // `cleanable` draws `IconName::CircleX`, which resolves to `icons/circle-x.svg`.
                // That asset is vendored, so the clear button is a picture the app owns rather
                // than the invisible-but-clickable control an unvendored path would produce —
                // `Assets::load` answers a missing path with `Ok(None)`, not an error.
                .child(Input::new(search).cleanable(true).with_size(Size::Small)),
        )
        .child(
            ControlRole::Essential,
            div().debug_selector(|| "toolbar-ask-toggle".into()).child(
                Button::new("toggle-ask")
                    .label("Ask")
                    .tooltip(if ask_open {
                        "Hide the Ask panel"
                    } else {
                        "Show the Ask panel"
                    })
                    .ghost()
                    .selected(ask_open)
                    .with_size(Size::Small)
                    .on_click(cx.listener(|this, _, window, cx| this.toggle_ask(window, cx))),
            ),
        )
        .finish()
        .w_full()
        .min_w(MIN_WORKSPACE_WIDTH)
        .flex_none()
        .h(super::TITLE_STRIP_HEIGHT)
        .gap(Space::SM)
        .pl(TOOLBAR_LEADING_INSET)
        .pr(Space::MD)
        .bg(tokens.ground)
        .debug_selector(|| "title-strip".into())
        .into_any_element()
}

/// Lays the transcript and notes columns out for the current shell state.
///
/// Review keeps *both* columns mounted and slides the inactive one out of the clipped stage rather
/// than unmounting it. GPUI drops element state — including the notes column's scroll offset — as
/// soon as an element stops being rendered, so unmounting the hidden tab would silently reset its
/// scroll position every time the user switched back.
fn render_stage(
    stage: Stage,
    tab: StageTab,
    start_choices: AnyElement,
    transcript_column: AnyElement,
    notes_column: AnyElement,
    tokens: WorkspaceTokens,
) -> impl IntoElement {
    let stage_root = div()
        .h_full()
        .flex_1()
        .min_w_0()
        .overflow_hidden()
        .debug_selector(|| "workspace-stage".into());
    match stage {
        // Nothing is open, so the stage is Home: the three equally weighted ways a recording
        // begins and what the library already holds, not an empty transcript.
        Stage::Home => stage_root.flex().child(
            div()
                .h_full()
                .flex_1()
                .min_w_0()
                .bg(tokens.surface)
                .debug_selector(|| "stage-home".into())
                .child(start_choices),
        ),
        Stage::Recording => stage_root
            .flex()
            .child(
                div()
                    .h_full()
                    .flex_grow()
                    .flex_shrink()
                    .flex_basis(relative(TRANSCRIPT_STAGE_SHARE))
                    .min_w_0()
                    .bg(tokens.surface)
                    .debug_selector(|| "stage-transcript".into())
                    .child(transcript_column),
            )
            .child(
                div()
                    .h_full()
                    .flex_grow()
                    .flex_shrink()
                    .flex_basis(relative(1.0 - TRANSCRIPT_STAGE_SHARE))
                    .min_w_0()
                    .bg(tokens.surface)
                    .border_l_1()
                    .border_color(tokens.line)
                    .debug_selector(|| "stage-notes".into())
                    .child(notes_column),
            ),
        Stage::Review => stage_root
            .relative()
            .bg(tokens.surface)
            .child(
                stage_layer(tab == StageTab::Transcript)
                    .debug_selector(|| "stage-transcript".into())
                    .child(transcript_column),
            )
            .child(
                stage_layer(tab == StageTab::Notes)
                    .debug_selector(|| "stage-notes".into())
                    .child(notes_column),
            ),
    }
}

/// One tab of the review stage. The inactive layer keeps its full width, parked off the clipped
/// edge of the stage, so its measured content and scroll offset survive the switch.
fn stage_layer(active: bool) -> gpui::Div {
    let layer = div()
        .absolute()
        .top_0()
        .bottom_0()
        .w_full()
        .overflow_hidden();
    if active {
        layer.left_0()
    } else {
        layer.left(relative(1.0))
    }
}

fn render_capture_bar(
    lifecycle: &SessionLifecycle,
    started_at: Option<std::time::Instant>,
    width: Pixels,
    tokens: WorkspaceTokens,
    cx: &mut Context<MeetingWorkspace>,
) -> AnyElement {
    let (kind, target, stopping) = match lifecycle {
        SessionLifecycle::ProvisioningModel {
            target,
            model,
            progress,
        } => (
            crate::session::progress_label(*model, *progress),
            target,
            false,
        ),
        SessionLifecycle::Running { target } => (
            if target.is_microphone_only() {
                "Recording · mic".to_owned()
            } else {
                "Recording".to_owned()
            },
            target,
            false,
        ),
        SessionLifecycle::Stopping { target, .. } => ("Finishing".to_owned(), target, true),
        _ => return div().into_any_element(),
    };
    let elapsed = started_at.map_or(Duration::ZERO, |value| value.elapsed());
    ControlRow::for_width(width)
        .child(
            ControlRole::Essential,
            div()
                .size(px(9.0))
                .rounded_full()
                .bg(tokens.live)
                .debug_selector(|| "capture-record-dot".into()),
        )
        .child(
            ControlRole::Essential,
            div()
                .text_size(TypeScale::CHIP)
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .text_color(tokens.live_ink)
                .child(kind),
        )
        .child(
            ControlRole::Ellipsizing,
            div()
                .text_size(TypeScale::BODY)
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .debug_selector(|| "capture-target-name".into())
                .child(capture_target_name(target)),
        )
        .child(
            ControlRole::Expendable,
            div()
                .flex()
                .items_center()
                .gap(Space::XS)
                .debug_selector(|| "capture-scope-chips".into())
                .children(
                    scope_chips(target)
                        .into_iter()
                        .map(|chip| scope_chip(chip, tokens)),
                ),
        )
        .child(
            ControlRole::Essential,
            div()
                .debug_selector(|| "capture-clock".into())
                .font_family("Menlo")
                .text_size(TypeScale::CLOCK)
                .child(format_clock(elapsed)),
        )
        // No Pause control, decided (not deferred) by T079. Compressing the timeline to keep
        // ADR-0018's transcript-time-is-media-time identity intact is buildable, but the writer,
        // origin tracking, and segment sink already carry real ScreenCaptureKit/AVFoundation
        // fragility (see ADR-0018's amendments), and pause behaviour can only be proven against a
        // real signed capture — infrastructure this codebase does not yet have outside a manual
        // maintainer run. Stop-and-start-a-new-recording is the supported way to break up a
        // session; two recordings are a more honest record than one with an unverified hole in it.
        .child(
            ControlRole::Essential,
            div()
                .debug_selector(|| "capture-stop-control".into())
                .child(
                    Button::new("stop-recording")
                        .label(if stopping { "Stopping…" } else { "Stop" })
                        .danger()
                        .with_size(Size::Small)
                        .disabled(stopping)
                        .on_click(cx.listener(|this, _, _, cx| this.stop_session(cx))),
                ),
        )
        .finish()
        .w_full()
        .min_w(MIN_WORKSPACE_WIDTH)
        .flex_none()
        .gap(Space::MD)
        .px(Space::MD)
        .py(Space::SM)
        .min_h(px(46.0))
        .bg(tokens.live_wash)
        .border_b_1()
        .border_color(tokens.live_line)
        .debug_selector(|| "capture-bar".into())
        .into_any_element()
}

#[expect(
    clippy::too_many_arguments,
    reason = "the view bar's inputs stay explicit rather than reaching back into the workspace"
)]
fn render_view_bar(
    session: &SessionSummary,
    recording: Option<&OpenRecording>,
    tab: StageTab,
    live_elsewhere: bool,
    retranscribing: bool,
    transcription_unavailability: Option<String>,
    width: Pixels,
    tokens: WorkspaceTokens,
    cx: &mut Context<MeetingWorkspace>,
) -> AnyElement {
    let retranscription_unavailable = transcription_unavailability.is_some();
    ControlRow::for_width(width)
        .child(
            ControlRole::Ellipsizing,
            div()
                .text_size(TypeScale::TITLE)
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .debug_selector(|| "view-title".into())
                .child(library::recording_name(session)),
        )
        .child(
            ControlRole::Essential,
            div()
                .font_family("Menlo")
                .text_size(TypeScale::META)
                .text_color(tokens.faint)
                .debug_selector(|| "view-meta".into())
                .child(view_meta(session, recording)),
        )
        .child(
            ControlRole::Expendable,
            div()
                .text_size(TypeScale::META)
                .text_color(tokens.faint)
                .child(format!(
                    "started {}",
                    format_wall_clock(session.started_at_unix_ms)
                )),
        )
        .spacer()
        .child(
            ControlRole::Essential,
            div()
                .flex()
                .items_center()
                .gap(px(2.0))
                .p(px(2.0))
                .rounded_md()
                .bg(tokens.sunken)
                .border_1()
                .border_color(tokens.line_soft)
                .debug_selector(|| "view-tabs".into())
                .child(stage_tab_button(StageTab::Notes, tab, tokens, cx))
                .child(stage_tab_button(StageTab::Transcript, tab, tokens, cx)),
        )
        .spacer()
        .child_when(live_elsewhere, ControlRole::Expendable, || {
            div()
                .debug_selector(|| "view-back-to-live".into())
                .child(
                    Button::new("back-to-live")
                        .label("Back to recording")
                        .with_size(Size::Small)
                        .on_click(cx.listener(|this, _, _, cx| this.return_to_live(cx))),
                )
                .into_any_element()
        })
        .child(
            ControlRole::Expendable,
            div().child(
                Button::new("retranscribe-session")
                    .label(retranscription_label(
                        retranscribing,
                        retranscription_unavailable,
                    ))
                    .with_size(Size::Small)
                    .disabled(retranscribing || transcription_unavailability.is_some())
                    .when_some(transcription_unavailability, |button, reason| {
                        button.tooltip(reason)
                    })
                    .on_click(cx.listener(|this, _, _, cx| this.retranscribe_selected(cx))),
            ),
        )
        .child(
            ControlRole::Expendable,
            div().debug_selector(|| "view-reveal-control".into()).child(
                // The mock's word, not an abbreviation of ours: the bar already says which
                // recording is open, so "recording" was repeating its own title.
                Button::new("reveal-recording")
                    .label("Reveal")
                    .tooltip("Show this recording in Finder")
                    .with_size(Size::Small)
                    .disabled(recording.is_none_or(|value| value.path.is_none()))
                    .on_click(cx.listener(|this, _, _, cx| this.reveal_open_recording(cx))),
            ),
        )
        .child(
            ControlRole::Essential,
            div().debug_selector(|| "view-delete-control".into()).child(
                delete_icon_button("delete-session", "this recording").on_click(
                    cx.listener(|this, _, window, cx| this.delete_open_session(window, cx)),
                ),
            ),
        )
        .finish()
        .w_full()
        .min_w(MIN_WORKSPACE_WIDTH)
        .flex_none()
        .gap(Space::MD)
        .px(Space::MD)
        .py(Space::SM)
        .min_h(px(46.0))
        .bg(tokens.surface)
        .border_b_1()
        .border_color(tokens.line)
        .debug_selector(|| "view-bar".into())
        .into_any_element()
}

const fn retranscription_label(running: bool, model_unavailable: bool) -> &'static str {
    if running {
        "Re-transcribing…"
    } else if model_unavailable {
        "Choose model on Home"
    } else {
        "Re-transcribe"
    }
}

fn stage_tab_button(
    tab: StageTab,
    selected: StageTab,
    tokens: WorkspaceTokens,
    cx: &mut Context<MeetingWorkspace>,
) -> AnyElement {
    let active = tab == selected;
    div()
        .id(tab.element_id())
        .px(Space::MD)
        .py(px(3.0))
        .rounded_md()
        .text_size(TypeScale::CONTROL)
        .cursor_pointer()
        .when(active, |view| {
            view.bg(tokens.surface).text_color(tokens.ink)
        })
        .when(!active, |view| view.text_color(tokens.muted))
        .debug_selector(move || tab.debug_selector().into())
        .child(tab.label())
        .on_click(cx.listener(move |this, _, _, cx| this.select_stage_tab(tab, cx)))
        .into_any_element()
}

fn capture_target_name(target: &sotto_core::CaptureTarget) -> String {
    match target.window_title.as_deref() {
        Some(title) if title != target.display_name => {
            format!("{} — {title}", target.display_name)
        }
        _ => target.display_name.clone(),
    }
}

/// The scope claims shown while recording, in the mock's vocabulary and never overstated.
///
/// This renders only inside `render_capture_bar`, which only ever draws for a live
/// `SessionLifecycle::Running`/`Stopping`/`ProvisioningModel` state — an import never reaches it,
/// since it has no live capture to show a bar for. The `Imported` arm below exists anyway, and is
/// handled rather than matched away with `_`, because the alternative — falling through to the
/// scoped-audio match below — would put a live capture-scope claim on a target that never had one.
fn scope_chips(target: &sotto_core::CaptureTarget) -> Vec<String> {
    if target.is_microphone_only() {
        return vec!["your mic only".to_owned(), "stays on this Mac".to_owned()];
    }
    if target.is_imported() {
        return vec!["imported".to_owned(), "stays on this Mac".to_owned()];
    }
    let audio = if target.audio_scoped {
        match target.kind {
            TargetKind::Application | TargetKind::Window => "app audio",
            TargetKind::Display => "target audio",
            TargetKind::Microphone | TargetKind::Imported => {
                unreachable!("handled above")
            }
        }
    } else {
        "system audio"
    };
    vec![
        audio.to_owned(),
        "screen".to_owned(),
        "your mic".to_owned(),
        "stays on this Mac".to_owned(),
    ]
}

fn scope_chip(label: String, tokens: WorkspaceTokens) -> AnyElement {
    div()
        .flex_none()
        .px(Space::SM)
        .py(px(2.0))
        .rounded_full()
        .border_1()
        .border_color(tokens.live_line)
        .bg(tokens.surface)
        .text_size(TypeScale::CHIP)
        .text_color(tokens.muted)
        .whitespace_nowrap()
        .child(label)
        .into_any_element()
}

fn view_meta(session: &SessionSummary, recording: Option<&OpenRecording>) -> String {
    let duration = recording
        .and_then(|value| value.duration)
        .or_else(|| {
            session
                .ended_at_unix_ms
                .and_then(|end| end.checked_sub(session.started_at_unix_ms))
                .map(Duration::from_millis)
        })
        .map_or_else(|| "—".to_owned(), format_clock);
    // Only say something when there is something to say. "— · no retained recording" announced two
    // unknowns at once and told the reader nothing they could act on; the recording's absence is
    // already stated where it matters, on the controls that would have used it.
    match recording.and_then(|value| value.byte_size) {
        Some(bytes) => format!("{duration} · {}", format_bytes(bytes)),
        None => duration,
    }
}

fn format_clock(value: Duration) -> String {
    let seconds = value.as_secs();
    let (hours, minutes, seconds) = (seconds / 3600, (seconds / 60) % 60, seconds % 60);
    if hours > 0 {
        format!("{hours}:{minutes:02}:{seconds:02}")
    } else {
        format!("{minutes:02}:{seconds:02}")
    }
}

pub(super) fn format_bytes(bytes: u64) -> String {
    #[expect(
        clippy::cast_precision_loss,
        reason = "a displayed recording size needs one decimal place, not exact integer bytes"
    )]
    let value = bytes as f64;
    if bytes >= 1_000_000_000 {
        format!("{:.1} GB", value / 1_000_000_000.0)
    } else if bytes >= 1_000_000 {
        format!("{:.0} MB", value / 1_000_000.0)
    } else {
        format!("{:.0} KB", (value / 1000.0).ceil())
    }
}

fn format_wall_clock(unix_ms: u64) -> String {
    let timestamp = std::time::UNIX_EPOCH + Duration::from_millis(unix_ms);
    chrono::DateTime::<chrono::Local>::from(timestamp)
        .format("%-I:%M %p")
        .to_string()
}

fn workspace_state_path(database: &Path) -> PathBuf {
    database.with_file_name("workspace-state.json")
}

pub(super) fn load_workspace_state(database: &Path) -> PersistedWorkspaceState {
    std::fs::read(workspace_state_path(database))
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

pub(super) fn save_workspace_state(
    database: &Path,
    state: PersistedWorkspaceState,
) -> Result<(), String> {
    let path = workspace_state_path(database);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    }
    let bytes = serde_json::to_vec_pretty(&state).map_err(|error| error.to_string())?;
    std::fs::write(path, bytes).map_err(|error| format!("Could not save workspace state: {error}"))
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::Duration;

    use std::ops::Deref as _;

    use gpui::{
        AppContext as _, Bounds, Modifiers, TestAppContext, VisualTestContext, WindowBounds,
        WindowOptions, point, px, size,
    };
    use gpui_component::{ActiveTheme as _, Root, Theme, ThemeMode, WindowExt as _};
    use rag::{SessionSummary, Store};
    use secrecy::SecretString;
    use sotto_core::{
        CaptureTarget, EventPayload, Session, SessionId, Source, TargetKind, TimelineBuilder,
        Utterance,
    };

    use super::{
        ASK_PANEL_WIDTH, LIBRARY_WIDTH, MIN_WORKSPACE_WIDTH, Stage, TOOLBAR_LEADING_INSET,
        TRANSCRIPT_STAGE_SHARE, available_stage_width, available_transcript_width, format_bytes,
        format_clock, format_wall_clock, load_workspace_state, retranscription_label,
        save_workspace_state, view_meta,
    };
    use crate::workspace::{
        Appearance, CONFIRM_CANCEL_SELECTOR, CONFIRM_OK_SELECTOR, FollowSystemAppearance,
        MeetingWorkspace, OpenRecording, OpenSettings, PersistedTheme, PersistedWorkspaceState,
        StageTab, UseDarkAppearance, UseLightAppearance,
    };
    use crate::{mcp, reasoning, session};

    /// Wide enough that every expendable control in both bars survives.
    const WIDE_WORKSPACE_WIDTH: gpui::Pixels = px(1400.0);

    struct NoOpenAiCredentials;

    #[test]
    fn transcript_width_uses_its_real_stage_share_not_the_viewport() {
        assert_eq!(
            available_stage_width(px(900.0), false, false),
            px(690.0),
            "the narrow rail is deducted before the stage receives width"
        );
        assert_eq!(
            available_transcript_width(px(900.0), Stage::Recording, false, false),
            px(690.0) * TRANSCRIPT_STAGE_SHARE,
            "the live transcript receives only its 57.5% share, so a 900 px viewport cannot keep the legend"
        );
        assert_eq!(
            available_transcript_width(px(900.0), Stage::Review, false, false),
            px(690.0),
            "the review transcript receives the full stage"
        );
        assert_eq!(
            available_stage_width(px(1_400.0), false, true),
            px(1_400.0) - LIBRARY_WIDTH - ASK_PANEL_WIDTH,
            "the wide rail and open Ask panel are both deducted"
        );
        assert_eq!(
            available_stage_width(px(900.0), true, false),
            px(900.0),
            "a collapsed rail takes no width"
        );
    }

    #[test]
    fn unavailable_retranscription_points_to_the_home_model_choice() {
        assert_eq!(retranscription_label(false, true), "Choose model on Home");
        assert_eq!(retranscription_label(false, false), "Re-transcribe");
    }

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
        session: gpui::Entity<session::SessionController>,
        visual: &'static mut VisualTestContext,
    }

    /// The shell mounted the way `main.rs` mounts it: under `gpui_component::Root`, with the
    /// menu-bar actions registered and the window activated.
    ///
    /// [`mount`] deliberately does not do this — it puts `MeetingWorkspace` at the window root so
    /// its layout assertions are about the shell alone. That difference is not cosmetic: the menu
    /// path only exists when the root view is a `Root` that has to be downcast, and
    /// `App::dispatch_action` only routes through a window when one is *active*. A test that
    /// skipped either would pass against the defect T082 fixed.
    fn mount_as_the_product_does(
        cx: &mut TestAppContext,
        directory: &std::path::Path,
        lifecycle: Option<session::SessionLifecycle>,
        width: gpui::Pixels,
    ) -> Result<MountedApp, Box<dyn std::error::Error>> {
        let database = directory.join("sotto.sqlite3");
        let reasoning_path = directory.join("reasoning.json");
        let mcp_path = directory.join("mcp.json");
        let (ingress, timeline) = cx.update(|cx| crate::devwindow::attach_ingress(cx, 16));
        let session = cx.new(|_| session::SessionController::new(ingress));
        if let Some(lifecycle) = lifecycle {
            cx.update(|cx| {
                session.update(cx, |controller, _| {
                    controller.set_lifecycle_for_test(lifecycle);
                });
            });
        }
        let reasoning = cx.new(|_| {
            reasoning::ReasoningController::load(reasoning_path, Arc::new(NoOpenAiCredentials))
        });
        let mcp = cx.new(|_| mcp::McpController::load(Some(mcp_path), Arc::new(NoMcpCredentials)));
        let handle = cx.update(|cx| {
            cx.open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(Bounds {
                        origin: point(px(0.0), px(0.0)),
                        size: size(width, px(720.0)),
                    })),
                    ..WindowOptions::default()
                },
                move |window, cx| {
                    let view = cx.new(|cx| {
                        MeetingWorkspace::new(
                            database, timeline, reasoning, session, mcp, window, cx,
                        )
                    });
                    cx.new(|cx| Root::new(view, window, cx))
                },
            )
        })?;
        let workspace = cx
            .update(|cx| {
                handle.update(cx, |root, _, _| {
                    root.view().clone().downcast::<MeetingWorkspace>()
                })
            })?
            .map_err(|_| std::io::Error::other("the window root must wrap a MeetingWorkspace"))?;
        cx.update(|cx| crate::workspace::register_menu_actions(handle, cx));
        let visual = VisualTestContext::from_window(*handle.deref(), cx).into_mut();
        // The real platform activates the window it opens; the test platform does not. Without
        // this, `App::dispatch_action` would fall back to `dispatch_global_action`, which never
        // holds a window — and the whole point of these tests is the path that does.
        visual.update(|window, _| window.activate_window());
        visual.run_until_parked();
        assert!(
            cx.update(|cx| cx.active_window()).is_some(),
            "these tests are only meaningful while a window is active"
        );
        Ok(MountedApp { workspace, visual })
    }

    struct MountedApp {
        workspace: gpui::Entity<MeetingWorkspace>,
        visual: &'static mut VisualTestContext,
    }

    /// Builds the real shell over a real store, in a window opened at the stated width.
    ///
    /// The window is opened at its final size rather than resized afterwards: GPUI's
    /// `Frame::clear` does not clear `debug_bounds`, so a control painted in an earlier, wider
    /// frame stays in the map forever and an "this control collapsed out" assertion would pass
    /// against a stale entry.
    fn mount(
        cx: &mut TestAppContext,
        directory: &std::path::Path,
        lifecycle: Option<session::SessionLifecycle>,
        width: gpui::Pixels,
    ) -> Result<MountedShell, Box<dyn std::error::Error>> {
        let database = directory.join("sotto.sqlite3");
        let reasoning_path = directory.join("reasoning.json");
        let mcp_path = directory.join("mcp.json");
        let (ingress, timeline) = cx.update(|cx| crate::devwindow::attach_ingress(cx, 16));
        let session = cx.new(|_| session::SessionController::new(ingress));
        if let Some(lifecycle) = lifecycle {
            cx.update(|cx| {
                session.update(cx, |controller, _| {
                    controller.set_lifecycle_for_test(lifecycle);
                });
            });
        }
        let reasoning = cx.new(|_| {
            reasoning::ReasoningController::load(reasoning_path, Arc::new(NoOpenAiCredentials))
        });
        let mcp = cx.new(|_| mcp::McpController::load(Some(mcp_path), Arc::new(NoMcpCredentials)));
        let session_for_shell = session.clone();
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
                            database,
                            timeline,
                            reasoning,
                            session_for_shell,
                            mcp,
                            window,
                            cx,
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
            session,
            visual,
        })
    }

    fn running_window_capture() -> session::SessionLifecycle {
        session::SessionLifecycle::Running {
            target: CaptureTarget {
                bundle_id: Some("com.example.meeting".into()),
                display_name: "Meeting app".into(),
                window_title: Some("A deliberately long selected capture window title".into()),
                kind: TargetKind::Window,
                audio_scoped: false,
            },
        }
    }

    /// Persists a stopped session with `finals` transcript rows and returns its id.
    async fn persist_stopped_session(
        database: &std::path::Path,
        finals: u64,
    ) -> Result<SessionId, Box<dyn std::error::Error>> {
        persist_stopped_session_titled(
            database,
            SessionId::new(1_786_625_633_040_598_000),
            "This is a persisted recording",
            finals,
        )
        .await
    }

    /// The same, for a second recording a test needs to prove was *not* touched.
    async fn persist_stopped_session_titled(
        database: &std::path::Path,
        session_id: SessionId,
        title: &str,
        finals: u64,
    ) -> Result<SessionId, Box<dyn std::error::Error>> {
        let mut record = Session::new(
            session_id,
            CaptureTarget {
                bundle_id: Some("com.example.meeting".into()),
                display_name: "Meeting app".into(),
                window_title: Some(title.into()),
                kind: TargetKind::Window,
                audio_scoped: true,
            },
            1_786_625_633_040,
        );
        record.end(1_786_625_700_000);
        let store = Store::open(database).await?;
        store.save_session(&record).await?;
        let mut timeline = TimelineBuilder::new(record);
        for index in 0..finals {
            let start = Duration::from_secs(index * 3);
            let partial = timeline.append(
                Duration::from_secs(10_000 + index),
                EventPayload::UtterancePartial(Utterance {
                    source: Source::System,
                    start,
                    end: start + Duration::from_secs(1),
                    text: format!("Draft {index}"),
                    avg_logprob: 0.0,
                    annotations: vec![],
                }),
            );
            timeline.supersede(
                Duration::from_secs(100 + index),
                EventPayload::UtteranceFinal(Utterance {
                    source: Source::System,
                    start,
                    end: start + Duration::from_secs(2),
                    text: format!("Persisted final {index}"),
                    avg_logprob: 0.0,
                    annotations: vec![],
                }),
                &partial,
            )?;
        }
        store.append_events(timeline.events()).await?;
        Ok(session_id)
    }

    #[test]
    fn ask_theme_and_sidebar_state_roundtrip() -> Result<(), Box<dyn std::error::Error>> {
        let dir = tempfile::tempdir()?;
        let db = dir.path().join("sotto.sqlite3");
        save_workspace_state(
            &db,
            PersistedWorkspaceState {
                ask_open: true,
                theme: Some(PersistedTheme::Dark),
                library_collapsed: true,
            },
        )?;
        let loaded = load_workspace_state(&db);
        assert!(
            loaded.ask_open,
            "the persisted Ask state must survive a round trip"
        );
        assert!(
            loaded.library_collapsed,
            "a collapsed rail must survive a round trip, the way Ask already does"
        );
        assert_eq!(
            loaded.theme,
            Some(PersistedTheme::Dark),
            "a chosen theme must survive a relaunch, or the toggle is a switch that forgets"
        );
        let fresh = tempfile::tempdir()?;
        assert_eq!(
            load_workspace_state(&fresh.path().join("sotto.sqlite3")).theme,
            None,
            "with no recorded choice Sotto keeps following the system appearance"
        );
        // A state file written before T083 carries no sidebar field. It must still parse — and
        // default to the rail being *shown*, because a person who never collapsed it must not come
        // back to a window that has hidden its only navigator.
        let legacy = tempfile::tempdir()?;
        let legacy_db = legacy.path().join("sotto.sqlite3");
        std::fs::write(
            legacy_db.with_file_name("workspace-state.json"),
            br#"{"ask_open":false,"theme":"light"}"#,
        )?;
        let upgraded = load_workspace_state(&legacy_db);
        assert!(
            !upgraded.library_collapsed,
            "an older state file must still load, with the rail shown"
        );
        assert_eq!(
            upgraded.theme,
            Some(PersistedTheme::Light),
            "adding a field must not cost the fields that were already there"
        );
        Ok(())
    }

    #[test]
    fn recording_start_time_uses_the_users_local_clock() {
        let timestamp = 1_786_625_633_040;
        let expected = chrono::DateTime::<chrono::Local>::from(
            std::time::UNIX_EPOCH + Duration::from_millis(timestamp),
        )
        .format("%-I:%M %p")
        .to_string();
        assert_eq!(
            format_wall_clock(timestamp),
            expected,
            "the view bar must show the user's own clock"
        );
        assert!(
            !format_wall_clock(timestamp).contains("UTC"),
            "the view bar must not leak UTC"
        );
    }

    #[test]
    fn the_clock_grows_an_hours_field_only_once_there_are_hours() {
        assert_eq!(
            format_clock(Duration::from_secs(59)),
            "00:59",
            "under a minute stays mm:ss"
        );
        assert_eq!(
            format_clock(Duration::from_secs(2_892)),
            "48:12",
            "the mock's 48:12 duration must render exactly"
        );
        assert_eq!(
            format_clock(Duration::from_secs(3_661)),
            "1:01:01",
            "past an hour the clock gains an hours field"
        );
    }

    #[test]
    fn a_recording_size_reads_in_the_units_the_mock_uses() {
        assert_eq!(
            format_bytes(412_000_000),
            "412 MB",
            "the mock's 412 MB must render exactly"
        );
        assert_eq!(
            format_bytes(2_100_000_000),
            "2.1 GB",
            "gigabyte sizes keep one decimal"
        );
        assert_eq!(format_bytes(12_000), "12 KB", "small sizes stay in KB");
    }

    #[test]
    fn view_meta_says_so_when_no_recording_is_retained() {
        let session = SessionSummary {
            id: SessionId::new(7),
            capture_target: CaptureTarget::microphone_only(),
            title: None,
            started_at_unix_ms: 1_786_625_633_040,
            ended_at_unix_ms: Some(1_786_625_633_040 + 2_892_000),
        };
        assert_eq!(
            view_meta(&session, None),
            "48:12",
            "an absent recording is stated, never implied by a blank size"
        );
        assert_eq!(
            view_meta(
                &session,
                Some(&OpenRecording {
                    path: Some("/tmp/recording.m4a".to_owned()),
                    duration: Some(Duration::from_secs(2_892)),
                    byte_size: Some(412_000_000),
                }),
            ),
            "48:12 · 412 MB",
            "a retained recording reports its measured duration and size"
        );
    }

    #[test]
    fn the_shell_mounts_and_renders_in_both_themes() -> Result<(), Box<dyn std::error::Error>> {
        let mut cx = TestAppContext::single();
        cx.update(gpui_component::init);
        let dir = tempfile::tempdir()?;
        let shell = mount(
            &mut cx,
            dir.path(),
            Some(running_window_capture()),
            WIDE_WORKSPACE_WIDTH,
        )?;
        let visual = shell.visual;
        for mode in [ThemeMode::Dark, ThemeMode::Light] {
            visual.update(|window, cx| Theme::change(mode, Some(window), cx));
            visual.refresh()?;
            visual.run_until_parked();
            assert!(
                visual.debug_bounds("capture-bar").is_some(),
                "the capture bar must render in every theme, not only the maintainer's"
            );
            assert!(
                visual.debug_bounds("library-rail").is_some(),
                "the library rail must render in every theme"
            );
        }
        Ok(())
    }

    #[test]
    fn an_empty_library_opens_on_the_three_ways_a_session_begins()
    -> Result<(), Box<dyn std::error::Error>> {
        let mut cx = TestAppContext::single();
        cx.update(gpui_component::init);
        let dir = tempfile::tempdir()?;
        let shell = mount(&mut cx, dir.path(), None, WIDE_WORKSPACE_WIDTH)?;
        let workspace = shell.workspace;
        let visual = shell.visual;
        visual.update(|_, cx| {
            let session = workspace.read(cx).session.clone();
            session.update(cx, |session, _| {
                session.set_transcription_availability_for_test(
                    crate::session::ModelAvailability::Missing,
                );
            });
        });
        visual.refresh()?;
        visual.run_until_parked();

        assert!(
            visual.debug_bounds("stage-home").is_some(),
            "with nothing open the stage must be Home"
        );
        assert!(
            visual.debug_bounds("start-choices").is_some(),
            "the three ways a session begins must be mounted, not merely built"
        );
        assert!(
            visual.debug_bounds("home-model-setup").is_some(),
            "Home must offer a model choice before any transcription action is available"
        );
        assert!(
            visual.debug_bounds("capture-bar").is_none()
                && visual.debug_bounds("view-bar").is_none(),
            "an idle shell shows neither bar"
        );
        assert!(
            visual.debug_bounds("home-live-note").is_none(),
            "with nothing recording Home must not claim a recording is running"
        );
        Ok(())
    }

    #[test]
    fn checking_a_cached_model_does_not_flash_the_choice_panel()
    -> Result<(), Box<dyn std::error::Error>> {
        let mut cx = TestAppContext::single();
        cx.update(gpui_component::init);
        let dir = tempfile::tempdir()?;
        let shell = mount(&mut cx, dir.path(), None, WIDE_WORKSPACE_WIDTH)?;
        let workspace = shell.workspace;
        let visual = shell.visual;
        visual.refresh()?;
        visual.run_until_parked();
        assert!(
            visual.debug_bounds("home-model-setup").is_none(),
            "the launch-time integrity check must not flash a false missing-model choice"
        );
        visual.update(|_, cx| {
            let session = workspace.read(cx).session.clone();
            session.update(cx, |session, _| {
                session.set_transcription_availability_for_test(
                    crate::session::ModelAvailability::Ready("/tmp/model.bin".into()),
                );
            });
        });
        visual.refresh()?;
        assert!(
            visual.debug_bounds("home-model-setup").is_none(),
            "a ready model must keep setup entirely off Home"
        );
        Ok(())
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn launch_opens_home_not_the_newest_recording() -> Result<(), Box<dyn std::error::Error>>
    {
        let mut cx = TestAppContext::single();
        cx.update(gpui_component::init);
        let dir = tempfile::tempdir()?;
        persist_stopped_session(&dir.path().join("sotto.sqlite3"), 4).await?;
        let shell = mount(&mut cx, dir.path(), None, WIDE_WORKSPACE_WIDTH)?;
        let workspace = shell.workspace;
        let visual = shell.visual;
        visual.refresh()?;
        visual.run_until_parked();

        assert!(
            visual.update(|_, cx| workspace.read(cx).transcript_session.is_none()),
            "a library with history must still launch with nothing selected"
        );
        assert!(
            visual.debug_bounds("stage-home").is_some()
                && visual.debug_bounds("start-choices").is_some(),
            "launch must land on Home, not inside the most recent recording"
        );
        assert!(
            visual.debug_bounds("view-bar").is_none(),
            "launch must not open a session's view bar"
        );
        assert!(
            visual.debug_bounds("library-row").is_some(),
            "the recording is still in the Library, one click away"
        );
        Ok(())
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn home_is_reachable_from_an_open_recording() -> Result<(), Box<dyn std::error::Error>> {
        let mut cx = TestAppContext::single();
        cx.update(gpui_component::init);
        let dir = tempfile::tempdir()?;
        let session_id = persist_stopped_session(&dir.path().join("sotto.sqlite3"), 6).await?;
        let shell = mount(&mut cx, dir.path(), None, WIDE_WORKSPACE_WIDTH)?;
        let workspace = shell.workspace;
        let visual = shell.visual;
        visual.update(|_, cx| {
            workspace.update(cx, |this, cx| this.select_meeting(session_id, cx));
        });
        visual.refresh()?;
        visual.run_until_parked();
        assert!(
            visual.debug_bounds("view-bar").is_some(),
            "the selected recording must be open before Home is tested as a way back"
        );

        let home = visual
            .debug_bounds("library-home")
            .ok_or_else(|| std::io::Error::other("the rail must carry a Home entry"))?;
        visual.simulate_click(home.center(), Modifiers::none());
        visual.refresh()?;
        visual.run_until_parked();

        // Selection is the authority here, not `debug_bounds`: GPUI's `Frame::clear` leaves the
        // map populated, so a bar painted in the previous frame would still answer `is_some`.
        // `transcript_session == None` is exactly what `render_stage` turns into Home.
        assert!(
            visual.update(|_, cx| workspace.read(cx).transcript_session.is_none()),
            "Home must deselect the open recording so the entry points come back"
        );
        let stage = visual
            .debug_bounds("workspace-stage")
            .ok_or_else(|| std::io::Error::other("the stage must render"))?;
        let choices = visual.debug_bounds("start-choices").ok_or_else(|| {
            std::io::Error::other("the ways a recording begins must be reachable again")
        })?;
        assert!(
            choices.left() >= stage.left()
                && choices.right() <= stage.right()
                && choices.size.width > px(0.0),
            "the entry points must occupy the stage the recording just vacated"
        );
        Ok(())
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn home_during_a_recording_keeps_stop_and_says_the_capture_is_running()
    -> Result<(), Box<dyn std::error::Error>> {
        let mut cx = TestAppContext::single();
        cx.update(gpui_component::init);
        let dir = tempfile::tempdir()?;
        let session_id = persist_stopped_session(&dir.path().join("sotto.sqlite3"), 3).await?;
        let shell = mount(
            &mut cx,
            dir.path(),
            Some(running_window_capture()),
            MIN_WORKSPACE_WIDTH,
        )?;
        let workspace = shell.workspace;
        let session = shell.session;
        let visual = shell.visual;
        // Stand inside the running recording, exactly as the shell does once capture identifies.
        visual.update(|_, cx| {
            workspace.update(cx, |this, cx| {
                this.show_live_transcript(session_id, cx);
                cx.notify();
            });
        });
        visual.refresh()?;
        visual.run_until_parked();

        let home = visual
            .debug_bounds("library-home")
            .ok_or_else(|| std::io::Error::other("the rail must carry a Home entry"))?;
        visual.simulate_click(home.center(), Modifiers::none());
        visual.refresh()?;
        visual.run_until_parked();

        assert!(
            visual.debug_bounds("stage-home").is_some(),
            "Home must be reachable while a capture runs"
        );
        assert!(
            visual.update(|_, cx| matches!(
                session.read(cx).lifecycle(),
                session::SessionLifecycle::Running { .. }
            )),
            "navigating Home must not stop or orphan the running capture"
        );
        let bar = visual
            .debug_bounds("capture-bar")
            .ok_or_else(|| std::io::Error::other("the capture bar must survive going Home"))?;
        let stop = visual
            .debug_bounds("capture-stop-control")
            .ok_or_else(|| std::io::Error::other("Stop must survive going Home"))?;
        let note = visual.debug_bounds("home-live-note").ok_or_else(|| {
            std::io::Error::other("Home must say a recording is running and where Stop is")
        })?;
        let stage = visual
            .debug_bounds("workspace-stage")
            .ok_or_else(|| std::io::Error::other("the stage must render"))?;
        assert!(
            stop.size.width > px(0.0)
                && stop.left() >= bar.left()
                && stop.right() <= bar.right()
                && bar.right() <= MIN_WORKSPACE_WIDTH,
            "Stop must stay wholly inside the capture bar at the narrowest supported width"
        );
        assert!(
            note.left() >= stage.left()
                && note.right() <= stage.right()
                && note.size.width > px(0.0),
            "the running-capture note must fit the stage rather than clip out of it"
        );
        visual.simulate_click(stop.center(), Modifiers::none());
        assert!(
            visual.update(|_, cx| matches!(
                session.read(cx).lifecycle(),
                session::SessionLifecycle::Stopping { .. }
            )),
            "Stop must remain clickable from Home at the narrowest supported width"
        );
        Ok(())
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn the_home_entry_fits_the_rail_and_reports_only_what_is_measured()
    -> Result<(), Box<dyn std::error::Error>> {
        let mut cx = TestAppContext::single();
        cx.update(gpui_component::init);
        let dir = tempfile::tempdir()?;
        persist_stopped_session(&dir.path().join("sotto.sqlite3"), 3).await?;
        let visual = mount(&mut cx, dir.path(), None, MIN_WORKSPACE_WIDTH)?.visual;
        visual.refresh()?;
        visual.run_until_parked();

        let rail = visual
            .debug_bounds("library-rail")
            .ok_or_else(|| std::io::Error::other("the library rail must render"))?;
        let home = visual
            .debug_bounds("library-home")
            .ok_or_else(|| std::io::Error::other("the rail must carry a Home entry"))?;
        let title = visual
            .debug_bounds("library-home-title")
            .ok_or_else(|| std::io::Error::other("the Home entry must render its label"))?;
        let footprint = visual.debug_bounds("home-footprint").ok_or_else(|| {
            std::io::Error::other("Home must state what the library holds on this Mac")
        })?;
        let stage = visual
            .debug_bounds("workspace-stage")
            .ok_or_else(|| std::io::Error::other("the stage must render"))?;
        assert!(
            home.left() >= rail.left() && home.right() <= rail.right() && home.size.width > px(0.0),
            "the Home entry must fit the rail at the narrowest supported width"
        );
        assert!(
            title.left() >= rail.left() && title.right() <= rail.right(),
            "the Home label must truncate inside the rail rather than clip past it"
        );
        assert!(
            footprint.left() >= stage.left() && footprint.right() <= stage.right(),
            "Home's storage claim must stay inside the stage"
        );
        // The measured claim: one persisted session and no retained media, because
        // `persist_stopped_session` writes a timeline and never a recording file.
        let measured = crate::workspace::library::LibraryFootprint::measure(
            &dir.path().join("sotto.sqlite3"),
            &[SessionSummary {
                id: SessionId::new(1_786_625_633_040_598_000),
                capture_target: CaptureTarget::microphone_only(),
                title: None,
                started_at_unix_ms: 1_786_625_633_040,
                ended_at_unix_ms: Some(1_786_625_700_000),
            }],
        );
        assert_eq!(
            measured.claim(),
            "1 recording · no retained media — all on this Mac.",
            "Home must report what the store actually holds, never an invented figure"
        );
        Ok(())
    }

    #[test]
    fn a_wide_capture_bar_shows_every_control_it_can() -> Result<(), Box<dyn std::error::Error>> {
        let mut cx = TestAppContext::single();
        cx.update(gpui_component::init);
        let dir = tempfile::tempdir()?;
        let shell = mount(
            &mut cx,
            dir.path(),
            Some(running_window_capture()),
            WIDE_WORKSPACE_WIDTH,
        )?;
        let visual = shell.visual;
        visual.refresh()?;
        visual.run_until_parked();
        assert!(
            visual.debug_bounds("capture-scope-chips").is_some(),
            "scope chips belong on a wide capture bar"
        );
        assert!(
            visual.debug_bounds("capture-pause-control").is_none(),
            "no Pause control ships while nothing can suspend a recording; a permanently disabled \
             button reads as temporarily unavailable rather than absent"
        );
        assert!(
            visual.debug_bounds("view-bar").is_none(),
            "the view bar must never share the shell with the capture bar"
        );
        Ok(())
    }

    #[test]
    fn a_narrow_capture_bar_keeps_the_clock_and_a_clickable_stop()
    -> Result<(), Box<dyn std::error::Error>> {
        let mut cx = TestAppContext::single();
        cx.update(gpui_component::init);
        let dir = tempfile::tempdir()?;
        let shell = mount(
            &mut cx,
            dir.path(),
            Some(running_window_capture()),
            MIN_WORKSPACE_WIDTH,
        )?;
        let session = shell.session;
        let visual = shell.visual;
        visual.refresh()?;
        visual.run_until_parked();

        let bar = visual
            .debug_bounds("capture-bar")
            .ok_or_else(|| std::io::Error::other("a running recording must render its bar"))?;
        let stop = visual
            .debug_bounds("capture-stop-control")
            .ok_or_else(|| std::io::Error::other("Stop must survive the narrowest width"))?;
        let clock = visual
            .debug_bounds("capture-clock")
            .ok_or_else(|| std::io::Error::other("the clock must survive the narrowest width"))?;
        let target = visual
            .debug_bounds("capture-target-name")
            .ok_or_else(|| std::io::Error::other("the target name must remain rendered"))?;
        assert!(
            visual.debug_bounds("capture-scope-chips").is_none(),
            "scope chips must collapse out rather than paint half-clipped"
        );
        assert!(
            visual.debug_bounds("capture-pause-control").is_none(),
            "Pause must collapse out before anything essential is compressed"
        );
        assert!(
            stop.size.width > px(0.0) && clock.size.width > px(0.0),
            "Stop and the clock must keep real width"
        );
        assert!(
            clock.left() >= bar.left()
                && stop.left() >= bar.left()
                && clock.right() <= bar.right()
                && stop.right() <= bar.right(),
            "Stop and the clock must remain wholly inside the capture bar"
        );
        assert!(
            target.right() <= clock.left(),
            "the ellipsizing target name must yield to the essential clock"
        );
        assert!(
            bar.right() <= MIN_WORKSPACE_WIDTH,
            "the capture bar must not overflow the stated minimum width"
        );
        // A closed Ask reserves nothing. The 42 px vertical rail that used to sit here existed
        // only to carry the control that opens the panel, and that control is in the toolbar now.
        let stage = visual
            .debug_bounds("workspace-stage")
            .ok_or_else(|| std::io::Error::other("the stage must render"))?;
        assert!(
            stage.right() >= MIN_WORKSPACE_WIDTH - px(1.0),
            "with Ask closed the stage must reach the window's right edge rather than stop short \
             of a rail that holds nothing"
        );
        visual.simulate_click(stop.center(), Modifiers::none());
        assert!(
            visual.update(|_, cx| matches!(
                session.read(cx).lifecycle(),
                session::SessionLifecycle::Stopping { .. }
            )),
            "Stop must remain clickable at the stated minimum width"
        );
        Ok(())
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_narrow_view_bar_keeps_the_tabs_and_delete() -> Result<(), Box<dyn std::error::Error>>
    {
        let mut cx = TestAppContext::single();
        cx.update(gpui_component::init);
        let dir = tempfile::tempdir()?;
        let session_id = persist_stopped_session(&dir.path().join("sotto.sqlite3"), 7).await?;
        let shell = mount(&mut cx, dir.path(), None, MIN_WORKSPACE_WIDTH)?;
        let workspace = shell.workspace;
        let visual = shell.visual;
        // Launch lands on Home, so the recording under test is opened the way a person opens it.
        visual.update(|_, cx| {
            workspace.update(cx, |this, cx| this.select_meeting(session_id, cx));
        });
        visual.refresh()?;
        visual.run_until_parked();

        let bar = visual
            .debug_bounds("view-bar")
            .ok_or_else(|| std::io::Error::other("an open stopped session must render its bar"))?;
        let tabs = visual.debug_bounds("view-tabs").ok_or_else(|| {
            std::io::Error::other("the tab pair must survive the narrowest width")
        })?;
        let delete = visual
            .debug_bounds("view-delete-control")
            .ok_or_else(|| std::io::Error::other("Delete must survive the narrowest width"))?;
        let meta = visual
            .debug_bounds("view-meta")
            .ok_or_else(|| std::io::Error::other("duration and size must survive"))?;
        let title = visual
            .debug_bounds("view-title")
            .ok_or_else(|| std::io::Error::other("the title must remain rendered"))?;
        assert!(
            visual.debug_bounds("view-reveal-control").is_none(),
            "Reveal must collapse out before anything essential is compressed"
        );
        assert!(
            tabs.size.width > px(0.0) && delete.size.width > px(0.0),
            "the tabs and Delete must keep real width"
        );
        assert!(
            tabs.left() >= bar.left()
                && delete.right() <= bar.right()
                && meta.right() <= bar.right(),
            "every essential view-bar control must stay inside the bar"
        );
        assert!(
            title.right() <= meta.left(),
            "the ellipsizing title must yield to the essential duration and size"
        );
        assert!(
            bar.right() <= MIN_WORKSPACE_WIDTH,
            "the view bar must not overflow the stated minimum width"
        );
        assert!(
            visual.debug_bounds("capture-bar").is_none(),
            "a stopped session must not show the capture bar"
        );
        // T082 moved the app name, Settings and the appearance switch to the menu bar. The window
        // must not draw a second copy of the OS's own chrome, in any state.
        assert!(
            visual.debug_bounds("titlebar").is_none()
                && visual.debug_bounds("titlebar-settings").is_none()
                && visual.debug_bounds("titlebar-theme").is_none(),
            "the in-window title-bar row is gone; macOS carries that chrome now"
        );
        // The one band that remains is the transparent titlebar's, kept clear because macOS draws
        // the traffic lights over it. It is not a second copy of the OS's chrome — it holds no
        // control at all — but the state bar does sit below it.
        let strip = visual
            .debug_bounds("title-strip")
            .ok_or_else(|| std::io::Error::other("the traffic-light strip must be reserved"))?;
        assert!(
            strip.top() <= px(1.0),
            "the reserved strip belongs at the very top of the window"
        );
        assert!(
            bar.top() >= strip.bottom() && bar.top() < strip.bottom() + px(24.0),
            "the state bar sits immediately under the traffic-light strip, not below a second band"
        );
        Ok(())
    }

    /// Settings is an overlay in this window, and since T082 the only way in is the menu bar.
    ///
    /// This test exists because the previous one lied. It called `toggle_settings` directly and
    /// then claimed "the menu item's path must open the same overlay the gear does" — which is
    /// exactly the assertion the shipped menu item failed. Here the action is dispatched through
    /// `App::dispatch_action`, which is verbatim what GPUI's macOS menu callback does, against a
    /// window rooted in `gpui_component::Root` and activated, as the product's is.
    #[test]
    fn the_settings_menu_action_opens_the_overlay_over_this_window()
    -> Result<(), Box<dyn std::error::Error>> {
        let mut cx = TestAppContext::single();
        cx.update(gpui_component::init);
        let dir = tempfile::tempdir()?;
        let app = mount_as_the_product_does(&mut cx, dir.path(), None, WIDE_WORKSPACE_WIDTH)?;
        let workspace = app.workspace;
        let visual = app.visual;
        visual.refresh()?;
        visual.run_until_parked();

        assert!(
            visual.update(|_, cx| !workspace.read(cx).settings_open)
                && visual.debug_bounds("settings-scrim").is_none(),
            "the shell must not open on settings"
        );

        cx.update(|cx| cx.dispatch_action(&OpenSettings));
        visual.refresh()?;
        visual.run_until_parked();
        assert!(
            visual.update(|_, cx| workspace.read(cx).settings_open),
            "`Sotto ▸ Settings…` must open the overlay; before T082 its listener ran and then \
             failed to re-enter the window the dispatch was already holding"
        );
        assert_eq!(
            cx.update(|cx| cx.windows().len()),
            1,
            "opening settings must not create a second window"
        );

        let stage = visual
            .debug_bounds("workspace-stage")
            .ok_or_else(|| std::io::Error::other("the workspace stage must render"))?;
        let scrim = visual.debug_bounds("settings-scrim").ok_or_else(|| {
            std::io::Error::other("settings must render as an overlay inside this window")
        })?;
        assert!(
            scrim.top() <= stage.top() && scrim.size.width >= stage.size.width,
            "the settings scrim must cover the workspace it configures"
        );
        // The pinned disclosures are the reason this move is dangerous. Prove the pane they lead
        // is really laid out here, not merely constructed.
        assert!(
            visual
                .debug_bounds("settings-disclosure-recordings")
                .is_some()
                && visual
                    .debug_bounds("settings-disclosure-reasoning")
                    .is_some(),
            "the storage pane's disclosures must survive being reached from the menu"
        );

        let close = visual
            .debug_bounds("settings-close")
            .ok_or_else(|| std::io::Error::other("the sheet's close control must render"))?;
        visual.simulate_click(close.center(), Modifiers::none());
        visual.run_until_parked();
        assert!(
            visual.update(|_, cx| !workspace.read(cx).settings_open),
            "the sheet's close control must dismiss the overlay"
        );

        // The menu item is a toggle over one overlay, so a second dispatch must re-open it rather
        // than land on stale state. Selection is the authority here rather than `debug_bounds`:
        // GPUI's `Frame::clear` leaves an earlier frame's entries in the map.
        cx.update(|cx| cx.dispatch_action(&OpenSettings));
        visual.refresh()?;
        visual.run_until_parked();
        assert!(
            visual.update(|_, cx| workspace.read(cx).settings_open),
            "the menu item must still open the overlay after it has been closed once"
        );
        visual.simulate_keystrokes("escape");
        visual.run_until_parked();
        assert!(
            visual.update(|_, cx| !workspace.read(cx).settings_open),
            "Escape must dismiss the settings overlay"
        );
        assert_eq!(
            cx.update(|cx| cx.windows().len()),
            1,
            "dismissing settings must leave the workspace window alone"
        );
        Ok(())
    }

    /// Settings had to stay reachable while a recording runs — that is the whole reason it could
    /// be taken out of the window. Prove the menu path works mid-capture and that Stop, which
    /// lives on the capture bar and is unaffected by this move, is still one visible action away.
    #[test]
    fn settings_is_reachable_from_the_menu_while_a_recording_runs()
    -> Result<(), Box<dyn std::error::Error>> {
        let mut cx = TestAppContext::single();
        cx.update(gpui_component::init);
        let dir = tempfile::tempdir()?;
        let app = mount_as_the_product_does(
            &mut cx,
            dir.path(),
            Some(running_window_capture()),
            MIN_WORKSPACE_WIDTH,
        )?;
        let workspace = app.workspace;
        let visual = app.visual;
        visual.refresh()?;
        visual.run_until_parked();

        cx.update(|cx| cx.dispatch_action(&OpenSettings));
        visual.refresh()?;
        visual.run_until_parked();
        assert!(
            visual.update(|_, cx| workspace.read(cx).settings_open),
            "Settings must be reachable from the menu bar while a recording runs"
        );
        let bar = visual.debug_bounds("capture-bar").ok_or_else(|| {
            std::io::Error::other("the capture bar must survive opening Settings")
        })?;
        let stop = visual
            .debug_bounds("capture-stop-control")
            .ok_or_else(|| std::io::Error::other("Stop must survive opening Settings"))?;
        assert!(
            stop.size.width > px(0.0) && stop.left() >= bar.left() && stop.right() <= bar.right(),
            "Stop stays on the capture bar, wholly inside it; this move does not touch it"
        );
        Ok(())
    }

    /// The appearance switch is only worth publishing because it really switches. Prove all three
    /// choices work from the menu-bar action, that the choice is written down rather than
    /// forgotten at quit, and that "Follow System" is a real state rather than a dead item.
    #[test]
    fn appearance_is_chosen_from_the_menu_bar_and_recorded()
    -> Result<(), Box<dyn std::error::Error>> {
        let mut cx = TestAppContext::single();
        cx.update(gpui_component::init);
        let dir = tempfile::tempdir()?;
        let database = dir.path().join("sotto.sqlite3");
        let app = mount_as_the_product_does(&mut cx, dir.path(), None, WIDE_WORKSPACE_WIDTH)?;
        let workspace = app.workspace;
        let visual = app.visual;
        visual.update(|window, cx| Theme::change(ThemeMode::Light, Some(window), cx));
        visual.run_until_parked();
        assert_eq!(
            visual.update(|_, cx| workspace.read(cx).appearance()),
            Appearance::FollowSystem,
            "with nothing recorded, Sotto follows the system"
        );

        cx.update(|cx| cx.dispatch_action(&UseDarkAppearance));
        visual.refresh()?;
        visual.run_until_parked();
        assert!(
            visual.update(|_, cx| cx.theme().is_dark()),
            "`View ▸ Appearance ▸ Dark` must actually switch the theme, or it is a dead item"
        );
        assert_eq!(
            load_workspace_state(&database).theme,
            Some(PersistedTheme::Dark),
            "the chosen appearance must be recorded so it survives a relaunch"
        );

        cx.update(|cx| cx.dispatch_action(&UseLightAppearance));
        visual.refresh()?;
        visual.run_until_parked();
        assert!(
            visual.update(|_, cx| !cx.theme().is_dark()),
            "the appearance menu must switch back, not only one way"
        );
        assert_eq!(
            visual.update(|_, cx| workspace.read(cx).theme),
            Some(PersistedTheme::Light),
            "the shell must hold the choice it just wrote"
        );

        cx.update(|cx| cx.dispatch_action(&FollowSystemAppearance));
        visual.refresh()?;
        visual.run_until_parked();
        assert_eq!(
            visual.update(|_, cx| workspace.read(cx).appearance()),
            Appearance::FollowSystem,
            "choosing Follow System must be a way back, not a no-op"
        );
        assert_eq!(
            load_workspace_state(&database).theme,
            None,
            "following the system means no recorded palette, so a later system change is adopted"
        );
        Ok(())
    }

    /// `KeyBinding::new` panics on a keystroke it cannot parse, and this one runs on the launch
    /// path — so a typo in `"cmd-,"` would be a crash on start, not a missing shortcut. Build the
    /// bindings and then prove the shortcut really opens Settings, through the same dispatch the
    /// menu item uses.
    #[test]
    fn the_settings_shortcut_parses_and_opens_settings() -> Result<(), Box<dyn std::error::Error>> {
        let mut cx = TestAppContext::single();
        cx.update(gpui_component::init);
        let dir = tempfile::tempdir()?;
        let app = mount_as_the_product_does(&mut cx, dir.path(), None, WIDE_WORKSPACE_WIDTH)?;
        let workspace = app.workspace;
        let visual = app.visual;
        cx.update(|cx| cx.bind_keys(crate::workspace::key_bindings()));
        visual.refresh()?;
        visual.run_until_parked();

        visual.simulate_keystrokes("cmd-,");
        visual.refresh()?;
        visual.run_until_parked();
        assert!(
            visual.update(|_, cx| workspace.read(cx).settings_open),
            "⌘, must open Settings; it is the shortcut people try before reading the menu"
        );
        Ok(())
    }

    /// GPUI 0.2.2 cannot check a menu item, so the mark lives in the item's text. If that mark
    /// drifts, the menu silently stops saying which appearance is in force.
    #[test]
    fn the_appearance_menu_marks_exactly_the_choice_in_force() {
        for appearance in [
            Appearance::FollowSystem,
            Appearance::Light,
            Appearance::Dark,
        ] {
            let menus = crate::workspace::application_menus(appearance);
            let names = menus
                .iter()
                .flat_map(|menu| &menu.items)
                .filter_map(|item| match item {
                    gpui::MenuItem::Submenu(submenu) if submenu.name == "Appearance" => {
                        Some(&submenu.items)
                    }
                    _ => None,
                })
                .flatten()
                .filter_map(|item| match item {
                    gpui::MenuItem::Action { name, .. } => Some(name.to_string()),
                    _ => None,
                })
                .collect::<Vec<_>>();
            assert_eq!(
                names.len(),
                3,
                "Appearance offers Follow System, Light and Dark — all three, always"
            );
            let marked = names
                .iter()
                .filter(|name| name.starts_with('\u{2713}'))
                .collect::<Vec<_>>();
            assert_eq!(
                marked.len(),
                1,
                "exactly one appearance is in force at a time: {names:?}"
            );
            assert!(
                marked[0].contains(match appearance {
                    Appearance::FollowSystem => "Follow System",
                    Appearance::Light => "Light",
                    Appearance::Dark => "Dark",
                }),
                "the marked item must be the one actually in force: {names:?}"
            );
        }
        // Settings stays in the app menu: it is the item people reach for by convention, and it
        // must not migrate into View along with the appearance switch.
        let app_menu = crate::workspace::application_menus(Appearance::FollowSystem);
        assert!(
            app_menu.first().is_some_and(|menu| menu.name == "Sotto"
                && menu.items.iter().any(|item| matches!(
                    item,
                    gpui::MenuItem::Action { name, .. } if name.starts_with("Settings")
                ))),
            "`Sotto ▸ Settings…` is the entry point the window no longer carries"
        );
    }

    /// Stopping a recording must enable Summarize on the very next frame.
    ///
    /// `transcript_live` is a cached flag cleared only by `load_transcript`, which sits behind three
    /// early returns in `refresh_after_session`. When one of them took, a stopped recording kept
    /// rendering as live and Summarize stayed disabled — and selecting another recording and coming
    /// back fixed it, because that path calls `load_transcript` directly. The shell now reads the
    /// flag against the controller's own `completed_session_id`, so a finished recording cannot
    /// render as live however the refresh went.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_finished_recording_stops_rendering_as_live_without_a_refresh()
    -> Result<(), Box<dyn std::error::Error>> {
        let mut cx = TestAppContext::single();
        cx.update(gpui_component::init);
        let dir = tempfile::tempdir()?;
        let session_id = persist_stopped_session(&dir.path().join("sotto.sqlite3"), 3).await?;
        let shell = mount(&mut cx, dir.path(), None, WIDE_WORKSPACE_WIDTH)?;
        let workspace = shell.workspace;
        let visual = shell.visual;

        // The state the bug left behind: the session is open and still flagged live, while the
        // controller has already published it as finished.
        visual.update(|_, cx| {
            workspace.update(cx, |workspace, cx| {
                workspace.select_meeting(session_id, cx);
                workspace.transcript_live = true;
            });
            shell.session.update(cx, |controller, _| {
                controller.set_completed_for_test(Some(session_id));
            });
        });
        visual.refresh()?;
        visual.run_until_parked();

        let summarize = visual.debug_bounds("summarize-control").ok_or_else(|| {
            std::io::Error::other("Summarize must render for a stopped recording")
        })?;
        assert!(
            summarize.size.width > px(0.0),
            "Summarize must be laid out rather than collapsed"
        );
        assert!(
            visual.debug_bounds("capture-bar").is_none(),
            "a finished recording must not keep drawing the capture bar"
        );
        Ok(())
    }

    /// The second recording a delete test must leave alone.
    const BYSTANDER_SESSION: SessionId = SessionId::new(1_786_625_633_040_599_000);

    /// Opens a stopped recording with measured retained media, ready for the trash control.
    ///
    /// The shell is mounted the way `main.rs` mounts it — under `gpui_component::Root` — because
    /// that is the whole precondition for a dialog: `Window::open_dialog` stores the open dialogs
    /// on the `Root`, and the layer that draws them reads them back from there.
    async fn mount_with_a_recording_open(
        cx: &mut TestAppContext,
        directory: &std::path::Path,
    ) -> Result<(SessionId, MountedApp), Box<dyn std::error::Error>> {
        let database = directory.join("sotto.sqlite3");
        let session_id = persist_stopped_session(&database, 4).await?;
        persist_stopped_session_titled(&database, BYSTANDER_SESSION, "A bystander recording", 2)
            .await?;
        let app = mount_as_the_product_does(cx, directory, None, WIDE_WORKSPACE_WIDTH)?;
        let workspace = app.workspace.clone();
        app.visual.update(|_, cx| {
            workspace.update(cx, |this, cx| {
                this.select_meeting(session_id, cx);
                // Stand in for retained media the store has no file for, so the prompt has a
                // measured size to name.
                this.open_recording = Some(OpenRecording {
                    path: Some("/tmp/recording.m4a".to_owned()),
                    duration: Some(Duration::from_secs(2_892)),
                    byte_size: Some(412_000_000),
                });
            });
        });
        app.visual.refresh()?;
        app.visual.run_until_parked();
        Ok((session_id, app))
    }

    async fn session_survives(database: &std::path::Path, session_id: SessionId) -> bool {
        let Ok(store) = rag::Store::open(database).await else {
            return false;
        };
        store
            .load_session(session_id)
            .await
            .is_ok_and(|events| !events.is_empty())
    }

    /// Opens the view bar's trash control and returns the dialog's Cancel and OK bounds.
    fn open_view_bar_delete_dialog(
        visual: &mut VisualTestContext,
    ) -> Result<(Bounds<gpui::Pixels>, Bounds<gpui::Pixels>), Box<dyn std::error::Error>> {
        let delete = visual
            .debug_bounds("view-delete-control")
            .ok_or_else(|| std::io::Error::other("the view bar must carry a delete control"))?;
        visual.simulate_click(delete.center(), Modifiers::none());
        visual.refresh()?;
        visual.run_until_parked();
        let cancel = visual
            .debug_bounds(CONFIRM_CANCEL_SELECTOR)
            .ok_or_else(|| std::io::Error::other("a destructive confirmation must offer Cancel"))?;
        let ok = visual
            .debug_bounds(CONFIRM_OK_SELECTOR)
            .ok_or_else(|| std::io::Error::other("a destructive confirmation must offer OK"))?;
        Ok((cancel, ok))
    }

    /// Delete is a glyph in the view bar, so the dialog it opens carries the whole target: which
    /// recording, and how much of this Mac goes with it. Cancel is a real answer, not a discovery.
    #[tokio::test(flavor = "multi_thread")]
    async fn the_view_bar_delete_asks_in_a_dialog_and_cancel_keeps_everything()
    -> Result<(), Box<dyn std::error::Error>> {
        let mut cx = TestAppContext::single();
        cx.update(gpui_component::init);
        let dir = tempfile::tempdir()?;
        let database = dir.path().join("sotto.sqlite3");
        let (session_id, app) = mount_with_a_recording_open(&mut cx, dir.path()).await?;
        let workspace = app.workspace;
        let visual = app.visual;

        assert_eq!(
            visual.update(|_, cx| workspace.update(cx, |this, cx| this.delete_prompt(session_id, cx))),
            "Delete “This is a persisted recording” and its 412 MB? This removes the recording, \
             its transcript and its notes from this Mac.",
            "the dialog must keep every word the armed state earned: the recording, its measured \
             size, and everything the deletion takes with it"
        );

        let (cancel, _) = open_view_bar_delete_dialog(visual)?;
        assert!(
            visual.update(|window, cx| window.has_active_dialog(cx)),
            "the trash control must ask before it acts"
        );
        assert!(
            visual.update(|_, cx| workspace.read(cx).message.is_none()),
            "the prompt belongs to the dialog beside the control, not to the message strip at the \
             top of the window"
        );

        visual.simulate_click(cancel.center(), Modifiers::none());
        visual.refresh()?;
        visual.run_until_parked();
        assert!(
            !visual.update(|window, cx| window.has_active_dialog(cx)),
            "Cancel must dismiss the dialog"
        );
        assert!(
            session_survives(&database, session_id).await,
            "Cancel must leave the recording, its transcript and its notes untouched"
        );
        assert_eq!(
            visual.update(|_, cx| workspace.read(cx).transcript_session),
            Some(session_id),
            "Cancel must leave the recording open, exactly as it was"
        );
        Ok(())
    }

    /// Escape is the keyboard's Cancel, and closing must hand focus back rather than strand it.
    #[tokio::test(flavor = "multi_thread")]
    async fn escape_cancels_the_view_bar_delete_dialog() -> Result<(), Box<dyn std::error::Error>> {
        let mut cx = TestAppContext::single();
        cx.update(gpui_component::init);
        let dir = tempfile::tempdir()?;
        let database = dir.path().join("sotto.sqlite3");
        let (session_id, app) = mount_with_a_recording_open(&mut cx, dir.path()).await?;
        let visual = app.visual;
        open_view_bar_delete_dialog(visual)?;

        visual.simulate_keystrokes("escape");
        visual.refresh()?;
        visual.run_until_parked();
        assert!(
            !visual.update(|window, cx| window.has_active_dialog(cx)),
            "Escape must cancel the dialog"
        );
        assert!(
            session_survives(&database, session_id).await,
            "Escape must leave the recording untouched"
        );
        // Focus is not stranded on an element that is no longer rendered: the trash control is
        // still reachable, and opening the dialog a second time works exactly as the first did.
        open_view_bar_delete_dialog(visual)?;
        assert!(
            visual.update(|window, cx| window.has_active_dialog(cx)),
            "the shell must still be interactive after a cancelled confirmation"
        );
        Ok(())
    }

    /// Confirming removes the recording the dialog named — and only that one.
    #[tokio::test(flavor = "multi_thread")]
    async fn confirming_the_view_bar_dialog_deletes_only_that_recording()
    -> Result<(), Box<dyn std::error::Error>> {
        let mut cx = TestAppContext::single();
        cx.update(gpui_component::init);
        let dir = tempfile::tempdir()?;
        let database = dir.path().join("sotto.sqlite3");
        let (session_id, app) = mount_with_a_recording_open(&mut cx, dir.path()).await?;
        let workspace = app.workspace;
        let visual = app.visual;
        let (_, ok) = open_view_bar_delete_dialog(visual)?;

        visual.simulate_click(ok.center(), Modifiers::none());
        visual.refresh()?;
        visual.run_until_parked();
        assert!(
            !visual.update(|window, cx| window.has_active_dialog(cx)),
            "confirming must close the dialog"
        );
        assert!(
            !session_survives(&database, session_id).await,
            "confirming must delete the recording the dialog named"
        );
        assert!(
            session_survives(&database, BYSTANDER_SESSION).await,
            "confirming must delete exactly the recording named and nothing else"
        );
        assert_eq!(
            visual.update(|_, cx| workspace.read(cx).transcript_session),
            None,
            "deleting what you were reading lands on Home rather than inside another recording"
        );
        Ok(())
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_stopped_session_opens_on_notes_with_transcript_one_tab_away()
    -> Result<(), Box<dyn std::error::Error>> {
        let mut cx = TestAppContext::single();
        cx.update(gpui_component::init);
        let dir = tempfile::tempdir()?;
        // Enough rows that the list genuinely scrolls; a transcript that fits the viewport has no
        // scroll position to lose and would prove nothing.
        let session_id = persist_stopped_session(&dir.path().join("sotto.sqlite3"), 80).await?;
        let shell = mount(&mut cx, dir.path(), None, WIDE_WORKSPACE_WIDTH)?;
        let workspace = shell.workspace;
        let visual = shell.visual;
        // Launch lands on Home, so the recording under test is opened the way a person opens it.
        visual.update(|_, cx| {
            workspace.update(cx, |this, cx| this.select_meeting(session_id, cx));
        });
        visual.refresh()?;
        visual.run_until_parked();

        assert_eq!(
            visual.update(|_, cx| workspace.read(cx).stage_tab),
            StageTab::Notes,
            "a stopped session must open on its summary"
        );
        let stage = visual
            .debug_bounds("workspace-stage")
            .ok_or_else(|| std::io::Error::other("the stage must render"))?;
        let notes = visual
            .debug_bounds("stage-notes")
            .ok_or_else(|| std::io::Error::other("the Notes layer must render"))?;
        let transcript = visual
            .debug_bounds("stage-transcript")
            .ok_or_else(|| std::io::Error::other("the Transcript layer must stay mounted"))?;
        assert!(
            notes.left() >= stage.left() && notes.right() <= stage.right() + px(1.0),
            "Notes must occupy the stage while it is the selected tab"
        );
        assert!(
            transcript.left() >= stage.right(),
            "the unselected tab must stay mounted outside the clipped stage so its scroll survives"
        );

        // Show the transcript, scroll it, leave, and come back.
        let transcript_tab = visual
            .debug_bounds("stage-tab-transcript")
            .ok_or_else(|| std::io::Error::other("the Transcript tab must be clickable"))?;
        visual.simulate_click(transcript_tab.center(), Modifiers::none());
        visual.refresh()?;
        visual.run_until_parked();
        assert_eq!(
            visual.update(|_, cx| workspace.read(cx).stage_tab),
            StageTab::Transcript,
            "clicking Transcript must switch the stage"
        );
        let shown_transcript = visual
            .debug_bounds("stage-transcript")
            .ok_or_else(|| std::io::Error::other("the Transcript layer must render"))?;
        assert!(
            shown_transcript.left() >= stage.left() && shown_transcript.left() < stage.right(),
            "the selected Transcript tab must occupy the stage"
        );

        visual.update(|_, cx| {
            workspace.update(cx, |workspace, _| {
                workspace.transcript_list.scroll_to_reveal_item(12);
            });
        });
        visual.refresh()?;
        visual.run_until_parked();
        let scrolled = visual.update(|_, cx| {
            let offset = workspace.read(cx).transcript_list.logical_scroll_top();
            (offset.item_ix, offset.offset_in_item)
        });

        let notes_tab = visual
            .debug_bounds("stage-tab-notes")
            .ok_or_else(|| std::io::Error::other("the Notes tab must be clickable"))?;
        visual.simulate_click(notes_tab.center(), Modifiers::none());
        visual.refresh()?;
        visual.run_until_parked();
        assert_eq!(
            visual.update(|_, cx| workspace.read(cx).stage_tab),
            StageTab::Notes,
            "clicking Notes must switch back"
        );

        let transcript_tab = visual
            .debug_bounds("stage-tab-transcript")
            .ok_or_else(|| std::io::Error::other("the Transcript tab must remain clickable"))?;
        visual.simulate_click(transcript_tab.center(), Modifiers::none());
        visual.refresh()?;
        visual.run_until_parked();
        assert_eq!(
            visual.update(|_, cx| {
                let offset = workspace.read(cx).transcript_list.logical_scroll_top();
                (offset.item_ix, offset.offset_in_item)
            }),
            scrolled,
            "returning to the Transcript tab must land on the row the user left"
        );
        assert!(
            visual.debug_bounds("stage-notes").is_some()
                && visual.debug_bounds("stage-transcript").is_some(),
            "both stage layers stay mounted so neither loses its scroll state"
        );
        Ok(())
    }

    /// The three toolbar controls, by their debug selectors, in the order they are read.
    const TOOLBAR_CONTROLS: [&str; 3] = [
        "toolbar-library-toggle",
        "toolbar-search",
        "toolbar-ask-toggle",
    ];

    /// Every toolbar control's rectangle, or a failure naming the one that did not render.
    fn toolbar_bounds(
        visual: &mut VisualTestContext,
    ) -> Result<Vec<gpui::Bounds<gpui::Pixels>>, Box<dyn std::error::Error>> {
        TOOLBAR_CONTROLS
            .into_iter()
            .map(|selector| {
                visual.debug_bounds(selector).ok_or_else(|| {
                    Box::new(std::io::Error::other(format!(
                        "{selector} must be laid out in the toolbar"
                    ))) as Box<dyn std::error::Error>
                })
            })
            .collect()
    }

    /// The strip macOS reserves for its own buttons is Sotto's toolbar now, and the whole
    /// justification for putting anything there is that the traffic lights keep their corner.
    #[test]
    fn the_toolbar_fills_the_strip_without_reaching_under_the_traffic_lights()
    -> Result<(), Box<dyn std::error::Error>> {
        let mut cx = TestAppContext::single();
        cx.update(gpui_component::init);
        let dir = tempfile::tempdir()?;
        let visual = mount(&mut cx, dir.path(), None, WIDE_WORKSPACE_WIDTH)?.visual;
        visual.refresh()?;
        visual.run_until_parked();

        let strip = visual
            .debug_bounds("title-strip")
            .ok_or_else(|| std::io::Error::other("the traffic-light strip must be reserved"))?;
        assert!(
            strip.top() <= px(1.0),
            "the toolbar is the reserved strip, so it belongs at the very top of the window"
        );
        assert_eq!(
            strip.size.height,
            crate::workspace::TITLE_STRIP_HEIGHT,
            "the toolbar must cost no height beyond the strip that already existed"
        );
        for (selector, bounds) in TOOLBAR_CONTROLS.into_iter().zip(toolbar_bounds(visual)?) {
            assert!(
                bounds.size.width > px(0.0),
                "{selector} must have real width, not be built and then collapsed away"
            );
            assert!(
                bounds.left() >= TOOLBAR_LEADING_INSET,
                "{selector} starts at {:?}, which is under the traffic lights",
                bounds.left()
            );
            assert!(
                bounds.right() <= WIDE_WORKSPACE_WIDTH,
                "{selector} must stay inside the window"
            );
            assert!(
                bounds.top() >= strip.top() && bounds.bottom() <= strip.bottom(),
                "{selector} must sit inside the strip rather than spill onto the bar below it"
            );
        }
        Ok(())
    }

    /// The sidebar collapses, gives its width to the stage, and comes back from a control that has
    /// not moved. The last clause is the one that matters: a person who collapses the rail and
    /// forgets must be able to *see* the way back, which rules out a hover-to-reveal edge.
    #[tokio::test(flavor = "multi_thread")]
    async fn the_library_collapses_from_the_toolbar_and_comes_back()
    -> Result<(), Box<dyn std::error::Error>> {
        let mut cx = TestAppContext::single();
        cx.update(gpui_component::init);
        let dir = tempfile::tempdir()?;
        persist_stopped_session(&dir.path().join("sotto.sqlite3"), 4).await?;
        let shell = mount(&mut cx, dir.path(), None, WIDE_WORKSPACE_WIDTH)?;
        let workspace = shell.workspace;
        let visual = shell.visual;
        visual.refresh()?;
        visual.run_until_parked();

        let expanded_stage = visual
            .debug_bounds("workspace-stage")
            .ok_or_else(|| std::io::Error::other("the stage must render"))?;
        let toggle = visual
            .debug_bounds("toolbar-library-toggle")
            .ok_or_else(|| std::io::Error::other("the toolbar must carry the sidebar toggle"))?;
        assert!(
            expanded_stage.left() >= LIBRARY_WIDTH,
            "with the rail shown the stage must begin after it"
        );

        visual.simulate_click(toggle.center(), Modifiers::none());
        visual.refresh()?;
        visual.run_until_parked();
        assert!(
            visual.update(|_, cx| workspace.read(cx).library_collapsed),
            "the toolbar control must actually collapse the rail"
        );
        let collapsed_stage = visual
            .debug_bounds("workspace-stage")
            .ok_or_else(|| std::io::Error::other("the stage must still render"))?;
        assert!(
            collapsed_stage.left() <= px(1.0),
            "a collapsed rail must give its width back to the stage, not merely hide its content"
        );
        assert!(
            collapsed_stage.size.width >= expanded_stage.size.width + LIBRARY_WIDTH - px(2.0),
            "the stage must gain the rail's whole width: {:?} then {:?}",
            expanded_stage.size.width,
            collapsed_stage.size.width
        );
        let after = visual
            .debug_bounds("toolbar-library-toggle")
            .ok_or_else(|| {
                std::io::Error::other("the way back must still be on screen once the rail is gone")
            })?;
        assert_eq!(
            (after.origin, after.size),
            (toggle.origin, toggle.size),
            "the control that brings the rail back must not move when the rail goes"
        );

        visual.simulate_click(after.center(), Modifiers::none());
        visual.refresh()?;
        visual.run_until_parked();
        assert!(
            visual.update(|_, cx| !workspace.read(cx).library_collapsed),
            "the same control must restore the rail"
        );
        let restored = visual
            .debug_bounds("workspace-stage")
            .ok_or_else(|| std::io::Error::other("the stage must render"))?;
        assert_eq!(
            (restored.origin, restored.size),
            (expanded_stage.origin, expanded_stage.size),
            "restoring must return the exact layout the person collapsed"
        );
        Ok(())
    }

    /// The choice has to outlive the launch that made it, the way `ask_open` already does.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_collapsed_library_is_still_collapsed_after_a_relaunch()
    -> Result<(), Box<dyn std::error::Error>> {
        let mut cx = TestAppContext::single();
        cx.update(gpui_component::init);
        let dir = tempfile::tempdir()?;
        let database = dir.path().join("sotto.sqlite3");
        persist_stopped_session(&database, 3).await?;
        let first = mount(&mut cx, dir.path(), None, WIDE_WORKSPACE_WIDTH)?;
        let visual = first.visual;
        visual.refresh()?;
        visual.run_until_parked();
        assert!(
            !load_workspace_state(&database).library_collapsed,
            "the rail starts shown, so nothing is recorded until the person chooses"
        );

        let toggle = visual
            .debug_bounds("toolbar-library-toggle")
            .ok_or_else(|| std::io::Error::other("the toolbar must carry the sidebar toggle"))?;
        visual.simulate_click(toggle.center(), Modifiers::none());
        visual.refresh()?;
        visual.run_until_parked();
        assert!(
            load_workspace_state(&database).library_collapsed,
            "collapsing must be written down, not only held in memory until quit"
        );

        // Relaunch: a second shell over the same store, exactly as a cold start reads it.
        let relaunched = mount(&mut cx, dir.path(), None, WIDE_WORKSPACE_WIDTH)?;
        let workspace = relaunched.workspace;
        let visual = relaunched.visual;
        visual.refresh()?;
        visual.run_until_parked();
        assert!(
            visual.update(|_, cx| workspace.read(cx).library_collapsed),
            "a relaunch must honour the recorded choice"
        );
        let stage = visual
            .debug_bounds("workspace-stage")
            .ok_or_else(|| std::io::Error::other("the relaunched stage must render"))?;
        assert!(
            stage.left() <= px(1.0),
            "the relaunched window must open with the rail's width already reclaimed"
        );
        assert!(
            visual.debug_bounds("toolbar-library-toggle").is_some(),
            "the way back must be on screen from the first frame after a relaunch"
        );
        Ok(())
    }

    /// Search moved to the toolbar because it used to live in the rail, and a search control that
    /// disappears with the list it filters is not one a person can use to find anything.
    #[tokio::test(flavor = "multi_thread")]
    async fn toolbar_search_reaches_a_collapsed_library_and_still_matches_bodies()
    -> Result<(), Box<dyn std::error::Error>> {
        let mut cx = TestAppContext::single();
        cx.update(gpui_component::init);
        let dir = tempfile::tempdir()?;
        persist_stopped_session(&dir.path().join("sotto.sqlite3"), 4).await?;
        let shell = mount(&mut cx, dir.path(), None, WIDE_WORKSPACE_WIDTH)?;
        let workspace = shell.workspace;
        let visual = shell.visual;
        visual.refresh()?;
        visual.run_until_parked();

        // The rail no longer draws a search field of its own; there is exactly one, in the toolbar.
        assert!(
            visual.debug_bounds("library-search").is_none(),
            "the rail's own search field is gone — two search fields would be two haystacks"
        );
        let toggle = visual
            .debug_bounds("toolbar-library-toggle")
            .ok_or_else(|| std::io::Error::other("the toolbar must carry the sidebar toggle"))?;
        visual.simulate_click(toggle.center(), Modifiers::none());
        visual.refresh()?;
        visual.run_until_parked();
        assert!(
            visual.update(|_, cx| workspace.read(cx).library_collapsed),
            "this test is about searching while the rail is hidden"
        );
        let search = visual.debug_bounds("toolbar-search").ok_or_else(|| {
            std::io::Error::other("search must stay reachable with the rail collapsed")
        })?;
        assert!(
            search.size.width > px(0.0) && search.left() >= TOOLBAR_LEADING_INSET,
            "the search field must keep real width, clear of the traffic lights"
        );

        // "persisted final" appears only in the transcript rows, never in the recording's title,
        // so a match on it can only have come from the indexed body.
        visual.update(|window, cx| {
            workspace.update(cx, |this, cx| {
                this.library_filter.update(cx, |state, cx| {
                    state.set_value("persisted final", window, cx);
                });
            });
        });
        visual.refresh()?;
        visual.run_until_parked();

        assert!(
            visual.update(|_, cx| !workspace.read(cx).library_collapsed),
            "typing a query must bring the rail back — results that land nowhere are no results"
        );
        let (query, indexed_by_body, title) = visual.update(|_, cx| {
            let workspace = workspace.read(cx);
            (
                workspace.library_filter.read(cx).value().to_string(),
                workspace
                    .library_index
                    .values()
                    .any(|text| text.contains("persisted final")),
                workspace
                    .library_index
                    .keys()
                    .next()
                    .copied()
                    .map(|id| id.get()),
            )
        });
        assert_eq!(
            query, "persisted final",
            "the rail is filtered by the toolbar field's own value"
        );
        assert!(
            indexed_by_body && title.is_some(),
            "the haystack must still hold what was said, not only the recording's title"
        );
        let stage = visual
            .debug_bounds("workspace-stage")
            .ok_or_else(|| std::io::Error::other("the stage must render"))?;
        assert!(
            stage.left() >= LIBRARY_WIDTH,
            "the restored rail must occupy its width again so the matches are visible"
        );
        Ok(())
    }

    /// Ask is toggled from the toolbar, and a closed Ask costs the stage nothing.
    #[test]
    fn ask_reserves_no_width_until_the_toolbar_opens_it() -> Result<(), Box<dyn std::error::Error>>
    {
        let mut cx = TestAppContext::single();
        cx.update(gpui_component::init);
        let dir = tempfile::tempdir()?;
        let shell = mount(&mut cx, dir.path(), None, WIDE_WORKSPACE_WIDTH)?;
        let workspace = shell.workspace;
        let visual = shell.visual;
        visual.refresh()?;
        visual.run_until_parked();

        let closed = visual
            .debug_bounds("workspace-stage")
            .ok_or_else(|| std::io::Error::other("the stage must render"))?;
        assert!(
            closed.right() >= WIDE_WORKSPACE_WIDTH - px(1.0),
            "with Ask closed nothing may sit between the stage and the window's right edge"
        );

        let toggle = visual
            .debug_bounds("toolbar-ask-toggle")
            .ok_or_else(|| std::io::Error::other("the toolbar must carry the Ask toggle"))?;
        visual.simulate_click(toggle.center(), Modifiers::none());
        visual.refresh()?;
        visual.run_until_parked();
        assert!(
            visual.update(|_, cx| workspace.read(cx).ask_open),
            "the toolbar control must open Ask"
        );
        let panel = visual
            .debug_bounds("ask-panel")
            .ok_or_else(|| std::io::Error::other("an open Ask must render its panel"))?;
        let opened = visual
            .debug_bounds("workspace-stage")
            .ok_or_else(|| std::io::Error::other("the stage must still render"))?;
        assert!(
            panel.size.width >= ASK_PANEL_WIDTH - px(1.0)
                && panel.right() <= WIDE_WORKSPACE_WIDTH + px(1.0),
            "the open panel takes its stated width on the right, where it has always opened"
        );
        assert!(
            opened.right() <= closed.right() - ASK_PANEL_WIDTH + px(1.0),
            "opening Ask must take width from the stage rather than overlap it"
        );
        assert!(
            load_workspace_state(&dir.path().join("sotto.sqlite3")).ask_open,
            "toggling Ask still records the choice; the control moved, the memory did not"
        );

        visual.simulate_click(toggle.center(), Modifiers::none());
        visual.refresh()?;
        visual.run_until_parked();
        let reclosed = visual
            .debug_bounds("workspace-stage")
            .ok_or_else(|| std::io::Error::other("the stage must render"))?;
        assert_eq!(
            (reclosed.origin, reclosed.size),
            (closed.origin, closed.size),
            "closing Ask must give the whole width back, leaving no reserved rail behind"
        );
        assert!(
            !load_workspace_state(&dir.path().join("sotto.sqlite3")).ask_open,
            "closing it again must be recorded too, or a relaunch reopens a panel nobody asked for"
        );
        Ok(())
    }

    /// The acceptance constraint that outranks every other: at the narrowest supported width, in
    /// **every** combination of collapsed/expanded and Ask open/closed, Stop is whole, inside its
    /// bar, and clickable — and nothing in the toolbar has drifted under the traffic lights.
    #[test]
    fn stop_survives_every_sidebar_and_ask_combination_at_the_minimum_width()
    -> Result<(), Box<dyn std::error::Error>> {
        let mut cx = TestAppContext::single();
        cx.update(gpui_component::init);
        let dir = tempfile::tempdir()?;
        let shell = mount(
            &mut cx,
            dir.path(),
            Some(running_window_capture()),
            MIN_WORKSPACE_WIDTH,
        )?;
        let workspace = shell.workspace;
        let session = shell.session;
        let visual = shell.visual;

        for collapsed in [false, true] {
            for ask_open in [false, true] {
                visual.update(|window, cx| {
                    workspace.update(cx, |this, cx| {
                        if this.library_collapsed != collapsed {
                            this.toggle_library(cx);
                        }
                        if this.ask_open != ask_open {
                            this.toggle_ask(window, cx);
                        }
                    });
                });
                visual.refresh()?;
                visual.run_until_parked();
                let state = format!("collapsed={collapsed}, ask_open={ask_open}");

                let bar = visual.debug_bounds("capture-bar").ok_or_else(|| {
                    std::io::Error::other(format!("the capture bar must render ({state})"))
                })?;
                let stop = visual
                    .debug_bounds("capture-stop-control")
                    .ok_or_else(|| std::io::Error::other(format!("Stop must render ({state})")))?;
                assert!(
                    stop.size.width > px(0.0)
                        && stop.left() >= bar.left()
                        && stop.right() <= bar.right()
                        && bar.right() <= MIN_WORKSPACE_WIDTH,
                    "Stop must stay whole and inside the bar at the minimum width ({state})"
                );
                for (selector, bounds) in TOOLBAR_CONTROLS.into_iter().zip(toolbar_bounds(visual)?)
                {
                    assert!(
                        bounds.size.width > px(0.0)
                            && bounds.left() >= TOOLBAR_LEADING_INSET
                            && bounds.right() <= MIN_WORKSPACE_WIDTH,
                        "{selector} must stay clear of the traffic lights and inside the window \
                         ({state})"
                    );
                }
                let stage = visual.debug_bounds("workspace-stage").ok_or_else(|| {
                    std::io::Error::other(format!("the stage must render ({state})"))
                })?;
                assert!(
                    stage.size.width > px(0.0) && stage.right() <= MIN_WORKSPACE_WIDTH + px(1.0),
                    "the stage must keep real width inside the window ({state})"
                );
            }
        }

        // Stop is only proven reachable by reaching it, and the last combination — collapsed rail,
        // Ask open — is the narrowest the stage ever gets.
        let stop = visual
            .debug_bounds("capture-stop-control")
            .ok_or_else(|| std::io::Error::other("Stop must render"))?;
        visual.simulate_click(stop.center(), Modifiers::none());
        assert!(
            visual.update(|_, cx| matches!(
                session.read(cx).lifecycle(),
                session::SessionLifecycle::Stopping { .. }
            )),
            "Stop must remain clickable in the tightest layout the shell can reach"
        );
        Ok(())
    }
}
