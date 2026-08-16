//! Product settings: a sheet over a scrim *inside the workspace window*, with a nav of four panes.
//!
//! `docs/design/workspace-v2-mock.html` is normative here. Settings is no longer one long scrolling
//! column: it is a sheet with a titled head, a close control, and four panes of which exactly one
//! renders. **Storage & privacy leads and is the default**, because it is where a reader checks
//! what Sotto keeps, what it costs on disk, and the one case where anything leaves the machine.
//!
//! The mock is equally unambiguous about *where* it lives: `#setScrim` is an overlay over the
//! workspace it configures, not a second OS window. Opening it in its own window put a scrim-backed
//! dialog inside a window frame — two competing containers, one redundant — so this view no longer
//! knows how it is mounted. It emits [`SettingsEvent::Dismissed`] and whoever rendered it stops.
//!
//! Two rules govern the content of these panes, and both exist because this project has already
//! shipped a claim ("audio is never written to disk") that outlived its truth:
//!
//! 1. **The pinned disclosure set survives layout changes.** `PRIVACY_DISCLOSURES` is an exact
//!    inventory regression. A disclosure may move between panes; it may not quietly disappear.
//! 2. **No control is rendered for a capability that does not exist.** The mock is aspirational in
//!    places — a start shortcut, a microphone picker, a language picker, a list of Whisper sizes.
//!    None of those are built, so this sheet says plainly that they are not built rather than
//!    painting a control that does nothing. That is the standard the Import entry point set.
//!
//! Dialog semantics are structural rather than declared: GPUI has no ARIA, so the sheet is a
//! scrim-backed panel that occludes the workspace beneath it, it holds focus, its close control is
//! always visible, and Escape dismisses it.

use std::{
    path::{Path, PathBuf},
    sync::mpsc::TryRecvError,
    time::Duration,
};

use gpui::{
    AnyElement, Context, Entity, EventEmitter, FocusHandle, IntoElement, KeyDownEvent,
    PathPromptOptions, Pixels, Render, Subscription, Timer, Window, div, prelude::*, px,
};
use gpui_component::{
    Disableable, IconName, WindowExt as _,
    button::Button,
    input::{Input, InputState},
    scroll::ScrollableElement,
};
use providers::{CODEX_CLI_BACKEND_ID, OPENAI_RESPONSES_BACKEND_ID, Role};
use secrecy::SecretString;
use sotto_core::SessionId;

use crate::{
    mcp::McpController,
    reasoning::ReasoningController,
    session::{RecordingLibrary, RecordingLibrarySnapshot},
    vault::VaultMirrorController,
    workspace::{
        confirm_delete_dialog, delete_icon_button, icon_button,
        tokens::{Space, TypeScale, WorkspaceTokens},
    },
};

/// What the sheet tells whoever mounted it.
///
/// The sheet cannot close itself any more: it is an overlay inside the workspace window, and the
/// workspace owns whether it renders. Emitting is the whole dismissal contract.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SettingsEvent {
    /// The close control or Escape asked for the sheet to go away.
    Dismissed,
}

impl EventEmitter<SettingsEvent> for SettingsView {}

/// The mock's settings sheet: `width: min(760px, 100vw - 48px)`, `height: min(600px, 100vh - 64px)`.
const SHEET_MAX_WIDTH: Pixels = px(760.0);
const SHEET_MAX_HEIGHT: Pixels = px(600.0);
const SHEET_INSET_X: Pixels = px(24.0);
const SHEET_INSET_Y: Pixels = px(32.0);
const NAV_WIDTH: Pixels = px(168.0);

/// Below this width the left nav stacks above the pane instead of eating it.
///
/// A 168px rail out of a 372px sheet leaves a pane too narrow for the action rows the Codex and
/// OpenAI cards carry, and a clipped button is exactly the defect this redesign is meant to remove.
const NAV_STACKS_BELOW: Pixels = px(640.0);

#[derive(Default)]
struct ValidationLease(Option<sotto_core::CancellationToken>);

impl ValidationLease {
    fn replace(&mut self, cancellation: sotto_core::CancellationToken) {
        if let Some(previous) = self.0.replace(cancellation) {
            previous.cancel();
        }
    }
}

impl Drop for ValidationLease {
    fn drop(&mut self) {
        if let Some(cancellation) = self.0.take() {
            cancellation.cancel();
        }
    }
}

pub struct SettingsView {
    reasoning: Entity<ReasoningController>,
    _reasoning_subscription: Subscription,
    key_input: Entity<InputState>,
    model_input: Entity<InputState>,
    codex_model_input: Entity<InputState>,
    mcp: Entity<McpController>,
    _mcp_subscription: Subscription,
    mcp_id_input: Entity<InputState>,
    mcp_name_input: Entity<InputState>,
    mcp_endpoint_input: Entity<InputState>,
    mcp_token_input: Entity<InputState>,
    recording_library: RecordingLibrary,
    recording_snapshot: Option<RecordingLibrarySnapshot>,
    recording_budget_input: Entity<InputState>,
    vault: Entity<VaultMirrorController>,
    _vault_subscription: Subscription,
    action_message: Option<String>,
    validation_lease: ValidationLease,
    /// Which of the four panes is showing. Not persisted: the sheet always opens on the pane the
    /// product wants read first.
    pane: SettingsPane,
    /// Focused on open so Escape reaches the sheet rather than the bare window root.
    focus_handle: FocusHandle,
}

/// Which settings pane is showing. Storage & privacy is first and default by product decision.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum SettingsPane {
    /// What Sotto keeps, what it costs, and the one case where anything leaves this Mac.
    #[default]
    StorageAndPrivacy,
    /// What is captured when a recording starts.
    Recording,
    /// The local Whisper pass over the retained recording.
    Transcription,
    /// The reasoning backend, and what a summary is allowed to reach.
    SummariesAndAsk,
}

impl SettingsPane {
    const ALL: [Self; 4] = [
        Self::StorageAndPrivacy,
        Self::Recording,
        Self::Transcription,
        Self::SummariesAndAsk,
    ];

    fn title(self) -> &'static str {
        match self {
            Self::StorageAndPrivacy => "Storage & privacy",
            Self::Recording => "Recording",
            Self::Transcription => "Transcription",
            Self::SummariesAndAsk => "Summaries & Ask",
        }
    }

    /// One line saying what the pane is for, per the mock's `pane-lede`.
    fn lede(self) -> &'static str {
        match self {
            Self::StorageAndPrivacy => {
                "What Sotto keeps, where it lives, and the one case where anything leaves this Mac."
            }
            Self::Recording => "What gets captured when a recording starts, and what stays out.",
            Self::Transcription => {
                "Runs on this Mac, from the retained recording. Re-run it any time — the recording \
                 is the source of truth."
            }
            Self::SummariesAndAsk => {
                "The summary takes the shape of the content — a lecture yields topics, a debugging \
                 session yields findings, a meeting yields decisions. Every claim cites the \
                 transcript or it is not written."
            }
        }
    }

    fn nav_selector(self) -> &'static str {
        match self {
            Self::StorageAndPrivacy => "settings-nav-storage",
            Self::Recording => "settings-nav-recording",
            Self::Transcription => "settings-nav-transcription",
            Self::SummariesAndAsk => "settings-nav-summaries",
        }
    }

    fn pane_selector(self) -> &'static str {
        match self {
            Self::StorageAndPrivacy => "settings-pane-storage",
            Self::Recording => "settings-pane-recording",
            Self::Transcription => "settings-pane-transcription",
            Self::SummariesAndAsk => "settings-pane-summaries",
        }
    }
}

impl SettingsView {
    /// Opens over the database the workspace is already showing, rather than looking one up.
    ///
    /// It used to call `RecordingLibrary::application_default()`, which reads
    /// `~/Library/Application Support/Sotto/sotto.sqlite3` no matter who opened it. Every shell
    /// test that mounts the product against a `tempfile::tempdir` and then opens Settings therefore
    /// reached past its fixture and opened — and migrated — the real one on the developer's
    /// machine. Taking the path from the caller makes the sheet configure the library it is
    /// actually part of, which is also what the product wants.
    #[must_use]
    pub fn new(
        window: &mut Window,
        reasoning: Entity<ReasoningController>,
        mcp: Entity<McpController>,
        vault: Entity<VaultMirrorController>,
        database: PathBuf,
        cx: &mut Context<Self>,
    ) -> Self {
        let recordings = database
            .parent()
            .map_or_else(std::env::temp_dir, Path::to_path_buf)
            .join("recordings");
        Self::new_with_recording_library(
            window,
            reasoning,
            mcp,
            vault,
            RecordingLibrary::new(database, recordings),
            cx,
        )
    }

    fn new_with_recording_library(
        window: &mut Window,
        reasoning: Entity<ReasoningController>,
        mcp: Entity<McpController>,
        vault: Entity<VaultMirrorController>,
        recording_library: RecordingLibrary,
        cx: &mut Context<Self>,
    ) -> Self {
        let reasoning_subscription = cx.observe(&reasoning, |_, _, cx| cx.notify());
        let mcp_subscription = cx.observe(&mcp, |_, _, cx| cx.notify());
        let vault_subscription = cx.observe(&vault, |_, _, cx| cx.notify());
        let model = reasoning.read(cx).openai_model().to_owned();
        let codex_model = reasoning.read(cx).codex_model().to_owned();
        let recording_result = recording_library.snapshot();
        let recording_budget_gb = recording_result
            .as_ref()
            .map_or(20, |snapshot| snapshot.usage.budget_bytes / BYTES_PER_GB);
        let action_message = recording_result
            .as_ref()
            .err()
            .map(|error| format!("Could not load the recording library: {error}"));
        // The sheet takes focus on open so Escape reaches it before anything inside is clicked.
        // Without a focused ancestor, GPUI dispatches key events to the window root only.
        let focus_handle = cx.focus_handle();
        window.focus(&focus_handle);
        let mut view = Self {
            reasoning,
            _reasoning_subscription: reasoning_subscription,
            key_input: cx.new(|cx| {
                InputState::new(window, cx)
                    .placeholder("Paste OpenAI API key (write-only)")
                    .masked(true)
            }),
            model_input: cx.new(|cx| {
                InputState::new(window, cx)
                    .placeholder("OpenAI Responses model id")
                    .default_value(model)
            }),
            codex_model_input: cx.new(|cx| {
                InputState::new(window, cx)
                    .placeholder("Codex CLI model id")
                    .default_value(codex_model)
            }),
            mcp,
            _mcp_subscription: mcp_subscription,
            mcp_id_input: cx
                .new(|cx| InputState::new(window, cx).placeholder("source id, e.g. project.docs")),
            mcp_name_input: cx.new(|cx| InputState::new(window, cx).placeholder("Source name")),
            mcp_endpoint_input: cx
                .new(|cx| InputState::new(window, cx).placeholder("https://host.example/mcp")),
            mcp_token_input: cx.new(|cx| {
                InputState::new(window, cx)
                    .placeholder("Optional bearer token (write-only)")
                    .masked(true)
            }),
            recording_library,
            recording_snapshot: recording_result.ok(),
            recording_budget_input: cx.new(|cx| {
                InputState::new(window, cx)
                    .placeholder("Recording budget in GB")
                    .default_value(recording_budget_gb.to_string())
            }),
            vault,
            _vault_subscription: vault_subscription,
            action_message,
            validation_lease: ValidationLease::default(),
            pane: SettingsPane::default(),
            focus_handle,
        };
        view.poll_codex(cx);
        view.poll_mcp(cx);
        view
    }

    fn select_pane(&mut self, pane: SettingsPane, cx: &mut Context<Self>) {
        self.pane = pane;
        cx.notify();
    }

    /// Re-opens an already-built sheet.
    ///
    /// The sheet is built once and kept, because building it probes the Codex CLI and starts the
    /// MCP poll — work cold launch deliberately does not do, and work that must not be repeated
    /// per open. Re-opening therefore restores everything a fresh sheet would have had: the pane
    /// the product wants read first, a freshly measured recording library, and focus, so Escape
    /// reaches the sheet rather than the window root.
    pub fn reopen(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.pane = SettingsPane::default();
        self.action_message = None;
        self.refresh_recordings();
        window.focus(&self.focus_handle);
        cx.notify();
    }

    /// Asks to be dismissed. The workspace owns whether the sheet renders; this view never
    /// removes a window, because it no longer has one of its own.
    fn dismiss(&mut self, cx: &mut Context<Self>) {
        cx.emit(SettingsEvent::Dismissed);
    }

    fn apply_to_all(&mut self, backend: Option<&str>, cx: &mut Context<Self>) {
        let result = self
            .reasoning
            .update(cx, |controller, cx| controller.apply_to_all(backend, cx));
        self.action_message = result.err().map(|error| error.to_string());
        cx.notify();
    }

    fn select_role(&mut self, role: Role, backend: Option<&str>, cx: &mut Context<Self>) {
        let result = self.reasoning.update(cx, |controller, cx| {
            controller.select_role(role, backend, cx)
        });
        self.action_message = result.err().map(|error| error.to_string());
        cx.notify();
    }

    fn save_model(&mut self, cx: &mut Context<Self>) {
        let model = self.model_input.read(cx).value().trim().to_owned();
        let result = self
            .reasoning
            .update(cx, |controller, cx| controller.set_openai_model(model, cx));
        self.action_message = result.err().map(|error| error.to_string());
        cx.notify();
    }

    fn save_codex_model(&mut self, cx: &mut Context<Self>) {
        let model = self.codex_model_input.read(cx).value().trim().to_owned();
        let result = self
            .reasoning
            .update(cx, |controller, cx| controller.set_codex_model(model, cx));
        self.action_message = result.err().map(|error| error.to_string());
        cx.notify();
    }

    fn enable_codex(&mut self, cx: &mut Context<Self>) {
        let result = self.reasoning.update(cx, |controller, cx| {
            controller.enable_codex_experimental(cx)
        });
        self.action_message = result.err().map(|error| error.to_string());
        cx.notify();
    }

    fn disable_codex(&mut self, cx: &mut Context<Self>) {
        let result = self.reasoning.update(cx, |controller, cx| {
            controller.disable_codex_experimental(cx)
        });
        self.action_message = result.err().map(|error| error.to_string());
        cx.notify();
    }

    fn check_codex(&mut self, cx: &mut Context<Self>) {
        let attempt = self
            .reasoning
            .update(cx, |controller, cx| controller.begin_codex_probe(cx));
        let Ok(ticket) = attempt else {
            self.action_message = attempt.err().map(|error| error.to_string());
            cx.notify();
            return;
        };
        let reasoning = self.reasoning.clone();
        cx.spawn(async move |_, cx| {
            loop {
                match ticket.try_recv() {
                    Ok(result) => {
                        let _ = reasoning.update(cx, |controller, cx| {
                            controller.finish_codex_probe(result, cx);
                        });
                        return;
                    }
                    Err(TryRecvError::Disconnected) => {
                        let _ = reasoning.update(cx, |controller, cx| {
                            controller.codex_probe_worker_disconnected(ticket.generation(), cx);
                        });
                        return;
                    }
                    Err(TryRecvError::Empty) => {
                        let _ = Timer::after(Duration::from_millis(50)).await;
                    }
                }
            }
        })
        .detach();
        self.action_message = None;
        cx.notify();
    }

    fn poll_codex(&mut self, cx: &mut Context<Self>) {
        self.check_codex(cx);
    }

    fn save_key(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let value = self.key_input.read(cx).value().to_string();
        self.key_input
            .update(cx, |input, cx| input.set_value("", window, cx));
        if value.is_empty() {
            self.action_message = Some("Paste an OpenAI API key first.".to_owned());
        } else {
            let result = self.reasoning.update(cx, |controller, cx| {
                controller.store_openai_key(SecretString::from(value), cx)
            });
            self.action_message = result.err().map(|error| error.to_string());
        }
        cx.notify();
    }

    /// Asks before removing the stored key from the Keychain.
    ///
    /// This was the one destructive control in the sheet with nothing in front of it: a single
    /// click took a credential out of the OS Keychain, and Sotto cannot put it back — the key is
    /// write-only here, so nothing in the app can even show what was lost.
    fn delete_key(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let view = cx.entity().downgrade();
        window.open_dialog(cx, move |dialog, _, _| {
            let view = view.clone();
            confirm_delete_dialog(
                dialog,
                "Delete API key",
                DELETE_KEY_PROMPT,
                "Delete key",
                move |_, cx| {
                    let _ = view.update(cx, |this, cx| this.confirm_delete_key(cx));
                },
            )
        });
    }

    fn confirm_delete_key(&mut self, cx: &mut Context<Self>) {
        let result = self
            .reasoning
            .update(cx, |controller, cx| controller.delete_openai_key(cx));
        self.action_message = result.err().map(|error| error.to_string());
        cx.notify();
    }

    fn validate_key(&mut self, cx: &mut Context<Self>) {
        let attempt = self
            .reasoning
            .update(cx, |controller, cx| controller.begin_openai_validation(cx));
        let Ok(ticket) = attempt else {
            self.action_message = attempt.err().map(|error| error.to_string());
            cx.notify();
            return;
        };
        self.validation_lease.replace(ticket.cancellation());
        let reasoning = self.reasoning.clone();
        cx.spawn(async move |_, cx| {
            loop {
                match ticket.try_recv() {
                    Ok(result) => {
                        let _ = reasoning.update(cx, |controller, cx| {
                            controller.finish_openai_validation(result, cx);
                        });
                        return;
                    }
                    Err(TryRecvError::Disconnected) => {
                        let _ = reasoning.update(cx, |controller, cx| {
                            controller.validation_worker_disconnected(ticket.generation(), cx);
                        });
                        return;
                    }
                    Err(TryRecvError::Empty) => {
                        Timer::after(Duration::from_millis(50)).await;
                    }
                }
            }
        })
        .detach();
        self.action_message = None;
        cx.notify();
    }

    fn configure_mcp(&mut self, cx: &mut Context<Self>) {
        let id = self.mcp_id_input.read(cx).value().to_string();
        let name = self.mcp_name_input.read(cx).value().to_string();
        let endpoint = self.mcp_endpoint_input.read(cx).value().to_string();
        self.action_message = self
            .mcp
            .update(cx, |controller, cx| {
                controller.configure_remote(&id, &name, &endpoint, cx)
            })
            .err()
            .map(|error| error.to_string());
        cx.notify();
    }

    fn poll_mcp(&self, cx: &mut Context<Self>) {
        let mcp = self.mcp.clone();
        cx.spawn(async move |_, cx| {
            loop {
                Timer::after(Duration::from_millis(50)).await;
                if mcp
                    .update(cx, |controller, cx| {
                        let _ = controller.poll(cx);
                    })
                    .is_err()
                {
                    return;
                }
            }
        })
        .detach();
    }

    fn choose_vault_folder(&mut self, cx: &mut Context<Self>) {
        let receiver = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("Choose vault".into()),
        });
        let view = cx.entity();
        cx.spawn(async move |_, cx| {
            let selected = match receiver.await {
                Ok(Ok(Some(mut paths))) if !paths.is_empty() => Some(paths.remove(0)),
                _ => None,
            };
            let Some(folder) = selected else {
                return;
            };
            let _ = view.update(cx, |this, cx| {
                this.action_message = this
                    .vault
                    .update(cx, |vault, cx| vault.set_folder(folder, cx))
                    .err();
                cx.notify();
            });
        })
        .detach();
    }

    fn set_vault_enabled(&mut self, enabled: bool, cx: &mut Context<Self>) {
        self.action_message = match self
            .vault
            .update(cx, |vault, cx| vault.set_enabled(enabled, cx))
        {
            Ok(()) if enabled => {
                Some("Markdown mirror enabled; rebuilding from the local store.".to_owned())
            }
            Ok(()) => Some(
                "Markdown mirror disabled. Existing files remain in the chosen folder.".to_owned(),
            ),
            Err(error) => Some(error),
        };
        cx.notify();
    }

    fn save_mcp_token(
        &mut self,
        server_id: mcp::ServerId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let token = self.mcp_token_input.read(cx).value().to_string();
        self.mcp_token_input
            .update(cx, |input, cx| input.set_value("", window, cx));
        self.action_message = if token.is_empty() {
            Some("Paste a bearer token first.".to_owned())
        } else {
            self.mcp
                .update(cx, |controller, cx| {
                    controller.store_bearer(&server_id, SecretString::from(token), cx)
                })
                .err()
                .map(|error| error.to_string())
        };
        cx.notify();
    }

    fn refresh_recordings(&mut self) {
        match self.recording_library.snapshot() {
            Ok(snapshot) => self.recording_snapshot = Some(snapshot),
            Err(error) => {
                self.action_message =
                    Some(format!("Could not refresh the recording library: {error}"));
            }
        }
    }

    fn save_recording_budget(&mut self, cx: &mut Context<Self>) {
        let raw = self.recording_budget_input.read(cx).value().to_string();
        let current_budget = self
            .recording_snapshot
            .as_ref()
            .map_or(BYTES_PER_GB, |snapshot| snapshot.usage.budget_bytes);
        let result = parse_raised_budget(&raw, current_budget).and_then(|budget| {
            self.recording_library
                .set_budget(budget)
                .map_err(|error| error.to_string())
        });
        self.action_message = match result {
            Ok(pruned) if pruned.is_empty() => Some("Recording budget saved.".to_owned()),
            Ok(pruned) => Some(format!(
                "Recording budget saved; {} oldest recording(s) were pruned.",
                pruned.len()
            )),
            Err(error) => Some(error),
        };
        self.refresh_recordings();
        cx.notify();
    }

    /// Asks, in a dialog over the row, before removing one retained recording's media.
    ///
    /// `prompt` names the row and its measured size, because the control that got here is a glyph.
    /// A red picture says a file is at risk; only the sentence says *which* file and how much of
    /// this Mac it occupies — and the dialog puts that sentence next to the trash icon rather than
    /// in the shell's message strip at the top of the window.
    fn delete_recording(
        &mut self,
        session_id: SessionId,
        prompt: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let prompt = prompt.to_owned();
        let view = cx.entity().downgrade();
        window.open_dialog(cx, move |dialog, _, _| {
            let view = view.clone();
            confirm_delete_dialog(
                dialog,
                "Delete recording",
                &prompt,
                "Delete",
                move |_, cx| {
                    let _ =
                        view.update(cx, |this, cx| this.confirm_delete_recording(session_id, cx));
                },
            )
        });
    }

    /// Deletes exactly the recording the dialog named.
    fn confirm_delete_recording(&mut self, session_id: SessionId, cx: &mut Context<Self>) {
        self.action_message = match self.recording_library.delete(session_id) {
            Ok(true) => {
                Some("Recording deleted. The transcript and meeting record were kept.".to_owned())
            }
            Ok(false) => Some("That meeting no longer has local media.".to_owned()),
            Err(error) => Some(format!("Could not delete the recording: {error}")),
        };
        self.refresh_recordings();
        cx.notify();
    }
}

/// What the confirm dialog says before it removes one retained recording.
///
/// Built here rather than at each call site so both storage rows — settled and still growing —
/// name their target the same way, and so the still-growing case keeps saying its final size is
/// not known rather than inventing one.
fn delete_recording_prompt(label: &str, measured: Option<&str>) -> String {
    match measured {
        Some(size) => format!(
            "Delete the recording for “{label}” and its {size}? Its transcript and notes stay on \
             this Mac."
        ),
        None => format!(
            "Delete the still-growing recording for “{label}”? Its final size is not known yet, \
             and its transcript and notes stay on this Mac."
        ),
    }
}

/// What the confirm dialog says before the stored OpenAI key leaves the Keychain.
const DELETE_KEY_PROMPT: &str = "Delete the stored OpenAI API key? It is removed from this Mac's \
                                 Keychain, and OpenAI reasoning stops resolving until you paste a \
                                 key again. Sotto cannot show you the key it is about to remove, \
                                 and cannot put it back. Your recordings, transcripts and notes \
                                 are untouched.";

const BYTES_PER_GB: u64 = 1_000_000_000;

const REASONING_DISCLOSURE: &str = "No reasoning is the default and leaves capture, the live transcript, persistence, and review fully available. OpenAI reasoning sends redacted transcript text to OpenAI over the network.";
const CODEX_DISCLOSURE: &str = "Notes-only experiment. Uses the Codex CLI login already cached on this Mac; a ChatGPT subscription login is accepted and Sotto never asks for or reads an API key. Enabled calls send redacted transcript text and any source excerpts you explicitly enabled for that meeting through Codex to OpenAI over the network; audio and screen images are not sent. Execution is ephemeral, read-only, and disables known tool features, but the current CLI cannot prove a zero-tool model surface. Enable only if you accept that limitation.";
const OPENAI_DISCLOSURE: &str = "The API key is write-only and stored in the OS Keychain. It is never saved in Sotto settings. Model changes affect new resolutions only.";
const RECORDINGS_DISCLOSURE: &str = "Sotto keeps each meeting's audio and selected screen recording locally. Deleting or automatic pruning removes only the media; the transcript and notes remain reviewable.";
const MCP_DISCLOSURE: &str = "Adding a source permits network contact only when you explicitly check it. Resources remain off for every meeting until selected. Sotto exposes no MCP tools or actions.";
const LOCAL_MCP_DISCLOSURE: &str = "The bounded stdio spike failed process-tree cleanup. Sotto cannot configure or launch local MCP commands.";

#[cfg(test)]
const PRIVACY_DISCLOSURES: [&str; 6] = [
    REASONING_DISCLOSURE,
    CODEX_DISCLOSURE,
    OPENAI_DISCLOSURE,
    RECORDINGS_DISCLOSURE,
    MCP_DISCLOSURE,
    LOCAL_MCP_DISCLOSURE,
];

/// Debug selectors for the six pinned disclosures, so a mounted test can prove each one is really
/// on the pane it was moved to rather than merely still present as a string constant.
const REASONING_DISCLOSURE_SELECTOR: &str = "settings-disclosure-reasoning";
const CODEX_DISCLOSURE_SELECTOR: &str = "settings-disclosure-codex";
const OPENAI_DISCLOSURE_SELECTOR: &str = "settings-disclosure-openai";
const RECORDINGS_DISCLOSURE_SELECTOR: &str = "settings-disclosure-recordings";
const MCP_DISCLOSURE_SELECTOR: &str = "settings-disclosure-mcp";
const LOCAL_MCP_DISCLOSURE_SELECTOR: &str = "settings-disclosure-local-mcp";

/// Debug selectors for the sheet's three destructive controls, so a mounted test clicks the same
/// trash icon and the same button a person does rather than calling the handler behind them.
const DELETE_RECORDING_SELECTOR: &str = "settings-delete-recording";
const DELETE_GROWING_RECORDING_SELECTOR: &str = "settings-delete-growing-recording";
const DELETE_OPENAI_KEY_SELECTOR: &str = "settings-delete-openai-key";

impl Render for SettingsView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let tokens = WorkspaceTokens::resolve(cx);
        let stacked_nav = window.viewport_size().width < NAV_STACKS_BELOW;
        let pane = self.pane;
        let body = match pane {
            SettingsPane::StorageAndPrivacy => self.storage_pane(tokens, cx),
            SettingsPane::Recording => recording_pane(tokens),
            SettingsPane::Transcription => transcription_pane(tokens),
            SettingsPane::SummariesAndAsk => self.summaries_pane(tokens, cx),
        };
        let mcp_message = self.mcp.read(cx).status_message().map(ToOwned::to_owned);
        let controller_message = self
            .reasoning
            .read(cx)
            .status_message()
            .map(ToOwned::to_owned);
        let action_message = self.action_message.clone();

        let nav = SettingsPane::ALL
            .into_iter()
            .map(|item| nav_item(item, pane, stacked_nav, tokens, cx))
            .collect::<Vec<_>>();

        let split = div()
            .flex_1()
            .min_h_0()
            .min_w_0()
            .flex()
            .when(stacked_nav, |view| view.flex_col())
            .child(
                div()
                    .flex_none()
                    .flex()
                    .gap(px(2.0))
                    .p(Space::SM)
                    .bg(tokens.ground)
                    .debug_selector(|| "settings-nav".into())
                    .when(stacked_nav, |view| {
                        view.w_full()
                            .flex_wrap()
                            .border_b_1()
                            .border_color(tokens.line_soft)
                    })
                    .when(!stacked_nav, |view| {
                        view.w(NAV_WIDTH)
                            .h_full()
                            .flex_col()
                            .border_r_1()
                            .border_color(tokens.line_soft)
                    })
                    .children(nav),
            )
            // The scrolling wrapper deliberately carries no layout of its own: `Scrollable` lifts
            // its element's style onto an outer div and clears it, so a column declared here would
            // lose its direction, gap and padding and the pane would lay out as a row.
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .min_h_0()
                    .child(
                        div()
                            .w_full()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .gap(Space::MD)
                            .px(px(20.0))
                            .py(px(18.0))
                            .debug_selector(move || pane.pane_selector().into())
                            .child(
                                div()
                                    .child(
                                        div()
                                            .text_size(px(15.0))
                                            .font_weight(gpui::FontWeight::SEMIBOLD)
                                            .child(pane.title()),
                                    )
                                    .child(
                                        div()
                                            .mt(Space::XS)
                                            .text_size(TypeScale::CONTROL)
                                            .text_color(tokens.muted)
                                            .child(pane.lede()),
                                    ),
                            )
                            .child(body),
                    )
                    .overflow_y_scrollbar(),
            );

        div()
            .track_focus(&self.focus_handle)
            .size_full()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .px(SHEET_INSET_X)
            .py(SHEET_INSET_Y)
            .bg(tokens.scrim)
            .text_color(tokens.ink)
            .text_size(TypeScale::BODY)
            .occlude()
            .debug_selector(|| "settings-scrim".into())
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, _, cx| {
                if event.keystroke.key.as_str() == "escape" {
                    this.dismiss(cx);
                }
            }))
            .child(
                div()
                    .w_full()
                    .max_w(SHEET_MAX_WIDTH)
                    .h_full()
                    .max_h(SHEET_MAX_HEIGHT)
                    .min_w_0()
                    .min_h_0()
                    .flex()
                    .flex_col()
                    .rounded_xl()
                    .border_1()
                    .border_color(tokens.line)
                    .bg(tokens.surface)
                    .overflow_hidden()
                    .debug_selector(|| "settings-page".into())
                    .child(
                        div()
                            .flex_none()
                            .flex()
                            .items_center()
                            .gap(Space::SM)
                            .px(Space::LG)
                            .py(Space::MD)
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .text_size(px(15.0))
                                    .font_weight(gpui::FontWeight::SEMIBOLD)
                                    .child("Settings"),
                            )
                            .child(
                                div()
                                    .flex_none()
                                    .debug_selector(|| "settings-close".into())
                                    .child(
                                        icon_button(
                                            "close-settings",
                                            IconName::Close,
                                            "Close settings (Escape)",
                                        )
                                        .on_click(cx.listener(|this, _, _, cx| this.dismiss(cx))),
                                    ),
                            ),
                    )
                    .child(split)
                    .when_some(mcp_message, |view, message| {
                        view.child(notice(message, tokens))
                    })
                    .when_some(controller_message, |view, message| {
                        view.child(notice(message, tokens))
                    })
                    .when_some(action_message, |view, message| {
                        view.child(notice(message, tokens))
                    }),
            )
    }
}

impl SettingsView {
    /// Leads, and is the default pane: what is kept, what it costs, and the one egress case.
    fn storage_pane(&self, tokens: WorkspaceTokens, cx: &mut Context<Self>) -> AnyElement {
        let recordings = self.recording_snapshot.clone();
        let vault = self.vault.read(cx);
        let vault_preferences = vault.preferences();
        let vault_status = vault.status();
        let directory = self
            .recording_library
            .recording_directory()
            .display()
            .to_string();
        pane_column()
            .child(claim(
                "Recordings stay on this Mac",
                RECORDINGS_DISCLOSURE,
                RECORDINGS_DISCLOSURE_SELECTOR,
                tokens,
                false,
            ))
            .child(claim(
                "Summaries can leave this Mac",
                REASONING_DISCLOSURE,
                REASONING_DISCLOSURE_SELECTOR,
                tokens,
                true,
            ))
            .child(statement(
                "Where recordings live",
                &directory,
                tokens,
                false,
            ))
            .child(
                settings_card(
                    "Markdown vault",
                    &format!("Current state · {}", vault_status.summary()),
                    tokens,
                )
                .child(statement(
                    "Where notes mirror",
                    &vault_preferences.folder.as_ref().map_or_else(
                        || "No folder chosen".to_owned(),
                        |folder| folder.display().to_string(),
                    ),
                    tokens,
                    false,
                ))
                .child(
                    action_row()
                        .child(
                            div().debug_selector(|| "choose-vault-folder".into()).child(
                                Button::new("choose-vault-folder-button")
                                    .label("Choose folder…")
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.choose_vault_folder(cx);
                                    })),
                            ),
                        )
                        .child(if vault_preferences.enabled {
                            div().debug_selector(|| "disable-vault".into()).child(
                                Button::new("disable-vault-button")
                                    .label("Turn off mirror")
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.set_vault_enabled(false, cx);
                                    })),
                            )
                        } else {
                            div().debug_selector(|| "enable-vault".into()).child(
                                Button::new("enable-vault-button")
                                    .label("Turn on mirror")
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.set_vault_enabled(true, cx);
                                    })),
                            )
                        }),
                ),
            )
            .when_some(recordings, |view, snapshot| {
                let usage = format!(
                    "{} used of {}",
                    format_bytes(snapshot.usage.used_bytes),
                    format_bytes(snapshot.usage.budget_bytes)
                );
                view.child(
                    settings_card(
                        "Storage budget",
                        &format!("Current state · {usage}"),
                        tokens,
                    )
                    .child(Input::new(&self.recording_budget_input))
                    .child(
                        action_row().child(
                            Button::new("save-recording-budget")
                                .label("Save budget")
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.save_recording_budget(cx);
                                })),
                        ),
                    ),
                )
                .children(snapshot.items.into_iter().enumerate().map(|(index, item)| {
                    let session_id = item.session_id;
                    let detail = recording_detail(&item.recording);
                    let measured = match item.recording {
                        sotto_core::types::SessionRecording::Available { byte_size, .. } => {
                            Some(format_bytes(byte_size))
                        }
                        sotto_core::types::SessionRecording::Missing { .. } => None,
                    };
                    let label = item.session_label.clone();
                    settings_card(&item.session_label, &detail, tokens).when_some(
                        measured,
                        |row, size| {
                            let prompt = delete_recording_prompt(&label, Some(&size));
                            row.child(
                                action_row().child(
                                    div()
                                        .debug_selector(|| DELETE_RECORDING_SELECTOR.into())
                                        .child(
                                            delete_icon_button(
                                                ("delete-recording", index),
                                                &format!("the recording for “{label}”"),
                                            )
                                            .on_click(
                                                cx.listener(move |this, _, window, cx| {
                                                    this.delete_recording(
                                                        session_id, &prompt, window, cx,
                                                    );
                                                }),
                                            ),
                                        ),
                                ),
                            )
                        },
                    )
                }))
                .children(
                    snapshot
                        .growing
                        .into_iter()
                        .enumerate()
                        .map(|(index, reference)| {
                            let session_id = reference.session_id();
                            let detail = match reference {
                                rag::RecordingReference::Growing {
                                    finalization_error: Some(error),
                                    ..
                                    // The stored reason already carries this prefix, applied by
                                    // `finalization_failure_reason`. Adding it again produced
                                    // "Recording finalization failed: Recording finalization
                                    // failed: …" on the maintainer's screen.
                                } => error,
                                rag::RecordingReference::Growing { .. } => {
                                    "Recording is still growing; final duration and size are not \
                                 available yet."
                                        .to_owned()
                                }
                                rag::RecordingReference::Settled(_) => {
                                    unreachable!(
                                        "the recording library separates settled references"
                                    )
                                }
                            };
                            let failed = detail.starts_with("Recording finalization failed:");
                            let label = format!("Meeting {}", session_id.get());
                            let prompt = delete_recording_prompt(&label, None);
                            settings_card_with_tone(&label, &detail, tokens, failed).child(
                                action_row().child(
                                    div()
                                        .debug_selector(|| DELETE_GROWING_RECORDING_SELECTOR.into())
                                        .child(
                                            delete_icon_button(
                                                ("delete-growing-recording", index),
                                                &format!("the recording for “{label}”"),
                                            )
                                            .on_click(
                                                cx.listener(move |this, _, window, cx| {
                                                    this.delete_recording(
                                                        session_id, &prompt, window, cx,
                                                    );
                                                }),
                                            ),
                                        ),
                                ),
                            )
                        }),
                )
            })
            .into_any_element()
    }

    /// The reasoning backend, and every source a summary may reach.
    fn summaries_pane(&self, tokens: WorkspaceTokens, cx: &mut Context<Self>) -> AnyElement {
        let reasoning = self.reasoning.read(cx);
        let watcher = selection_label(reasoning, Role::Watcher);
        let suggester = selection_label(reasoning, Role::Suggester);
        let summarizer = selection_label(reasoning, Role::Summarizer);
        let openai_status = reasoning.openai_readiness().label();
        let codex_readiness = reasoning.codex_readiness();
        let codex_status = codex_readiness.label();
        let codex_ready = matches!(
            codex_readiness,
            crate::reasoning::CodexReadiness::Ready { .. }
        );
        let codex_checking = matches!(codex_readiness, crate::reasoning::CodexReadiness::Checking);
        let codex_enabled = reasoning.codex_experimental_enabled();
        let openai_validating = matches!(
            reasoning.openai_readiness(),
            crate::reasoning::OpenAiReadiness::Validating
        );
        let codex_state = format!(
            "Current state · {} · {codex_status}",
            if codex_enabled { "Enabled" } else { "Disabled" }
        );
        let mcp_servers = self.mcp.read(cx).servers();

        pane_column()
            .child(statement(
                "Uncited claims fail closed",
                "A claim the transcript cannot support is dropped rather than softened. This is \
                 not configurable.",
                tokens,
                false,
            ))
            .child(statement(
                "Summarize when a recording stops — not built",
                "Summaries are written when you press Summarize on an open recording. Sotto does \
                 not summarize automatically, so no switch is offered here.",
                tokens,
                true,
            ))
            .child(pane_group(
                "Reasoning backends",
                "No reasoning is the default. What each backend sends is stated on its own card, \
                 and summarised under Storage & privacy.",
                tokens,
            ))
            .child(
                settings_card(
                    "Default for new work",
                    "Current state · role-specific selections below",
                    tokens,
                )
                .child(
                    action_row()
                        .child(
                            Button::new("all-no-reasoning")
                                .label("Use no reasoning")
                                .on_click(
                                    cx.listener(|this, _, _, cx| this.apply_to_all(None, cx)),
                                ),
                        )
                        .child(
                            Button::new("all-openai")
                                .label("Use OpenAI for all")
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.apply_to_all(Some(OPENAI_RESPONSES_BACKEND_ID), cx);
                                })),
                        ),
                ),
            )
            .child(
                settings_card("Codex subscription — experimental", &codex_state, tokens)
                    .child(disclosure(
                        CODEX_DISCLOSURE,
                        CODEX_DISCLOSURE_SELECTOR,
                        tokens,
                    ))
                    .child(Input::new(&self.codex_model_input))
                    .child(
                        action_row()
                            .child(
                                Button::new("save-codex-model")
                                    .label("Save model id")
                                    .on_click(
                                        cx.listener(|this, _, _, cx| this.save_codex_model(cx)),
                                    ),
                            )
                            .child(
                                Button::new("check-codex")
                                    .label("Check login")
                                    .disabled(codex_checking)
                                    .on_click(cx.listener(|this, _, _, cx| this.check_codex(cx))),
                            )
                            // ADR-0014's acknowledgement gate: Codex cannot become a role's
                            // backend until this button has been pressed with the disclosure
                            // above it on screen.
                            .child(if codex_enabled {
                                Button::new("disable-codex")
                                    .label("Disable Codex")
                                    .on_click(cx.listener(|this, _, _, cx| this.disable_codex(cx)))
                            } else {
                                Button::new("enable-codex")
                                    .label("Enable Codex — I understand")
                                    .disabled(!codex_ready)
                                    .on_click(cx.listener(|this, _, _, cx| this.enable_codex(cx)))
                            }),
                    ),
            )
            .child(
                settings_card(
                    "OpenAI Responses API",
                    &format!("Current state · {openai_status}"),
                    tokens,
                )
                .child(disclosure(
                    OPENAI_DISCLOSURE,
                    OPENAI_DISCLOSURE_SELECTOR,
                    tokens,
                ))
                .child(Input::new(&self.model_input))
                .child(
                    action_row().child(
                        Button::new("save-openai-model")
                            .label("Save model id")
                            .on_click(cx.listener(|this, _, _, cx| this.save_model(cx))),
                    ),
                )
                .child(Input::new(&self.key_input))
                .child(
                    action_row()
                        .child(
                            Button::new("save-openai-key").label("Save key").on_click(
                                cx.listener(|this, _, window, cx| this.save_key(window, cx)),
                            ),
                        )
                        .child(
                            div()
                                .debug_selector(|| DELETE_OPENAI_KEY_SELECTOR.into())
                                .child(
                                    Button::new("delete-openai-key")
                                        .label("Delete key…")
                                        .on_click(cx.listener(|this, _, window, cx| {
                                            this.delete_key(window, cx);
                                        })),
                                ),
                        )
                        .child(
                            Button::new("validate-openai-key")
                                .label("Validate with OpenAI")
                                .disabled(openai_validating)
                                .on_click(cx.listener(|this, _, _, cx| this.validate_key(cx))),
                        ),
                ),
            )
            .child(pane_group(
                "Reasoning roles",
                "Each role shows its current backend before the available changes.",
                tokens,
            ))
            .children([
                role_controls("watcher", "Watcher", watcher, Role::Watcher, false, cx),
                role_controls(
                    "suggester",
                    "Suggester",
                    suggester,
                    Role::Suggester,
                    false,
                    cx,
                ),
                role_controls(
                    "summarizer",
                    "Summarizer",
                    summarizer,
                    Role::Summarizer,
                    codex_enabled && codex_ready,
                    cx,
                ),
            ])
            .child(pane_group(
                "Sources a summary may reach",
                "Beyond the transcript, a summary sees only sources you enabled for that meeting.",
                tokens,
            ))
            .child(disclosure(MCP_DISCLOSURE, MCP_DISCLOSURE_SELECTOR, tokens))
            .child(
                settings_card(
                    "Add remote HTTPS source",
                    "Current state · not added",
                    tokens,
                )
                .child(Input::new(&self.mcp_id_input))
                .child(Input::new(&self.mcp_name_input))
                .child(Input::new(&self.mcp_endpoint_input))
                .child(
                    action_row().child(
                        Button::new("configure-mcp-http")
                            .label("Add HTTPS source")
                            .on_click(cx.listener(|this, _, _, cx| this.configure_mcp(cx))),
                    ),
                ),
            )
            .child(
                settings_card_with_tone("Local MCP process — unavailable", "", tokens, true).child(
                    disclosure(LOCAL_MCP_DISCLOSURE, LOCAL_MCP_DISCLOSURE_SELECTOR, tokens),
                ),
            )
            .child(
                settings_card(
                    "Bearer credential",
                    "Current state · paste a write-only token, then store it on one configured \
                     source below",
                    tokens,
                )
                .child(Input::new(&self.mcp_token_input)),
            )
            .children(mcp_servers.into_iter().enumerate().map(|(index, server)| {
                let discover_id = server.id.clone();
                let save_id = server.id.clone();
                let delete_id = server.id.clone();
                let remove_id = server.id.clone();
                let health = server.health.label();
                let credential = server.credential.label();
                settings_card(
                    &format!("{} — {}", server.display_name, server.endpoint.host()),
                    &format!("Current state · Remote HTTPS · {health} · {credential}"),
                    tokens,
                )
                .child(
                    action_row()
                        .child(
                            Button::new(("mcp-check", index))
                                .label("Check and discover resources")
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.action_message = this
                                        .mcp
                                        .update(cx, |controller, cx| {
                                            controller.begin_discovery(discover_id.clone(), cx)
                                        })
                                        .err()
                                        .map(|error| error.to_string());
                                    cx.notify();
                                })),
                        )
                        .child(
                            Button::new(("mcp-token", index))
                                .label("Store pasted token")
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    this.save_mcp_token(save_id.clone(), window, cx);
                                })),
                        )
                        .child(
                            Button::new(("mcp-token-delete", index))
                                .label("Delete token")
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.action_message = this
                                        .mcp
                                        .update(cx, |controller, cx| {
                                            controller.delete_bearer(&delete_id, cx)
                                        })
                                        .err()
                                        .map(|error| error.to_string());
                                    cx.notify();
                                })),
                        )
                        .child(
                            Button::new(("mcp-remove", index))
                                .label("Remove source")
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.action_message = this
                                        .mcp
                                        .update(cx, |controller, cx| {
                                            controller.remove_source(&remove_id, cx)
                                        })
                                        .err()
                                        .map(|error| error.to_string());
                                    cx.notify();
                                })),
                        ),
                )
            }))
            .into_any_element()
    }
}

/// What is captured when a recording starts.
///
/// Every mock control on this pane is unbuilt — a microphone picker and a start shortcut — so the
/// pane is statements only. Capture scope and microphone inclusion are per-recording choices made
/// at the picker, not persisted settings, and saying so is more useful than a switch that lies.
fn recording_pane(tokens: WorkspaceTokens) -> AnyElement {
    pane_column()
        .child(statement(
            "Capture scope",
            "Chosen per recording through the macOS picker: one application, one window, one \
             display, or your microphone alone. The system content filter enforces that choice, so \
             nothing outside it ever reaches Sotto.",
            tokens,
            false,
        ))
        .child(statement(
            "Audio scope is reported per recording",
            "macOS cannot always scope audio to the chosen application. The capture bar states \
             which you got — app audio or system audio — while a recording runs, and the session \
             record keeps that answer afterwards.",
            tokens,
            false,
        ))
        .child(statement(
            "Include your microphone",
            "Chosen when a recording starts, not here. An application capture records your \
             microphone as a separate labelled track; a microphone-only recording captures nothing \
             else.",
            tokens,
            false,
        ))
        .child(statement(
            "Screen frames",
            "Video is written into the retained recording. A frame is decoded from it only when \
             something explicitly asks for one; Sotto never samples frames into the timeline in \
             the background.",
            tokens,
            false,
        ))
        .child(statement(
            "Microphone input — not built",
            "Sotto records the system's default input device. Choosing a different microphone from \
             Sotto is not implemented; change the input in System Settings › Sound.",
            tokens,
            true,
        ))
        .child(statement(
            "Start shortcut — not built",
            "Sotto registers no key bindings. A recording starts from the workspace, and printing \
             a shortcut that does nothing is the same defect as a dead button.",
            tokens,
            true,
        ))
        .into_any_element()
}

/// The local Whisper pass. Selection lives on Home because it gates all three beginnings; this
/// pane records the same exact artifact costs and the compatibility reason for the default.
fn transcription_pane(tokens: WorkspaceTokens) -> AnyElement {
    let choices = crate::session::MODEL_CHOICES
        .into_iter()
        .map(|size| {
            format!(
                "{} · {} download",
                crate::session::model_label(size),
                crate::session::download_size_label(size)
            )
        })
        .collect::<Vec<_>>()
        .join(" · ");
    pane_column()
        .child(statement(
            "Model · choose on Home",
            &format!(
                "{choices}. Every size comes from the pinned model specification. small.en is the \
                 default to preserve existing transcription behavior; T065 lacked an independent \
                 reference and established no accuracy ranking. Runtime cost stays qualitative \
                 because no complete three-model measurement is recorded."
            ),
            tokens,
            false,
        ))
        .child(statement(
            "Model override",
            "SOTTO_WHISPER_MODEL points transcription at a specific non-empty model file. Sotto \
             neither downloads nor digest-checks an override, so it remains a developer tool \
             rather than a managed choice.",
            tokens,
            false,
        ))
        .child(statement(
            "Language — not built",
            "All offered .en models are English-only and Sotto passes no language option. Speech in \
             another language is transcribed as though it were English.",
            tokens,
            true,
        ))
        .child(statement(
            "The transcript is append-only",
            "Captured speech is never edited in place. Corrections and your own notes are appended \
             alongside it, so the record stays honest.",
            tokens,
            false,
        ))
        .child(statement(
            "Re-transcribing is safe",
            "Re-running transcription on a stopped recording replaces only the derived transcript; \
             the captured timeline is not modified. The control sits in the open recording's bar.",
            tokens,
            false,
        ))
        .into_any_element()
}

fn nav_item(
    pane: SettingsPane,
    selected: SettingsPane,
    stacked: bool,
    tokens: WorkspaceTokens,
    cx: &mut Context<SettingsView>,
) -> AnyElement {
    let current = pane == selected;
    div()
        .id(pane.nav_selector())
        .flex_none()
        .px(Space::MD)
        .py(px(6.0))
        .rounded_md()
        .text_size(TypeScale::CONTROL)
        .cursor_pointer()
        .when(!stacked, |view| view.w_full())
        .when(current, |view| {
            view.bg(tokens.accent_wash)
                .text_color(tokens.accent_ink)
                .font_weight(gpui::FontWeight::SEMIBOLD)
        })
        .when(!current, |view| view.text_color(tokens.muted))
        .debug_selector(move || pane.nav_selector().into())
        .child(pane.title())
        .on_click(cx.listener(move |this, _, _, cx| this.select_pane(pane, cx)))
        .into_any_element()
}

fn pane_column() -> gpui::Div {
    div().w_full().min_w_0().flex().flex_col().gap(Space::MD)
}

fn selection_label(controller: &ReasoningController, role: Role) -> &'static str {
    match controller.selected_id(role).map(|id| id.as_str()) {
        Some(OPENAI_RESPONSES_BACKEND_ID) => "OpenAI API",
        Some(CODEX_CLI_BACKEND_ID) => "Codex experimental",
        Some(_) => "Unavailable backend",
        None => "No reasoning",
    }
}

fn role_controls(
    id: &'static str,
    label: &'static str,
    current: &'static str,
    role: Role,
    codex_selectable: bool,
    cx: &mut Context<SettingsView>,
) -> impl IntoElement + use<> {
    let tokens = WorkspaceTokens::resolve(cx);
    settings_card(label, &format!("Current state · {current}"), tokens).child(
        action_row()
            .child(
                Button::new((id, 0_u32))
                    .label("No reasoning")
                    .on_click(cx.listener(move |this, _, _, cx| this.select_role(role, None, cx))),
            )
            .child(
                Button::new((id, 1_u32))
                    .label("OpenAI")
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.select_role(role, Some(OPENAI_RESPONSES_BACKEND_ID), cx);
                    })),
            )
            .child(
                Button::new((id, 2_u32))
                    .label("Codex")
                    .disabled(!codex_selectable)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.select_role(role, Some(CODEX_CLI_BACKEND_ID), cx);
                    })),
            ),
    )
}

/// A group heading inside a pane.
fn pane_group(title: &str, detail: &str, tokens: WorkspaceTokens) -> gpui::Div {
    div()
        .w_full()
        .min_w_0()
        .child(
            div()
                .text_size(TypeScale::TITLE)
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .child(title.to_owned()),
        )
        .child(
            div()
                .mt(Space::XS)
                .text_size(TypeScale::CONTROL)
                .text_color(tokens.muted)
                .child(detail.to_owned()),
        )
}

/// The mock's `.claim`: a leading statement about what Sotto does with your data.
///
/// `honest` paints the warn palette, which the mock reserves for a limitation stated plainly.
fn claim(
    title: &str,
    detail: &str,
    selector: &'static str,
    tokens: WorkspaceTokens,
    honest: bool,
) -> gpui::Div {
    div()
        .w_full()
        .min_w_0()
        .flex()
        .flex_col()
        .gap(Space::XS)
        .p(Space::MD)
        .rounded_lg()
        .border_1()
        .border_color(if honest {
            tokens.warn
        } else {
            tokens.accent_line
        })
        .bg(if honest {
            tokens.warn_wash
        } else {
            tokens.accent_wash
        })
        .debug_selector(|| "settings-claim".into())
        .child(
            div()
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .text_color(if honest {
                    tokens.warn
                } else {
                    tokens.accent_ink
                })
                .child(title.to_owned()),
        )
        .child(disclosure(detail, selector, tokens))
}

/// A labelled statement with no control, for a fact the user cannot change — including the mock
/// controls Sotto has not built.
fn statement(title: &str, detail: &str, tokens: WorkspaceTokens, unbuilt: bool) -> gpui::Div {
    settings_card_with_tone(title, detail, tokens, unbuilt)
}

fn settings_card(title: &str, state: &str, tokens: WorkspaceTokens) -> gpui::Div {
    settings_card_with_tone(title, state, tokens, false)
}

fn settings_card_with_tone(
    title: &str,
    state: &str,
    tokens: WorkspaceTokens,
    warning: bool,
) -> gpui::Div {
    div()
        .w_full()
        .min_w_0()
        .flex()
        .flex_col()
        .gap(Space::MD)
        .p(Space::LG)
        .rounded_lg()
        .border_1()
        .border_color(if warning { tokens.warn } else { tokens.line })
        .bg(if warning {
            tokens.warn_wash
        } else {
            tokens.surface
        })
        .debug_selector(|| "settings-card".into())
        .child(
            div()
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .child(title.to_owned()),
        )
        .when(!state.is_empty(), |view| {
            view.child(
                div()
                    .text_size(TypeScale::CONTROL)
                    .text_color(if warning { tokens.warn } else { tokens.muted })
                    .child(state.to_owned()),
            )
        })
}

/// One pinned privacy statement, carrying its own debug selector so a test can find it on a pane.
fn disclosure(detail: &str, selector: &'static str, tokens: WorkspaceTokens) -> gpui::Div {
    div()
        .w_full()
        .min_w_0()
        .text_size(TypeScale::CONTROL)
        .text_color(tokens.ink_2)
        .debug_selector(move || selector.to_owned())
        .child(detail.to_owned())
}

fn action_row() -> gpui::Div {
    div()
        .w_full()
        .min_w_0()
        .flex()
        .flex_wrap()
        .items_center()
        .gap(Space::SM)
        .debug_selector(|| "settings-action-row".into())
}

fn notice(message: String, tokens: WorkspaceTokens) -> gpui::Div {
    div()
        .flex_none()
        .w_full()
        .min_w_0()
        .p(Space::MD)
        .border_t_1()
        .border_color(tokens.line_soft)
        .bg(tokens.warn_wash)
        .text_size(TypeScale::CONTROL)
        .text_color(tokens.warn)
        .debug_selector(|| "settings-notice".into())
        .child(message)
}

fn format_bytes(bytes: u64) -> String {
    if bytes >= BYTES_PER_GB {
        format!("{:.1} GB", bytes as f64 / BYTES_PER_GB as f64)
    } else {
        format!("{:.1} MB", bytes as f64 / 1_000_000_f64)
    }
}

fn parse_raised_budget(raw: &str, current_budget: u64) -> Result<u64, String> {
    let budget = raw
        .trim()
        .parse::<u64>()
        .map_err(|_| "Enter a whole number of GB greater than zero.".to_owned())?
        .checked_mul(BYTES_PER_GB)
        .filter(|budget| *budget > 0)
        .ok_or_else(|| "Enter a whole number of GB greater than zero.".to_owned())?;
    if budget < current_budget {
        return Err(format!(
            "The recording budget can only be raised. It is currently {}.",
            format_bytes(current_budget)
        ));
    }
    Ok(budget)
}

fn recording_detail(recording: &sotto_core::types::SessionRecording) -> String {
    use sotto_core::types::{RecordingMissingReason, SessionRecording};

    match recording {
        SessionRecording::Available {
            container,
            duration,
            byte_size,
            ..
        } => format!(
            "{} · {} · {}",
            container.as_str().to_uppercase(),
            format_duration(*duration),
            format_bytes(*byte_size)
        ),
        SessionRecording::Missing {
            reason: RecordingMissingReason::Deleted,
            ..
        } => "Recording deleted · transcript retained".to_owned(),
        SessionRecording::Missing {
            reason: RecordingMissingReason::Pruned,
            ..
        } => "Recording pruned to stay within budget · transcript retained".to_owned(),
    }
}

fn format_duration(duration: Duration) -> String {
    let seconds = duration.as_secs();
    format!("{}:{:02}", seconds / 60, seconds % 60)
}

#[cfg(test)]
mod tests {
    use std::{cell::Cell, rc::Rc, sync::Arc, time::Duration};

    use std::ops::Deref as _;

    use super::{
        BYTES_PER_GB, CODEX_DISCLOSURE_SELECTOR, DELETE_KEY_PROMPT, DELETE_OPENAI_KEY_SELECTOR,
        DELETE_RECORDING_SELECTOR, LOCAL_MCP_DISCLOSURE_SELECTOR, MCP_DISCLOSURE_SELECTOR,
        OPENAI_DISCLOSURE_SELECTOR, PRIVACY_DISCLOSURES, REASONING_DISCLOSURE_SELECTOR,
        RECORDINGS_DISCLOSURE_SELECTOR, SettingsEvent, SettingsPane, SettingsView, ValidationLease,
        delete_recording_prompt, format_bytes, format_duration, parse_raised_budget,
    };
    use crate::{
        mcp::{McpController, McpCredentialStore, McpUiError},
        reasoning::{
            CodexProbeSource, OpenAiCredentialStore, OpenAiReadiness, ReasoningController,
        },
        session::RecordingLibrary,
        vault::VaultMirrorController,
    };
    use gpui::{
        AppContext as _, Entity, Modifiers, TestAppContext, VisualTestContext, prelude::*, px, size,
    };
    use gpui_component::WindowExt as _;
    use mcp::{HttpEndpoint, ServerId};
    use providers::{AuthStatus, codex::CodexProbe};
    use secrecy::SecretString;
    use sotto_core::{CancellationToken, ProviderError};

    struct NoOpenAiCredentials;

    impl OpenAiCredentialStore for NoOpenAiCredentials {
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

    struct ReadyCodex;

    impl CodexProbeSource for ReadyCodex {
        fn probe(&self, _: CancellationToken) -> CodexProbe {
            CodexProbe {
                version: Some("test".to_owned()),
                login_status: AuthStatus::Ready,
            }
        }
    }

    struct NoMcpCredentials;

    impl McpCredentialStore for NoMcpCredentials {
        fn store(
            &self,
            _: &ServerId,
            _: &HttpEndpoint,
            _: &SecretString,
        ) -> Result<(), McpUiError> {
            Ok(())
        }

        fn load(&self, _: &ServerId, _: &HttpEndpoint) -> Result<Option<SecretString>, McpUiError> {
            Ok(None)
        }

        fn delete(&self, _: &ServerId, _: &HttpEndpoint) -> Result<(), McpUiError> {
            Ok(())
        }
    }

    struct MountedSheet<'a> {
        settings: Entity<SettingsView>,
        visual: &'a mut VisualTestContext,
        directory: tempfile::TempDir,
    }

    impl MountedSheet<'_> {
        fn database(&self) -> std::path::PathBuf {
            self.directory.path().join("sotto.sqlite3")
        }

        fn recordings(&self) -> std::path::PathBuf {
            self.directory.path().join("recordings")
        }
    }

    /// Stands in for the workspace that mounts the sheet in the product.
    ///
    /// It exists for one reason the sheet cannot supply itself: `gpui_component::Root` stores the
    /// open dialogs but draws nothing, so the view it wraps has to render the dialog layer. In the
    /// product that view is `MeetingWorkspace`, which renders the sheet and the layer together;
    /// here it is this.
    struct SheetHost {
        settings: Entity<SettingsView>,
    }

    impl gpui::Render for SheetHost {
        fn render(
            &mut self,
            window: &mut gpui::Window,
            cx: &mut gpui::Context<Self>,
        ) -> impl gpui::IntoElement {
            gpui::div()
                .size_full()
                .child(self.settings.clone())
                .children(gpui_component::Root::render_dialog_layer(window, cx))
        }
    }

    fn mount(
        cx: &mut TestAppContext,
        width: gpui::Pixels,
    ) -> Result<MountedSheet<'_>, Box<dyn std::error::Error>> {
        cx.update(gpui_component::init);
        let directory = tempfile::tempdir()?;
        let reasoning_path = directory.path().join("reasoning.json");
        let mcp_path = directory.path().join("mcp.json");
        let database = directory.path().join("sotto.sqlite3");
        let recordings = directory.path().join("recordings");
        std::fs::create_dir_all(&recordings)?;
        let reasoning = cx.new(|_| {
            ReasoningController::load_with_probe_source(
                reasoning_path,
                Arc::new(NoOpenAiCredentials),
                Arc::new(ReadyCodex),
            )
        });
        let mcp = cx.new(|_| McpController::load(Some(mcp_path), Arc::new(NoMcpCredentials)));
        let library = RecordingLibrary::new(&database, recordings);
        let handle = cx.update(|cx| {
            cx.open_window(gpui::WindowOptions::default(), move |window, cx| {
                let vault = cx.new(|cx| VaultMirrorController::new(database.clone(), cx));
                let settings = cx.new(|cx| {
                    SettingsView::new_with_recording_library(
                        window, reasoning, mcp, vault, library, cx,
                    )
                });
                let host = cx.new(|_| SheetHost { settings });
                cx.new(|cx| gpui_component::Root::new(host, window, cx))
            })
        })?;
        let settings = cx
            .update(|cx| {
                handle.update(cx, |root, _, cx| {
                    root.view()
                        .clone()
                        .downcast::<SheetHost>()
                        .map(|host| host.read(cx).settings.clone())
                })
            })?
            .map_err(|_| std::io::Error::other("the window root must wrap the settings sheet"))?;
        let visual = VisualTestContext::from_window(*handle.deref(), cx).into_mut();
        visual.update(|window, _| window.activate_window());
        visual.simulate_resize(size(width, px(720.0)));
        visual.refresh()?;
        visual.run_until_parked();
        Ok(MountedSheet {
            settings,
            visual,
            directory,
        })
    }

    /// Scrolls `pane` until `selector` is on screen, and returns where it landed.
    ///
    /// The sheet is capped at 600 px tall by design and its panes scroll, so the storage pane's
    /// recording rows and the OpenAI card's buttons sit below the fold at every viewport a test
    /// can open. GPUI records no debug bounds for a fully clipped element, so a control has to be
    /// scrolled into view before it can be found — or clicked, which is the point: this drives the
    /// same trash icon and the same button a person reaches.
    fn scroll_into_view(
        visual: &mut VisualTestContext,
        pane: &'static str,
        selector: &'static str,
    ) -> Result<gpui::Bounds<gpui::Pixels>, Box<dyn std::error::Error>> {
        for _ in 0..40_u8 {
            let page = visual
                .debug_bounds("settings-page")
                .ok_or_else(|| std::io::Error::other("the settings sheet must render"))?;
            // Found is not the same as clickable: a control straddling the bottom of the scroll
            // viewport paints, and so has bounds, while its centre is behind the clip.
            if let Some(bounds) = visual
                .debug_bounds(selector)
                .filter(|bounds| bounds.center().y < page.bottom() - px(16.0))
            {
                return Ok(bounds);
            }
            let area = visual
                .debug_bounds(pane)
                .ok_or_else(|| std::io::Error::other("the selected pane must render"))?;
            let over = gpui::point(
                page.center().x,
                area.center().y.min(page.bottom() - px(80.0)),
            );
            visual.simulate_mouse_move(over, None, Modifiers::none());
            visual.simulate_event(gpui::ScrollWheelEvent {
                position: over,
                delta: gpui::ScrollDelta::Pixels(gpui::point(px(0.0), px(-90.0))),
                modifiers: Modifiers::none(),
                touch_phase: gpui::TouchPhase::Moved,
            });
            visual.refresh()?;
            visual.run_until_parked();
        }
        Err(Box::new(std::io::Error::other(format!(
            "{selector} never came into view"
        ))))
    }

    /// Writes a settled recording with real bytes on disk so a delete has something to remove.
    async fn seed_settled_recording(
        database: &std::path::Path,
        recordings: &std::path::Path,
        session_id: sotto_core::SessionId,
    ) -> Result<std::path::PathBuf, Box<dyn std::error::Error>> {
        let path = recordings.join(format!("{}.mp4", session_id.get()));
        std::fs::write(&path, vec![0_u8; 4_096])?;
        let store = rag::Store::open(database).await?;
        // The recording row references a session; without one the insert trips the foreign key.
        let mut record = sotto_core::Session::new(
            session_id,
            sotto_core::CaptureTarget::microphone_only(),
            1_786_625_633_040,
        );
        record.end(1_786_625_700_000);
        store.save_session(&record).await?;
        store
            .save_recording(&sotto_core::types::SessionRecording::Available {
                session_id,
                path: path.to_string_lossy().into_owned(),
                container: sotto_core::types::RecordingContainer::Mp4,
                duration: Duration::from_secs(120),
                byte_size: 4_096,
                time_mapping: sotto_core::types::MediaTimeMapping::IDENTITY,
            })
            .await?;
        Ok(path)
    }

    /// Counts the dismissals the sheet asks for.
    ///
    /// The sheet no longer owns a window, so "closed" cannot be observed as a window that vanished.
    /// It is an emitted request, and the workspace that mounted the sheet is what acts on it.
    fn watch_dismissals(
        visual: &mut VisualTestContext,
        settings: &Entity<SettingsView>,
    ) -> Rc<Cell<usize>> {
        let count = Rc::new(Cell::new(0_usize));
        let sink = Rc::clone(&count);
        visual.update(|_, cx| {
            cx.subscribe(settings, move |_, SettingsEvent::Dismissed, _| {
                sink.set(sink.get() + 1);
            })
            .detach();
        });
        count
    }

    #[test]
    fn openai_errors_keep_actionable_product_states() {
        assert_eq!(
            OpenAiReadiness::from_provider_error(ProviderError::Auth),
            OpenAiReadiness::BadKey
        );
        assert_eq!(
            OpenAiReadiness::from_provider_error(ProviderError::CredentialStore(
                "access denied".to_owned()
            )),
            OpenAiReadiness::CredentialStore("access denied".to_owned())
        );
        assert_eq!(
            OpenAiReadiness::from_provider_error(ProviderError::RateLimit { retry_after: None }),
            OpenAiReadiness::RateLimited
        );
    }

    #[test]
    fn dropping_validation_lease_cancels_view_owned_attempt() {
        let cancellation = CancellationToken::new();
        let mut lease = ValidationLease::default();
        lease.replace(cancellation.clone());
        drop(lease);
        assert!(cancellation.is_cancelled());
    }

    #[test]
    fn recording_measurements_are_human_readable() {
        assert_eq!(format_bytes(12_500_000), "12.5 MB");
        assert_eq!(format_bytes(20_000_000_000), "20.0 GB");
        assert_eq!(format_duration(Duration::from_secs(3_725)), "62:05");
    }

    #[test]
    fn recording_budget_is_positive_and_raise_only() {
        assert_eq!(
            parse_raised_budget("0", 20 * BYTES_PER_GB),
            Err("Enter a whole number of GB greater than zero.".to_owned())
        );
        assert_eq!(
            parse_raised_budget("19", 20 * BYTES_PER_GB),
            Err("The recording budget can only be raised. It is currently 20.0 GB.".to_owned())
        );
        assert_eq!(
            parse_raised_budget("25", 20 * BYTES_PER_GB),
            Ok(25 * BYTES_PER_GB)
        );
    }

    #[test]
    fn privacy_disclosure_inventory_is_complete() {
        assert_eq!(
            PRIVACY_DISCLOSURES,
            [
                "No reasoning is the default and leaves capture, the live transcript, persistence, and review fully available. OpenAI reasoning sends redacted transcript text to OpenAI over the network.",
                "Notes-only experiment. Uses the Codex CLI login already cached on this Mac; a ChatGPT subscription login is accepted and Sotto never asks for or reads an API key. Enabled calls send redacted transcript text and any source excerpts you explicitly enabled for that meeting through Codex to OpenAI over the network; audio and screen images are not sent. Execution is ephemeral, read-only, and disables known tool features, but the current CLI cannot prove a zero-tool model surface. Enable only if you accept that limitation.",
                "The API key is write-only and stored in the OS Keychain. It is never saved in Sotto settings. Model changes affect new resolutions only.",
                "Sotto keeps each meeting's audio and selected screen recording locally. Deleting or automatic pruning removes only the media; the transcript and notes remain reviewable.",
                "Adding a source permits network contact only when you explicitly check it. Resources remain off for every meeting until selected. Sotto exposes no MCP tools or actions.",
                "The bounded stdio spike failed process-tree cleanup. Sotto cannot configure or launch local MCP commands.",
            ],
            "settings hierarchy must not drop or silently rewrite a privacy disclosure"
        );
    }

    /// The inventory above proves the strings still exist. This proves they are still *rendered*,
    /// on the pane each one moved to — the failure mode a four-pane split introduces.
    #[test]
    fn every_pinned_disclosure_still_renders_on_a_pane() -> Result<(), Box<dyn std::error::Error>> {
        let mut cx = TestAppContext::single();
        let sheet = mount(&mut cx, px(900.0))?;

        for selector in [
            RECORDINGS_DISCLOSURE_SELECTOR,
            REASONING_DISCLOSURE_SELECTOR,
        ] {
            assert!(
                sheet.visual.debug_bounds(selector).is_some(),
                "{selector} must render on the default Storage & privacy pane"
            );
        }

        let settings = sheet.settings.clone();
        sheet.visual.update(|_, cx| {
            settings.update(cx, |this, cx| {
                this.select_pane(SettingsPane::SummariesAndAsk, cx);
            });
        });
        sheet.visual.refresh()?;
        sheet.visual.run_until_parked();
        for selector in [
            CODEX_DISCLOSURE_SELECTOR,
            OPENAI_DISCLOSURE_SELECTOR,
            MCP_DISCLOSURE_SELECTOR,
            LOCAL_MCP_DISCLOSURE_SELECTOR,
        ] {
            assert!(
                sheet.visual.debug_bounds(selector).is_some(),
                "{selector} must render on the Summaries & Ask pane"
            );
        }
        Ok(())
    }

    #[test]
    fn settings_opens_on_storage_and_privacy_with_one_pane_rendered()
    -> Result<(), Box<dyn std::error::Error>> {
        let mut cx = TestAppContext::single();
        let sheet = mount(&mut cx, px(900.0))?;

        assert!(
            sheet.visual.debug_bounds("settings-scrim").is_some(),
            "settings must render as a sheet over a scrim"
        );
        assert!(
            sheet.visual.debug_bounds("settings-close").is_some(),
            "the sheet must always offer its close control"
        );
        assert!(
            sheet
                .visual
                .debug_bounds(SettingsPane::StorageAndPrivacy.pane_selector())
                .is_some(),
            "Storage & privacy leads and is the default pane"
        );
        for pane in [
            SettingsPane::Recording,
            SettingsPane::Transcription,
            SettingsPane::SummariesAndAsk,
        ] {
            assert!(
                sheet.visual.debug_bounds(pane.pane_selector()).is_none(),
                "only the selected pane may render"
            );
        }
        for pane in SettingsPane::ALL {
            assert!(
                sheet.visual.debug_bounds(pane.nav_selector()).is_some(),
                "every pane must be reachable from the nav"
            );
        }

        Ok(())
    }

    #[test]
    fn vault_controls_render_on_storage_and_privacy() -> Result<(), Box<dyn std::error::Error>> {
        let mut cx = TestAppContext::single();
        let sheet = mount(&mut cx, px(900.0))?;

        for selector in ["choose-vault-folder", "enable-vault"] {
            scroll_into_view(
                sheet.visual,
                SettingsPane::StorageAndPrivacy.pane_selector(),
                selector,
            )?;
        }
        assert!(
            sheet.visual.update(|_, cx| {
                sheet
                    .settings
                    .read(cx)
                    .vault
                    .read(cx)
                    .status()
                    .summary()
                    .contains("Off")
            }),
            "the off-by-default state must be visible through the mounted settings model"
        );
        Ok(())
    }

    /// Selecting a pane renders it, and leaves the panes that were never opened unrendered.
    ///
    /// Each pane gets a **fresh window**, because GPUI's `Frame::clear` does not clear
    /// `debug_bounds`: an entry recorded in one frame survives into later ones, so an `is_none()`
    /// assertion only proves an element has *never* rendered in that window. Storage & privacy is
    /// therefore excluded from the absence check below — it renders on open by definition, and the
    /// test above is what pins that.
    #[test]
    fn selecting_a_pane_renders_that_pane_and_no_other() -> Result<(), Box<dyn std::error::Error>> {
        for pane in [
            SettingsPane::Recording,
            SettingsPane::Transcription,
            SettingsPane::SummariesAndAsk,
        ] {
            let mut cx = TestAppContext::single();
            let sheet = mount(&mut cx, px(900.0))?;
            let settings = sheet.settings.clone();
            sheet.visual.update(|_, cx| {
                settings.update(cx, |this, cx| this.select_pane(pane, cx));
            });
            sheet.visual.refresh()?;
            sheet.visual.run_until_parked();

            assert!(
                sheet.visual.debug_bounds(pane.pane_selector()).is_some(),
                "selecting {} must render it",
                pane.title()
            );
            for other in SettingsPane::ALL {
                if other == pane || other == SettingsPane::StorageAndPrivacy {
                    continue;
                }
                assert!(
                    sheet.visual.debug_bounds(other.pane_selector()).is_none(),
                    "{} must not render while {} is selected",
                    other.title(),
                    pane.title()
                );
            }
        }
        Ok(())
    }

    /// Escape reaches the sheet because the sheet takes focus on open. Without the tracked focus
    /// handle GPUI would dispatch the keystroke to the window root, which has no listener, and the
    /// sheet would silently refuse to close.
    #[test]
    fn escape_dismisses_the_sheet() -> Result<(), Box<dyn std::error::Error>> {
        let mut cx = TestAppContext::single();
        let MountedSheet {
            settings,
            visual,
            directory: _directory,
        } = mount(&mut cx, px(900.0))?;
        let dismissals = watch_dismissals(visual, &settings);
        visual.simulate_keystrokes("escape");
        visual.run_until_parked();
        assert_eq!(
            dismissals.get(),
            1,
            "Escape must ask the workspace to dismiss the settings sheet"
        );
        assert_eq!(
            cx.update(|cx| cx.windows().len()),
            1,
            "dismissing settings must never close a window; the sheet has none of its own"
        );
        Ok(())
    }

    /// The two pointer paths a person actually uses: click a nav entry, click the close glyph.
    #[test]
    fn the_nav_switches_panes_and_close_dismisses_the_sheet()
    -> Result<(), Box<dyn std::error::Error>> {
        let mut cx = TestAppContext::single();
        let MountedSheet {
            settings,
            visual,
            directory: _directory,
        } = mount(&mut cx, px(900.0))?;
        let dismissals = watch_dismissals(visual, &settings);

        let nav = visual
            .debug_bounds(SettingsPane::Transcription.nav_selector())
            .ok_or_else(|| std::io::Error::other("the Transcription nav entry must render"))?;
        visual.simulate_click(nav.center(), Modifiers::none());
        visual.refresh()?;
        visual.run_until_parked();
        assert!(
            visual
                .debug_bounds(SettingsPane::Transcription.pane_selector())
                .is_some(),
            "clicking a nav entry must select its pane"
        );

        let close = visual
            .debug_bounds("settings-close")
            .ok_or_else(|| std::io::Error::other("the close control must render"))?;
        visual.simulate_click(close.center(), Modifiers::none());
        visual.run_until_parked();
        assert_eq!(
            dismissals.get(),
            1,
            "the close control must ask the workspace to dismiss the sheet"
        );
        Ok(())
    }

    /// Delete on a storage row is a picture, so the words have to arrive somewhere. They arrive in
    /// the dialog, which names the row and the megabytes it is about to free.
    #[test]
    fn a_recording_delete_prompt_names_its_target_and_its_size() {
        assert_eq!(
            delete_recording_prompt("Sprint 41 planning", Some("412.0 MB")),
            "Delete the recording for “Sprint 41 planning” and its 412.0 MB? Its transcript and \
             notes stay on this Mac.",
            "the prompt must name the recording and its measured size"
        );
        assert_eq!(
            delete_recording_prompt("Meeting 7", None),
            "Delete the still-growing recording for “Meeting 7”? Its final size is not known yet, \
             and its transcript and notes stay on this Mac.",
            "a still-growing recording must say its size is not known rather than invent one"
        );
    }

    /// The trash icon on a storage row opens a confirm dialog; Cancel keeps the media.
    #[tokio::test(flavor = "multi_thread")]
    async fn deleting_a_recording_asks_in_a_dialog() -> Result<(), Box<dyn std::error::Error>> {
        let mut cx = TestAppContext::single();
        let sheet = mount(&mut cx, px(900.0))?;
        let settings = sheet.settings.clone();
        let session_id = sotto_core::SessionId::new(42);
        let media =
            seed_settled_recording(&sheet.database(), &sheet.recordings(), session_id).await?;
        let visual = sheet.visual;
        visual.update(|_, cx| settings.update(cx, |this, _| this.refresh_recordings()));
        visual.refresh()?;
        visual.run_until_parked();

        let delete = scroll_into_view(
            visual,
            SettingsPane::StorageAndPrivacy.pane_selector(),
            DELETE_RECORDING_SELECTOR,
        )?;
        visual.simulate_click(delete.center(), Modifiers::none());
        visual.refresh()?;
        visual.run_until_parked();

        assert!(
            visual.update(|window, cx| window.has_active_dialog(cx)),
            "the row's trash icon must ask before it removes a file"
        );
        assert!(
            visual.update(|_, cx| settings.read(cx).action_message.is_none()),
            "the prompt belongs to the dialog beside the row, not to the sheet's message strip"
        );

        let cancel = visual
            .debug_bounds(crate::workspace::CONFIRM_CANCEL_SELECTOR)
            .ok_or_else(|| std::io::Error::other("a destructive confirmation must offer Cancel"))?;
        visual.simulate_click(cancel.center(), Modifiers::none());
        visual.refresh()?;
        visual.run_until_parked();
        assert!(
            !visual.update(|window, cx| window.has_active_dialog(cx)),
            "Cancel must dismiss the dialog"
        );
        assert!(media.exists(), "Cancel must leave the recording on disk");
        assert!(
            visual.update(|_, cx| settings.read(cx).action_message.is_none()),
            "Cancel must report nothing, because nothing happened"
        );

        // Escape is the keyboard's Cancel, and it must be just as harmless.
        visual.simulate_click(delete.center(), Modifiers::none());
        visual.refresh()?;
        visual.run_until_parked();
        visual.simulate_keystrokes("escape");
        visual.refresh()?;
        visual.run_until_parked();
        assert!(
            !visual.update(|window, cx| window.has_active_dialog(cx)),
            "Escape must cancel the dialog"
        );
        assert!(media.exists(), "Escape must leave the recording on disk");
        Ok(())
    }

    /// Confirming removes the media the dialog named, and says so.
    #[tokio::test(flavor = "multi_thread")]
    async fn confirming_the_recording_dialog_removes_the_media()
    -> Result<(), Box<dyn std::error::Error>> {
        let mut cx = TestAppContext::single();
        let sheet = mount(&mut cx, px(900.0))?;
        let settings = sheet.settings.clone();
        let session_id = sotto_core::SessionId::new(42);
        let media =
            seed_settled_recording(&sheet.database(), &sheet.recordings(), session_id).await?;
        let visual = sheet.visual;
        visual.update(|_, cx| settings.update(cx, |this, _| this.refresh_recordings()));
        visual.refresh()?;
        visual.run_until_parked();

        let delete = scroll_into_view(
            visual,
            SettingsPane::StorageAndPrivacy.pane_selector(),
            DELETE_RECORDING_SELECTOR,
        )?;
        visual.simulate_click(delete.center(), Modifiers::none());
        visual.refresh()?;
        visual.run_until_parked();
        let ok = visual
            .debug_bounds(crate::workspace::CONFIRM_OK_SELECTOR)
            .ok_or_else(|| std::io::Error::other("a destructive confirmation must offer OK"))?;
        visual.simulate_click(ok.center(), Modifiers::none());
        visual.refresh()?;
        visual.run_until_parked();

        assert!(
            !visual.update(|window, cx| window.has_active_dialog(cx)),
            "confirming must close the dialog"
        );
        assert!(!media.exists(), "confirming must remove the media file");
        assert_eq!(
            visual.update(|_, cx| settings.read(cx).action_message.clone()),
            Some("Recording deleted. The transcript and meeting record were kept.".to_owned()),
            "the sheet must report what the store actually did"
        );
        Ok(())
    }

    /// The stored API key was the one destructive control with nothing in front of it.
    #[test]
    fn deleting_the_stored_api_key_asks_first() -> Result<(), Box<dyn std::error::Error>> {
        assert_eq!(
            DELETE_KEY_PROMPT,
            "Delete the stored OpenAI API key? It is removed from this Mac's Keychain, and OpenAI \
             reasoning stops resolving until you paste a key again. Sotto cannot show you the key \
             it is about to remove, and cannot put it back. Your recordings, transcripts and \
             notes are untouched.",
            "the prompt must name the credential, where it lives, and what stops working"
        );

        let mut cx = TestAppContext::single();
        let sheet = mount(&mut cx, px(900.0))?;
        let settings = sheet.settings.clone();
        let visual = sheet.visual;
        visual.update(|_, cx| {
            settings.update(cx, |this, cx| {
                this.select_pane(SettingsPane::SummariesAndAsk, cx);
            });
        });
        visual.refresh()?;
        visual.run_until_parked();

        let dismissals = watch_dismissals(visual, &settings);
        let delete = scroll_into_view(
            visual,
            SettingsPane::SummariesAndAsk.pane_selector(),
            DELETE_OPENAI_KEY_SELECTOR,
        )?;
        visual.simulate_click(delete.center(), Modifiers::none());
        visual.refresh()?;
        visual.run_until_parked();
        assert!(
            visual.update(|window, cx| window.has_active_dialog(cx)),
            "removing a credential from the Keychain must be confirmed, not assumed"
        );

        visual.simulate_keystrokes("escape");
        visual.refresh()?;
        visual.run_until_parked();
        assert!(
            !visual.update(|window, cx| window.has_active_dialog(cx)),
            "Escape must cancel the key deletion"
        );
        assert_eq!(
            dismissals.get(),
            0,
            "the Escape that cancels the dialog must not also close the sheet behind it"
        );

        // Focus is handed back rather than stranded: the sheet answers Escape again, which it
        // could only do if it were focused.
        visual.simulate_keystrokes("escape");
        visual.run_until_parked();
        assert_eq!(
            dismissals.get(),
            1,
            "cancelling a dialog must return focus to the sheet that raised it"
        );
        Ok(())
    }

    #[test]
    fn narrow_settings_keeps_cards_and_actions_inside_the_sheet()
    -> Result<(), Box<dyn std::error::Error>> {
        let mut cx = TestAppContext::single();
        let minimum_width = px(420.0);
        let sheet = mount(&mut cx, minimum_width)?;

        let page = sheet
            .visual
            .debug_bounds("settings-page")
            .ok_or_else(|| std::io::Error::other("the settings sheet must render"))?;
        let nav = sheet
            .visual
            .debug_bounds("settings-nav")
            .ok_or_else(|| std::io::Error::other("the pane nav must render"))?;
        let card = sheet
            .visual
            .debug_bounds("settings-card")
            .ok_or_else(|| std::io::Error::other("settings card must render"))?;
        let actions = sheet
            .visual
            .debug_bounds("settings-action-row")
            .ok_or_else(|| std::io::Error::other("settings actions must render"))?;
        assert!(page.right() <= minimum_width);
        assert!(nav.left() >= page.left() && nav.right() <= page.right());
        assert!(card.left() >= page.left() && card.right() <= page.right());
        assert!(actions.left() >= card.left() && actions.right() <= card.right());

        // Each pane must survive the same width; a pane that clips is a pane nobody can use.
        let settings = sheet.settings.clone();
        for pane in SettingsPane::ALL {
            let target = pane;
            let handle = settings.clone();
            sheet.visual.update(|_, cx| {
                handle.update(cx, |this, cx| this.select_pane(target, cx));
            });
            sheet.visual.refresh()?;
            sheet.visual.run_until_parked();
            let body = sheet
                .visual
                .debug_bounds(pane.pane_selector())
                .ok_or_else(|| std::io::Error::other("the selected pane must render"))?;
            assert!(
                body.left() >= page.left() && body.right() <= page.right(),
                "{} must stay inside the sheet at the stated minimum width",
                pane.title()
            );
            // Cards stack down the pane. If the pane ever laid out as a row — the failure mode
            // `Scrollable`'s style-lifting invites — each card would take a fraction of the width.
            let card = sheet
                .visual
                .debug_bounds("settings-card")
                .ok_or_else(|| std::io::Error::other("every pane must render at least one row"))?;
            assert!(
                card.size.width >= body.size.width * 0.6,
                "{} must stack its rows in a column, not compete for one line",
                pane.title()
            );
            assert!(
                card.left() >= body.left() && card.right() <= body.right(),
                "{} must keep its rows inside the pane",
                pane.title()
            );
        }
        Ok(())
    }
}
