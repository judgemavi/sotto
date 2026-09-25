//! Selection and stale-result-fenced generation state for meeting notes.

use std::{
    path::PathBuf,
    sync::{Arc, mpsc},
    thread::JoinHandle,
};

use insight::{
    GroundedMeetingNotesReport, GroundingInput, MeetingNotesGenerator, NotesOverlayOperation,
    PresentedNotesDocument, RecordingNotes, ScreenConsultation, ScreenConsultationLog,
    SourceStatus, append_notes_overlay_operation, load_latest_grounded_notes_status,
    load_presented_notes_document,
};
use providers::backend::ObservedRequestNormalization;
use providers::{BackendFingerprint, ReasoningProvider, ResolvedBackend};
use rag::{SessionSummary, Store};
use screen::ScreenInspectionSource;
use sotto_core::CancellationToken;
use sotto_core::SessionId;

use crate::persistence_runtime::block_on;
use crate::reasoning::inspection::{ScreenInspectorAssembly, product_screen_inspectors};

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum NotesState {
    NoMeeting,
    Disabled,
    Generating,
    Ready {
        notes: Box<RecordingNotes>,
        document: Box<PresentedNotesDocument>,
        bundle: mcp::ContextBundle,
        source_status: SourceStatus,
        cached: bool,
        model: String,
        /// Controls the selected backend could not honor while producing this run.
        /// Cached reports retain the same run qualification as fresh reports.
        normalizations: Vec<ObservedRequestNormalization>,
        screen_consultations: Vec<ScreenConsultation>,
    },
    Stale {
        notes: Box<RecordingNotes>,
        document: Box<PresentedNotesDocument>,
        bundle: mcp::ContextBundle,
        source_status: SourceStatus,
        model: String,
        /// Controls the selected backend could not honor while producing this cached run.
        normalizations: Vec<ObservedRequestNormalization>,
        screen_consultations: Vec<ScreenConsultation>,
    },
    Failed(String),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NotesSnapshot {
    pub meetings: Vec<SessionSummary>,
    pub selected_session: Option<SessionId>,
    pub state: NotesState,
    /// Every screen frame the last notes run for the selected meeting consulted. Empty is the
    /// common and default case: a transcript that needs no visual context inspects nothing.
    pub screen_consultations: Vec<ScreenConsultation>,
}

struct GenerationResult {
    generation: u64,
    session_id: SessionId,
    result: Result<GroundedMeetingNotesReport, String>,
    /// Consultations are reported even when the run failed; the model still looked.
    consultations: Vec<ScreenConsultation>,
}

struct PendingGeneration {
    generation: u64,
    session_id: SessionId,
    receiver: mpsc::Receiver<GenerationResult>,
    cancellation: CancellationToken,
    worker: JoinHandle<()>,
}

/// Headless state machine used by the GPUI view and deterministic tests.
pub struct NotesController {
    database: PathBuf,
    meetings: Vec<SessionSummary>,
    selected_session: Option<SessionId>,
    state: NotesState,
    generation: u64,
    pending: Option<PendingGeneration>,
    retired_workers: Vec<JoinHandle<()>>,
    screen_inspectors: Arc<dyn ScreenInspectorAssembly>,
    screen_consultations: Vec<ScreenConsultation>,
}

impl NotesController {
    #[must_use]
    pub fn new(database: PathBuf) -> Self {
        let screen_inspectors = Arc::new(product_screen_inspectors(database.clone()));
        Self {
            database,
            meetings: Vec::new(),
            selected_session: None,
            state: NotesState::NoMeeting,
            generation: 0,
            pending: None,
            retired_workers: Vec::new(),
            screen_inspectors,
            screen_consultations: Vec::new(),
        }
    }

    /// Replaces the recording-backed inspector assembly, keeping the rest of the runtime intact.
    #[must_use]
    pub fn with_screen_inspectors(mut self, assembly: Arc<dyn ScreenInspectorAssembly>) -> Self {
        self.screen_inspectors = assembly;
        self
    }

    /// Screen frames the last completed run for the selected meeting consulted.
    #[must_use]
    pub fn screen_consultations(&self) -> &[ScreenConsultation] {
        &self.screen_consultations
    }

    pub fn refresh_catalogue(&mut self) -> Result<(), String> {
        if let Some(parent) = self
            .database
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            std::fs::create_dir_all(parent).map_err(|error| {
                format!(
                    "Could not prepare meeting storage directory {}: {error}",
                    parent.display()
                )
            })?;
        }
        self.meetings =
            block_on(async { Store::open(&self.database).await?.list_sessions().await })
                .map_err(|error| error.to_string())?;
        if self.selected_session.is_none() {
            self.selected_session = self
                .meetings
                .iter()
                .find(|meeting| meeting.ended_at_unix_ms.is_some())
                .map(|meeting| meeting.id);
        }
        self.reset_selected_state();
        Ok(())
    }

    pub fn select(&mut self, session_id: SessionId, reasoning_enabled: bool) -> bool {
        self.reap_retired_workers();
        if !self.meetings.iter().any(|meeting| meeting.id == session_id) {
            return false;
        }
        self.invalidate_pending();
        self.selected_session = Some(session_id);
        // Consultations belong to one meeting's run and must never follow the user to another.
        self.screen_consultations.clear();
        self.reset_selected_state();
        if reasoning_enabled && matches!(self.state, NotesState::Disabled) {
            self.state = NotesState::Failed(
                "Notes have not been generated for this meeting yet.".to_owned(),
            );
        }
        true
    }

    pub fn set_reasoning_enabled(&mut self, enabled: bool) {
        self.reap_retired_workers();
        if self.selected_session.is_none() {
            self.state = NotesState::NoMeeting;
        } else if !enabled {
            self.invalidate_pending();
            if !matches!(
                self.state,
                NotesState::Ready { .. } | NotesState::Stale { .. }
            ) {
                self.state = NotesState::Disabled;
            }
        } else if matches!(self.state, NotesState::Disabled) {
            self.state = NotesState::Failed(
                "Notes have not been generated for this meeting yet.".to_owned(),
            );
        }
    }

    pub fn start_generation(
        &mut self,
        backend: ResolvedBackend,
        grounding: Option<crate::mcp::FrozenGrounding>,
    ) -> Result<(), String> {
        self.reap_retired_workers();
        let session_id = self
            .selected_session
            .ok_or_else(|| "Select a completed meeting first.".to_owned())?;
        if grounding
            .as_ref()
            .is_some_and(|grounding| grounding.session_id != session_id)
        {
            return Err("Source choices belong to a different meeting.".to_owned());
        }
        if self.pending.is_some() {
            return Err("Notes generation is already running.".to_owned());
        }
        self.generation = self.generation.saturating_add(1);
        let generation = self.generation;
        let database = self.database.clone();
        let provider = backend.provider();
        let fingerprint = backend.cache_fingerprint().clone();
        // The recording is resolved inside the inspector, per request, not captured here: a
        // recording pruned or deleted between now and the model's question must still be honest.
        let (inspector, consultations) = self.screen_inspectors.assemble(session_id);
        self.screen_consultations.clear();
        let (sender, receiver) = mpsc::sync_channel(1);
        let cancellation = CancellationToken::new();
        let worker_cancellation = cancellation.clone();
        let worker = std::thread::Builder::new()
            .name("sotto-meeting-notes".to_owned())
            .spawn(move || {
                let result = run_generation(
                    &database,
                    session_id,
                    provider,
                    fingerprint,
                    grounding,
                    inspector,
                    consultations.clone(),
                    worker_cancellation,
                );
                let _ = sender.send(GenerationResult {
                    generation,
                    session_id,
                    result,
                    consultations: consultations.entries(),
                });
            })
            .map_err(|error| format!("Could not start notes generation: {error}"))?;
        self.pending = Some(PendingGeneration {
            generation,
            session_id,
            receiver,
            cancellation,
            worker,
        });
        self.state = NotesState::Generating;
        Ok(())
    }

    /// Applies a completed worker result only if both generation and session still match.
    pub fn poll(&mut self) -> bool {
        self.reap_retired_workers();
        let Some(pending) = self.pending.take() else {
            return false;
        };
        let result = match pending.receiver.try_recv() {
            Ok(result) => result,
            Err(mpsc::TryRecvError::Empty) => {
                self.pending = Some(pending);
                return false;
            }
            Err(mpsc::TryRecvError::Disconnected) => GenerationResult {
                generation: pending.generation,
                session_id: pending.session_id,
                result: Err("Notes worker stopped unexpectedly. Retry generation.".to_owned()),
                consultations: Vec::new(),
            },
        };
        self.retired_workers.push(pending.worker);
        self.reap_retired_workers();
        if result.generation != self.generation || Some(result.session_id) != self.selected_session
        {
            return false;
        }
        self.state = match result.result.and_then(|report| {
            self.screen_consultations = report.screen_consultations.clone();
            let document = self.presented_document(result.session_id, &report.artifact)?;
            Ok((report, document))
        }) {
            Ok((report, document)) => NotesState::Ready {
                notes: Box::new(report.artifact),
                document: Box::new(document),
                bundle: report.bundle,
                source_status: report.source_status,
                cached: report.cached,
                model: report.model,
                normalizations: report.normalizations,
                screen_consultations: report.screen_consultations,
            },
            Err(error) => NotesState::Failed(error),
        };
        if matches!(self.state, NotesState::Failed(_)) {
            self.screen_consultations = result.consultations;
        }
        true
    }

    #[must_use]
    pub fn snapshot(&self) -> NotesSnapshot {
        NotesSnapshot {
            meetings: self.meetings.clone(),
            selected_session: self.selected_session,
            state: self.state.clone(),
            screen_consultations: self.screen_consultations.clone(),
        }
    }

    pub fn append_overlay_operation(
        &mut self,
        operation: &NotesOverlayOperation,
        created_at_unix_ms: u64,
    ) -> Result<(), String> {
        let session_id = self
            .selected_session
            .ok_or_else(|| "Select a recording first.".to_owned())?;
        let artifact = match &self.state {
            NotesState::Ready { notes, .. } | NotesState::Stale { notes, .. } => notes.clone(),
            _ => return Err("Generate a summary before editing its blocks.".to_owned()),
        };
        let document = block_on(async {
            let store = Store::open(&self.database).await?;
            let entry_id = store.entry_for_session(session_id).await?;
            append_notes_overlay_operation(
                &store,
                entry_id,
                &artifact,
                operation,
                created_at_unix_ms,
            )
            .await
            .map_err(|error| sotto_core::RagError::Storage(error.to_string()))?;
            load_presented_notes_document(&store, entry_id, &artifact)
                .await
                .map_err(|error| sotto_core::RagError::Storage(error.to_string()))
        })
        .map_err(|error| error.to_string())?;
        match &mut self.state {
            NotesState::Ready {
                document: current, ..
            }
            | NotesState::Stale {
                document: current, ..
            } => **current = document,
            _ => {
                return Err("The selected summary changed while its edit was saved.".to_owned());
            }
        }
        Ok(())
    }

    fn presented_document(
        &self,
        session_id: SessionId,
        artifact: &RecordingNotes,
    ) -> Result<PresentedNotesDocument, String> {
        block_on(async {
            let store = Store::open(&self.database).await?;
            let entry_id = store.entry_for_session(session_id).await?;
            load_presented_notes_document(&store, entry_id, artifact)
                .await
                .map_err(|error| sotto_core::RagError::Storage(error.to_string()))
        })
        .map_err(|error| error.to_string())
    }

    fn invalidate_pending(&mut self) {
        self.generation = self.generation.saturating_add(1);
        if let Some(pending) = self.pending.take() {
            pending.cancellation.cancel();
            self.retired_workers.push(pending.worker);
        }
        self.reap_retired_workers();
    }

    fn reap_retired_workers(&mut self) {
        let mut still_running = Vec::new();
        for worker in self.retired_workers.drain(..) {
            if worker.is_finished() {
                let _ = worker.join();
            } else {
                still_running.push(worker);
            }
        }
        self.retired_workers = still_running;
    }

    fn reset_selected_state(&mut self) {
        let Some(session_id) = self.selected_session else {
            self.state = NotesState::NoMeeting;
            return;
        };
        let loaded = block_on(async {
            let store = Store::open(&self.database).await?;
            let cached = load_latest_grounded_notes_status(&store, session_id)
                .await
                .map_err(|error| sotto_core::RagError::Storage(error.to_string()))?;
            let Some(cached) = cached else {
                return Ok::<_, sotto_core::RagError>(None);
            };
            let entry_id = store.entry_for_session(session_id).await?;
            let document = load_presented_notes_document(&store, entry_id, &cached.report.artifact)
                .await
                .map_err(|error| sotto_core::RagError::Storage(error.to_string()))?;
            Ok(Some((cached, document)))
        });
        self.screen_consultations = loaded
            .as_ref()
            .ok()
            .and_then(Option::as_ref)
            .map_or_else(Vec::new, |(cached, _)| {
                cached.report.screen_consultations.clone()
            });
        self.state = match loaded {
            Ok(Some((cached, document))) if cached.stale => NotesState::Stale {
                notes: Box::new(cached.report.artifact),
                document: Box::new(document),
                bundle: cached.report.bundle,
                source_status: cached.report.source_status,
                model: cached.report.model,
                normalizations: cached.report.normalizations,
                screen_consultations: cached.report.screen_consultations,
            },
            Ok(Some((cached, document))) => NotesState::Ready {
                notes: Box::new(cached.report.artifact),
                document: Box::new(document),
                bundle: cached.report.bundle,
                source_status: cached.report.source_status,
                cached: true,
                model: cached.report.model,
                normalizations: cached.report.normalizations,
                screen_consultations: cached.report.screen_consultations,
            },
            Ok(None) => NotesState::Disabled,
            Err(_) => NotesState::Failed(
                "Saved notes evidence failed integrity checks and cannot be displayed.".to_owned(),
            ),
        };
    }
}

impl Drop for NotesController {
    fn drop(&mut self) {
        self.generation = self.generation.saturating_add(1);
        if let Some(pending) = self.pending.take() {
            pending.cancellation.cancel();
        }
        // Dropping JoinHandles detaches any non-cooperative workers. Teardown must never turn a
        // provider that ignores cancellation into an unbounded GPUI-thread join.
    }
}

#[expect(
    clippy::too_many_arguments,
    reason = "the worker boundary keeps backend identity, grounding, inspection audit, and cancellation explicit"
)]
fn run_generation(
    database: &std::path::Path,
    session_id: SessionId,
    provider: Arc<dyn ReasoningProvider>,
    fingerprint: BackendFingerprint,
    grounding: Option<crate::mcp::FrozenGrounding>,
    screen_inspector: Arc<dyn ScreenInspectionSource>,
    screen_consultation_log: ScreenConsultationLog,
    cancellation: CancellationToken,
) -> Result<GroundedMeetingNotesReport, String> {
    block_on(async {
        let store = Store::open(database)
            .await
            .map_err(|error| error.to_string())?;
        MeetingNotesGenerator::new(&store, provider.clone())
            .with_reasoning_provider(provider)
            .with_backend_fingerprint(fingerprint)
            .with_screen_inspector(screen_inspector)
            .with_screen_consultation_log(screen_consultation_log)
            .generate_grounded_with_cancellation(
                session_id,
                grounding.map(|grounding| GroundingInput {
                    grant: grounding.grant,
                    grant_fingerprint: grounding.fingerprint,
                    source: grounding.source,
                }),
                cancellation,
            )
            .await
            .map_err(|error| error.to_string())
    })
}

#[cfg(test)]
mod tests {
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    };

    use futures_util::stream;
    use providers::backend::{ObservedRequestNormalization, RequestNormalization, SamplingControl};
    use providers::{
        AuthKind, AuthStatus, BackendCapabilities, BackendDescriptor, BackendId, ReasoningSurface,
        Registry,
    };
    use rag::Store;
    use sotto_core::{
        BoxFuture, BoxStream, CaptureTarget, CompletionProvider, CompletionRequest, Delta,
        EventPayload, ProviderError, Session, SessionId, Source, StopReason, TargetKind,
        TimelineBuilder, Usage, Utterance,
    };

    use super::{
        GenerationResult, MeetingNotesGenerator, NotesController, NotesState, PendingGeneration,
    };

    fn wait_for_flag(flag: &AtomicBool) -> bool {
        let deadline = std::time::Instant::now() + std::time::Duration::from_millis(100);
        while std::time::Instant::now() < deadline {
            if flag.load(Ordering::Acquire) {
                return true;
            }
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        flag.load(Ordering::Acquire)
    }

    async fn controller_with_meetings()
    -> Result<(tempfile::TempDir, NotesController), Box<dyn std::error::Error>> {
        let directory = tempfile::tempdir()?;
        let database = directory.path().join("sotto.sqlite3");
        let store = Store::open(&database).await?;
        for (id, started, ended) in [(1, 10, Some(20)), (2, 30, Some(40))] {
            let mut session = Session::new(
                SessionId::new(id),
                CaptureTarget {
                    bundle_id: None,
                    display_name: format!("Meeting {id}"),
                    window_title: None,
                    kind: TargetKind::Window,
                    audio_scoped: true,
                },
                started,
            );
            if let Some(ended) = ended {
                session.end(ended);
            }
            store.save_session(&session).await?;
        }
        let mut controller = NotesController::new(database);
        controller.refresh_catalogue()?;
        Ok((directory, controller))
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_fresh_result_keeps_backend_downgrades_for_the_summary_view()
    -> Result<(), Box<dyn std::error::Error>> {
        let (_directory, mut controller) = controller_with_meetings().await?;
        let normalization = ObservedRequestNormalization {
            dispatch_id: 17,
            normalization: RequestNormalization {
                backend_id: BackendId::new("test.notes")?,
                control: SamplingControl::Temperature,
            },
        };
        let (sender, receiver) = std::sync::mpsc::sync_channel(1);
        let worker = std::thread::spawn({
            let normalization = normalization.clone();
            move || {
                let _ = sender.send(GenerationResult {
                    generation: 1,
                    session_id: SessionId::new(2),
                    result: Ok(insight::GroundedMeetingNotesReport {
                        artifact: insight::RecordingNotes::default(),
                        bundle: mcp::ContextBundle::empty(),
                        source_status: insight::SourceStatus::NotSelected,
                        usage: Usage::default(),
                        model: "test-model".to_owned(),
                        backend_fingerprint: "test-fingerprint".to_owned(),
                        grant_fingerprint: None,
                        cached: false,
                        calls: 1,
                        normalizations: vec![normalization],
                        screen_consultations: Vec::new(),
                    }),
                    consultations: Vec::new(),
                });
            }
        });
        controller.generation = 1;
        controller.state = NotesState::Generating;
        controller.pending = Some(PendingGeneration {
            generation: 1,
            session_id: SessionId::new(2),
            receiver,
            cancellation: sotto_core::CancellationToken::new(),
            worker,
        });

        while !controller.poll() {
            std::thread::yield_now();
        }

        let NotesState::Ready { normalizations, .. } = controller.snapshot().state else {
            return Err("fresh notes result did not become ready".into());
        };
        assert_eq!(normalizations, vec![normalization]);
        Ok(())
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn downgraded_summary_keeps_its_qualification_after_reopen()
    -> Result<(), Box<dyn std::error::Error>> {
        let directory = tempfile::tempdir()?;
        let database = directory.path().join("sotto.sqlite3");
        let store = Store::open(&database).await?;
        let mut session = Session::new(
            SessionId::new(96),
            CaptureTarget {
                bundle_id: None,
                display_name: "Meeting".to_owned(),
                window_title: None,
                kind: TargetKind::Window,
                audio_scoped: true,
            },
            1,
        );
        session.end(2);
        store.save_session(&session).await?;
        let mut timeline = TimelineBuilder::new(session);
        timeline.append(
            std::time::Duration::from_secs(1),
            EventPayload::UtteranceFinal(Utterance {
                source: Source::Mic,
                start: std::time::Duration::from_secs(1),
                end: std::time::Duration::from_secs(2),
                text: "Plan".to_owned(),
                avg_logprob: 0.0,
                annotations: Vec::new(),
            }),
        );
        store.append_events(timeline.events()).await?;

        let backend_id = BackendId::new("test.downgraded-reopen")?;
        let descriptor = BackendDescriptor::new(
            backend_id.clone(),
            "Downgraded reopen",
            "replay-model",
            1,
            BackendCapabilities::reasoning_baseline(),
            AuthKind::None,
            AuthStatus::Ready,
        )?;
        let mut registry = Registry::default();
        registry.register(descriptor, Arc::new(ReplayProvider(AtomicBool::new(false))))?;
        registry.select(ReasoningSurface::Notes, Some(&backend_id))?;
        let backend = registry
            .resolve(ReasoningSurface::Notes)?
            .ok_or("summarizer resolution missing")?;
        let fresh =
            MeetingNotesGenerator::new(&store, Arc::new(ReplayProvider(AtomicBool::new(false))))
                .with_reasoning_provider(backend.provider())
                .with_backend_fingerprint(backend.cache_fingerprint().clone())
                .generate_grounded_with_cancellation(
                    SessionId::new(96),
                    None,
                    sotto_core::CancellationToken::new(),
                )
                .await?;
        assert!(!fresh.normalizations.is_empty());
        drop(store);

        let mut reopened = NotesController::new(database);
        reopened.refresh_catalogue()?;
        let NotesState::Ready {
            cached,
            normalizations,
            ..
        } = reopened.snapshot().state
        else {
            return Err("reopened downgraded notes did not remain ready".into());
        };
        assert!(cached);
        assert_eq!(normalizations, fresh.normalizations);
        Ok(())
    }

    #[test]
    fn catalogue_prepares_a_missing_database_directory() -> Result<(), Box<dyn std::error::Error>> {
        let directory = tempfile::tempdir()?;
        let database = directory.path().join("nested").join("sotto.sqlite3");
        let mut controller = NotesController::new(database.clone());

        controller.refresh_catalogue()?;

        assert!(
            database.exists(),
            "catalogue refresh must create its database"
        );
        assert!(
            controller.snapshot().meetings.is_empty(),
            "a new database must begin with an empty meeting catalogue"
        );
        Ok(())
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn catalogue_selects_newest_completed_meeting_without_starting_reasoning()
    -> Result<(), Box<dyn std::error::Error>> {
        let (_directory, controller) = controller_with_meetings().await?;
        let snapshot = controller.snapshot();
        assert_eq!(snapshot.meetings.len(), 2);
        assert_eq!(snapshot.selected_session, Some(SessionId::new(2)));
        assert_eq!(snapshot.state, NotesState::Disabled);
        Ok(())
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn session_switch_fences_stale_worker_result() -> Result<(), Box<dyn std::error::Error>> {
        let (_directory, mut controller) = controller_with_meetings().await?;
        let (_sender, receiver) = std::sync::mpsc::sync_channel(1);
        let cancellation = sotto_core::CancellationToken::new();
        let worker_cancellation = cancellation.clone();
        let observed = Arc::new(AtomicBool::new(false));
        let worker_observed = observed.clone();
        let worker = std::thread::spawn(move || {
            while !worker_cancellation.is_cancelled() {
                std::thread::yield_now();
            }
            worker_observed.store(true, Ordering::Release);
        });
        controller.generation = 7;
        controller.pending = Some(PendingGeneration {
            generation: 7,
            session_id: SessionId::new(2),
            receiver,
            cancellation,
            worker,
        });
        controller.state = NotesState::Generating;
        assert!(controller.select(SessionId::new(1), false));
        assert!(!controller.poll());
        assert!(wait_for_flag(&observed));
        let _ = controller.poll();
        assert!(controller.retired_workers.is_empty());
        assert_eq!(
            controller.snapshot().selected_session,
            Some(SessionId::new(1))
        );
        assert_eq!(controller.snapshot().state, NotesState::Disabled);
        Ok(())
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn disabling_reasoning_cancels_and_reaps_the_worker()
    -> Result<(), Box<dyn std::error::Error>> {
        let (_directory, mut controller) = controller_with_meetings().await?;
        let (_sender, receiver) = std::sync::mpsc::sync_channel(1);
        let cancellation = sotto_core::CancellationToken::new();
        let worker_cancellation = cancellation.clone();
        let observed = Arc::new(AtomicBool::new(false));
        let worker_observed = observed.clone();
        let worker = std::thread::spawn(move || {
            while !worker_cancellation.is_cancelled() {
                std::thread::yield_now();
            }
            worker_observed.store(true, Ordering::Release);
        });
        controller.pending = Some(PendingGeneration {
            generation: 1,
            session_id: SessionId::new(2),
            receiver,
            cancellation,
            worker,
        });
        controller.state = NotesState::Generating;

        controller.set_reasoning_enabled(false);

        assert!(wait_for_flag(&observed));
        let deadline = std::time::Instant::now() + std::time::Duration::from_millis(100);
        while std::time::Instant::now() < deadline {
            let _ = controller.poll();
            if controller.retired_workers.is_empty() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        assert!(controller.retired_workers.is_empty());
        assert_eq!(controller.snapshot().state, NotesState::Disabled);
        Ok(())
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn dropping_controller_cancels_without_waiting_for_a_non_cooperative_worker()
    -> Result<(), Box<dyn std::error::Error>> {
        let (_directory, mut controller) = controller_with_meetings().await?;
        let (_sender, receiver) = std::sync::mpsc::sync_channel(1);
        let cancellation = sotto_core::CancellationToken::new();
        let cancellation_observer = cancellation.clone();
        // Two seconds, against a 500 ms budget below. The property is "does not join", and the
        // margin has to survive a loaded `--workspace` run: at 200 ms against 50 ms this failed
        // roughly one full run in three while the non-blocking path itself was merely slow.
        let worker = std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_secs(2));
        });
        controller.pending = Some(PendingGeneration {
            generation: 1,
            session_id: SessionId::new(2),
            receiver,
            cancellation,
            worker,
        });

        let started = std::time::Instant::now();
        drop(controller);

        assert!(cancellation_observer.is_cancelled());
        assert!(started.elapsed() < std::time::Duration::from_millis(500));
        Ok(())
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn session_switch_does_not_join_a_non_cooperative_worker()
    -> Result<(), Box<dyn std::error::Error>> {
        let (_directory, mut controller) = controller_with_meetings().await?;
        let (sender, receiver) = std::sync::mpsc::sync_channel(1);
        let cancellation = sotto_core::CancellationToken::new();
        let cancellation_observer = cancellation.clone();
        // Two seconds, against the 500 ms budget below — see the sibling test for why the margin
        // is this wide on both sides.
        let worker = std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_secs(2));
            let _ = sender.send(GenerationResult {
                generation: 1,
                session_id: SessionId::new(2),
                result: Err("stale".to_owned()),
                consultations: Vec::new(),
            });
        });
        controller.generation = 1;
        controller.pending = Some(PendingGeneration {
            generation: 1,
            session_id: SessionId::new(2),
            receiver,
            cancellation,
            worker,
        });
        controller.state = NotesState::Generating;

        let started = std::time::Instant::now();
        assert!(controller.select(SessionId::new(1), false));

        assert!(started.elapsed() < std::time::Duration::from_millis(500));
        assert!(cancellation_observer.is_cancelled());
        assert!(!controller.poll());
        assert_eq!(controller.snapshot().state, NotesState::Disabled);
        Ok(())
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn disabled_and_retry_states_are_explicit() -> Result<(), Box<dyn std::error::Error>> {
        let (_directory, mut controller) = controller_with_meetings().await?;
        assert_eq!(controller.snapshot().state, NotesState::Disabled);
        controller.set_reasoning_enabled(true);
        assert!(matches!(controller.snapshot().state, NotesState::Failed(_)));
        controller.set_reasoning_enabled(false);
        assert_eq!(controller.snapshot().state, NotesState::Disabled);
        Ok(())
    }

    struct ReplayProvider(AtomicBool);

    impl CompletionProvider for ReplayProvider {
        fn stream(
            &self,
            _request: CompletionRequest,
            _cancellation: sotto_core::CancellationToken,
        ) -> BoxFuture<'_, Result<BoxStream<'static, Result<Delta, ProviderError>>, ProviderError>>
        {
            self.0.store(true, Ordering::Release);
            Box::pin(async {
                Ok(Box::pin(stream::iter([Ok(Delta {
                    text: r#"{"sections":[{"kind":"overview","blocks":[{"type":"claim","text":"Planning","meeting_citations":[1],"external_citations":[]}]}]}"#.to_owned(),
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

    #[tokio::test(flavor = "multi_thread")]
    async fn cold_reopen_stays_ready_when_reasoning_is_disabled_without_new_provider_work()
    -> Result<(), Box<dyn std::error::Error>> {
        let directory = tempfile::tempdir()?;
        let database = directory.path().join("sotto.sqlite3");
        let store = Store::open(&database).await?;
        let mut session = Session::new(
            SessionId::new(9),
            CaptureTarget {
                bundle_id: None,
                display_name: "Meeting".to_owned(),
                window_title: None,
                kind: TargetKind::Window,
                audio_scoped: true,
            },
            1,
        );
        session.end(2);
        store.save_session(&session).await?;
        let mut timeline = TimelineBuilder::new(session);
        timeline.append(
            std::time::Duration::from_secs(1),
            EventPayload::UtteranceFinal(Utterance {
                source: Source::Mic,
                start: std::time::Duration::from_secs(1),
                end: std::time::Duration::from_secs(2),
                text: "Plan".to_owned(),
                avg_logprob: 0.0,
                annotations: Vec::new(),
            }),
        );
        store.append_events(timeline.events()).await?;
        let provider = Arc::new(ReplayProvider(AtomicBool::new(false)));
        let fingerprint = BackendDescriptor::new(
            BackendId::new("test.replay")?,
            "Replay",
            "replay-model",
            1,
            BackendCapabilities::reasoning_baseline(),
            AuthKind::None,
            AuthStatus::Ready,
        )?
        .fingerprint()
        .clone();
        MeetingNotesGenerator::new(&store, provider.clone())
            .with_backend_fingerprint(fingerprint)
            .generate_grounded_with_cancellation(
                SessionId::new(9),
                None,
                sotto_core::CancellationToken::new(),
            )
            .await?;
        assert!(provider.0.load(Ordering::Acquire));
        provider.0.store(false, Ordering::Release);

        let mut controller = NotesController::new(database);
        controller.refresh_catalogue()?;
        let NotesState::Ready { normalizations, .. } = controller.snapshot().state else {
            return Err("clean reopened notes did not remain ready".into());
        };
        assert!(normalizations.is_empty());
        controller.set_reasoning_enabled(false);
        assert!(matches!(
            controller.snapshot().state,
            NotesState::Ready { .. }
        ));
        assert!(!provider.0.load(Ordering::Acquire));
        Ok(())
    }

    mod screen {
        use std::{
            collections::VecDeque,
            path::Path,
            sync::{
                Arc, Mutex, MutexGuard, PoisonError,
                atomic::{AtomicUsize, Ordering},
            },
            time::Duration,
        };

        use providers::{
            AuthKind, AuthStatus, BackendCapabilities, BackendDescriptor, BackendId,
            ReasoningProvider, ReasoningSurface, Registry, ResolvedBackend,
        };
        use rag::Store;
        use screen::{
            AuthorizedReasoningImage, Frame, OcrEngine, RecordingFrameDecoder, ScreenError,
            ScreenInspectionSource,
        };
        use sotto_core::{
            BoxFuture, BoxStream, CancellationToken, CaptureTarget, CompletionProvider,
            CompletionRequest, Delta, EventId, EventPayload, ProviderError, ReasoningRequest,
            Session, SessionId, Source, StopReason, TargetKind, TimelineBuilder, Usage, Utterance,
            types::{MediaTimeMapping, RecordingContainer, SessionRecording},
        };

        use crate::notes::{NotesController, NotesState};
        use crate::reasoning::inspection::{
            ConsultationOutcome, ConsultedPrecision, RecordingScreenInspectors, RecordingSource,
            ScreenConsultationLog, ScreenInspectorAssembly,
        };

        type TestResult = Result<(), Box<dyn std::error::Error>>;

        const ONE_PIXEL_PNG: &[u8] = &[
            0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48,
            0x44, 0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00,
            0x00, 0x1f, 0x15, 0xc4, 0x89, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x44, 0x41, 0x54, 0x78,
            0xda, 0x63, 0x60, 0x67, 0x60, 0xf8, 0x0f, 0x00, 0x01, 0x20, 0x01, 0x07, 0x45, 0xfa,
            0xc7, 0x0d, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4e, 0x44, 0xae, 0x42, 0x60, 0x82,
        ];

        fn locked<T>(value: &Mutex<T>) -> MutexGuard<'_, T> {
            value.lock().unwrap_or_else(PoisonError::into_inner)
        }

        /// One reasoning turn as it left the app: the serialized request plus whether an image
        /// was handed to the transport at all.
        struct CapturedTurn {
            serialized_request: String,
            image_attached: bool,
        }

        /// Replays queued model turns and records exactly what the transport received.
        struct CapturingProvider {
            outputs: Mutex<VecDeque<String>>,
            turns: Arc<Mutex<Vec<CapturedTurn>>>,
        }

        impl CapturingProvider {
            fn new(outputs: impl IntoIterator<Item = String>) -> Self {
                Self {
                    outputs: Mutex::new(outputs.into_iter().collect()),
                    turns: Arc::new(Mutex::new(Vec::new())),
                }
            }
        }

        impl CompletionProvider for CapturingProvider {
            fn stream(
                &self,
                _request: CompletionRequest,
                _cancellation: CancellationToken,
            ) -> BoxFuture<
                '_,
                Result<BoxStream<'static, Result<Delta, ProviderError>>, ProviderError>,
            > {
                Box::pin(async {
                    Err(ProviderError::InvalidRequest(
                        "reasoning must dispatch through the advanced seam".to_owned(),
                    ))
                })
            }

            fn model_id(&self) -> &str {
                "replay-model"
            }
        }

        impl ReasoningProvider for CapturingProvider {
            fn stream_advanced_reasoning(
                &self,
                request: ReasoningRequest,
                image: Option<AuthorizedReasoningImage>,
                _cancellation: CancellationToken,
            ) -> BoxFuture<
                '_,
                Result<BoxStream<'static, Result<Delta, ProviderError>>, ProviderError>,
            > {
                let serialized = serde_json::to_string(&request.completion);
                let output = locked(&self.outputs).pop_front();
                let turns = Arc::clone(&self.turns);
                Box::pin(async move {
                    let serialized_request =
                        serialized.map_err(|error| ProviderError::Decode(error.to_string()))?;
                    locked(&turns).push(CapturedTurn {
                        serialized_request,
                        image_attached: image.is_some(),
                    });
                    let output = output.ok_or_else(|| {
                        ProviderError::Network("missing queued model turn".to_owned())
                    })?;
                    Ok(Box::pin(futures_util::stream::iter([Ok(Delta {
                        text: output,
                        is_final: true,
                        usage: Some(Usage::default()),
                        stop_reason: Some(StopReason::EndTurn),
                    })]))
                        as BoxStream<'static, Result<Delta, ProviderError>>)
                })
            }
        }

        #[derive(Clone)]
        struct CountingDecoder(Arc<AtomicUsize>);

        impl RecordingFrameDecoder for CountingDecoder {
            fn decode_png(
                &self,
                _path: &Path,
                requested: Duration,
            ) -> Result<(Vec<u8>, Duration), ScreenError> {
                self.0.fetch_add(1, Ordering::Relaxed);
                Ok((
                    ONE_PIXEL_PNG.to_vec(),
                    requested + Duration::from_millis(25),
                ))
            }
        }

        #[derive(Clone)]
        struct CountingOcr(Arc<AtomicUsize>);

        impl OcrEngine for CountingOcr {
            fn recognize(&self, _frame: &Frame) -> Result<String, ScreenError> {
                self.0.fetch_add(1, Ordering::Relaxed);
                Ok("Pricing".to_owned())
            }
        }

        struct FixedRecording(Option<SessionRecording>);

        impl RecordingSource for FixedRecording {
            fn recording(
                &self,
                _session_id: SessionId,
            ) -> Result<Option<SessionRecording>, String> {
                Ok(self.0.clone())
            }
        }

        /// Wraps the product assembly so a test can prove an inspector was really handed over.
        struct CountingAssembly {
            inner: RecordingScreenInspectors<FixedRecording, CountingDecoder, CountingOcr>,
            assembled: Arc<AtomicUsize>,
        }

        impl ScreenInspectorAssembly for CountingAssembly {
            fn assemble(
                &self,
                session_id: SessionId,
            ) -> (Arc<dyn ScreenInspectionSource>, ScreenConsultationLog) {
                self.assembled.fetch_add(1, Ordering::Relaxed);
                self.inner.assemble(session_id)
            }
        }

        struct Fixture {
            _directory: tempfile::TempDir,
            database: std::path::PathBuf,
            session_id: SessionId,
            event_id: EventId,
            recording_path: std::path::PathBuf,
        }

        async fn fixture() -> Result<Fixture, Box<dyn std::error::Error>> {
            let directory = tempfile::tempdir()?;
            let database = directory.path().join("sotto.sqlite3");
            let session_id = SessionId::new(59);
            let mut record = Session::new(
                session_id,
                CaptureTarget {
                    bundle_id: None,
                    display_name: "Meeting".to_owned(),
                    window_title: Some("Pricing".to_owned()),
                    kind: TargetKind::Window,
                    audio_scoped: true,
                },
                1,
            );
            record.end(2);
            let store = Store::open(&database).await?;
            store.save_session(&record).await?;
            let mut timeline = TimelineBuilder::new(record);
            let event = timeline.append(
                Duration::from_secs(8),
                EventPayload::UtteranceFinal(Utterance {
                    source: Source::System,
                    start: Duration::from_secs(8),
                    end: Duration::from_secs(9),
                    text: "Look at the number on the slide".to_owned(),
                    avg_logprob: -0.1,
                    annotations: Vec::new(),
                }),
            );
            store.append_events(timeline.events()).await?;
            let recording_path = directory.path().join("recording.mp4");
            std::fs::write(&recording_path, b"contract-only; not real media")?;
            Ok(Fixture {
                _directory: directory,
                database,
                session_id,
                event_id: event.id(),
                recording_path,
            })
        }

        fn available(fixture: &Fixture) -> SessionRecording {
            SessionRecording::Available {
                session_id: fixture.session_id,
                path: fixture.recording_path.to_string_lossy().into_owned(),
                container: RecordingContainer::Mp4,
                duration: Duration::from_secs(60),
                byte_size: 29,
                time_mapping: MediaTimeMapping::IDENTITY,
            }
        }

        fn notes_json(event_id: EventId) -> String {
            format!(
                r#"{{"sections":[{{"kind":"overview","blocks":[{{"type":"claim","text":"The slide showed the revised number","meeting_citations":[{}],"external_citations":[]}}]}}]}}"#,
                event_id.get()
            )
        }

        fn inspect_action(event_id: EventId, evidence: &str) -> String {
            format!(
                r#"{{"action":"inspect_screen","event_id":{},"evidence":"{evidence}","reason":"read the number on the cited slide"}}"#,
                event_id.get()
            )
        }

        fn resolved(
            provider: Arc<CapturingProvider>,
        ) -> Result<ResolvedBackend, Box<dyn std::error::Error>> {
            let id = BackendId::new("test.replay")?;
            let descriptor = BackendDescriptor::new(
                id.clone(),
                "Replay",
                "replay-model",
                1,
                BackendCapabilities::reasoning_baseline(),
                AuthKind::None,
                AuthStatus::Ready,
            )?;
            let mut registry = Registry::default();
            registry.register_reasoning(descriptor, provider)?;
            registry.select(ReasoningSurface::Notes, Some(&id))?;
            registry
                .resolve(ReasoningSurface::Notes)?
                .ok_or_else(|| "the test backend must resolve".into())
        }

        fn run(controller: &mut NotesController, backend: ResolvedBackend) -> Result<(), String> {
            controller.start_generation(backend, None)?;
            let deadline = std::time::Instant::now() + Duration::from_secs(10);
            while std::time::Instant::now() < deadline {
                if controller.poll() {
                    return Ok(());
                }
                std::thread::sleep(Duration::from_millis(5));
            }
            Err("notes generation did not finish".to_owned())
        }

        fn controller_for(
            fixture: &Fixture,
            recording: Option<SessionRecording>,
            decodes: &Arc<AtomicUsize>,
            ocr: &Arc<AtomicUsize>,
            assembled: &Arc<AtomicUsize>,
        ) -> Result<NotesController, Box<dyn std::error::Error>> {
            let assembly = CountingAssembly {
                inner: RecordingScreenInspectors::new(
                    FixedRecording(recording),
                    CountingDecoder(Arc::clone(decodes)),
                    Some(CountingOcr(Arc::clone(ocr))),
                ),
                assembled: Arc::clone(assembled),
            };
            let mut controller = NotesController::new(fixture.database.clone())
                .with_screen_inspectors(Arc::new(assembly));
            controller.refresh_catalogue()?;
            assert!(
                controller.select(fixture.session_id, true),
                "the fixture meeting must be selectable"
            );
            Ok(controller)
        }

        /// Every turn that left the app must be free of image bytes, whatever happened locally.
        fn assert_no_image_left_the_app(turns: &[CapturedTurn], recording_path: &Path) {
            for turn in turns {
                assert!(
                    !turn.image_attached,
                    "no reasoning turn may carry an image without the separate opt-in"
                );
                for forbidden in [
                    "iVBORw0KGgo",
                    "\\u0089PNG",
                    "data:image",
                    "base64",
                    recording_path.to_string_lossy().as_ref(),
                ] {
                    assert!(
                        !turn.serialized_request.contains(forbidden),
                        "serialized request disclosed {forbidden}"
                    );
                }
            }
        }

        #[tokio::test(flavor = "multi_thread")]
        async fn a_notes_run_pulls_one_frame_through_the_apps_own_runtime_assembly() -> TestResult {
            let fixture = fixture().await?;
            let decodes = Arc::new(AtomicUsize::new(0));
            let ocr = Arc::new(AtomicUsize::new(0));
            let assembled = Arc::new(AtomicUsize::new(0));
            let mut controller = controller_for(
                &fixture,
                Some(available(&fixture)),
                &decodes,
                &ocr,
                &assembled,
            )?;
            let provider = Arc::new(CapturingProvider::new([
                inspect_action(fixture.event_id, "local_ocr"),
                notes_json(fixture.event_id),
            ]));
            let turns = Arc::clone(&provider.turns);

            run(&mut controller, resolved(Arc::clone(&provider))?)?;

            assert!(
                matches!(controller.snapshot().state, NotesState::Ready { .. }),
                "the run must complete with cited notes"
            );
            assert_eq!(assembled.load(Ordering::Relaxed), 1, "one inspector wired");
            assert_eq!(decodes.load(Ordering::Relaxed), 1, "exactly one decode");
            assert_eq!(ocr.load(Ordering::Relaxed), 1, "exactly one local OCR pass");

            let consultations = controller.screen_consultations();
            assert_eq!(consultations.len(), 1, "one consultation was disclosed");
            assert_eq!(
                consultations[0].outcome,
                ConsultationOutcome::Frame {
                    requested_media_time: Duration::from_secs(8),
                    decoded_media_time: Duration::from_millis(8_025),
                    precision: ConsultedPrecision::DecodedVideoFrame,
                    ocr_characters: Some(7),
                },
                "the user must see the moment and the decode precision"
            );

            let turns = locked(&turns);
            assert_eq!(turns.len(), 2, "one inspection means exactly two turns");
            assert!(
                turns[1]
                    .serialized_request
                    .contains("decoded_media_time=8.025s"),
                "the second turn must carry the honest decoded timestamp"
            );
            assert!(
                turns[1].serialized_request.contains("Local OCR: Pricing"),
                "local OCR text is the evidence the model asked for"
            );
            assert_no_image_left_the_app(&turns, &fixture.recording_path);

            let mut reopened = NotesController::new(fixture.database);
            reopened.refresh_catalogue()?;
            assert_eq!(
                reopened.screen_consultations(),
                consultations,
                "a cached reopen after app restart must restore the full consultation receipt"
            );
            Ok(())
        }

        #[tokio::test(flavor = "multi_thread")]
        async fn the_fourth_screen_request_is_refused_reported_and_logged_by_the_app_runtime()
        -> TestResult {
            let fixture = fixture().await?;
            let decodes = Arc::new(AtomicUsize::new(0));
            let ocr = Arc::new(AtomicUsize::new(0));
            let assembled = Arc::new(AtomicUsize::new(0));
            let mut controller = controller_for(
                &fixture,
                Some(available(&fixture)),
                &decodes,
                &ocr,
                &assembled,
            )?;
            let inspect = inspect_action(fixture.event_id, "local_ocr");
            let provider = Arc::new(CapturingProvider::new([
                inspect.clone(),
                inspect.clone(),
                inspect.clone(),
                inspect,
                notes_json(fixture.event_id),
            ]));
            let turns = Arc::clone(&provider.turns);

            run(&mut controller, resolved(Arc::clone(&provider))?)?;

            assert!(matches!(
                controller.snapshot().state,
                NotesState::Ready { .. }
            ));
            assert_eq!(
                decodes.load(Ordering::Relaxed),
                3,
                "the fixed budget is three"
            );
            assert_eq!(ocr.load(Ordering::Relaxed), 3, "refusal performs no OCR");
            let consultations = controller.screen_consultations();
            assert_eq!(
                consultations.len(),
                4,
                "the refused request remains auditable"
            );
            assert_eq!(
                consultations.last().map(|entry| &entry.outcome),
                Some(&ConsultationOutcome::Unavailable {
                    reason: "inspection_budget_exhausted".to_owned(),
                })
            );
            let turns = locked(&turns);
            assert_eq!(
                turns.len(),
                5,
                "the refusal is returned before the final answer"
            );
            assert!(
                turns[4]
                    .serialized_request
                    .contains("reason=inspection_budget_exhausted"),
                "the model must receive the budget refusal"
            );
            assert_no_image_left_the_app(&turns, &fixture.recording_path);
            Ok(())
        }

        #[tokio::test(flavor = "multi_thread")]
        async fn a_pruned_recording_still_completes_the_run_from_the_transcript_alone() -> TestResult
        {
            for recording in [
                Some(SessionRecording::Missing {
                    session_id: SessionId::new(59),
                    reason: sotto_core::types::RecordingMissingReason::Pruned,
                }),
                // No settled row at all: deleted before the row landed, or still growing.
                None,
            ] {
                let fixture = fixture().await?;
                let decodes = Arc::new(AtomicUsize::new(0));
                let ocr = Arc::new(AtomicUsize::new(0));
                let assembled = Arc::new(AtomicUsize::new(0));
                let mut controller =
                    controller_for(&fixture, recording, &decodes, &ocr, &assembled)?;
                let provider = Arc::new(CapturingProvider::new([
                    inspect_action(fixture.event_id, "local_ocr"),
                    notes_json(fixture.event_id),
                ]));
                let turns = Arc::clone(&provider.turns);

                run(&mut controller, resolved(Arc::clone(&provider))?)?;

                assert!(
                    matches!(controller.snapshot().state, NotesState::Ready { .. }),
                    "a missing recording must degrade to transcript-only, not fail"
                );
                assert_eq!(decodes.load(Ordering::Relaxed), 0, "nothing to decode");
                assert_eq!(ocr.load(Ordering::Relaxed), 0, "nothing to recognize");
                assert!(
                    matches!(
                        controller.screen_consultations()[0].outcome,
                        ConsultationOutcome::Unavailable { .. }
                    ),
                    "the unavailable state must be surfaced, not swallowed"
                );
                let turns = locked(&turns);
                assert!(
                    turns[1]
                        .serialized_request
                        .contains("availability=unavailable"),
                    "the model must be told the frame was unavailable"
                );
                assert_no_image_left_the_app(&turns, &fixture.recording_path);
            }
            Ok(())
        }

        #[tokio::test(flavor = "multi_thread")]
        async fn an_image_request_is_refused_locally_and_no_image_reaches_the_backend() -> TestResult
        {
            let fixture = fixture().await?;
            let decodes = Arc::new(AtomicUsize::new(0));
            let ocr = Arc::new(AtomicUsize::new(0));
            let assembled = Arc::new(AtomicUsize::new(0));
            let mut controller = controller_for(
                &fixture,
                Some(available(&fixture)),
                &decodes,
                &ocr,
                &assembled,
            )?;
            let provider = Arc::new(CapturingProvider::new([
                inspect_action(fixture.event_id, "image"),
                notes_json(fixture.event_id),
            ]));
            let turns = Arc::clone(&provider.turns);

            run(&mut controller, resolved(Arc::clone(&provider))?)?;

            assert!(matches!(
                controller.snapshot().state,
                NotesState::Ready { .. }
            ));
            assert_eq!(
                decodes.load(Ordering::Relaxed),
                0,
                "a refused image request must not even decode the recording"
            );
            assert_eq!(
                controller.screen_consultations()[0].outcome,
                ConsultationOutcome::Unavailable {
                    reason: "image_opt_in_required".to_owned()
                }
            );
            let turns = locked(&turns);
            assert!(
                turns[1]
                    .serialized_request
                    .contains("reason=image_opt_in_required"),
                "the refusal must be stated to the model in the request"
            );
            assert_no_image_left_the_app(&turns, &fixture.recording_path);
            Ok(())
        }

        #[tokio::test(flavor = "multi_thread")]
        async fn a_run_needing_no_visual_context_decodes_nothing_with_the_inspector_present()
        -> TestResult {
            let fixture = fixture().await?;
            let decodes = Arc::new(AtomicUsize::new(0));
            let ocr = Arc::new(AtomicUsize::new(0));
            let assembled = Arc::new(AtomicUsize::new(0));
            let mut controller = controller_for(
                &fixture,
                Some(available(&fixture)),
                &decodes,
                &ocr,
                &assembled,
            )?;
            let provider = Arc::new(CapturingProvider::new([notes_json(fixture.event_id)]));
            let turns = Arc::clone(&provider.turns);

            run(&mut controller, resolved(Arc::clone(&provider))?)?;

            assert!(matches!(
                controller.snapshot().state,
                NotesState::Ready { .. }
            ));
            assert_eq!(
                assembled.load(Ordering::Relaxed),
                1,
                "the inspector must be present, not None, for this to mean anything"
            );
            assert_eq!(decodes.load(Ordering::Relaxed), 0, "no decode by default");
            assert_eq!(ocr.load(Ordering::Relaxed), 0, "no OCR by default");
            assert!(
                controller.screen_consultations().is_empty(),
                "the default path consults nothing and must claim nothing"
            );
            let turns = locked(&turns);
            assert_eq!(turns.len(), 1, "one turn means no inspection round trip");
            assert_no_image_left_the_app(&turns, &fixture.recording_path);
            Ok(())
        }
    }
}
