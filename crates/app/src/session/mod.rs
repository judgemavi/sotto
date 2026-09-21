//! Explicit, picker-scoped map-session lifecycle.

mod import;
mod model;
mod recordings;

pub use import::ImportOutcome;
pub use model::{
    MODEL_CHOICES, ModelAvailability, TranscriptionModel, download_size_label, model_label,
};
pub use recordings::{RecordingLibrary, RecordingLibraryItem, RecordingLibrarySnapshot};

use std::{
    path::{Path, PathBuf},
    sync::{Arc, Mutex, mpsc},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use asr::{
    Config as AsrConfig, LiveRecordingTranscriber, ModelSize, RecordingConfig,
    model::{ModelProvisionError, ModelProvisioner, ProvisionPhase, ProvisionProgress},
};
use capture::macos::{
    CaptureStatus, MacCapture, PickOutcome, PickedMacCapture, PickedTarget, probe_recording,
};
use futures_util::FutureExt;
use gpui::{Context, PathPromptOptions, Timer};
use rag::{Store, TimelinePersistence};
use sotto_core::types::{MediaTimeMapping, RecordingContainer, SessionRecording};
use sotto_core::{
    AudioFrame, CancellationToken, CaptureBackend, CaptureError, CaptureTarget, EntryId, EventId,
    EventPayload, EventReceiver, MarkKind, PermissionStatus, Pipeline, Session, SessionId, Source,
    TargetKind, TimelineEvent, TranscriptUpdate,
};
use vad::{SileroVad, VadConfig};

use crate::devwindow::TimelineIngress;

const SHUTDOWN_DRAIN_TIMEOUT: Duration = Duration::from_secs(2);
const SHUTDOWN_READY_SWEEP_TIMEOUT: Duration = Duration::from_millis(100);
const SHUTDOWN_STOP_TIMEOUT: Duration = Duration::from_secs(2);
const APP_QUIT_WORKER_TIMEOUT: Duration = Duration::from_secs(5);
const LIFECYCLE_POLL_INTERVAL: Duration = Duration::from_millis(50);
const LIFECYCLE_CHANNEL_CAPACITY: usize = 32;
// Stop is not the same as a playable file. ScreenCaptureKit can emit UserStopped immediately,
// while Swift still runs `AVAssetWriter.finishWriting` and then re-encodes the whole AAC
// timeline into a conventional MP4. That remux is what emits CaptureStatus::Stopped, and on a
// real meeting it is minutes, not seconds. Five seconds left 45-minute recordings stranded as
// `growing` with a timeout error even after the MP4 landed. Tests inject a short deadline.
const RECORDING_FINALIZE_TIMEOUT: Duration = Duration::from_secs(10 * 60);
// The writer commits two-second fMP4 segments, so boundary rounding and the final segment flush
// can only move a finalized file two seconds off the wall-clock capture bracket. Five seconds is
// therefore a loose bound rather than a fitted one, and deliberately so: this exists to catch
// mixed-clock corruption, where the error is measured in hours, and tightening it to the segment
// interval would buy nothing while risking rejection of sound recordings. It is not a tolerance
// for picker, capture-start negotiation, shutdown drain, or ASR finalization.
const RECORDING_DURATION_TOLERANCE: Duration = Duration::from_secs(5);

#[derive(Default)]
struct CaptureRunClock {
    started: Option<std::time::Instant>,
}

impl CaptureRunClock {
    fn observe_running(&mut self, observed_at: std::time::Instant) {
        self.started.get_or_insert(observed_at);
    }

    fn elapsed_at(&self, stopped_at: std::time::Instant) -> Option<Duration> {
        self.started
            .map(|started| stopped_at.saturating_duration_since(started))
    }
}

/// The single user-visible map-session lifecycle.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SessionLifecycle {
    Idle {
        last_outcome: Option<String>,
    },
    ChoosingTarget,
    ProvisioningModel {
        target: CaptureTarget,
        model: ModelSize,
        progress: ProvisionProgress,
    },
    Running {
        target: CaptureTarget,
    },
    Stopping {
        target: CaptureTarget,
        was_recording: bool,
    },
    Error(SessionFailure),
}

impl Default for SessionLifecycle {
    fn default() -> Self {
        Self::Idle { last_outcome: None }
    }
}

impl SessionLifecycle {
    #[must_use]
    pub const fn can_start(&self) -> bool {
        matches!(self, Self::Idle { .. } | Self::Error(_))
    }

    #[must_use]
    pub const fn can_stop(&self) -> bool {
        matches!(self, Self::ProvisioningModel { .. } | Self::Running { .. })
    }

    /// Closing the sole control shell must be vetoed while work can still capture or finalize.
    #[must_use]
    pub const fn requires_visible_control(&self) -> bool {
        matches!(
            self,
            Self::ProvisioningModel { .. } | Self::Running { .. } | Self::Stopping { .. }
        )
    }

    #[must_use]
    pub fn status_message(&self) -> String {
        match self {
            Self::Idle { last_outcome } => last_outcome.clone().unwrap_or_else(|| {
                "Capture is stopped. Start a session whenever you are ready; reasoning is optional."
                    .to_owned()
            }),
            Self::ChoosingTarget => {
                "Choose one application, window, or display in the system picker.".to_owned()
            }
            Self::ProvisioningModel {
                model, progress, ..
            } => progress_label(*model, *progress),
            Self::Running { target } if target.is_microphone_only() => {
                "A microphone-only audio recording is being kept on this Mac.".to_owned()
            }
            Self::Running { .. } => {
                "A local audio and screen recording is being kept on this Mac.".to_owned()
            }
            Self::Stopping { was_recording, .. } => {
                if *was_recording {
                    "Stopping capture and finishing the local recording…".to_owned()
                } else {
                    "Cancelling model setup…".to_owned()
                }
            }
            Self::Error(error) => error.actionable_message(),
        }
    }

    #[must_use]
    pub fn indicator(&self) -> Option<RecordingIndicator> {
        match self {
            Self::Running { target } => Some(RecordingIndicator {
                target: target.clone(),
                finalizing: false,
            }),
            Self::Stopping {
                target,
                was_recording: true,
            } => Some(RecordingIndicator {
                target: target.clone(),
                finalizing: true,
            }),
            Self::Idle { .. }
            | Self::ChoosingTarget
            | Self::ProvisioningModel { .. }
            | Self::Stopping {
                was_recording: false,
                ..
            }
            | Self::Error(_) => None,
        }
    }
}

/// The Screen & System Audio Recording outcome Home must state next to Capture.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ScreenPermissionNote {
    pub message: String,
    /// Whether an Open Settings action belongs beside the note. Only a stated denial has
    /// somewhere for that action to send the person; not-determined already has the system
    /// prompt on screen.
    pub open_settings: bool,
}

/// Non-disableable, truthful capture-scope indicator data.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RecordingIndicator {
    target: CaptureTarget,
    finalizing: bool,
}

impl RecordingIndicator {
    #[must_use]
    pub fn label(&self) -> String {
        let prefix = if self.finalizing {
            "Finalizing capture"
        } else {
            "Recording"
        };
        if self.target.is_microphone_only() {
            return format!(
                "{prefix} locally · mic: microphone · screen: off · application audio: off"
            );
        }
        let target = target_label(&self.target);
        let audio = if self.target.audio_scoped {
            match self.target.kind {
                TargetKind::Application | TargetKind::Window => {
                    format!("{} application", self.target.display_name)
                }
                TargetKind::Display => format!("{} display", self.target.display_name),
                TargetKind::Microphone => unreachable!("handled above"),
                // A `RecordingIndicator` exists only for `SessionLifecycle::Running`/`Stopping`,
                // and an import never enters either — it has no live capture to indicate. If a
                // future wiring mistake ever constructed one anyway, panicking here is preferable
                // to silently rendering an audio-scope claim an import cannot support.
                TargetKind::Imported => {
                    unreachable!("an import never becomes a live capture indicator")
                }
            }
        } else {
            "system-wide".to_owned()
        };
        format!("{prefix} locally · mic: microphone · screen: {target} · captured audio: {audio}")
    }
}

/// Stable error categories rendered with distinct recovery guidance.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SessionFailureKind {
    Cancelled,
    OfflineNoCache,
    CorruptModel,
    InvalidOverride,
    ModelSetup,
    Capture,
    /// Screen & System Audio Recording has never been decided; the system prompt was just
    /// requested and is on screen, unanswered.
    ScreenPermissionNotDetermined,
    /// Screen & System Audio Recording was refused; macOS will not prompt again.
    ScreenPermissionDenied,
    Recording,
    Timeline,
    Persistence,
    Worker,
}

/// Actionable lifecycle failure. It never contains credentials or transcript text.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SessionFailure {
    pub kind: SessionFailureKind,
    pub message: String,
}

impl SessionFailure {
    fn new(kind: SessionFailureKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }

    #[cfg(test)]
    fn from_model(error: ModelProvisionError) -> Self {
        let kind = match error {
            ModelProvisionError::Cancelled { .. } => SessionFailureKind::Cancelled,
            ModelProvisionError::OfflineNoCache => SessionFailureKind::OfflineNoCache,
            ModelProvisionError::SizeMismatch { .. }
            | ModelProvisionError::DigestMismatch { .. } => SessionFailureKind::CorruptModel,
            ModelProvisionError::InvalidOverride { .. } => SessionFailureKind::InvalidOverride,
            ModelProvisionError::HomeUnavailable
            | ModelProvisionError::Network(_)
            | ModelProvisionError::Http { .. }
            | ModelProvisionError::InvalidRange(_)
            | ModelProvisionError::Io { .. } => SessionFailureKind::ModelSetup,
        };
        Self::new(kind, error.to_string())
    }

    #[must_use]
    pub fn actionable_message(&self) -> String {
        let action = match self.kind {
            SessionFailureKind::Cancelled => "Model setup was cancelled. Start again when ready.",
            SessionFailureKind::OfflineNoCache => {
                "Connect to the internet and retry, or set SOTTO_WHISPER_MODEL to a local model file."
            }
            SessionFailureKind::CorruptModel => {
                "The model failed integrity verification. Retry to replace it, or set SOTTO_WHISPER_MODEL to a verified local file."
            }
            SessionFailureKind::InvalidOverride => {
                "Fix or remove SOTTO_WHISPER_MODEL, then retry target selection."
            }
            SessionFailureKind::ModelSetup => {
                "Retry model setup. A verified cached model remains usable without a network connection."
            }
            SessionFailureKind::Capture => {
                "Check capture and microphone permissions, then retry target selection."
            }
            SessionFailureKind::ScreenPermissionNotDetermined => {
                "Approve the system prompt, then quit and reopen Sotto — the grant only takes effect for a newly launched process."
            }
            SessionFailureKind::ScreenPermissionDenied => {
                "If a system prompt just appeared, approve it. If not, macOS has already been asked and will not ask again — open Settings to grant Screen & System Audio Recording. Either way, quit and reopen Sotto afterwards."
            }
            SessionFailureKind::Recording => {
                "Check available storage and retry. If this repeats, restart Sotto and report the recording detail shown above."
            }
            SessionFailureKind::Timeline => {
                "Capture stopped before more invalid timeline data could be shown. Restart Sotto and report this clock detail."
            }
            SessionFailureKind::Persistence => {
                "Check access to Sotto's Application Support directory, then retry."
            }
            SessionFailureKind::Worker => "Retry. If this repeats, restart Sotto.",
        };
        format!("{} {action}", self.message)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum SessionEnd {
    UserRequested,
    Capture(CaptureStatus),
    RecordingFailed(String),
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct SessionCompletion {
    end: SessionEnd,
    session_id: SessionId,
    persisted_events: usize,
    persisted_tail: Option<EventId>,
    recording_discrepancy: Option<String>,
}

enum WorkerEvent {
    #[cfg(test)]
    Progress(ProvisionProgress),
    Identified(SessionId),
    Running,
    Finalizing,
    Finished(Result<SessionCompletion, SessionFailure>),
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
enum StartGateState {
    #[default]
    Preparing,
    Started,
    Cancelled,
}

/// Coordinates Stop with the exact capture-start boundary across the GPUI and worker threads.
#[derive(Clone)]
struct StartGate {
    state: Arc<Mutex<StartGateState>>,
    cancellation: CancellationToken,
}

impl StartGate {
    fn new() -> Self {
        Self {
            state: Arc::new(Mutex::new(StartGateState::Preparing)),
            cancellation: CancellationToken::new(),
        }
    }

    const fn cancellation(&self) -> &CancellationToken {
        &self.cancellation
    }

    /// Holds the Stop boundary across the synchronous OS-capture start side effect.
    #[cfg(test)]
    fn start_if_not_cancelled<T, E>(
        &self,
        start: impl FnOnce() -> Result<T, E>,
    ) -> Result<Option<T>, E> {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        match *state {
            StartGateState::Preparing => {
                let started = start()?;
                *state = StartGateState::Started;
                Ok(Some(started))
            }
            StartGateState::Started => Ok(None),
            StartGateState::Cancelled => Ok(None),
        }
    }

    #[expect(
        clippy::await_holding_lock,
        reason = "the gate mutex deliberately makes cancellation and the async capture-start boundary atomic"
    )]
    async fn start_if_not_cancelled_async<T, E, F>(
        &self,
        start: impl FnOnce() -> F,
    ) -> Result<Option<T>, E>
    where
        F: std::future::Future<Output = Result<T, E>>,
    {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        match *state {
            StartGateState::Preparing => {
                let started = start().await?;
                *state = StartGateState::Started;
                Ok(Some(started))
            }
            StartGateState::Started | StartGateState::Cancelled => Ok(None),
        }
    }

    /// Cancels setup/capture and reports whether the worker had crossed the live boundary.
    fn cancel(&self) -> bool {
        let was_started = {
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let was_started = *state == StartGateState::Started;
            *state = StartGateState::Cancelled;
            was_started
        };
        self.cancellation.cancel();
        was_started
    }
}

#[derive(Default)]
struct LifecycleModel {
    state: SessionLifecycle,
    generation: u64,
    start_gate: Option<StartGate>,
}

impl LifecycleModel {
    fn begin(&mut self) -> Option<u64> {
        if !self.state.can_start() {
            return None;
        }
        self.generation = self.generation.saturating_add(1);
        self.start_gate = None;
        self.state = SessionLifecycle::ChoosingTarget;
        Some(self.generation)
    }

    fn picker_cancelled(&mut self, generation: u64) {
        if generation == self.generation && matches!(self.state, SessionLifecycle::ChoosingTarget) {
            self.state = SessionLifecycle::Idle {
                last_outcome: Some(
                    "Target selection cancelled. No session or model setup was started.".to_owned(),
                ),
            };
        }
    }

    /// Screen & System Audio Recording has never been decided. The prompt was just requested as
    /// a side effect of the picker attempt and is now on screen; this must not read as an idle
    /// cancellation, because nothing was cancelled — a decision is pending.
    fn permission_not_determined(&mut self, generation: u64) {
        self.enter_permission_failure(
            generation,
            SessionFailureKind::ScreenPermissionNotDetermined,
            "Screen & System Audio Recording has not been decided yet.",
        );
    }

    /// Screen & System Audio Recording was refused. macOS will not prompt again for this app
    /// identity, so the only way forward is Settings plus a relaunch.
    fn permission_denied(&mut self, generation: u64) {
        self.enter_permission_failure(
            generation,
            SessionFailureKind::ScreenPermissionDenied,
            "Screen & System Audio Recording is off for Sotto.",
        );
    }

    fn enter_permission_failure(
        &mut self,
        generation: u64,
        kind: SessionFailureKind,
        message: &str,
    ) {
        if generation == self.generation && matches!(self.state, SessionLifecycle::ChoosingTarget) {
            self.state = SessionLifecycle::Error(SessionFailure::new(kind, message));
        }
    }

    fn target_picked(
        &mut self,
        generation: u64,
        target: CaptureTarget,
        model: ModelSize,
        start_gate: StartGate,
    ) -> bool {
        if generation != self.generation || !matches!(self.state, SessionLifecycle::ChoosingTarget)
        {
            return false;
        }
        let spec = model.spec();
        self.start_gate = Some(start_gate);
        self.state = SessionLifecycle::ProvisioningModel {
            target,
            model,
            progress: ProvisionProgress {
                phase: ProvisionPhase::Resolving,
                downloaded: 0,
                total: spec.byte_len,
            },
        };
        true
    }

    fn request_stop(&mut self) -> bool {
        let (target, state_was_recording) = match &self.state {
            SessionLifecycle::ProvisioningModel { target, .. } => (target.clone(), false),
            SessionLifecycle::Running { target } => (target.clone(), true),
            SessionLifecycle::Idle { .. }
            | SessionLifecycle::ChoosingTarget
            | SessionLifecycle::Stopping { .. }
            | SessionLifecycle::Error(_) => return false,
        };
        let gate_was_started = self.start_gate.as_ref().is_some_and(StartGate::cancel);
        self.state = SessionLifecycle::Stopping {
            target,
            was_recording: state_was_recording || gate_was_started,
        };
        true
    }

    fn apply(&mut self, generation: u64, event: WorkerEvent) {
        if generation != self.generation {
            return;
        }
        match event {
            #[cfg(test)]
            WorkerEvent::Progress(progress) => {
                if let SessionLifecycle::ProvisioningModel {
                    progress: current, ..
                } = &mut self.state
                {
                    *current = progress;
                }
            }
            WorkerEvent::Identified(_) => {}
            WorkerEvent::Running => {
                if let SessionLifecycle::ProvisioningModel { target, .. } = &self.state {
                    self.state = SessionLifecycle::Running {
                        target: target.clone(),
                    };
                }
            }
            WorkerEvent::Finalizing => {
                if let SessionLifecycle::Running { target } = &self.state {
                    self.state = SessionLifecycle::Stopping {
                        target: target.clone(),
                        was_recording: true,
                    };
                }
            }
            WorkerEvent::Finished(result) => {
                self.start_gate = None;
                self.state = finish_state(result);
            }
        }
    }

    fn worker_disconnected(&mut self, generation: u64) {
        if generation == self.generation
            && !matches!(
                self.state,
                SessionLifecycle::Idle { .. } | SessionLifecycle::Error(_)
            )
        {
            self.start_gate = None;
            self.state = SessionLifecycle::Error(SessionFailure::new(
                SessionFailureKind::Worker,
                "The session worker stopped before reporting a final state.",
            ));
        }
    }
}

/// App-owned controller. Settings render this entity; they do not own a second session state.
pub struct SessionController {
    ingress: TimelineIngress,
    model: LifecycleModel,
    transcription_model: TranscriptionModel,
    model_operation_generation: u64,
    model_download_cancellation: Option<CancellationToken>,
    worker: Option<WorkerThread>,
    active_session_id: Option<SessionId>,
    completed_session_id: Option<SessionId>,
    next_entry_id: Option<EntryId>,
    #[cfg(test)]
    last_requested_entry_id: Option<EntryId>,
    annotation_sender: Option<tokio::sync::mpsc::UnboundedSender<AnnotationRequest>>,
    /// Whether a file is currently being imported. Deliberately not part of [`SessionLifecycle`]:
    /// an import captures nothing live, holds no [`StartGate`], and reusing `Running`'s scope-bound
    /// vocabulary for it would put a capture-target claim on the indicator where none exists.
    importing: bool,
    /// The stated reason the most recent import did not become a session, if it did not. Cleared by
    /// [`Self::take_import_error`] so the same failure cannot be shown twice.
    import_error: Option<String>,
}

struct AnnotationRequest {
    anchor: EventId,
    target: Option<EventId>,
    text: String,
    mark: MarkKind,
}

struct WorkerThread {
    handle: std::thread::JoinHandle<()>,
    done: mpsc::Receiver<()>,
    app_shutdown: Arc<std::sync::atomic::AtomicBool>,
}

enum CaptureSelection {
    Scoped(PickedTarget),
    MicrophoneOnly,
}

enum ModelDownloadEvent {
    Progress(ProvisionProgress),
    Finished(ModelDownloadResult),
}

enum ModelDownloadResult {
    Ready(PathBuf),
    Cancelled,
    Failed(String),
}

impl CaptureSelection {
    fn description(&self) -> CaptureTarget {
        match self {
            Self::Scoped(target) => target.description().clone(),
            Self::MicrophoneOnly => CaptureTarget::microphone_only(),
        }
    }

    fn into_capture(self) -> ActiveCapture {
        match self {
            Self::Scoped(target) => ActiveCapture::Scoped(target.into_capture()),
            Self::MicrophoneOnly => ActiveCapture::MicrophoneOnly(MacCapture::microphone_only()),
        }
    }
}

enum ActiveCapture {
    Scoped(PickedMacCapture),
    MicrophoneOnly(MacCapture),
}

impl ActiveCapture {
    fn record_to(&mut self, path: &Path) -> Result<(), CaptureError> {
        match self {
            Self::Scoped(capture) => capture.record_to(path),
            Self::MicrophoneOnly(capture) => capture.record_to(path),
        }
    }

    fn subscribe_errors(&self) -> tokio::sync::broadcast::Receiver<CaptureError> {
        match self {
            Self::Scoped(capture) => capture.subscribe_errors(),
            Self::MicrophoneOnly(capture) => capture.subscribe_errors(),
        }
    }

    fn subscribe_status(&self) -> tokio::sync::broadcast::Receiver<CaptureStatus> {
        match self {
            Self::Scoped(capture) => capture.subscribe_status(),
            Self::MicrophoneOnly(capture) => capture.subscribe_status(),
        }
    }
}

impl CaptureBackend for ActiveCapture {
    fn start(
        &mut self,
        sink: tokio::sync::broadcast::Sender<AudioFrame>,
    ) -> Result<(), CaptureError> {
        match self {
            Self::Scoped(capture) => capture.start(sink),
            Self::MicrophoneOnly(capture) => capture.start(sink),
        }
    }

    fn stop(&mut self) {
        match self {
            Self::Scoped(capture) => capture.stop(),
            Self::MicrophoneOnly(capture) => capture.stop(),
        }
    }

    fn permission_status(&self) -> PermissionStatus {
        match self {
            Self::Scoped(capture) => capture.permission_status(),
            Self::MicrophoneOnly(capture) => capture.permission_status(),
        }
    }
}

impl SessionController {
    #[must_use]
    pub fn new(ingress: TimelineIngress) -> Self {
        Self {
            ingress,
            model: LifecycleModel::default(),
            transcription_model: TranscriptionModel::load_default(),
            model_operation_generation: 0,
            model_download_cancellation: None,
            worker: None,
            active_session_id: None,
            completed_session_id: None,
            next_entry_id: None,
            #[cfg(test)]
            last_requested_entry_id: None,
            annotation_sender: None,
            importing: false,
            import_error: None,
        }
    }

    #[must_use]
    pub const fn lifecycle(&self) -> &SessionLifecycle {
        &self.model.state
    }

    #[must_use]
    pub const fn transcription_model(&self) -> &TranscriptionModel {
        &self.transcription_model
    }

    /// The in-place reason start, import, and re-transcribe must present while Whisper is absent.
    #[must_use]
    pub fn transcription_unavailability(&self) -> Option<String> {
        self.transcription_model.unavailable_reason()
    }

    /// The same refusal, worded for a card that sits under the model choice itself.
    #[must_use]
    pub fn transcription_home_reason(&self) -> Option<String> {
        self.transcription_model.home_card_reason()
    }

    /// The stated Screen & System Audio Recording outcome, when the last picker attempt could not
    /// even present the picker. `None` covers every other lifecycle, including a quiet picker
    /// cancellation — the one case that is correctly silent.
    #[must_use]
    pub fn screen_permission_note(&self) -> Option<ScreenPermissionNote> {
        let SessionLifecycle::Error(failure) = &self.model.state else {
            return None;
        };
        match failure.kind {
            SessionFailureKind::ScreenPermissionNotDetermined => Some(ScreenPermissionNote {
                message: failure.actionable_message(),
                open_settings: false,
            }),
            SessionFailureKind::ScreenPermissionDenied => Some(ScreenPermissionNote {
                message: failure.actionable_message(),
                open_settings: true,
            }),
            _ => None,
        }
    }

    /// Verifies the persisted choice away from GPUI's launch thread and never contacts a server.
    pub fn begin_launch_model_check(&mut self, cx: &mut Context<Self>) {
        self.model_operation_generation = self.model_operation_generation.saturating_add(1);
        let generation = self.model_operation_generation;
        let state = self.transcription_model.clone();
        let (sender, receiver) = mpsc::sync_channel(1);
        let spawn = std::thread::Builder::new()
            .name("sotto-model-check".to_owned())
            .spawn(move || {
                let _ = sender.send(state.inspect());
            });
        if let Err(error) = spawn {
            self.transcription_model
                .set_error(format!("Could not check the selected model: {error}"));
            cx.notify();
            return;
        }
        let controller = cx.entity();
        cx.spawn(async move |_, cx| {
            loop {
                Timer::after(LIFECYCLE_POLL_INTERVAL).await;
                match receiver.try_recv() {
                    Ok(availability) => {
                        let _ = controller.update(cx, |controller, cx| {
                            if controller.model_operation_is_current(generation) {
                                controller
                                    .transcription_model
                                    .set_availability(availability);
                                cx.notify();
                            }
                        });
                        return;
                    }
                    Err(mpsc::TryRecvError::Empty) => {}
                    Err(mpsc::TryRecvError::Disconnected) => return,
                }
            }
        })
        .detach();
    }

    /// Persists the user's model choice, then downloads exactly that artifact with live progress.
    pub fn choose_transcription_model(&mut self, selected: ModelSize, cx: &mut Context<Self>) {
        self.cancel_model_download_inner();
        self.model_operation_generation = self.model_operation_generation.saturating_add(1);
        let generation = self.model_operation_generation;
        if let Err(error) = self.transcription_model.choose(selected) {
            self.transcription_model.set_error(error);
            cx.notify();
            return;
        }
        if self.transcription_model.ready_path().is_some() {
            cx.notify();
            return;
        }
        let spec = selected.spec();
        let cancellation = CancellationToken::new();
        self.model_download_cancellation = Some(cancellation.clone());
        self.transcription_model
            .set_provisioning(ProvisionProgress {
                phase: ProvisionPhase::Resolving,
                downloaded: 0,
                total: spec.byte_len,
            });
        let (sender, receiver) = mpsc::sync_channel(LIFECYCLE_CHANNEL_CAPACITY);
        let spawn = std::thread::Builder::new()
            .name("sotto-model-download".to_owned())
            .spawn(move || {
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .map_err(|error| format!("Could not start model setup: {error}"));
                let result = match runtime {
                    Err(error) => ModelDownloadResult::Failed(error),
                    Ok(runtime) => match ModelProvisioner::for_current_user() {
                        Err(error) => ModelDownloadResult::Failed(error.to_string()),
                        Ok(provisioner) => {
                            match runtime.block_on(provisioner.resolve_configured_or_download(
                                selected,
                                &cancellation,
                                |progress| {
                                    let _ = sender.try_send(ModelDownloadEvent::Progress(progress));
                                },
                            )) {
                                Ok(path) => ModelDownloadResult::Ready(path),
                                Err(ModelProvisionError::Cancelled { .. }) => {
                                    ModelDownloadResult::Cancelled
                                }
                                Err(error) => ModelDownloadResult::Failed(error.to_string()),
                            }
                        }
                    },
                };
                let _ = sender.send(ModelDownloadEvent::Finished(result));
            });
        if let Err(error) = spawn {
            self.transcription_model
                .set_error(format!("Could not start model setup: {error}"));
            cx.notify();
            return;
        }
        let controller = cx.entity();
        cx.spawn(async move |_, cx| {
            loop {
                Timer::after(LIFECYCLE_POLL_INTERVAL).await;
                match receiver.try_recv() {
                    Ok(ModelDownloadEvent::Progress(progress)) => {
                        let _ = controller.update(cx, |controller, cx| {
                            if controller.model_operation_is_current(generation) {
                                controller.transcription_model.set_provisioning(progress);
                                cx.notify();
                            }
                        });
                    }
                    Ok(ModelDownloadEvent::Finished(result)) => {
                        let _ = controller.update(cx, |controller, cx| {
                            if !controller.model_operation_is_current(generation) {
                                return;
                            }
                            controller.model_download_cancellation = None;
                            match result {
                                ModelDownloadResult::Ready(path) => {
                                    controller.transcription_model.set_ready(path)
                                }
                                ModelDownloadResult::Cancelled => {
                                    controller.transcription_model.mark_missing()
                                }
                                ModelDownloadResult::Failed(error) => {
                                    controller.transcription_model.set_error(error)
                                }
                            }
                            cx.notify();
                        });
                        return;
                    }
                    Err(mpsc::TryRecvError::Empty) => {}
                    Err(mpsc::TryRecvError::Disconnected) => return,
                }
            }
        })
        .detach();
        cx.notify();
    }

    /// Cancels only model setup. The provisioner retains its `.partial` file for the next choice.
    pub fn cancel_model_download(&mut self, cx: &mut Context<Self>) {
        self.cancel_model_download_inner();
        self.model_operation_generation = self.model_operation_generation.saturating_add(1);
        self.transcription_model.mark_missing();
        cx.notify();
    }

    fn cancel_model_download_inner(&mut self) {
        if let Some(cancellation) = self.model_download_cancellation.take() {
            cancellation.cancel();
        }
    }

    const fn model_operation_is_current(&self, generation: u64) -> bool {
        self.model_operation_generation == generation
    }

    #[cfg(test)]
    pub(crate) fn set_lifecycle_for_test(&mut self, lifecycle: SessionLifecycle) {
        self.model.state = lifecycle;
    }

    #[cfg(test)]
    pub(crate) fn set_transcription_availability_for_test(
        &mut self,
        availability: ModelAvailability,
    ) {
        self.transcription_model.set_availability(availability);
    }

    #[cfg(test)]
    pub(crate) fn set_completed_for_test(&mut self, completed: Option<SessionId>) {
        self.completed_session_id = completed;
    }

    #[cfg(test)]
    pub(crate) const fn pending_entry_for_test(&self) -> Option<EntryId> {
        self.last_requested_entry_id
    }

    /// Exact durable meeting identity, published only after finalization and reload succeed.
    #[must_use]
    pub const fn completed_session_id(&self) -> Option<SessionId> {
        self.completed_session_id
    }

    /// Identity emitted by the worker before any event from this session can enter the UI seam.
    #[must_use]
    pub const fn active_session_id(&self) -> Option<SessionId> {
        self.active_session_id
    }

    /// Starts only the system picker. No model or session work happens until selection succeeds.
    pub fn start(&mut self, cx: &mut Context<Self>) {
        self.reap_finished_worker();
        if self.transcription_model.ready_path().is_none() {
            self.next_entry_id = None;
            cx.notify();
            return;
        }
        let Some(generation) = self.model.begin() else {
            self.next_entry_id = None;
            return;
        };
        let controller = cx.entity();
        cx.spawn(async move |_, cx| {
            let outcome = MacCapture::pick_target().await;
            let _ = controller.update(cx, |controller, cx| match outcome {
                PickOutcome::Picked(target) => {
                    controller.begin_worker(generation, CaptureSelection::Scoped(target), cx)
                }
                PickOutcome::Cancelled => {
                    controller.next_entry_id = None;
                    controller.model.picker_cancelled(generation);
                    cx.notify();
                }
                PickOutcome::NotDetermined => {
                    controller.next_entry_id = None;
                    controller.model.permission_not_determined(generation);
                    cx.notify();
                }
                PickOutcome::Denied => {
                    controller.next_entry_id = None;
                    controller.model.permission_denied(generation);
                    cx.notify();
                }
            });
        })
        .detach();
        cx.notify();
    }

    /// Starts a scoped recording which must land on an existing entry.
    pub fn start_in_entry(&mut self, entry_id: EntryId, cx: &mut Context<Self>) {
        #[cfg(test)]
        {
            self.last_requested_entry_id = Some(entry_id);
        }
        self.next_entry_id = Some(entry_id);
        self.start(cx);
    }

    /// Starts a local voice-note session without presenting the system picker or querying
    /// Screen & System Audio Recording permission.
    pub fn start_microphone_only(&mut self, cx: &mut Context<Self>) {
        self.reap_finished_worker();
        if self.transcription_model.ready_path().is_none() {
            self.next_entry_id = None;
            cx.notify();
            return;
        }
        let Some(generation) = self.model.begin() else {
            self.next_entry_id = None;
            return;
        };
        self.begin_worker(generation, CaptureSelection::MicrophoneOnly, cx);
    }

    /// Starts a microphone-only recording which must land on an existing entry.
    pub fn start_microphone_only_in_entry(&mut self, entry_id: EntryId, cx: &mut Context<Self>) {
        #[cfg(test)]
        {
            self.last_requested_entry_id = Some(entry_id);
        }
        self.next_entry_id = Some(entry_id);
        self.start_microphone_only(cx);
    }

    /// Whether a file is currently being turned into a session. Guards against a second import
    /// starting while one is already running; unlike capture, an import never blocks on
    /// [`SessionLifecycle`], so this is the only guard against overlap.
    #[must_use]
    pub const fn is_importing(&self) -> bool {
        self.importing
    }

    /// Takes the stated reason the most recent import did not become a session, if any.
    ///
    /// `take` rather than `read`: the caller shows it once and the state is consumed, the same
    /// shape [`Self::completed_session_id`]'s sibling `active_session_id`/`completed_session_id`
    /// pair already uses to avoid re-showing a stale outcome.
    pub fn take_import_error(&mut self) -> Option<String> {
        self.import_error.take()
    }

    /// Presents the OS file picker and, on a selection, imports the chosen file as a new session on
    /// a background thread.
    ///
    /// This deliberately does not go through [`Self::begin_worker`]/[`SessionLifecycle`]: an import
    /// captures nothing live, so it never legitimately claims `Running`, holds no [`StartGate`], and
    /// does not contend with a live capture's worker slot. It reaches the same destination a
    /// finished capture does — [`Self::completed_session_id`] set, then `cx.notify()` — so the
    /// workspace's existing `refresh_after_session` opens the imported session exactly the way it
    /// opens a freshly captured one, with no separate wiring for import to duplicate.
    pub fn start_import(&mut self, cx: &mut Context<Self>) {
        let Some(model_path) = self.transcription_model.ready_path().map(Path::to_path_buf) else {
            cx.notify();
            return;
        };
        if self.importing {
            return;
        }
        let path_receiver = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some("Import".into()),
        });
        self.importing = true;
        self.import_error = None;
        let controller = cx.entity();
        cx.spawn(async move |_, cx| {
            let picked = match path_receiver.await {
                Ok(Ok(Some(mut paths))) if !paths.is_empty() => Some(paths.remove(0)),
                _ => None,
            };
            let Some(source) = picked else {
                let _ = controller.update(cx, |controller, cx| {
                    controller.importing = false;
                    cx.notify();
                });
                return;
            };
            let database = application_database_path();
            let recording_directory = application_recording_directory();
            let (result_sender, result_receiver) = mpsc::sync_channel(1);
            let spawn = std::thread::Builder::new()
                .name("sotto-import".to_owned())
                .spawn(move || {
                    let outcome = tokio::runtime::Builder::new_current_thread()
                        .enable_all()
                        .build()
                        .map_err(|error| format!("Could not start the import runtime: {error}"))
                        .map(|runtime| {
                            runtime.block_on(import::import_recording(
                                &source,
                                &database,
                                &recording_directory,
                                &model_path,
                            ))
                        })
                        .and_then(|result| result);
                    let _ = result_sender.send(outcome);
                });
            if let Err(error) = spawn {
                let _ = controller.update(cx, |controller, cx| {
                    controller.importing = false;
                    controller.import_error = Some(format!("Could not start the import: {error}"));
                    cx.notify();
                });
                return;
            }
            loop {
                Timer::after(LIFECYCLE_POLL_INTERVAL).await;
                match result_receiver.try_recv() {
                    Ok(outcome) => {
                        let _ = controller.update(cx, |controller, cx| {
                            controller.importing = false;
                            match outcome {
                                Ok(outcome) => {
                                    controller.completed_session_id = Some(outcome.session_id);
                                }
                                Err(error) => controller.import_error = Some(error),
                            }
                            cx.notify();
                        });
                        return;
                    }
                    Err(mpsc::TryRecvError::Empty) => {}
                    Err(mpsc::TryRecvError::Disconnected) => {
                        let _ = controller.update(cx, |controller, cx| {
                            controller.importing = false;
                            controller.import_error =
                                Some("The import ended unexpectedly.".to_owned());
                            cx.notify();
                        });
                        return;
                    }
                }
            }
        })
        .detach();
        cx.notify();
    }

    /// Cancels provisioning or requests bounded capture shutdown. It never starts another run.
    pub fn stop(&mut self, cx: &mut Context<Self>) {
        if self.model.request_stop() {
            cx.notify();
        }
    }

    /// Appends user-authored text only to the currently running session. Past-session review is
    /// deliberately read-only, and the pipeline actor remains the sole event-id allocator.
    pub fn append_user_annotation(
        &self,
        anchor: EventId,
        text: String,
        mark: MarkKind,
    ) -> Result<(), String> {
        if self.active_session_id.is_none() {
            return Err("Typed notes can only be added to the live meeting.".to_owned());
        }
        let sender = self
            .annotation_sender
            .as_ref()
            .ok_or_else(|| "The live meeting is not ready to accept typed notes yet.".to_owned())?;
        sender
            .send(AnnotationRequest {
                anchor,
                target: None,
                text,
                mark,
            })
            .map_err(|_| {
                "The live meeting stopped before the typed note could be added.".to_owned()
            })
    }

    pub fn supersede_user_annotation(
        &self,
        target: EventId,
        text: String,
        mark: MarkKind,
    ) -> Result<(), String> {
        if self.active_session_id.is_none() {
            return Err("Typed notes can only be edited during the live meeting.".to_owned());
        }
        self.annotation_sender
            .as_ref()
            .ok_or_else(|| "The live meeting is not ready to edit typed notes yet.".to_owned())?
            .send(AnnotationRequest {
                anchor: target,
                target: Some(target),
                text,
                mark,
            })
            .map_err(|_| {
                "The live meeting stopped before the typed note edit was saved.".to_owned()
            })
    }

    fn begin_worker(
        &mut self,
        generation: u64,
        selection: CaptureSelection,
        cx: &mut Context<Self>,
    ) {
        let entry_id = self.next_entry_id.take();
        let target = selection.description();
        let Some(model_path) = self.transcription_model.ready_path().map(Path::to_path_buf) else {
            return;
        };
        let selected_model = self.transcription_model.selected();
        let start_gate = StartGate::new();
        if !self
            .model
            .target_picked(generation, target, selected_model, start_gate.clone())
        {
            return;
        }
        let (sender, receiver) = mpsc::sync_channel(LIFECYCLE_CHANNEL_CAPACITY);
        let (annotation_sender, annotation_receiver) = tokio::sync::mpsc::unbounded_channel();
        self.annotation_sender = Some(annotation_sender);
        let ingress = self.ingress.clone();
        let (done_sender, done_receiver) = mpsc::sync_channel(1);
        let app_shutdown = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let worker_shutdown = Arc::clone(&app_shutdown);
        let spawn = std::thread::Builder::new()
            .name("sotto-map-session".to_owned())
            .spawn(move || {
                run_worker(WorkerRun {
                    target: selection,
                    entry_id,
                    ingress,
                    start_gate,
                    model_path,
                    sender,
                    annotation_receiver,
                    app_shutdown: worker_shutdown,
                });
                let _ = done_sender.send(());
            });
        let handle = match spawn {
            Ok(handle) => handle,
            Err(error) => {
                self.model.apply(
                    generation,
                    WorkerEvent::Finished(Err(SessionFailure::new(
                        SessionFailureKind::Worker,
                        format!("Could not start the session worker: {error}"),
                    ))),
                );
                self.annotation_sender = None;
                cx.notify();
                return;
            }
        };
        self.worker = Some(WorkerThread {
            handle,
            done: done_receiver,
            app_shutdown,
        });

        let controller = cx.entity();
        cx.spawn(async move |_, cx| {
            loop {
                Timer::after(LIFECYCLE_POLL_INTERVAL).await;
                loop {
                    match receiver.try_recv() {
                        Ok(event) => {
                            let finished = matches!(event, WorkerEvent::Finished(_));
                            if controller
                                .update(cx, |controller, cx| {
                                    if generation == controller.model.generation {
                                        if let WorkerEvent::Identified(session_id) = &event {
                                            controller.active_session_id = Some(*session_id);
                                        }
                                        if let WorkerEvent::Finished(Ok(completion)) = &event {
                                            controller.completed_session_id =
                                                Some(completion.session_id);
                                        }
                                        if matches!(event, WorkerEvent::Finished(_)) {
                                            controller.active_session_id = None;
                                            controller.annotation_sender = None;
                                        }
                                    }
                                    controller.model.apply(generation, event);
                                    cx.notify();
                                })
                                .is_err()
                            {
                                return;
                            }
                            if finished {
                                return;
                            }
                        }
                        Err(mpsc::TryRecvError::Empty) => break,
                        Err(mpsc::TryRecvError::Disconnected) => {
                            let _ = controller.update(cx, |controller, cx| {
                                if generation == controller.model.generation {
                                    controller.active_session_id = None;
                                    controller.annotation_sender = None;
                                }
                                controller.model.worker_disconnected(generation);
                                cx.notify();
                            });
                            return;
                        }
                    }
                }
            }
        })
        .detach();
        cx.notify();
    }

    fn reap_finished_worker(&mut self) {
        if self
            .worker
            .as_ref()
            .is_some_and(|worker| worker.handle.is_finished())
            && let Some(worker) = self.worker.take()
        {
            let _ = worker.handle.join();
        }
    }

    /// GPUI's quit hook allows only 100 ms for returned futures, so perform the bounded worker
    /// join synchronously while the quit callback itself is being constructed.
    pub fn shutdown_for_app_quit(&mut self) {
        self.cancel_model_download_inner();
        if let Some(start_gate) = &self.model.start_gate {
            start_gate.cancel();
        }
        let Some(worker) = self.worker.take() else {
            return;
        };
        worker
            .app_shutdown
            .store(true, std::sync::atomic::Ordering::Release);
        if worker.done.recv_timeout(APP_QUIT_WORKER_TIMEOUT).is_ok() {
            let _ = worker.handle.join();
        } else {
            eprintln!(
                "Session worker did not finish within {:?} during app quit",
                APP_QUIT_WORKER_TIMEOUT
            );
        }
    }
}

impl Drop for SessionController {
    fn drop(&mut self) {
        self.cancel_model_download_inner();
        if let Some(start_gate) = &self.model.start_gate {
            start_gate.cancel();
        }
        self.reap_finished_worker();
    }
}

struct WorkerRun {
    target: CaptureSelection,
    entry_id: Option<EntryId>,
    ingress: TimelineIngress,
    start_gate: StartGate,
    model_path: PathBuf,
    sender: mpsc::SyncSender<WorkerEvent>,
    annotation_receiver: tokio::sync::mpsc::UnboundedReceiver<AnnotationRequest>,
    app_shutdown: Arc<std::sync::atomic::AtomicBool>,
}

fn run_worker(run: WorkerRun) {
    let WorkerRun {
        target,
        entry_id,
        ingress,
        start_gate,
        model_path,
        sender,
        annotation_receiver,
        app_shutdown,
    } = run;
    let result = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| {
            SessionFailure::new(
                SessionFailureKind::Worker,
                format!("Could not create the session runtime: {error}"),
            )
        })
        .and_then(|runtime| {
            runtime.block_on(resolve_and_run(
                target,
                entry_id,
                model_path,
                ingress,
                &start_gate,
                &sender,
                annotation_receiver,
            ))
        });
    send_finished(&sender, &app_shutdown, WorkerEvent::Finished(result));
}

fn send_finished(
    sender: &mpsc::SyncSender<WorkerEvent>,
    app_shutdown: &std::sync::atomic::AtomicBool,
    mut event: WorkerEvent,
) {
    loop {
        match sender.try_send(event) {
            Ok(()) | Err(mpsc::TrySendError::Disconnected(_)) => return,
            Err(mpsc::TrySendError::Full(returned)) => {
                if app_shutdown.load(std::sync::atomic::Ordering::Acquire) {
                    return;
                }
                event = returned;
                std::thread::park_timeout(Duration::from_millis(10));
            }
        }
    }
}

async fn resolve_and_run(
    target: CaptureSelection,
    entry_id: Option<EntryId>,
    model_path: PathBuf,
    ingress: TimelineIngress,
    start_gate: &StartGate,
    sender: &mpsc::SyncSender<WorkerEvent>,
    annotation_receiver: tokio::sync::mpsc::UnboundedReceiver<AnnotationRequest>,
) -> Result<SessionCompletion, SessionFailure> {
    ensure_not_cancelled(start_gate.cancellation())?;
    Box::pin(run(
        target,
        entry_id,
        &model_path,
        ingress,
        start_gate,
        sender,
        annotation_receiver,
    ))
    .await
}

#[cfg(test)]
fn send_phase_progress(
    sender: &mpsc::SyncSender<WorkerEvent>,
    cancellation: &CancellationToken,
    progress: ProvisionProgress,
) {
    let mut event = WorkerEvent::Progress(progress);
    loop {
        match sender.try_send(event) {
            Ok(()) | Err(mpsc::TrySendError::Disconnected(_)) => return,
            Err(mpsc::TrySendError::Full(returned)) => {
                if cancellation.is_cancelled() {
                    return;
                }
                event = returned;
                std::thread::park_timeout(Duration::from_millis(10));
            }
        }
    }
}

async fn run(
    target: CaptureSelection,
    entry_id: Option<EntryId>,
    model_path: &Path,
    ingress: TimelineIngress,
    start_gate: &StartGate,
    sender: &mpsc::SyncSender<WorkerEvent>,
    mut annotation_receiver: tokio::sync::mpsc::UnboundedReceiver<AnnotationRequest>,
) -> Result<SessionCompletion, SessionFailure> {
    ensure_not_cancelled(start_gate.cancellation())?;
    let capture_target = target.description();
    let mut capture = target.into_capture();
    let mut statuses = capture.subscribe_status();
    let mut capture_errors = capture.subscribe_errors();
    let now = wall_clock().map_err(worker_failure)?;
    let session_id = SessionId::new(now.as_nanos());
    let _ = sender.send(WorkerEvent::Identified(session_id));
    let started_at_unix_ms = u64::try_from(now.as_millis()).unwrap_or(u64::MAX);
    let session = Session::new(session_id, capture_target.clone(), started_at_unix_ms);
    let (persistence, store) = persistence().await?;
    let recording_directory = application_recording_directory();
    std::fs::create_dir_all(&recording_directory).map_err(|error| {
        SessionFailure::new(
            SessionFailureKind::Persistence,
            format!(
                "Could not create recording directory {}: {error}",
                recording_directory.display()
            ),
        )
    })?;
    let recording_path = recording_directory.join(format!("{}.mp4", session_id.get()));

    let asr_config = AsrConfig::new(model_path);
    let mic_vad = SileroVad::new(Source::Mic, VadConfig::default()).map_err(capture_failure)?;
    let system_vad =
        SileroVad::new(Source::System, VadConfig::default()).map_err(capture_failure)?;
    let recording_config = if capture_target.is_microphone_only() {
        RecordingConfig::new(asr_config).microphone_only()
    } else {
        RecordingConfig::new(asr_config)
    };
    let (transcriber, recording_transcription) =
        LiveRecordingTranscriber::new(&recording_path, recording_config)
            .map_err(capture_failure)?;

    // Model and VAD construction can be expensive. Honor a Stop pressed during that work before
    // the session record or OS capture pipeline acquires any live-session side effect.
    ensure_not_cancelled(start_gate.cancellation())?;
    // This epoch brackets capture startup and is therefore never later than the capture-owned
    // monotonic epoch. It is also the upper bound used to reject impossible persisted coordinates.
    let session_started = std::time::Instant::now();
    let start_result = start_gate
        .start_if_not_cancelled_async(|| async {
            if let Some(entry_id) = entry_id {
                store
                    .save_session_in_entry(&session, entry_id)
                    .await
                    .map_err(persistence_failure)?;
            } else {
                store
                    .save_session(&session)
                    .await
                    .map_err(persistence_failure)?;
            }
            if let Err(error) = store
                .save_growing_recording(session_id, &recording_path)
                .await
            {
                finish_session_record(
                    &store,
                    session_id,
                    capture_target.clone(),
                    started_at_unix_ms,
                )
                .await?;
                return Err(persistence_failure(error));
            }
            if let Err(error) = capture.record_to(&recording_path) {
                let failure = capture_failure(error);
                store
                    .mark_recording_finalization_failed(session_id, &failure.message)
                    .await
                    .map_err(persistence_failure)?;
                finish_session_record(
                    &store,
                    session_id,
                    capture_target.clone(),
                    started_at_unix_ms,
                )
                .await?;
                return Err(failure);
            }
            match Pipeline::builder(session)
                .capture(capture)
                .vad(mic_vad, system_vad)
                .transcriber(transcriber)
                .annotator(prosody::Annotator::default())
                .persistence(persistence)
                .start()
            {
                Ok(pipeline) => Ok(pipeline),
                Err(error) => {
                    store
                        .mark_recording_finalization_failed(
                            session_id,
                            "Capture pipeline failed after recording start.",
                        )
                        .await
                        .map_err(persistence_failure)?;
                    finish_session_record(
                        &store,
                        session_id,
                        capture_target.clone(),
                        started_at_unix_ms,
                    )
                    .await?;
                    Err(capture_failure(error))
                }
            }
        })
        .await;
    let pipeline = match start_result {
        Ok(Some(started)) => started,
        Ok(None) => {
            return Err(SessionFailure::new(
                SessionFailureKind::Cancelled,
                "Session setup was cancelled before capture started.",
            ));
        }
        Err(error) => return Err(error),
    };
    let mut events = pipeline.events().subscribe("app-timeline-ingress");
    let mut reported_running = false;
    let mut capture_run_clock = CaptureRunClock::default();
    let mut annotation_open = true;
    let terminal = loop {
        tokio::select! {
            () = start_gate.cancellation().cancelled() => break SessionEnd::UserRequested,
            request = annotation_receiver.recv(), if annotation_open => {
                if let Some(request) = request {
                    if let Some(target) = request.target {
                        let _ = pipeline.supersede_user_annotation(session_started.elapsed(), target, request.text, request.mark).await;
                    } else {
                        let _ = pipeline.append_user_annotation(session_started.elapsed(), request.anchor, request.text, request.mark).await;
                    }
                } else {
                    annotation_open = false;
                }
            }
            event = events.recv() => {
                match event {
                    Ok(event) => {
                        if let Err(error) = assert_timeline_within_session(
                            std::slice::from_ref(&event),
                            session_started.elapsed(),
                        ) {
                            eprintln!("{}", error.message);
                            break SessionEnd::Capture(CaptureStatus::Failed);
                        }
                        if ingress.send(event).await.is_err() {
                            break SessionEnd::Capture(CaptureStatus::Failed);
                        }
                    }
                    Err(_) => break SessionEnd::Capture(CaptureStatus::Failed),
                }
            }
            status = statuses.recv() => {
                match status {
                    Ok(CaptureStatus::Running) => {
                        // `Pipeline::start()` returns after launching the bridge's asynchronous
                        // startup task, before ScreenCaptureKit finishes negotiation. The bridge
                        // emits `Running` only after `SCStream.startCapture()` completes, so this
                        // lifecycle edge is the first honest control-shell boundary for comparing
                        // wall-clock capture time with finalized media duration.
                        capture_run_clock.observe_running(std::time::Instant::now());
                        if !reported_running {
                            reported_running = true;
                            let _ = sender.send(WorkerEvent::Running);
                        }
                    }
                    Ok(status) => {
                        if let Some(status) = terminal_status(status) {
                            if status == CaptureStatus::Failed
                                && let Ok(error) = capture_errors.try_recv()
                            {
                                eprintln!("Live capture stream failed: {error}");
                                break capture_error_end(&error);
                            }
                            break SessionEnd::Capture(status);
                        }
                    }
                    Err(_) => break SessionEnd::Capture(CaptureStatus::Failed),
                }
            }
            error = capture_errors.recv() => {
                match error {
                    Ok(error) => {
                        eprintln!("Live capture stream failed: {error}");
                        break capture_error_end(&error);
                    }
                    Err(error) => eprintln!("Live capture error channel failed: {error}"),
                }
                break SessionEnd::Capture(CaptureStatus::Failed);
            }
        }
    };
    // Freeze the interval before pipeline shutdown, recording flush, probing, complete-file ASR,
    // and tail persistence. None of that work produces live capture media.
    let capture_elapsed = capture_run_clock.elapsed_at(std::time::Instant::now());
    let _ = sender.send(WorkerEvent::Finalizing);
    let mut stopping = tokio::spawn(pipeline.stop());
    if tokio::time::timeout(
        SHUTDOWN_DRAIN_TIMEOUT,
        drain_until_closed(&mut events, &ingress),
    )
    .await
    .is_err()
    {
        eprintln!(
            "Live session event drain exceeded {:?}; forwarding events ready now and ending the session",
            SHUTDOWN_DRAIN_TIMEOUT
        );
        if tokio::time::timeout(
            SHUTDOWN_READY_SWEEP_TIMEOUT,
            drain_ready(&mut events, &ingress),
        )
        .await
        .is_err()
        {
            eprintln!(
                "Live session ready-event sweep exceeded {:?}; ending the session",
                SHUTDOWN_READY_SWEEP_TIMEOUT
            );
        }
    }
    match tokio::time::timeout(SHUTDOWN_STOP_TIMEOUT, &mut stopping).await {
        Ok(result) => result.map_err(|error| {
            SessionFailure::new(
                SessionFailureKind::Worker,
                format!("Session shutdown failed: {error}"),
            )
        })?,
        Err(_) => {
            eprintln!(
                "Live session pipeline stop exceeded {:?}; aborting shutdown work",
                SHUTDOWN_STOP_TIMEOUT
            );
            stopping.abort();
        }
    }
    let finalization = async {
        wait_for_recording_finalization(&mut statuses).await;
        let probe = probe_recording(&recording_path).map_err(capture_failure)?;
        let tail_updates = recording_transcription
            .transcribe_complete()
            .map_err(capture_failure)?;
        let tail_utterances = tail_updates
            .into_iter()
            .filter_map(|update| match update {
                TranscriptUpdate::Final(utterance) => Some(utterance),
                TranscriptUpdate::Partial(_) => None,
            })
            .collect::<Vec<_>>();
        let tail_events = store
            .append_final_utterances(session_id, &tail_utterances).await
            .map_err(persistence_failure)?;
        for event in tail_events {
            ingress.send(event).await.map_err(|_| {
                SessionFailure::new(
                    SessionFailureKind::Worker,
                    "The meeting workspace closed before the finalized transcript tail arrived.",
                )
            })?;
        }
        let persisted_events = store
            .load_session(session_id).await
            .map_err(persistence_failure)?;
        // ASR coordinates are media time. Compare them with the probed media duration rather than
        // the independently sampled wall clock used by the control shell.
        assert_timeline_within_session(&persisted_events, probe.duration)?;
        let recording = SessionRecording::Available {
            session_id,
            path: recording_path.to_string_lossy().into_owned(),
            container: RecordingContainer::Mp4,
            duration: probe.duration,
            byte_size: probe.byte_size,
            time_mapping: MediaTimeMapping::IDENTITY,
        };
        let duration_discrepancy =
            settle_recording(&store, &recording, probe.duration, capture_elapsed).await?;
        if let Some(discrepancy) = duration_discrepancy.as_deref() {
            eprintln!(
                "Recording duration discrepancy: {discrepancy} The playable recording remains available."
            );
        }
        let pruned = store
            .enforce_recording_budget(&recording_directory).await
            .map_err(persistence_failure)?;
        if !pruned.contains(&session_id) {
            eprintln!(
                "{}",
                retained_recording_message(
                    probe.duration.as_secs_f64(),
                    probe.byte_size,
                    recording_path.display()
                )
            );
        }
        if !pruned.is_empty() {
            eprintln!(
                "Recording retention pruned {} oldest recording(s) to stay within budget.",
                pruned.len()
            );
        }
        Ok::<Option<String>, SessionFailure>(duration_discrepancy)
    }
    .await;

    let recording_discrepancy = match finalization {
        Ok(discrepancy) => discrepancy,
        Err(error) => {
            let reason = finalization_failure_reason(&error.message);
            preserve_failed_finalization(
                &store,
                session_id,
                capture_target.clone(),
                started_at_unix_ms,
                &reason,
            )
            .await?;
            return Err(SessionFailure::new(SessionFailureKind::Recording, reason));
        }
    };

    finish_session_record(
        &store,
        session_id,
        capture_target.clone(),
        started_at_unix_ms,
    )
    .await?;
    let persisted_record = store
        .load_session_record(session_id)
        .await
        .map_err(persistence_failure)?;
    if persisted_record.capture_target() != &capture_target
        || persisted_record.ended_at_unix_ms().is_none()
    {
        return Err(SessionFailure::new(
            SessionFailureKind::Persistence,
            "The completed session record did not reload with its capture scope and end time.",
        ));
    }
    let persisted = store
        .load_session(session_id)
        .await
        .map_err(persistence_failure)?;
    // Every completed recording joins the cross-recording index, so Ask can reach the whole
    // library without anyone opting a recording in first. Deliberately non-fatal and last: the
    // recording, its timeline and its media are already durable by this point, and a search index
    // that could not be built is a degraded search, not a lost call. `index_missing_prior_meetings`
    // repairs whatever this pass missed.
    if let Err(error) = store.index_prior_meeting(session_id, None).await {
        eprintln!(
            "Recording saved, but it could not be added to cross-recording search: {error}. It stays reviewable, and Sotto retries when you ask across recordings."
        );
    }
    Ok(SessionCompletion {
        end: terminal,
        session_id,
        persisted_events: persisted.len(),
        persisted_tail: persisted.last().map(|event| event.id()),
        recording_discrepancy,
    })
}

fn retained_recording_message(
    duration_seconds: f64,
    byte_size: u64,
    path: impl std::fmt::Display,
) -> String {
    format!("Retained recording: duration {duration_seconds:.3} s, size {byte_size} bytes ({path})")
}

fn recording_duration_discrepancy(
    media_duration: Duration,
    capture_elapsed: Option<Duration>,
) -> Option<String> {
    let capture_elapsed = capture_elapsed?;
    (media_duration.abs_diff(capture_elapsed) > RECORDING_DURATION_TOLERANCE).then(|| {
        format!(
            "Recording duration {:?} disagreed with capture run time {:?} beyond {:?}.",
            media_duration, capture_elapsed, RECORDING_DURATION_TOLERANCE
        )
    })
}

async fn settle_recording(
    store: &Store,
    recording: &SessionRecording,
    media_duration: Duration,
    capture_elapsed: Option<Duration>,
) -> Result<Option<String>, SessionFailure> {
    let discrepancy = recording_duration_discrepancy(media_duration, capture_elapsed);
    // A duration disagreement is diagnostic metadata, not evidence that playable local media
    // should be made unreachable. Settle the reference first and report the discrepancy after.
    store
        .save_recording(recording)
        .await
        .map_err(persistence_failure)?;
    Ok(discrepancy)
}

fn finalization_failure_reason(message: &str) -> String {
    const PREFIX: &str = "Recording finalization failed:";
    let detail = message
        .strip_prefix(PREFIX)
        .map_or(message, str::trim_start);
    format!("{PREFIX} {detail}")
}

async fn preserve_failed_finalization(
    store: &Store,
    session_id: SessionId,
    capture_target: CaptureTarget,
    started_at_unix_ms: u64,
    reason: &str,
) -> Result<(), SessionFailure> {
    store
        .mark_recording_finalization_failed(session_id, reason)
        .await
        .map_err(persistence_failure)?;
    finish_session_record(store, session_id, capture_target, started_at_unix_ms).await
}

/// How far a timeline coordinate may sit past the recording's measured end.
///
/// The invariant exists to catch **mixed clocks** — it once caught a 223,555-second timeline
/// against a 19.9-second session — and at that scale a second of slack changes nothing. What it
/// must not do is reject correct data: transcription reads committed media segments, so the final
/// flushed utterance can end a fraction past the duration `probe_recording` reports for the same
/// file. A real 221-second recording failed finalization over 0.44s of that rounding, which
/// discarded the recording reference along with it.
const TIMELINE_OVERSHOOT_TOLERANCE: Duration = Duration::from_secs(1);

fn assert_timeline_within_session(
    events: &[TimelineEvent],
    elapsed: Duration,
) -> Result<(), SessionFailure> {
    let limit = elapsed.saturating_add(TIMELINE_OVERSHOOT_TOLERANCE);
    for event in events {
        let latest = latest_timeline_coordinate(event);
        if latest > limit {
            return Err(SessionFailure::new(
                SessionFailureKind::Timeline,
                format!(
                    "Timeline clock invariant failed: event {} ({}) reaches {:?}, beyond the recording's {:?} by more than {:?}.",
                    event.id().get(),
                    event.kind().as_str(),
                    latest,
                    elapsed,
                    TIMELINE_OVERSHOOT_TOLERANCE
                ),
            ));
        }
    }
    Ok(())
}

fn latest_timeline_coordinate(event: &TimelineEvent) -> Duration {
    let payload_latest = match event.payload() {
        EventPayload::UtterancePartial(value) | EventPayload::UtteranceFinal(value) => {
            value.start.max(value.end)
        }
        EventPayload::Vad(value) => value.end.unwrap_or(value.start).max(value.start),
        EventPayload::ScreenSnapshot(value) => value
            .visible_to
            .unwrap_or(value.visible_from)
            .max(value.visible_from),
        EventPayload::Prosody(_)
        | EventPayload::Proposal(_)
        | EventPayload::ProposalDisposition(_)
        | EventPayload::ProposalRunAudit(_)
        | EventPayload::UserAnnotation(_)
        | EventPayload::Error(_) => event.ts(),
    };
    event.ts().max(payload_latest)
}

fn capture_error_end(error: &sotto_core::CaptureError) -> SessionEnd {
    match error {
        sotto_core::CaptureError::StreamFailed(reason)
            if reason.starts_with("Local recording stopped because ") =>
        {
            SessionEnd::RecordingFailed(reason.clone())
        }
        sotto_core::CaptureError::PermissionDenied { .. }
        | sotto_core::CaptureError::PermissionRevoked
        | sotto_core::CaptureError::DeviceUnavailable { .. }
        | sotto_core::CaptureError::StreamFailed(_)
        | sotto_core::CaptureError::Unsupported(_) => SessionEnd::Capture(CaptureStatus::Failed),
    }
}

fn ensure_not_cancelled(cancellation: &CancellationToken) -> Result<(), SessionFailure> {
    if cancellation.is_cancelled() {
        Err(SessionFailure::new(
            SessionFailureKind::Cancelled,
            "Session setup was cancelled before capture started.",
        ))
    } else {
        Ok(())
    }
}

const fn terminal_status(status: CaptureStatus) -> Option<CaptureStatus> {
    match status {
        CaptureStatus::Stopped
        | CaptureStatus::Failed
        | CaptureStatus::TargetEnded
        | CaptureStatus::UserStopped => Some(status),
        CaptureStatus::Starting | CaptureStatus::Running | CaptureStatus::Stopping => None,
    }
}

async fn drain_until_closed(events: &mut EventReceiver, ingress: &TimelineIngress) {
    while let Ok(event) = events.recv().await {
        if ingress.send(event).await.is_err() {
            return;
        }
    }
}

async fn drain_ready(events: &mut EventReceiver, ingress: &TimelineIngress) {
    // EventReceiver intentionally hides Tokio's broadcast receiver and has no try_recv.
    // Polling recv once is the equivalent non-blocking operation through its public contract.
    while let Some(result) = events.recv().now_or_never() {
        let Ok(event) = result else {
            return;
        };
        if ingress.send(event).await.is_err() {
            return;
        }
    }
}

/// Shared application database path used by capture persistence and the review workspace.
#[must_use]
pub fn application_database_path() -> PathBuf {
    match std::env::var_os("SOTTO_DATABASE") {
        Some(path) => PathBuf::from(path),
        None => {
            let home = std::env::var_os("HOME")
                .filter(|home| !home.is_empty())
                .map(PathBuf::from)
                .unwrap_or_else(std::env::temp_dir);
            home.join("Library")
                .join("Application Support")
                .join("Sotto")
                .join("sotto.sqlite3")
        }
    }
}

/// Managed directory containing only session-id-named MP4 recordings.
#[must_use]
pub fn application_recording_directory() -> PathBuf {
    application_database_path()
        .parent()
        .map_or_else(std::env::temp_dir, Path::to_path_buf)
        .join("recordings")
}

async fn wait_for_recording_finalization(
    statuses: &mut tokio::sync::broadcast::Receiver<CaptureStatus>,
) {
    wait_for_recording_finalization_until(statuses, RECORDING_FINALIZE_TIMEOUT).await;
}

/// Waits for Swift to report Stopped after remux, then lets the caller probe the file.
///
/// A missed Stopped (broadcast lag) is followed by a closed channel once CallbackState is
/// dropped; that is enough to proceed. A timeout is also not fatal: the remux Task can still
/// finish later, and `probe_recording` / stranded-growing recovery decide whether the MP4 is
/// usable. Tests pass a short deadline so CI does not wait ten minutes.
async fn wait_for_recording_finalization_until(
    statuses: &mut tokio::sync::broadcast::Receiver<CaptureStatus>,
    timeout: Duration,
) {
    let wait = async {
        loop {
            match statuses.recv().await {
                Ok(CaptureStatus::Stopped) => return,
                Ok(_) | Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
                Err(tokio::sync::broadcast::error::RecvError::Closed) => return,
            }
        }
    };
    if tokio::time::timeout(timeout, wait).await.is_err() {
        eprintln!("Local recording finalization exceeded {timeout:?}; probing the file anyway");
    }
}

async fn persistence() -> Result<(Arc<TimelinePersistence>, Arc<Store>), SessionFailure> {
    let path = application_database_path();
    if let Some(directory) = path.parent() {
        std::fs::create_dir_all(directory).map_err(|error| {
            SessionFailure::new(
                SessionFailureKind::Persistence,
                format!(
                    "Could not create application support directory {}: {error}",
                    directory.display()
                ),
            )
        })?;
    }
    let store = Arc::new(Store::open(&path).await.map_err(|error| {
        SessionFailure::new(
            SessionFailureKind::Persistence,
            format!(
                "Could not open session database {}: {error}",
                path.display()
            ),
        )
    })?);
    Ok((
        Arc::new(TimelinePersistence::new(Arc::clone(&store))),
        store,
    ))
}

async fn finish_session_record(
    store: &Store,
    session_id: SessionId,
    capture_target: CaptureTarget,
    started_at_unix_ms: u64,
) -> Result<(), SessionFailure> {
    let ended = wall_clock().map_err(worker_failure)?;
    let mut session = Session::new(session_id, capture_target, started_at_unix_ms);
    session.end(u64::try_from(ended.as_millis()).unwrap_or(u64::MAX));
    store
        .save_session(&session)
        .await
        .map_err(persistence_failure)
}

fn finish_state(result: Result<SessionCompletion, SessionFailure>) -> SessionLifecycle {
    match result {
        Ok(completion) => match completion.end {
            SessionEnd::RecordingFailed(reason) => {
                SessionLifecycle::Error(SessionFailure::new(SessionFailureKind::Recording, reason))
            }
            SessionEnd::Capture(CaptureStatus::Failed) => {
                SessionLifecycle::Error(SessionFailure::new(
                    SessionFailureKind::Capture,
                    "Capture reported a terminal failure.",
                ))
            }
            end => SessionLifecycle::Idle {
                last_outcome: Some(completion_message(
                    &end,
                    completion.persisted_events,
                    completion.persisted_tail,
                    completion.recording_discrepancy.as_deref(),
                )),
            },
        },
        Err(error) if error.kind == SessionFailureKind::Cancelled => SessionLifecycle::Idle {
            last_outcome: Some("Model setup cancelled. No capture session was started.".to_owned()),
        },
        Err(error) => SessionLifecycle::Error(error),
    }
}

fn completion_message(
    end: &SessionEnd,
    count: usize,
    tail: Option<EventId>,
    recording_discrepancy: Option<&str>,
) -> String {
    let reason = match end {
        SessionEnd::UserRequested => "You stopped capture.".to_owned(),
        SessionEnd::RecordingFailed(reason) => reason.clone(),
        SessionEnd::Capture(CaptureStatus::Stopped) => "Capture stopped.".to_owned(),
        SessionEnd::Capture(CaptureStatus::TargetEnded) => {
            "The selected target closed, so capture stopped.".to_owned()
        }
        SessionEnd::Capture(CaptureStatus::UserStopped) => {
            "Stop Sharing in the system UI ended capture.".to_owned()
        }
        SessionEnd::Capture(CaptureStatus::Failed) => "Capture failed.".to_owned(),
        SessionEnd::Capture(
            CaptureStatus::Starting | CaptureStatus::Running | CaptureStatus::Stopping,
        ) => "Capture ended during a lifecycle transition.".to_owned(),
    };
    let tail = tail.map_or_else(
        || "no persisted tail event".to_owned(),
        |id| format!("persisted tail #{}", id.get()),
    );
    let discrepancy = recording_discrepancy.map_or_else(String::new, |detail| {
        format!(
            " Recording duration discrepancy: {detail} The playable recording remains available."
        )
    });
    format!("{reason} Saved {count} timeline events ({tail}).{discrepancy}")
}

pub(crate) fn progress_label(model: ModelSize, progress: ProvisionProgress) -> String {
    let model = model_label(model);
    let percent = progress
        .downloaded
        .saturating_mul(100)
        .checked_div(progress.total)
        .unwrap_or(0)
        .min(100);
    match progress.phase {
        ProvisionPhase::Resolving => format!("Checking the verified {model} model cache…"),
        ProvisionPhase::Downloading => format!(
            "Downloading {model}… {percent}% ({} of {} MB). Cancelling keeps a resumable partial.",
            progress.downloaded.saturating_add(500_000) / 1_000_000,
            progress.total.saturating_add(500_000) / 1_000_000
        ),
        ProvisionPhase::Verifying => format!("Verifying {model} integrity… {percent}%"),
        ProvisionPhase::Ready => format!("Verified {model} is ready. Starting capture…"),
    }
}

fn target_label(target: &CaptureTarget) -> String {
    match target.window_title.as_deref() {
        Some(title) if title != target.display_name => {
            format!("{} — {title}", target.display_name)
        }
        Some(_) | None => target.display_name.clone(),
    }
}

fn wall_clock() -> Result<Duration, String> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| format!("System clock precedes Unix epoch: {error}"))
}

fn worker_failure(error: impl ToString) -> SessionFailure {
    SessionFailure::new(SessionFailureKind::Worker, error.to_string())
}

fn capture_failure(error: impl ToString) -> SessionFailure {
    SessionFailure::new(SessionFailureKind::Capture, error.to_string())
}

fn persistence_failure(error: impl ToString) -> SessionFailure {
    SessionFailure::new(SessionFailureKind::Persistence, error.to_string())
}

#[cfg(test)]
mod tests {
    use std::{
        num::NonZeroUsize,
        sync::{
            Arc, Barrier,
            atomic::{AtomicBool, Ordering},
            mpsc,
        },
        time::Duration,
    };

    use asr::{
        ModelSize,
        model::{ModelProvisionError, ProvisionPhase, ProvisionProgress},
    };
    use capture::macos::CaptureStatus;
    use gpui::{AppContext as _, TestAppContext};
    use sotto_core::types::{MediaTimeMapping, RecordingContainer, SessionRecording};
    use sotto_core::{
        CancellationToken, CaptureError, CaptureTarget, EventBus, EventId, EventPayload, MarkKind,
        Session, SessionId, Source, SpeechState, TargetKind, TimelineBuilder, Utterance,
        VadSegment,
    };

    use super::{
        CaptureRunClock, LifecycleModel, SessionController, SessionEnd, SessionFailure,
        SessionFailureKind, SessionLifecycle, StartGate, Store, WorkerEvent,
        assert_timeline_within_session, capture_error_end, completion_message, drain_ready,
        drain_until_closed, ensure_not_cancelled, finalization_failure_reason, finish_state,
        preserve_failed_finalization, recording_duration_discrepancy, retained_recording_message,
        send_finished, send_phase_progress, settle_recording, terminal_status,
        wait_for_recording_finalization_until,
    };
    use crate::devwindow::test_ingress;

    fn target(audio_scoped: bool) -> CaptureTarget {
        CaptureTarget {
            bundle_id: Some("us.zoom.xos".to_owned()),
            display_name: "Zoom".to_owned(),
            window_title: Some("Acme call".to_owned()),
            kind: TargetKind::Window,
            audio_scoped,
        }
    }

    #[test]
    fn cold_lifecycle_is_idle_and_does_not_imply_setup() {
        let model = LifecycleModel::default();
        assert_eq!(
            model.state,
            SessionLifecycle::Idle { last_outcome: None },
            "cold lifecycle must be idle"
        );
        assert!(
            model.start_gate.is_none(),
            "cold lifecycle must have no start gate"
        );
        assert!(
            model.state.can_start(),
            "cold lifecycle must permit explicit Start"
        );
        assert!(
            !model.state.can_stop(),
            "cold lifecycle must not imply stoppable work"
        );
    }

    #[test]
    fn retained_recording_success_names_duration_size_and_path() {
        let message = retained_recording_message(40.169_333, 12_182_734, "/recordings/62.mp4");

        assert_eq!(
            message,
            "Retained recording: duration 40.169 s, size 12182734 bytes (/recordings/62.mp4)",
            "successful finalization must be distinguishable from silent loss"
        );
    }

    #[test]
    fn capture_startup_overhead_is_not_part_of_the_duration_check() {
        let pipeline_start = std::time::Instant::now();
        let capture_running = pipeline_start + Duration::from_secs_f64(2.3);
        let capture_stopped = capture_running + Duration::from_secs_f64(67.4);
        let mut clock = CaptureRunClock::default();
        clock.observe_running(capture_running);

        let media_duration = Duration::from_secs_f64(67.177_333_333);
        let capture_elapsed = clock.elapsed_at(capture_stopped);
        let pipeline_bracket_elapsed = capture_stopped.duration_since(pipeline_start);

        assert!(
            media_duration.abs_diff(pipeline_bracket_elapsed) > Duration::from_secs(2),
            "fixture must reproduce startup negotiation contaminating the old bracket"
        );
        assert!(
            recording_duration_discrepancy(media_duration, capture_elapsed).is_none(),
            "startup negotiation before the Running edge must not fail recording finalization"
        );
    }

    #[test]
    fn capture_run_clock_keeps_the_first_running_edge() {
        let first_running = std::time::Instant::now();
        let duplicate_running = first_running + Duration::from_secs(9);
        let stopped = first_running + Duration::from_secs(20);
        let mut clock = CaptureRunClock::default();

        clock.observe_running(first_running);
        clock.observe_running(duplicate_running);

        assert_eq!(clock.elapsed_at(stopped), Some(Duration::from_secs(20)));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn genuine_duration_mismatch_is_reported_after_recording_becomes_available()
    -> Result<(), Box<dyn std::error::Error>> {
        let directory = tempfile::tempdir()?;
        let store = Store::open(directory.path().join("sotto.sqlite3")).await?;
        let session_id = SessionId::new(68);
        store
            .save_session(&Session::new(session_id, target(true), 1))
            .await?;
        let recording = SessionRecording::Available {
            session_id,
            path: directory
                .path()
                .join("68.mp4")
                .to_string_lossy()
                .into_owned(),
            container: RecordingContainer::Mp4,
            duration: Duration::from_secs(223_555),
            byte_size: 42,
            time_mapping: MediaTimeMapping::IDENTITY,
        };

        let discrepancy = settle_recording(
            &store,
            &recording,
            Duration::from_secs(223_555),
            Some(Duration::from_secs_f64(19.9)),
        )
        .await
        .map_err(|error| std::io::Error::other(error.message))?
        .ok_or("mixed-clock duration unexpectedly passed")?;

        assert!(discrepancy.contains("223555"));
        assert!(discrepancy.contains("19.9"));
        assert_eq!(
            store.load_recording_reference(session_id).await?,
            Some(rag::RecordingReference::Settled(recording)),
            "a detected mismatch must keep the recording usable"
        );
        Ok(())
    }

    #[test]
    fn finalization_failure_prefix_is_idempotent() {
        let detail = "recording probe failed";
        let expected = "Recording finalization failed: recording probe failed";

        assert_eq!(finalization_failure_reason(detail), expected);
        assert_eq!(finalization_failure_reason(expected), expected);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn recording_finalization_wait_accepts_stopped() {
        let (tx, mut rx) = tokio::sync::broadcast::channel(8);
        assert!(
            tx.send(CaptureStatus::Stopped).is_ok(),
            "test status send must succeed"
        );
        wait_for_recording_finalization_until(&mut rx, Duration::from_secs(1)).await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn recording_finalization_wait_treats_closed_as_finished() {
        let (tx, mut rx) = tokio::sync::broadcast::channel::<CaptureStatus>(8);
        drop(tx);
        wait_for_recording_finalization_until(&mut rx, Duration::from_secs(1)).await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn recording_finalization_wait_does_not_fail_when_stopped_is_lagged_away() {
        let (tx, mut rx) = tokio::sync::broadcast::channel(1);
        assert!(
            tx.send(CaptureStatus::Stopped).is_ok(),
            "test status send must succeed"
        );
        for _ in 0..8 {
            let _ = tx.send(CaptureStatus::Running);
        }
        drop(tx);
        wait_for_recording_finalization_until(&mut rx, Duration::from_secs(1)).await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn recording_finalization_wait_times_out_without_failing() {
        let (_tx, mut rx) = tokio::sync::broadcast::channel::<CaptureStatus>(8);
        wait_for_recording_finalization_until(&mut rx, Duration::from_millis(30)).await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn failure_before_final_recording_write_keeps_counted_deletable_reference()
    -> Result<(), Box<dyn std::error::Error>> {
        let directory = tempfile::tempdir()?;
        let database = directory.path().join("sotto.sqlite3");
        let recordings = directory.path().join("recordings");
        std::fs::create_dir_all(&recordings)?;
        let store = Store::open(&database).await?;
        let session_id = SessionId::new(62);
        let capture_target = target(true);
        store
            .save_session(&Session::new(session_id, capture_target.clone(), 1))
            .await?;
        let path = recordings.join("62.mp4");
        std::fs::write(&path, vec![1_u8; 23])?;
        store.save_growing_recording(session_id, &path).await?;

        preserve_failed_finalization(
            &store,
            session_id,
            capture_target,
            1,
            "injected tail persistence failure",
        )
        .await
        .map_err(|error| std::io::Error::other(error.message))?;

        assert!(
            store
                .load_session_record(session_id)
                .await?
                .ended_at_unix_ms()
                .is_some()
        );
        assert_eq!(store.recording_usage().await?.used_bytes, 23);
        assert!(matches!(
            store.load_recording_reference(session_id).await?,
            Some(rag::RecordingReference::Growing {
                finalization_error: Some(reason),
                ..
            }) if reason == "injected tail persistence failure"
        ));
        assert!(
            store
                .remove_recording_media(
                    session_id,
                    sotto_core::types::RecordingMissingReason::Deleted,
                    &recordings,
                )
                .await?
        );
        assert!(!path.exists());
        Ok(())
    }

    #[test]
    fn session_elapsed_invariant_rejects_the_observed_uptime_clock()
    -> Result<(), Box<dyn std::error::Error>> {
        let session = Session::new(SessionId::new(57), target(false), 1_786_625_633_040);
        let mut timeline = TimelineBuilder::new(session);
        timeline.append(
            Duration::from_secs_f64(19.9),
            EventPayload::UtteranceFinal(Utterance {
                source: Source::System,
                start: Duration::from_secs_f64(19.0),
                end: Duration::from_secs_f64(19.9),
                text: "valid relative speech".into(),
                avg_logprob: 0.0,
                annotations: vec![],
            }),
        );
        timeline.append(
            Duration::from_secs_f64(223_547.5),
            EventPayload::Vad(VadSegment {
                source: Source::System,
                start: Duration::from_secs_f64(223_547.5),
                end: None,
                kind: SpeechState::SpeechStart,
            }),
        );

        let result =
            assert_timeline_within_session(timeline.events(), Duration::from_secs_f64(41.2));
        let error = result
            .err()
            .ok_or("mixed-clock timeline unexpectedly passed")?;
        assert_eq!(error.kind, SessionFailureKind::Timeline);
        assert!(error.message.contains("223547.5"));
        assert!(error.message.contains("41.2"));
        Ok(())
    }

    #[test]
    fn idle_controller_refuses_to_misfile_a_typed_note() {
        let (ingress, _receiver) = test_ingress(4);
        let controller = SessionController::new(ingress);
        let result = controller.append_user_annotation(
            EventId::new(1),
            "remember this".to_owned(),
            MarkKind::Note,
        );
        assert!(
            result
                .as_ref()
                .is_err_and(|error| error.contains("live meeting"))
        );
    }

    #[test]
    fn picker_cancellation_returns_idle_before_model_setup() {
        let mut model = LifecycleModel::default();
        let generation = model.begin();
        assert!(generation.is_some(), "idle lifecycle must accept Start");
        let Some(generation) = generation else {
            return;
        };
        model.picker_cancelled(generation);

        assert!(
            matches!(model.state, SessionLifecycle::Idle { .. }),
            "picker cancel must return idle"
        );
        assert!(
            model.start_gate.is_none(),
            "picker cancel must not retain a worker gate"
        );
        assert!(
            model.state.status_message().contains("No session or model"),
            "picker cancel must report that no session side effect occurred"
        );
    }

    #[test]
    fn screen_permission_denied_blocks_start_with_a_stated_reason() {
        let mut model = LifecycleModel::default();
        let generation = model.begin();
        assert!(generation.is_some(), "idle lifecycle must accept Start");
        let Some(generation) = generation else {
            return;
        };
        model.permission_denied(generation);

        assert!(
            matches!(model.state, SessionLifecycle::Error(_)),
            "denied permission must land in Error, not Idle: {:?}",
            model.state
        );
        let SessionLifecycle::Error(failure) = &model.state else {
            return;
        };
        assert_eq!(failure.kind, SessionFailureKind::ScreenPermissionDenied);
        assert!(
            model
                .state
                .status_message()
                .contains("Screen & System Audio Recording is off"),
            "the denial must be stated, not merely categorized"
        );
        assert!(
            model.state.status_message().contains("Settings"),
            "a denial macOS will never re-prompt for must point at Settings"
        );
        assert!(
            model.state.can_start(),
            "a stated denial must still allow trying again after Settings + relaunch"
        );
    }

    #[test]
    fn screen_permission_not_determined_does_not_silently_return_to_idle() {
        let mut model = LifecycleModel::default();
        let generation = model.begin();
        assert!(generation.is_some(), "idle lifecycle must accept Start");
        let Some(generation) = generation else {
            return;
        };
        model.permission_not_determined(generation);

        assert!(
            !matches!(model.state, SessionLifecycle::Idle { .. }),
            "an unanswered permission prompt is not the same as a quiet cancellation"
        );
        assert!(
            matches!(model.state, SessionLifecycle::Error(_)),
            "not-determined permission must land in Error: {:?}",
            model.state
        );
        let SessionLifecycle::Error(failure) = &model.state else {
            return;
        };
        assert_eq!(
            failure.kind,
            SessionFailureKind::ScreenPermissionNotDetermined
        );
        let message = model.state.status_message();
        assert!(
            message.contains("Approve the system prompt"),
            "the message must say what to do with the prompt now on screen"
        );
        assert!(
            message.contains("relaunch") || message.contains("newly launched"),
            "the message must say a relaunch is required, since a grant needs a new process"
        );
    }

    #[test]
    fn stop_cancels_provisioning_and_never_claims_recording() {
        let mut model = LifecycleModel::default();
        let generation = model.begin();
        assert!(generation.is_some(), "idle lifecycle must accept Start");
        let Some(generation) = generation else {
            return;
        };
        let start_gate = StartGate::new();
        let cancellation = start_gate.cancellation().clone();
        assert!(
            model.target_picked(generation, target(false), ModelSize::SmallEn, start_gate,),
            "current picker result must enter provisioning"
        );
        assert!(model.request_stop(), "provisioning must expose Stop");

        assert!(
            cancellation.is_cancelled(),
            "Stop must cancel model provisioning"
        );
        assert!(
            matches!(
                model.state,
                SessionLifecycle::Stopping {
                    was_recording: false,
                    ..
                }
            ),
            "pre-start cancellation must not claim capture was recording"
        );
        assert!(
            model.state.indicator().is_none(),
            "pre-start cancellation must not show a recording indicator"
        );
    }

    #[test]
    fn running_and_finalizing_keep_truthful_scope_visible() {
        let mut model = LifecycleModel::default();
        let generation = model.begin();
        assert!(generation.is_some(), "idle lifecycle must accept Start");
        let Some(generation) = generation else {
            return;
        };
        let start_gate = StartGate::new();
        assert!(
            model.target_picked(
                generation,
                target(false),
                ModelSize::SmallEn,
                start_gate.clone(),
            ),
            "current picker result must enter provisioning"
        );
        let started = start_gate.start_if_not_cancelled(|| Ok::<_, ()>(()));
        assert!(
            matches!(started, Ok(Some(()))),
            "uncancelled worker must cross live boundary"
        );
        model.apply(generation, WorkerEvent::Running);
        assert_eq!(
            model.state.indicator().map(|value| value.label()),
            Some(
                "Recording locally · mic: microphone · screen: Zoom — Acme call · captured audio: system-wide"
                    .to_owned()
            ),
            "running indicator must name truthful screen and system-wide audio scope"
        );

        model.apply(generation, WorkerEvent::Finalizing);
        assert_eq!(
            model.state.indicator().map(|value| value.label()),
            Some(
                "Finalizing capture locally · mic: microphone · screen: Zoom — Acme call · captured audio: system-wide"
                    .to_owned()
            ),
            "bounded finalization must retain the truthful scope indicator"
        );
    }

    #[test]
    fn window_audio_scope_names_the_application_not_the_window() {
        let state = SessionLifecycle::Running {
            target: target(true),
        };
        assert_eq!(
            state.indicator().map(|indicator| indicator.label()),
            Some(
                "Recording locally · mic: microphone · screen: Zoom — Acme call · captured audio: Zoom application"
                    .to_owned()
            ),
            "window screen scope and application audio scope must not be conflated"
        );
    }

    #[test]
    fn microphone_only_indicator_excludes_screen_and_application_audio() {
        let state = SessionLifecycle::Running {
            target: CaptureTarget::microphone_only(),
        };

        assert_eq!(
            state.indicator().map(|indicator| indicator.label()),
            Some(
                "Recording locally · mic: microphone · screen: off · application audio: off"
                    .to_owned()
            )
        );
        assert_eq!(
            state.status_message(),
            "A microphone-only audio recording is being kept on this Mac."
        );
    }

    #[test]
    fn stale_worker_updates_cannot_restart_a_new_generation() {
        let mut model = LifecycleModel::default();
        let old = model.begin();
        assert!(old.is_some(), "idle lifecycle must accept the first Start");
        let Some(old) = old else {
            return;
        };
        model.picker_cancelled(old);
        let new = model.begin();
        assert!(new.is_some(), "idle lifecycle must accept a later Start");
        let Some(new) = new else {
            return;
        };
        model.apply(old, WorkerEvent::Running);

        assert_ne!(old, new, "each Start must allocate a distinct generation");
        assert_eq!(
            model.state,
            SessionLifecycle::ChoosingTarget,
            "stale worker events must not mutate the current picker lifecycle"
        );
    }

    #[test]
    fn offline_and_corrupt_model_failures_remain_distinct() {
        let offline = SessionFailure::from_model(ModelProvisionError::OfflineNoCache);
        let corrupt = SessionFailure::from_model(ModelProvisionError::DigestMismatch {
            path: "model.bin".into(),
            expected: "expected".to_owned(),
            actual: "actual".to_owned(),
        });

        assert_eq!(
            offline.kind,
            SessionFailureKind::OfflineNoCache,
            "offline cache miss must remain typed"
        );
        assert_eq!(
            corrupt.kind,
            SessionFailureKind::CorruptModel,
            "digest mismatch must remain typed"
        );
        assert!(
            offline
                .actionable_message()
                .contains("Connect to the internet"),
            "offline cache miss must offer a network action"
        );
        assert!(
            corrupt
                .actionable_message()
                .contains("integrity verification"),
            "corrupt model must offer an integrity recovery action"
        );
    }

    #[test]
    fn progress_updates_only_while_provisioning() {
        let mut model = LifecycleModel::default();
        let generation = model.begin();
        assert!(generation.is_some(), "idle lifecycle must accept Start");
        let Some(generation) = generation else {
            return;
        };
        assert!(
            model.target_picked(
                generation,
                target(true),
                ModelSize::SmallEn,
                StartGate::new(),
            ),
            "current picker result must enter provisioning"
        );
        let progress = ProvisionProgress {
            phase: ProvisionPhase::Downloading,
            downloaded: 74,
            total: 148,
        };
        model.apply(generation, WorkerEvent::Progress(progress));
        assert!(
            model
                .state
                .status_message()
                .contains("Downloading small.en… 50%"),
            "download progress must render the selected model and measured percentage"
        );
    }

    #[test]
    fn missing_model_keeps_start_and_import_inert() {
        let mut cx = TestAppContext::single();
        let (ingress, _receiver) = test_ingress(4);
        let controller = cx.new(|_| SessionController::new(ingress));
        cx.update(|cx| {
            controller.update(cx, |controller, cx| {
                controller
                    .transcription_model
                    .set_availability(super::ModelAvailability::Missing);
                controller.start(cx);
                assert!(
                    matches!(controller.lifecycle(), SessionLifecycle::Idle { .. }),
                    "missing weights must stop before the target picker lifecycle"
                );
                controller.start_import(cx);
                assert!(
                    !controller.is_importing(),
                    "missing weights must stop before the import picker"
                );
            });
        });
    }

    #[test]
    fn microphone_only_start_never_touches_screen_permission() {
        let mut cx = TestAppContext::single();
        let (ingress, _receiver) = test_ingress(4);
        let controller = cx.new(|_| SessionController::new(ingress));
        cx.update(|cx| {
            controller.update(cx, |controller, cx| {
                controller
                    .transcription_model
                    .set_ready(std::path::PathBuf::from("model.bin"));
                controller.start_microphone_only(cx);
                assert!(
                    matches!(
                        controller.lifecycle(),
                        SessionLifecycle::ProvisioningModel { .. }
                    ),
                    "microphone-only must skip the picker lifecycle entirely: {:?}",
                    controller.lifecycle()
                );
                assert!(
                    controller.screen_permission_note().is_none(),
                    "a path that never queries screen permission must never carry its note"
                );
            });
        });
    }

    #[test]
    fn cancelling_a_download_fences_its_generation_and_restores_missing() {
        let mut cx = TestAppContext::single();
        let (ingress, _receiver) = test_ingress(4);
        let controller = cx.new(|_| SessionController::new(ingress));
        let cancellation = CancellationToken::new();
        let observed = cancellation.clone();
        cx.update(|cx| {
            controller.update(cx, |controller, cx| {
                controller.model_operation_generation = 7;
                controller.model_download_cancellation = Some(cancellation);
                controller
                    .transcription_model
                    .set_provisioning(ProvisionProgress {
                        phase: ProvisionPhase::Downloading,
                        downloaded: 10,
                        total: 100,
                    });
                controller.cancel_model_download(cx);
                assert!(
                    observed.is_cancelled(),
                    "Cancel must reach the provisioner token"
                );
                assert!(
                    controller.model_operation_is_current(8)
                        && !controller.model_operation_is_current(7),
                    "the cancelled generation must not be allowed to publish later progress"
                );
                assert_eq!(
                    controller.transcription_model.availability(),
                    &super::ModelAvailability::Missing
                );
            });
        });
    }

    #[test]
    fn stop_during_expensive_setup_is_observed_before_capture_start() {
        let start_gate = StartGate::new();
        start_gate.cancel();

        let result = ensure_not_cancelled(start_gate.cancellation());
        assert!(
            result.is_err(),
            "cancel must stop expensive setup before capture"
        );
        let Err(error) = result else {
            return;
        };
        assert_eq!(
            error.kind,
            SessionFailureKind::Cancelled,
            "setup cancellation must remain typed"
        );
    }

    #[test]
    fn stop_and_capture_start_have_one_atomic_truth_boundary() {
        let stopped_first = StartGate::new();
        assert!(
            !stopped_first.cancel(),
            "pre-start Stop must not claim recording"
        );
        let side_effect = AtomicBool::new(false);
        let after_stop = stopped_first.start_if_not_cancelled(|| {
            side_effect.store(true, Ordering::SeqCst);
            Ok::<_, ()>(())
        });
        assert!(
            matches!(after_stop, Ok(None)),
            "Stop-before-start must prevent the start closure"
        );
        assert!(
            !side_effect.load(Ordering::SeqCst),
            "Stop-before-start must prevent session persistence and OS capture side effects"
        );

        let started_first = StartGate::new();
        let started = started_first.start_if_not_cancelled(|| Ok::<_, ()>(()));
        assert!(
            matches!(started, Ok(Some(()))),
            "worker must execute an uncancelled start closure"
        );
        assert!(
            started_first.cancel(),
            "Stop after the live boundary must keep a finalizing indicator"
        );
        assert!(
            started_first.cancellation().is_cancelled(),
            "both boundary orderings must cancel the worker token"
        );
    }

    #[test]
    fn stop_waits_for_the_synchronous_start_critical_section() {
        let gate = StartGate::new();
        let start_gate = gate.clone();
        let entered = Arc::new(Barrier::new(2));
        let release = Arc::new(Barrier::new(2));
        let invoked = Arc::new(AtomicBool::new(false));
        let start_thread = {
            let entered = Arc::clone(&entered);
            let release = Arc::clone(&release);
            let invoked = Arc::clone(&invoked);
            std::thread::spawn(move || {
                start_gate.start_if_not_cancelled(|| {
                    invoked.store(true, Ordering::SeqCst);
                    entered.wait();
                    release.wait();
                    Ok::<_, ()>(())
                })
            })
        };
        entered.wait();
        let stop_thread = std::thread::spawn(move || gate.cancel());
        assert!(
            invoked.load(Ordering::SeqCst),
            "start closure must be inside the locked boundary"
        );
        release.wait();

        let started = start_thread.join();
        let stopped_after_start = stop_thread.join();
        assert!(
            matches!(started, Ok(Ok(Some(())))),
            "start closure must complete before Stop acquires gate"
        );
        assert!(
            matches!(stopped_after_start, Ok(true)),
            "Stop after the critical section must report recording/finalizing"
        );
    }

    #[test]
    fn control_must_remain_visible_until_shutdown_is_terminal() {
        let target = target(false);
        for state in [
            SessionLifecycle::ProvisioningModel {
                target: target.clone(),
                model: ModelSize::SmallEn,
                progress: ProvisionProgress {
                    phase: ProvisionPhase::Resolving,
                    downloaded: 0,
                    total: 1,
                },
            },
            SessionLifecycle::Running {
                target: target.clone(),
            },
            SessionLifecycle::Stopping {
                target,
                was_recording: true,
            },
        ] {
            assert!(
                state.requires_visible_control(),
                "active and finalizing states must veto control close"
            );
        }
        assert!(
            !SessionLifecycle::Idle { last_outcome: None }.requires_visible_control(),
            "terminal idle state may close the control shell"
        );
    }

    #[test]
    fn provisioning_phase_transitions_are_not_dropped_by_a_full_channel() {
        let (sender, receiver) = mpsc::sync_channel(1);
        let initial = ProvisionProgress {
            phase: ProvisionPhase::Downloading,
            downloaded: 1,
            total: 2,
        };
        assert!(
            sender.send(WorkerEvent::Progress(initial)).is_ok(),
            "fixture progress must enter the bounded channel"
        );
        let phase_sender = sender;
        let cancellation = CancellationToken::new();
        let worker = std::thread::spawn(move || {
            send_phase_progress(
                &phase_sender,
                &cancellation,
                ProvisionProgress {
                    phase: ProvisionPhase::Verifying,
                    downloaded: 2,
                    total: 2,
                },
            );
        });

        assert!(
            matches!(receiver.recv(), Ok(WorkerEvent::Progress(progress)) if progress == initial),
            "queued download progress must arrive first"
        );
        assert!(
            matches!(receiver.recv(), Ok(WorkerEvent::Progress(progress)) if progress.phase == ProvisionPhase::Verifying),
            "Verifying transition must survive backpressure"
        );
        assert!(
            worker.join().is_ok(),
            "phase sender thread must finish after queue drains"
        );
    }

    #[test]
    fn app_quit_terminal_signal_never_blocks_worker_completion() {
        let (sender, _receiver) = mpsc::sync_channel(1);
        assert!(
            sender
                .send(WorkerEvent::Progress(ProvisionProgress {
                    phase: ProvisionPhase::Ready,
                    downloaded: 1,
                    total: 1,
                }))
                .is_ok(),
            "fixture must fill the lifecycle queue"
        );
        let app_shutdown = AtomicBool::new(true);

        send_finished(
            &sender,
            &app_shutdown,
            WorkerEvent::Finished(Err(SessionFailure::new(
                SessionFailureKind::Cancelled,
                "app quit",
            ))),
        );

        assert!(
            app_shutdown.load(Ordering::Acquire),
            "full lifecycle queue must not hold the worker behind terminal delivery during quit"
        );
    }

    #[test]
    fn runtime_capture_errors_are_terminal() {
        let error = CaptureError::StreamFailed("microphone disconnected".to_owned());
        assert!(
            matches!(
                capture_error_end(&error),
                SessionEnd::Capture(CaptureStatus::Failed)
            ),
            "runtime mic/capture errors must enter bounded terminal shutdown"
        );
    }

    #[test]
    fn recording_failure_keeps_native_cause_without_guessing_disk_full() {
        let reason = "Local recording stopped because system audio encoding failed with OSStatus -16341. Capture stopped; the committed recording prefix was kept.".to_owned();
        let error = CaptureError::StreamFailed(reason.clone());
        assert_eq!(
            capture_error_end(&error),
            SessionEnd::RecordingFailed(reason.clone()),
            "recording errors must retain their distinct terminal kind and native cause"
        );
        assert!(
            reason.contains("OSStatus -16341"),
            "the native writer cause must survive the session boundary"
        );
        assert!(
            !reason.contains("disk is full"),
            "an unclassified writer failure must not be relabelled as disk-full"
        );
    }

    #[test]
    fn dropping_the_control_owner_cancels_active_work() {
        let start_gate = StartGate::new();
        let cancellation = start_gate.cancellation().clone();
        let (ingress, _receiver) = test_ingress(1);
        let mut controller = SessionController::new(ingress);
        controller.model.start_gate = Some(start_gate);

        drop(controller);

        assert!(
            cancellation.is_cancelled(),
            "closing the control shell must not detach active capture"
        );
    }

    #[test]
    fn every_terminal_status_preserves_its_cause() {
        for status in [
            CaptureStatus::Stopped,
            CaptureStatus::Failed,
            CaptureStatus::TargetEnded,
            CaptureStatus::UserStopped,
        ] {
            assert_eq!(
                terminal_status(status),
                Some(status),
                "terminal capture status must preserve its cause"
            );
        }
        for status in [
            CaptureStatus::Starting,
            CaptureStatus::Running,
            CaptureStatus::Stopping,
        ] {
            assert_eq!(
                terminal_status(status),
                None,
                "transitional capture status must remain nonterminal"
            );
        }
    }

    #[test]
    fn completion_names_target_close_and_persisted_tail() {
        let message = completion_message(
            &SessionEnd::Capture(CaptureStatus::TargetEnded),
            42,
            Some(sotto_core::EventId::new(81)),
            None,
        );
        assert!(
            message.contains("selected target closed"),
            "completion must explain target closure"
        );
        assert!(
            message.contains("42 timeline events"),
            "completion must report persisted event count"
        );
        assert!(
            message.contains("tail #81"),
            "completion must report the reloaded persisted tail"
        );
    }

    #[test]
    fn every_terminal_result_clears_the_recording_indicator() {
        for end in [
            SessionEnd::UserRequested,
            SessionEnd::Capture(CaptureStatus::Stopped),
            SessionEnd::Capture(CaptureStatus::TargetEnded),
            SessionEnd::Capture(CaptureStatus::UserStopped),
        ] {
            let state = finish_state(Ok(super::SessionCompletion {
                end,
                session_id: sotto_core::SessionId::new(1),
                persisted_events: 1,
                persisted_tail: Some(sotto_core::EventId::new(1)),
                recording_discrepancy: None,
            }));
            assert!(
                state.indicator().is_none(),
                "terminal completion must clear recording indicator"
            );
            assert!(
                matches!(state, SessionLifecycle::Idle { .. }),
                "nonfailure terminal completion must return idle"
            );
        }

        let state = finish_state(Err(SessionFailure::new(
            SessionFailureKind::Capture,
            "capture failed",
        )));
        assert!(
            state.indicator().is_none(),
            "terminal failure must clear recording indicator"
        );
        assert!(
            matches!(state, SessionLifecycle::Error(_)),
            "terminal failure must remain actionable"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn ready_sweep_returns_when_an_event_sender_is_leaked()
    -> Result<(), Box<dyn std::error::Error>> {
        let bus = EventBus::new(NonZeroUsize::MIN);
        let mut events = bus.subscribe("leaked-sender-regression");
        let (ingress, _receiver) = test_ingress(1);

        drain_ready(&mut events, &ingress).await;
        Ok(())
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn leaked_event_sender_can_be_bounded_during_drain()
    -> Result<(), Box<dyn std::error::Error>> {
        let bus = EventBus::new(NonZeroUsize::MIN);
        let mut events = bus.subscribe("bounded-drain-regression");
        let (ingress, _receiver) = test_ingress(1);
        let result = tokio::time::timeout(
            Duration::from_millis(10),
            drain_until_closed(&mut events, &ingress),
        )
        .await;

        assert!(
            result.is_err(),
            "leaked sender drain must be bounded by timeout"
        );
        Ok(())
    }
}
