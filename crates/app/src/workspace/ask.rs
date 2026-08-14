//! User-initiated, **app-level** Ask controller and dock panel.
//!
//! The panel opens on the right, where it always has. T083 moved only the *control* that opens it
//! into the toolbar; the panel itself stays put because relocating it as well would have moved two
//! things for one request, and the left edge is the library rail's. What did change is that a
//! closed Ask now reserves no width at all — the 42 px vertical rail existed to carry the toggle,
//! and the toggle is in the toolbar now.
//!
//! ADR-0020 changed what Ask is *about*. It used to be scoped to, gated on, and configured from
//! whichever stopped recording happened to be selected — dead on Home, dead while a capture ran,
//! and dead even with its own "use all recordings" toggle on, because the code path still demanded
//! a selection. It now answers from the whole library by default, stays live on Home and during a
//! capture, and narrows to one recording only when the person picks that scope. The per-recording
//! search opt-in that used to sit in this panel is gone: every completed recording is indexed.
//!
//! The copy here is ADR-0019's: a session is a **recording**, not a meeting. Sotto records lectures,
//! interviews and debugging calls, and a panel that insists on asking about "this meeting" is the
//! same mistake that made a model reply "no meeting content is present" to a correct transcript.

use std::sync::mpsc;

use gpui::{
    Context, Entity, EventEmitter, FocusHandle, Focusable, IntoElement, Render, Window, div,
    prelude::*,
};
use gpui_component::{
    Disableable, Selectable as _, Sizable as _,
    button::Button,
    dock::{Panel, PanelEvent},
    input::{Input, InputEvent, InputState},
};
use insight::{AskCitation, AskEngine, AskEvidence, AskReply, AskResult, AskTurn};
use rag::{DocumentKind, SearchFilter};
use sotto_core::{CancellationToken, SessionId};

use super::{
    MeetingWorkspace,
    control_row::{ControlRole, ControlRow},
    tokens::{Space, WorkspaceTokens},
    transcript,
};

#[cfg(test)]
const MIN_ASK_PANEL_WIDTH: gpui::Pixels = gpui::px(300.0);

/// What a question is answered from. The person picks; nothing picks for them silently.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(super) enum AskScope {
    /// Every recording in the library, through the local cross-recording index.
    #[default]
    AllRecordings,
    /// Only the recording currently open — including one that is still running.
    OpenRecording,
}

pub(super) struct AskPanel {
    focus_handle: FocusHandle,
    input: Entity<InputState>,
    /// The open recording and its name, live or stopped. `None` on Home.
    scope: Option<(SessionId, String)>,
    backend_ready: bool,
    chosen: AskScope,
    /// Whether the open recording is still capturing, which the scope line has to say.
    live: bool,
    running: bool,
    progress: Option<String>,
    turns: Vec<AskTurn>,
    message: Option<String>,
}

#[derive(Clone, Debug)]
pub(super) enum AskPanelEvent {
    Submit(String),
    Cancel,
    Citation(AskCitation),
    SelectScope(AskScope),
}

pub(super) struct PendingAsk {
    pub session_id: SessionId,
    pub cancellation: CancellationToken,
    pub progress: mpsc::Receiver<String>,
    pub result: mpsc::Receiver<Result<AskResult, String>>,
}

impl AskPanel {
    #[must_use]
    pub(super) fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        // The placeholder names the default scope, not the open note. Ask answers from the library
        // unless the person narrows it, and a prompt that says "this recording" while the panel is
        // set to all of them is the app telling on itself.
        let input =
            cx.new(|cx| InputState::new(window, cx).placeholder("Ask about your recordings"));
        cx.subscribe_in(&input, window, |this, _, event: &InputEvent, _, cx| {
            if matches!(event, InputEvent::PressEnter { .. }) {
                this.emit_submit(cx);
            }
        })
        .detach();
        Self {
            focus_handle: cx.focus_handle(),
            input,
            scope: None,
            backend_ready: false,
            chosen: AskScope::default(),
            live: false,
            running: false,
            progress: None,
            turns: Vec::new(),
            message: None,
        }
    }

    fn emit_submit(&mut self, cx: &mut Context<Self>) {
        let question = self.input.read(cx).value().trim().to_owned();
        if !question.is_empty() && !self.running && self.backend_ready {
            cx.emit(AskPanelEvent::Submit(question));
        }
    }

    /// The scope a question would actually run against.
    ///
    /// There is always one. "This recording" with nothing open is not a state a person can be left
    /// in — the rail deselects, the choice silently means nothing, and the panel goes dead for a
    /// reason that has nothing to do with Ask — so it resolves back to the library instead.
    pub(super) fn effective(&self) -> AskScope {
        match self.chosen {
            AskScope::OpenRecording if self.scope.is_some() => AskScope::OpenRecording,
            _ => AskScope::AllRecordings,
        }
    }

    pub(super) fn set_scope(
        &mut self,
        scope: Option<(SessionId, String)>,
        ready: bool,
        live: bool,
    ) {
        if self.chosen == AskScope::OpenRecording
            && self.scope.as_ref().map(|value| value.0) != scope.as_ref().map(|value| value.0)
        {
            self.turns.clear();
            self.message = None;
            self.progress = None;
        }
        self.scope = scope;
        self.backend_ready = ready;
        self.live = live;
    }

    /// The open recording's id, when that is what a question would be answered from.
    pub(super) fn open_recording(&self) -> Option<SessionId> {
        (self.chosen == AskScope::OpenRecording)
            .then_some(self.scope.as_ref().map(|value| value.0))
            .flatten()
    }

    pub(super) fn begin(&mut self) {
        self.running = true;
        self.progress = Some("Receiving a grounded answer…".to_owned());
        self.message = None;
    }

    pub(super) fn progress(&mut self, bytes: usize) {
        self.progress = Some(format!(
            "Receiving a grounded answer… {bytes} bytes validated at completion"
        ));
    }

    pub(super) fn finish(&mut self, question: String, result: Result<AskResult, String>) {
        self.running = false;
        self.progress = None;
        match result {
            Ok(result) => {
                self.turns.push(AskTurn {
                    question,
                    reply: result.reply,
                });
                self.message = (!result.normalizations.is_empty()).then(|| {
                    let controls = result
                        .normalizations
                        .iter()
                        .map(|item| item.normalization.control.to_string())
                        .collect::<Vec<_>>()
                        .join(", ");
                    format!("This backend could not apply: {controls}.")
                });
            }
            Err(error) => self.message = Some(error),
        }
    }

    pub(super) fn history(&self) -> Vec<AskTurn> {
        self.turns.clone()
    }

    pub(super) fn select_scope(&mut self, scope: AskScope) {
        if self.chosen == scope {
            return;
        }
        self.chosen = scope;
        // A thread answered from one recording does not carry over to a question about the whole
        // library, and vice versa: the citations behind it no longer describe what is being asked.
        self.turns.clear();
        self.message = None;
        self.progress = None;
    }
}

impl Render for AskPanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let tokens = WorkspaceTokens::resolve(cx);
        let effective = self.effective();
        let scope = scope_line(
            self.scope.as_ref().map(|(_, name)| name.as_str()),
            effective,
            self.live,
        );
        let disabled_reason = if self.backend_ready {
            None
        } else {
            Some(
                "Ask is unavailable until a ready reasoning backend is enabled. The record remains usable.",
            )
        };
        let mut ask_form = ControlRow::new()
            .child(
                ControlRole::Ellipsizing,
                Input::new(&self.input).disabled(disabled_reason.is_some() || self.running),
            )
            .child(
                ControlRole::Essential,
                Button::new("submit-ask")
                    .label("Ask")
                    .disabled(disabled_reason.is_some() || self.running)
                    .on_click(cx.listener(|this, _, _, cx| this.emit_submit(cx))),
            );
        if self.running {
            ask_form = ask_form.child(
                ControlRole::Essential,
                Button::new("cancel-ask")
                    .label("Cancel")
                    .on_click(cx.listener(|_, _, _, cx| cx.emit(AskPanelEvent::Cancel))),
            );
        }
        div()
            .size_full()
            .min_w_0()
            .overflow_hidden()
            .p_4()
            .bg(tokens.surface)
            .text_color(tokens.ink)
            // Two peers, not a toggle that renames itself. A control labelled "Use all recordings"
            // never says which scope is *current* — you have to infer it from the label of the
            // thing you would switch to. Both scopes are always shown, and the selected one is
            // selected; "Only this recording" simply has nothing to point at on Home.
            .child(
                ControlRow::new()
                    .child(
                        ControlRole::Essential,
                        Button::new("ask-scope-all")
                            .label("All recordings")
                            .small()
                            .selected(effective == AskScope::AllRecordings)
                            .disabled(self.running)
                            .on_click(cx.listener(|_, _, _, cx| {
                                cx.emit(AskPanelEvent::SelectScope(AskScope::AllRecordings));
                            })),
                    )
                    .child(
                        ControlRole::Essential,
                        Button::new("ask-scope-open")
                            .label(if self.live {
                                "This recording (live)"
                            } else {
                                "This recording"
                            })
                            .small()
                            .selected(effective == AskScope::OpenRecording)
                            .disabled(self.scope.is_none() || self.running)
                            .on_click(cx.listener(|_, _, _, cx| {
                                cx.emit(AskPanelEvent::SelectScope(AskScope::OpenRecording));
                            })),
                    )
                    .finish()
                    .gap(Space::SM)
                    .debug_selector(|| "ask-scope-row".into()),
            )
            .child(div().mt_1().text_sm().text_color(tokens.muted).child(scope))
            .when_some(disabled_reason, |view, reason| {
                view.child(div().mt_3().text_color(tokens.muted).child(reason))
            })
            .children(
                self.turns
                    .iter()
                    .enumerate()
                    .map(|(turn_ix, turn)| render_turn(turn_ix, turn, cx)),
            )
            .when_some(self.progress.clone(), |view, progress| {
                view.child(div().mt_3().child(progress))
            })
            .when_some(self.message.clone(), |view, message| {
                view.child(div().mt_3().text_color(tokens.warn).child(message))
            })
            .child(
                ask_form
                    .finish()
                    .mt_4()
                    .gap(Space::SM)
                    .debug_selector(|| "ask-form-row".into()),
            )
    }
}

/// The one line the panel says about what a question would be answered *from*.
///
/// Pure so it can be pinned by a test: this sentence is where the panel makes its scope claim, and
/// ADR-0019's vocabulary is a product decision rather than a cosmetic one — Sotto records lectures
/// and debugging calls, and telling someone their recording is "a meeting" is how the summarizer
/// ended up refusing a correct transcript for containing no meeting content.
///
/// A live scope says so. An answer drawn from a call still in progress is answered from the part
/// that has been transcribed so far, and the person deciding whether to trust it needs that.
fn scope_line(session: Option<&str>, scope: AskScope, live: bool) -> String {
    match (scope, session) {
        (AskScope::OpenRecording, Some(name)) if live => {
            format!("This recording, so far · {name}")
        }
        (AskScope::OpenRecording, Some(name)) => format!("This recording · {name}"),
        (AskScope::AllRecordings | AskScope::OpenRecording, _) => {
            "Every recording in your library".to_owned()
        }
    }
}

fn render_turn(index: usize, turn: &AskTurn, cx: &mut Context<AskPanel>) -> gpui::AnyElement {
    let tokens = WorkspaceTokens::resolve(cx);
    let answer = match &turn.reply {
        AskReply::Refusal { reason, covered } => div()
            .child(format!("I can't answer that from this record: {reason}"))
            .when(!covered.is_empty(), |view| {
                view.child(format!(" Covered: {}", covered.join(", ")))
            }),
        AskReply::Answer { claims } => {
            div().children(claims.iter().enumerate().map(|(claim_ix, claim)| {
                div().mt_2().child(claim.text.clone()).children(
                    claim
                        .citations
                        .iter()
                        .enumerate()
                        .map(|(citation_ix, citation)| {
                            let citation = citation.clone();
                            Button::new((
                                "ask-citation",
                                index * 10_000 + claim_ix * 100 + citation_ix,
                            ))
                            .label("Open transcript evidence")
                            .on_click(cx.listener(
                                move |_, _, _, cx| {
                                    cx.emit(AskPanelEvent::Citation(citation.clone()))
                                },
                            ))
                        }),
                )
            }))
        }
    };
    div()
        .mt_4()
        .child(div().text_color(tokens.muted).child(turn.question.clone()))
        .child(answer)
        .into_any_element()
}

impl EventEmitter<AskPanelEvent> for AskPanel {}
impl EventEmitter<PanelEvent> for AskPanel {}
impl Focusable for AskPanel {
    fn focus_handle(&self, _: &gpui::App) -> FocusHandle {
        self.focus_handle.clone()
    }
}
impl Panel for AskPanel {
    fn panel_name(&self) -> &'static str {
        "SottoAskPanel"
    }
    fn tab_name(&self, _: &gpui::App) -> Option<gpui::SharedString> {
        Some("Ask".into())
    }
    fn title(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        "Ask"
    }
    fn closable(&self, _: &gpui::App) -> bool {
        false
    }
    fn zoomable(&self, _: &gpui::App) -> Option<gpui_component::dock::PanelControl> {
        None
    }
}

impl MeetingWorkspace {
    /// Runs one question against whichever scope the panel is showing.
    ///
    /// The single-recording path reads the **live** timeline out of memory when the recording is
    /// still running, and the persisted log otherwise. That difference is the whole reason a person
    /// can ask about the call they are in: nothing has been written to the cross-recording index
    /// yet, and waiting for the recording to stop before it can be questioned would make Ask a
    /// review tool rather than an app-level one.
    pub(super) fn start_ask(&mut self, question: String, cx: &mut Context<Self>) {
        if self.pending_ask.is_some() {
            return;
        }
        let scope = self.ask_panel.read(cx).open_recording();
        let backend = match self.reasoning_backend(cx) {
            Ok(Some(value)) => value,
            Ok(None) => return,
            Err(error) => {
                self.message = Some(error);
                return;
            }
        };
        let store = match rag::Store::open(&self.database) {
            Ok(value) => value,
            Err(error) => {
                self.message = Some(error.to_string());
                return;
            }
        };
        // The library scope has no one recording behind it, so it borrows the open recording's id
        // only as the token `poll_ask` uses to notice the selection moved out from under a run.
        let session_id = scope.or(self.transcript_session).unwrap_or(LIBRARY_ASK);
        let single = match scope {
            None => None,
            Some(id) => {
                let record = match store.load_session_record(id) {
                    Ok(value) => value,
                    Err(error) => {
                        self.message = Some(error.to_string());
                        return;
                    }
                };
                // A running recording's log lives in `TimelineState`, not the store: the actor is
                // still checkpointing into it, so the persisted copy trails what is on screen.
                let events = if self.transcript_live {
                    transcript::scope_to_session(self.timeline.read(cx).events(), Some(id))
                        .into_owned()
                } else {
                    match store.load_session(id) {
                        Ok(value) => value,
                        Err(error) => {
                            self.message = Some(error.to_string());
                            return;
                        }
                    }
                };
                if events.is_empty() {
                    self.message = Some(
                        "This recording has nothing transcribed yet. Ask again once it has words, or ask across all recordings."
                            .to_owned(),
                    );
                    cx.notify();
                    return;
                }
                Some((id, record.capture_target().clone(), events))
            }
        };
        let history = self.ask_panel.read(cx).history();
        let database = self.database.clone();
        let (progress_sender, progress) = mpsc::channel();
        let (result_sender, result) = mpsc::channel();
        let cancellation = CancellationToken::new();
        let worker_cancel = cancellation.clone();
        let provider = backend.provider();
        let worker_question = question.clone();
        let spawn = std::thread::Builder::new()
            .name("sotto-ask".into())
            .spawn(move || {
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build();
                let result = runtime
                    .map_err(|error| error.to_string())
                    .and_then(|runtime| {
                        let engine = AskEngine::new(provider);
                        match single {
                            Some((id, target, events)) => runtime
                                .block_on(engine.ask(
                                    id,
                                    &target,
                                    &events,
                                    &history,
                                    &worker_question,
                                    worker_cancel,
                                    Some(progress_sender),
                                ))
                                .map_err(|error| error.to_string()),
                            None => {
                                let evidence = retained_evidence(&database, &worker_question)?;
                                runtime
                                    .block_on(engine.ask_across(
                                        &evidence,
                                        &history,
                                        &worker_question,
                                        worker_cancel,
                                        Some(progress_sender),
                                    ))
                                    .map_err(|error| error.to_string())
                            }
                        }
                    });
                let _ = result_sender.send(result);
            });
        if let Err(error) = spawn {
            self.message = Some(format!("Could not start Ask: {error}"));
            return;
        }
        self.ask_panel.update(cx, |panel, _| panel.begin());
        self.pending_ask = Some((
            PendingAsk {
                session_id,
                cancellation,
                progress,
                result,
            },
            question,
        ));
        cx.notify();
    }

    pub(super) fn cancel_ask(&mut self, cx: &mut Context<Self>) {
        if let Some((pending, _)) = self.pending_ask.take() {
            pending.cancellation.cancel();
        }
        self.ask_panel.update(cx, |panel, _| {
            panel.finish(
                String::new(),
                Err("Ask was cancelled; no partial answer was kept.".into()),
            )
        });
        cx.notify();
    }

    pub(super) fn poll_ask(&mut self, cx: &mut Context<Self>) -> bool {
        let Some((pending, question)) = self.pending_ask.take() else {
            return false;
        };
        // A library-wide question survives the selection changing under it — it was never about
        // the open recording. A single-recording question does not: its answer would cite a
        // transcript the workspace is no longer showing. A *live* recording moving on is fine; the
        // answer is honestly labelled "so far", and stopping it mid-call would be the surprise.
        if pending.session_id != LIBRARY_ASK && self.transcript_session != Some(pending.session_id)
        {
            pending.cancellation.cancel();
            return true;
        }
        if let Ok(value) = pending.progress.try_recv() {
            self.ask_panel
                .update(cx, |panel, _| panel.progress(value.len()));
            self.pending_ask = Some((pending, question));
            return true;
        }
        match pending.result.try_recv() {
            Ok(result) => {
                self.ask_panel
                    .update(cx, |panel, _| panel.finish(question, result));
                true
            }
            Err(mpsc::TryRecvError::Empty) => {
                self.pending_ask = Some((pending, question));
                false
            }
            Err(mpsc::TryRecvError::Disconnected) => {
                self.ask_panel.update(cx, |panel, _| {
                    panel.finish(question, Err("Ask worker stopped unexpectedly.".into()))
                });
                true
            }
        }
    }
}

/// The sentinel a library-wide run carries where a single-recording run carries its recording.
///
/// [`PendingAsk`] exists so a finished run can be matched against what the workspace is still
/// showing. A question about the whole library has no such recording, and reusing whatever happened
/// to be open would cancel a perfectly valid run the moment the person clicked another row.
const LIBRARY_ASK: SessionId = SessionId::new(0);

/// Gathers cross-recording evidence, indexing anything the library is still missing first.
///
/// The backfill is here rather than at launch on purpose: it embeds, and paying that cost at
/// startup for a person who never opens Ask is the wrong trade. This already runs on the Ask
/// worker thread with progress on screen, it is idempotent, and it is the exact moment "all
/// recordings" has to actually be true.
fn retained_evidence(
    database: &std::path::Path,
    question: &str,
) -> Result<Vec<AskEvidence>, String> {
    let store = rag::Store::open(database).map_err(|error| error.to_string())?;
    let report = store
        .index_missing_prior_meetings()
        .map_err(|error| error.to_string())?;
    for (session_id, error) in &report.failed {
        eprintln!(
            "Recording {} could not be added to cross-recording search: {error}. Other recordings are still searchable.",
            session_id.get()
        );
    }
    let chunks = store
        .search_filtered(
            question,
            5,
            &SearchFilter {
                kind: Some(DocumentKind::PriorMeeting),
                collection_id: None,
                source_session_id: None,
            },
        )
        .map_err(|error| error.to_string())?;
    chunks
        .into_iter()
        .map(|chunk| {
            let raw_session = chunk
                .metadata
                .get("source_session_id")
                .ok_or_else(|| "Retained evidence has no meeting provenance.".to_owned())?;
            let session_id = raw_session
                .parse::<u128>()
                .map(SessionId::new)
                .map_err(|error| format!("invalid retained session provenance: {error}"))?;
            let event_ids = chunk
                .text
                .split("[event:")
                .skip(1)
                .filter_map(|tail| {
                    tail.split(|character: char| !character.is_ascii_digit())
                        .next()
                        .and_then(|value| value.parse::<u64>().ok())
                        .map(sotto_core::EventId::new)
                })
                .collect();
            Ok(AskEvidence {
                evidence_id: chunk.id,
                session_id,
                session_label: chunk.source,
                text: chunk.text,
                event_ids,
            })
        })
        .collect()
}

#[cfg(test)]
mod layout_tests {
    use gpui::{TestAppContext, px, size};

    use super::{AskPanel, AskScope, MIN_ASK_PANEL_WIDTH, scope_line};

    const ALL: AskScope = AskScope::AllRecordings;
    const OPEN: AskScope = AskScope::OpenRecording;

    /// ADR-0019: a session is a recording of something. The panel's scope line is the sentence a
    /// person reads before deciding whether to trust an answer, so it must not tell someone who
    /// recorded a lecture that they are asking about a meeting.
    #[test]
    fn the_scope_line_speaks_about_recordings() {
        for line in [
            scope_line(None, ALL, false),
            scope_line(Some("CS231n lecture"), OPEN, false),
            scope_line(Some("CS231n lecture"), ALL, false),
            scope_line(Some("CS231n lecture"), OPEN, true),
        ] {
            assert!(
                !line.to_lowercase().contains("meeting"),
                "the Ask panel must not call every recording a meeting: {line}"
            );
        }
        assert_eq!(
            scope_line(Some("CS231n lecture"), OPEN, false),
            "This recording · CS231n lecture",
            "a single scope names the recording it would answer from"
        );
        assert_eq!(
            scope_line(Some("CS231n lecture"), ALL, false),
            "Every recording in your library",
            "the library scope says plainly that it is not one recording"
        );
    }

    /// Nothing open is a perfectly good state to ask a question in, and it means the library.
    ///
    /// Ask used to go dead here — "Select a stopped recording to ask about it" — which made a
    /// library-wide capability read as a property of whichever note happened to be open.
    #[test]
    fn with_no_recording_open_the_scope_is_the_whole_library() {
        assert_eq!(
            scope_line(None, ALL, false),
            "Every recording in your library"
        );
        assert_eq!(
            scope_line(None, OPEN, false),
            "Every recording in your library",
            "a single-recording choice with nothing open resolves back to the library"
        );
    }

    /// An answer from a call still running is answered from the part transcribed so far, and the
    /// person deciding whether to act on it has to be told that.
    #[test]
    fn a_live_scope_says_it_is_still_running() {
        assert_eq!(
            scope_line(Some("Standup"), OPEN, true),
            "This recording, so far · Standup"
        );
    }

    /// Both scope choices and the question form stay inside the narrowest Ask panel.
    ///
    /// The retention row this used to also measure is gone: every recording is searchable, so
    /// there is no per-recording opt-in left to place. The two scope buttons took its budget.
    #[test]
    fn narrow_ask_panel_keeps_both_scopes_and_submit_inside_bounds()
    -> Result<(), Box<dyn std::error::Error>> {
        let mut cx = TestAppContext::single();
        cx.update(gpui_component::init);
        let (_panel, visual) = cx.add_window_view(AskPanel::new);

        visual.simulate_resize(size(MIN_ASK_PANEL_WIDTH, px(620.0)));
        visual.refresh()?;
        visual.run_until_parked();

        let selectors = ["ask-scope-row", "ask-form-row"];
        let bounds = selectors
            .map(|selector| {
                visual.debug_bounds(selector).ok_or_else(|| {
                    std::io::Error::other(format!("{selector} must remain rendered"))
                })
            })
            .into_iter()
            .collect::<Result<Vec<_>, _>>()?;
        for (selector, bounds) in selectors.into_iter().zip(&bounds) {
            assert!(
                bounds.left() >= px(0.0) && bounds.right() <= MIN_ASK_PANEL_WIDTH,
                "{selector} must remain inside the minimum Ask width"
            );
        }
        assert!(
            bounds[1].top() >= bounds[0].bottom(),
            "the Ask form must remain below the scope controls"
        );
        Ok(())
    }
}
