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

use std::{collections::BTreeSet, sync::mpsc};

use gpui_kit::component::input::{InputEvent, InputState};
use gpui_kit::{
    Context, Entity, EventEmitter, FocusHandle, Focusable, FontWeight, IntoElement, Render, Window,
    div, prelude::*, px,
};
use insight::{AskCitation, AskEngine, AskEvidence, AskReply, AskResult, AskTurn};
use providers::{CODEX_CLI_BACKEND_ID, OPENAI_RESPONSES_BACKEND_ID, ReasoningSurface};
use rag::{DocumentKind, SearchFilter};
use sotto_core::{CancellationToken, EventId, SessionId};

use super::{
    Button, MeetingWorkspace,
    control_row::{ControlRole, ControlRow},
    input::Input,
    motion, notes,
    tokens::{Space, TypeScale, WorkspaceTokens},
    transcript,
};
use crate::reasoning::inspection::{ScreenInspectorAssembly, product_screen_inspectors};

#[cfg(test)]
const MIN_ASK_PANEL_WIDTH: gpui_kit::Pixels = gpui_kit::px(300.0);

/// What a question is answered from. The person picks; nothing picks for them silently.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(super) enum AskScope {
    /// Every recording in the library, through the local cross-recording index.
    #[default]
    AllRecordings,
    /// Only the recording currently open — including one that is still running.
    OpenRecording,
    /// Exactly the finalized transcript rows the person selected.
    Selection,
}

/// One explicit, contiguous transcript selection offered to Ask.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct AskSelection {
    pub(super) session_id: SessionId,
    pub(super) event_ids: Vec<EventId>,
    /// Timecodes and sources, formatted from the same rows the copy gesture uses.
    pub(super) label: String,
}

pub(super) struct AskPanel {
    focus_handle: FocusHandle,
    input: Entity<InputState>,
    /// The open recording and its name, live or stopped. `None` on Home.
    scope: Option<(Vec<SessionId>, String)>,
    backend_ready: bool,
    openai_ok: bool,
    codex_ok: bool,
    selected_provider: Option<String>,
    chosen: AskScope,
    previous_non_selection: AskScope,
    selection: Option<AskSelection>,
    /// Whether the open recording is still capturing, which the scope line has to say.
    live: bool,
    running: bool,
    progress: Option<String>,
    turns: Vec<AskTurn>,
    message: Option<String>,
    notice: Option<String>,
    revealed_receipts: BTreeSet<usize>,
}

#[derive(Clone, Debug)]
pub(super) enum AskPanelEvent {
    Submit(String),
    Cancel,
    Citation(AskCitation),
    SelectScope(AskScope),
    ClearSelection,
    SelectProvider(Option<&'static str>),
}

pub(super) struct PendingAsk {
    pub session_id: SessionId,
    pub selection_event_ids: Option<Vec<EventId>>,
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
        let input = cx.new(|cx| {
            let mut input = InputState::new(window, cx);
            input.set_placeholder("Ask about your recordings", window, cx);
            input
        });
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
            openai_ok: false,
            codex_ok: false,
            selected_provider: None,
            chosen: AskScope::default(),
            previous_non_selection: AskScope::default(),
            selection: None,
            live: false,
            running: false,
            progress: None,
            turns: Vec::new(),
            message: None,
            notice: None,
            revealed_receipts: BTreeSet::new(),
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
            AskScope::Selection if self.selection.is_some() => AskScope::Selection,
            AskScope::Selection => self.effective_non_selection(self.previous_non_selection),
            AskScope::AllRecordings | AskScope::OpenRecording => AskScope::AllRecordings,
        }
    }

    fn effective_non_selection(&self, scope: AskScope) -> AskScope {
        match scope {
            AskScope::OpenRecording if self.scope.is_some() => AskScope::OpenRecording,
            AskScope::AllRecordings | AskScope::OpenRecording | AskScope::Selection => {
                AskScope::AllRecordings
            }
        }
    }

    #[cfg(test)]
    pub(super) fn set_scope(
        &mut self,
        scope: Option<(SessionId, String)>,
        ready: bool,
        live: bool,
    ) {
        self.set_entry_scope(
            scope.map(|(session_id, name)| (vec![session_id], name)),
            ready,
            live,
        );
    }

    pub(super) fn set_entry_scope(
        &mut self,
        scope: Option<(Vec<SessionId>, String)>,
        ready: bool,
        live: bool,
    ) {
        if self.chosen == AskScope::OpenRecording
            && self.scope.as_ref().map(|value| &value.0) != scope.as_ref().map(|value| &value.0)
        {
            self.turns.clear();
            self.revealed_receipts.clear();
            self.message = None;
            self.notice = None;
            self.progress = None;
        }
        self.scope = scope;
        self.backend_ready = ready;
        self.live = live;
    }

    pub(super) fn set_providers(
        &mut self,
        openai_ok: bool,
        codex_ok: bool,
        selected_provider: Option<String>,
    ) {
        self.openai_ok = openai_ok;
        self.codex_ok = codex_ok;
        self.selected_provider = selected_provider;
    }

    pub(super) fn set_selection(&mut self, selection: Option<AskSelection>) {
        if self.selection == selection {
            return;
        }
        self.selection = selection;
        self.turns.clear();
        self.revealed_receipts.clear();
        self.message = None;
        self.notice = None;
        self.progress = None;
        if self.selection.is_none() && self.chosen == AskScope::Selection {
            self.chosen = self.previous_non_selection;
        }
    }

    /// The open recording's id, when that is what a question would be answered from.
    pub(super) fn open_recording(&self) -> Option<SessionId> {
        (self.chosen == AskScope::OpenRecording)
            .then_some(
                self.scope
                    .as_ref()
                    .and_then(|value| (value.0.len() == 1).then_some(value.0[0])),
            )
            .flatten()
    }

    pub(super) fn open_entry_sessions(&self) -> Option<Vec<SessionId>> {
        (self.chosen == AskScope::OpenRecording)
            .then(|| self.scope.as_ref().map(|value| value.0.clone()))
            .flatten()
    }

    /// The explicit selection, only when it is the scope the person chose.
    pub(super) fn selected_range(&self) -> Option<AskSelection> {
        (self.effective() == AskScope::Selection)
            .then(|| self.selection.clone())
            .flatten()
    }

    pub(super) fn begin(&mut self) {
        self.running = true;
        self.progress = Some("Receiving a grounded answer…".to_owned());
        self.message = None;
        self.notice = None;
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
                    screen_consultations: result.screen_consultations,
                });
                self.notice = notes::normalization_line(&result.normalizations);
                self.message = None;
            }
            Err(error) => {
                self.notice = None;
                self.message = Some(error);
            }
        }
    }

    fn toggle_receipt(&mut self, index: usize, cx: &mut Context<Self>) {
        if !self.revealed_receipts.insert(index) {
            self.revealed_receipts.remove(&index);
        }
        cx.notify();
    }

    pub(super) fn history(&self) -> Vec<AskTurn> {
        self.turns.clone()
    }

    pub(super) fn select_scope(&mut self, scope: AskScope) {
        if scope == AskScope::Selection && self.selection.is_none() {
            return;
        }
        if self.chosen == scope {
            return;
        }
        if scope == AskScope::Selection {
            self.previous_non_selection = self.effective_non_selection(self.chosen);
        } else {
            self.previous_non_selection = scope;
        }
        self.chosen = scope;
        // A thread answered from one recording does not carry over to a question about the whole
        // library, and vice versa: the citations behind it no longer describe what is being asked.
        self.turns.clear();
        self.revealed_receipts.clear();
        self.message = None;
        self.notice = None;
        self.progress = None;
    }
}

impl Render for AskPanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let tokens = WorkspaceTokens::resolve(cx);
        let effective = self.effective();
        let scope = scope_line(
            self.scope.as_ref().map(|(_, name)| name.as_str()),
            self.selection.as_ref(),
            effective,
            self.live,
        );
        let disabled_reason = if self.backend_ready {
            None
        } else {
            Some(
                "Ask is unavailable until you add a provider in Settings and pick it here. The record remains usable.",
            )
        };
        let mut ask_form = ControlRow::new();
        if self.openai_ok {
            let selected = self.selected_provider.as_deref() == Some(OPENAI_RESPONSES_BACKEND_ID);
            ask_form = ask_form.child(
                ControlRole::Essential,
                Button::new("ask-provider-openai", tokens)
                    .label("OpenAI")
                    .small()
                    .selected(selected)
                    .disabled(self.running)
                    .on_click(cx.listener(|_, _, _, cx| {
                        cx.emit(AskPanelEvent::SelectProvider(Some(
                            OPENAI_RESPONSES_BACKEND_ID,
                        )));
                    })),
            );
        }
        if self.codex_ok {
            let selected = self.selected_provider.as_deref() == Some(CODEX_CLI_BACKEND_ID);
            ask_form = ask_form.child(
                ControlRole::Essential,
                Button::new("ask-provider-codex", tokens)
                    .label("Codex")
                    .small()
                    .selected(selected)
                    .disabled(self.running)
                    .on_click(cx.listener(|_, _, _, cx| {
                        cx.emit(AskPanelEvent::SelectProvider(Some(CODEX_CLI_BACKEND_ID)));
                    })),
            );
        }
        ask_form = ask_form
            .child(
                ControlRole::Ellipsizing,
                Input::new(&self.input)
                    .small()
                    .disabled(disabled_reason.is_some() || self.running),
            )
            .child(
                ControlRole::Essential,
                Button::new("submit-ask", tokens)
                    .label("Ask")
                    .small()
                    .disabled(disabled_reason.is_some() || self.running)
                    .on_click(cx.listener(|this, _, _, cx| this.emit_submit(cx))),
            );
        if self.running {
            ask_form = ask_form.child(
                ControlRole::Essential,
                Button::new("cancel-ask", tokens)
                    .label("Cancel")
                    .small()
                    .on_click(cx.listener(|_, _, _, cx| cx.emit(AskPanelEvent::Cancel))),
            );
        }
        div()
            .size_full()
            .min_w_0()
            .flex()
            .flex_col()
            .bg(tokens.surface)
            .text_color(tokens.ink)
            .text_size(TypeScale::control(&tokens))
            .child(
                div()
                    .flex_none()
                    .px(px(14.0))
                    .pt(px(14.0))
                    .pb(px(8.0))
                    .child(
                        div()
                            .text_size(TypeScale::control(&tokens))
                            .font_weight(FontWeight::SEMIBOLD)
                            .child("Ask"),
                    ),
            )
            .child(
                div()
                    .id("ask-panel-scroll")
                    .flex_1()
                    .min_h_0()
                    .min_w_0()
                    .id("ask-scroll-1")
                    .overflow_y_scroll()
                    .px(px(14.0))
                    .pb(px(14.0))
                    .text_size(TypeScale::control(&tokens))
                    // Two peers, not a toggle that renames itself. A control labelled "Use all recordings"
                    // never says which scope is *current* — you have to infer it from the label of the
                    // thing you would switch to. Both scopes are always shown, and the selected one is
                    // selected; "Only this recording" simply has nothing to point at on Home.
                    .child(
                        ControlRow::new()
                            .child(
                                ControlRole::Essential,
                                Button::new("ask-scope-all", tokens)
                                    .label("All recordings")
                                    .small()
                                    .selected(effective == AskScope::AllRecordings)
                                    .disabled(self.running)
                                    .on_click(cx.listener(|_, _, _, cx| {
                                        cx.emit(AskPanelEvent::SelectScope(
                                            AskScope::AllRecordings,
                                        ));
                                    })),
                            )
                            .child(
                                ControlRole::Essential,
                                div()
                                    .debug_selector(|| "ask-scope-entry-control".into())
                                    .child(
                                        Button::new("ask-scope-open", tokens)
                                            .label(if self.live {
                                                "This entry (live)"
                                            } else {
                                                "This entry"
                                            })
                                            .small()
                                            .selected(effective == AskScope::OpenRecording)
                                            .disabled(self.scope.is_none() || self.running)
                                            .on_click(cx.listener(|_, _, _, cx| {
                                                cx.emit(AskPanelEvent::SelectScope(
                                                    AskScope::OpenRecording,
                                                ));
                                            })),
                                    ),
                            )
                            .finish()
                            .gap(Space::SM)
                            .debug_selector(|| "ask-scope-row".into()),
                    )
                    .children(self.selection.as_ref().map(|selection| {
                        ControlRow::new()
                            .child(
                                ControlRole::Essential,
                                Button::new("ask-scope-selection", tokens)
                                    .label("This selection")
                                    .small()
                                    .selected(effective == AskScope::Selection)
                                    .disabled(self.running)
                                    .on_click(cx.listener(|_, _, _, cx| {
                                        cx.emit(AskPanelEvent::SelectScope(AskScope::Selection));
                                    })),
                            )
                            .child(
                                ControlRole::Ellipsizing,
                                div()
                                    .text_size(TypeScale::control(&tokens))
                                    .text_color(tokens.muted)
                                    .child(selection.label.clone()),
                            )
                            .child(
                                ControlRole::Essential,
                                Button::new("ask-clear-selection", tokens)
                                    .label("Clear")
                                    .ghost()
                                    .small()
                                    .disabled(self.running)
                                    .on_click(cx.listener(|_, _, _, cx| {
                                        cx.emit(AskPanelEvent::ClearSelection);
                                    })),
                            )
                            .finish()
                            .mt_1()
                            .gap(Space::SM)
                            .debug_selector(|| "ask-selection-row".into())
                    }))
                    // Selectable because this is the line the scope controls used to sit on top of: the
                    // defect that filed T069 was "Use every retained meeting" overlapping the retention
                    // sentence beneath it, and an overlap is only assertable if both sides have bounds.
                    .child(
                        div()
                            .mt_1()
                            .text_size(TypeScale::control(&tokens))
                            .text_color(tokens.muted)
                            .debug_selector(|| "ask-scope-line".into())
                            .child(scope),
                    )
                    .when_some(disabled_reason, |view, reason| {
                        view.child(
                            div()
                                .mt(Space::MD)
                                .text_size(TypeScale::control(&tokens))
                                .text_color(tokens.muted)
                                .child(reason),
                        )
                    })
                    .children(self.turns.iter().enumerate().map(|(turn_ix, turn)| {
                        render_turn(turn_ix, turn, self.revealed_receipts.contains(&turn_ix), cx)
                    }))
                    .when_some(self.progress.clone(), |view, progress| {
                        view.child(
                            div().mt_3().child(progress).child(
                                div()
                                    .mt_2()
                                    .child(motion::writing_progress("ask-writing", tokens)),
                            ),
                        )
                    })
                    .when_some(self.notice.clone(), |view, notice| {
                        view.child(
                            div()
                                .mt_3()
                                .text_size(TypeScale::control(&tokens))
                                .text_color(tokens.faint)
                                .child(notice),
                        )
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
                    ),
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
fn scope_line(
    session: Option<&str>,
    selection: Option<&AskSelection>,
    scope: AskScope,
    live: bool,
) -> String {
    match (scope, session) {
        (AskScope::Selection, _) => selection.map_or_else(
            || "Every recording in your library".to_owned(),
            |selection| {
                if live {
                    format!(
                        "This selection, so far · {} · finalized rows only; provisional words excluded",
                        selection.label
                    )
                } else {
                    format!("This selection · {}", selection.label)
                }
            },
        ),
        (AskScope::OpenRecording, Some(name)) if live => {
            format!("This entry, so far · {name}")
        }
        (AskScope::OpenRecording, Some(name)) => format!("This entry · {name}"),
        (AskScope::AllRecordings | AskScope::OpenRecording, _) => {
            "Every recording in your library".to_owned()
        }
    }
}

fn render_turn(
    index: usize,
    turn: &AskTurn,
    receipt_revealed: bool,
    cx: &mut Context<AskPanel>,
) -> gpui_kit::AnyElement {
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
                            Button::new(
                                (
                                    "ask-citation",
                                    index * 10_000 + claim_ix * 100 + citation_ix,
                                ),
                                tokens,
                            )
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
        .children((!turn.screen_consultations.is_empty()).then(|| {
            let count = turn.screen_consultations.len();
            div()
                .mt_1()
                .debug_selector(|| "ask-screen-receipt".into())
                .child(
                    Button::new(("ask-screen-receipt-toggle", index), tokens)
                        .label(if receipt_revealed {
                            "Hide screen consultation receipt".to_owned()
                        } else {
                            format!("Screen requests: {count} · Show receipt")
                        })
                        .ghost()
                        .xsmall()
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.toggle_receipt(index, cx);
                        })),
                )
                .children(receipt_revealed.then(|| {
                    div().children(turn.screen_consultations.iter().map(|entry| {
                        div()
                            .mt_1()
                            .text_size(TypeScale::control(&tokens))
                            .text_color(tokens.faint)
                            .child(entry.describe())
                    }))
                }))
        }))
        .into_any_element()
}

impl EventEmitter<AskPanelEvent> for AskPanel {}
impl Focusable for AskPanel {
    fn focus_handle(&self, _: &gpui_kit::App) -> FocusHandle {
        self.focus_handle.clone()
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
        let selection = self.ask_panel.read(cx).selected_range();
        let entry_sessions = self.ask_panel.read(cx).open_entry_sessions();
        let scope = selection
            .as_ref()
            .map(|selection| selection.session_id)
            .or_else(|| self.ask_panel.read(cx).open_recording());
        let backend = match self.reasoning_backend(ReasoningSurface::Ask, cx) {
            Ok(Some(value)) => value,
            Ok(None) => return,
            Err(error) => {
                self.message = Some(error);
                return;
            }
        };
        let store = match crate::persistence_runtime::block_on(rag::Store::open(&self.database)) {
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
                let record =
                    match crate::persistence_runtime::block_on(store.load_session_record(id)) {
                        Ok(value) => value,
                        Err(error) => {
                            self.message = Some(error.to_string());
                            return;
                        }
                    };
                // A running recording's log lives in `TimelineState`, not the store: the actor is
                // still checkpointing into it, so the persisted copy trails what is on screen.
                let mut events = if self.transcript_live {
                    transcript::scope_to_session(self.timeline.read(cx).events(), Some(id))
                        .into_owned()
                } else {
                    match crate::persistence_runtime::block_on(store.load_session(id)) {
                        Ok(value) => value,
                        Err(error) => {
                            self.message = Some(error.to_string());
                            return;
                        }
                    }
                };
                if let Some(selection) = &selection {
                    let selected = selection
                        .event_ids
                        .iter()
                        .copied()
                        .collect::<std::collections::BTreeSet<_>>();
                    events.retain(|event| selected.contains(&event.id()));
                }
                if events.is_empty() {
                    self.message = Some(if selection.is_some() {
                        "This selection has no finalized transcript rows. Select finalized words and try again."
                            .to_owned()
                    } else {
                        "This recording has nothing transcribed yet. Ask again once it has words, or ask across all recordings."
                            .to_owned()
                    });
                    cx.notify();
                    return;
                }
                Some((
                    id,
                    record.capture_target().clone(),
                    events,
                    selection.is_some(),
                ))
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
        let entry_scope = entry_sessions;
        let inspection_session = single.as_ref().map(|(id, _, _, _)| *id);
        let spawn = std::thread::Builder::new()
            .name("sotto-ask".into())
            .spawn(move || {
                let result = crate::persistence_runtime::block_on(async {
                    let engine = inspection_session.map_or_else(
                        || AskEngine::new(provider.clone()),
                        |id| {
                            let assembly = product_screen_inspectors(database.clone());
                            let (inspector, log) = assembly.assemble(id);
                            AskEngine::new(provider.clone()).with_screen_inspector(inspector, log)
                        },
                    );
                    match single {
                        Some((id, target, events, true)) => engine
                            .ask_selection(
                                id,
                                &target,
                                &events,
                                &history,
                                &worker_question,
                                worker_cancel,
                                Some(progress_sender),
                            )
                            .await
                            .map_err(|error| error.to_string()),
                        Some((id, target, events, false)) => engine
                            .ask(
                                id,
                                &target,
                                &events,
                                &history,
                                &worker_question,
                                worker_cancel,
                                Some(progress_sender),
                            )
                            .await
                            .map_err(|error| error.to_string()),
                        None => {
                            let evidence = retained_evidence(
                                &database,
                                &worker_question,
                                entry_scope.as_deref(),
                            )
                            .await?;
                            engine
                                .ask_across(
                                    &evidence,
                                    &history,
                                    &worker_question,
                                    worker_cancel,
                                    Some(progress_sender),
                                )
                                .await
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
                selection_event_ids: selection.map(|selection| selection.event_ids),
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
        if let Some(expected) = &pending.selection_event_ids
            && self
                .ask_selection
                .as_ref()
                .map(|selection| &selection.event_ids)
                != Some(expected)
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
async fn retained_evidence(
    database: &std::path::Path,
    question: &str,
    sessions: Option<&[SessionId]>,
) -> Result<Vec<AskEvidence>, String> {
    let store = rag::Store::open(database)
        .await
        .map_err(|error| error.to_string())?;
    let report = store
        .index_missing_prior_meetings()
        .await
        .map_err(|error| error.to_string())?;
    for (session_id, error) in &report.failed {
        eprintln!(
            "Recording {} could not be added to cross-recording search: {error}. Other recordings are still searchable.",
            session_id.get()
        );
    }
    let mut chunks = Vec::new();
    if let Some(sessions) = sessions {
        for session_id in sessions {
            chunks.extend(
                store
                    .search_filtered(
                        question,
                        5,
                        &SearchFilter {
                            kind: Some(DocumentKind::PriorMeeting),
                            collection_id: None,
                            source_session_id: Some(*session_id),
                        },
                    )
                    .await
                    .map_err(|error| error.to_string())?,
            );
        }
        chunks.truncate(5);
    } else {
        chunks = store
            .search_filtered(
                question,
                5,
                &SearchFilter {
                    kind: Some(DocumentKind::PriorMeeting),
                    collection_id: None,
                    source_session_id: None,
                },
            )
            .await
            .map_err(|error| error.to_string())?;
    }
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
    use gpui_kit::{TestAppContext, px, size};
    use sotto_core::{EventId, SessionId};

    use super::{AskPanel, AskScope, AskSelection, MIN_ASK_PANEL_WIDTH, scope_line};

    const ALL: AskScope = AskScope::AllRecordings;
    const OPEN: AskScope = AskScope::OpenRecording;
    const SELECTION: AskScope = AskScope::Selection;

    fn selection() -> AskSelection {
        AskSelection {
            session_id: SessionId::new(7),
            event_ids: vec![EventId::new(2), EventId::new(3)],
            label: "[02:10] captured audio → [02:42] your microphone · 2 finalized rows".into(),
        }
    }

    /// ADR-0019: a session is a recording of something. The panel's scope line is the sentence a
    /// person reads before deciding whether to trust an answer, so it must not tell someone who
    /// recorded a lecture that they are asking about a meeting.
    #[test]
    fn the_scope_line_speaks_about_entries() {
        for line in [
            scope_line(None, None, ALL, false),
            scope_line(Some("CS231n lecture"), None, OPEN, false),
            scope_line(Some("CS231n lecture"), None, ALL, false),
            scope_line(Some("CS231n lecture"), None, OPEN, true),
            scope_line(Some("CS231n lecture"), Some(&selection()), SELECTION, true),
        ] {
            assert!(
                !line.to_lowercase().contains("meeting"),
                "the Ask panel must not call every recording a meeting: {line}"
            );
        }
        assert_eq!(
            scope_line(Some("CS231n lecture"), None, OPEN, false),
            "This entry · CS231n lecture",
            "a single scope names the entry it would answer from"
        );
        assert_eq!(
            scope_line(Some("CS231n lecture"), None, ALL, false),
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
            scope_line(None, None, ALL, false),
            "Every recording in your library"
        );
        assert_eq!(
            scope_line(None, None, OPEN, false),
            "Every recording in your library",
            "a single-recording choice with nothing open resolves back to the library"
        );
    }

    /// An answer from a call still running is answered from the part transcribed so far, and the
    /// person deciding whether to act on it has to be told that.
    #[test]
    fn a_live_scope_says_it_is_still_running() {
        assert_eq!(
            scope_line(Some("Standup"), None, OPEN, true),
            "This entry, so far · Standup"
        );
    }

    #[test]
    fn a_live_selection_names_its_span_and_excludes_provisional_words() {
        let selection = selection();
        assert_eq!(
            scope_line(Some("Standup"), Some(&selection), SELECTION, true),
            "This selection, so far · [02:10] captured audio → [02:42] your microphone · 2 \
             finalized rows · finalized rows only; provisional words excluded"
        );
    }

    #[test]
    fn selection_is_never_default_and_clearing_it_restores_the_previous_scope() {
        let mut cx = TestAppContext::single();
        cx.update(gpui_kit::init);
        let (panel, visual) = cx.add_window_view(AskPanel::new);
        visual.update(|_, cx| {
            panel.update(cx, |panel, _| {
                panel.set_scope(Some((SessionId::new(7), "Standup".into())), true, false);
                panel.set_selection(Some(selection()));
                assert_eq!(panel.effective(), ALL, "a selection never chooses itself");
                panel.select_scope(OPEN);
                panel.select_scope(SELECTION);
                assert_eq!(panel.effective(), SELECTION);
                panel.set_selection(None);
                assert_eq!(
                    panel.effective(),
                    OPEN,
                    "clearing the selection restores the explicit prior scope"
                );
            });
        });
    }

    /// Both scope choices and the question form stay inside the narrowest Ask panel.
    ///
    /// The retention row this used to also measure is gone: every recording is searchable, so
    /// there is no per-recording opt-in left to place. The two scope buttons took its budget.
    #[test]
    fn narrow_ask_panel_keeps_both_scopes_and_submit_inside_bounds()
    -> Result<(), Box<dyn std::error::Error>> {
        let mut cx = TestAppContext::single();
        cx.update(gpui_kit::init);
        let (panel, visual) = cx.add_window_view(AskPanel::new);
        visual.update(|_, cx| {
            panel.update(cx, |panel, _| panel.set_selection(Some(selection())));
        });

        visual.simulate_resize(size(MIN_ASK_PANEL_WIDTH, px(620.0)));
        visual.refresh()?;
        visual.run_until_parked();

        let selectors = ["ask-scope-row", "ask-selection-row", "ask-form-row"];
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
            bounds[1].top() >= bounds[0].bottom() && bounds[2].top() >= bounds[1].bottom(),
            "the Ask form must remain below the scope controls"
        );
        Ok(())
    }

    /// The scope controls must not sit on top of the sentence that qualifies them.
    ///
    /// T069's second defect: "Use every retained meeting" overlapped the retention line beneath it.
    /// The narrow test above proves the three control rows stack, but the line those controls
    /// overlapped is not one of them, and the defect was seen at an ordinary width rather than at
    /// the minimum — so neither half of it was covered.
    #[test]
    fn the_scope_controls_never_sit_on_the_line_that_qualifies_them()
    -> Result<(), Box<dyn std::error::Error>> {
        let mut cx = TestAppContext::single();
        cx.update(gpui_kit::init);
        let (panel, visual) = cx.add_window_view(AskPanel::new);
        visual.update(|_, cx| {
            panel.update(cx, |panel, _| panel.set_selection(Some(selection())));
        });

        // Both an ordinary width and the minimum: the overlap was reported at the former, and a
        // row that stacks when wide can still collide once its children have to wrap.
        for width in [px(420.0), MIN_ASK_PANEL_WIDTH] {
            visual.simulate_resize(size(width, px(620.0)));
            visual.refresh()?;
            visual.run_until_parked();

            let stacked = ["ask-scope-row", "ask-selection-row", "ask-scope-line"]
                .map(|selector| {
                    visual.debug_bounds(selector).ok_or_else(|| {
                        std::io::Error::other(format!("{selector} must render at {width:?}"))
                    })
                })
                .into_iter()
                .collect::<Result<Vec<_>, _>>()?;

            for pair in stacked.windows(2) {
                assert!(
                    pair[1].top() >= pair[0].bottom(),
                    "the scope controls must stack rather than overlap at {width:?}"
                );
            }
            let line = stacked[2];
            assert!(
                line.left() >= px(0.0) && line.right() <= width,
                "the qualifying line must stay inside the panel at {width:?}"
            );
        }
        Ok(())
    }
}
