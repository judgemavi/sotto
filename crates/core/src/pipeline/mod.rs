//! Runtime wiring for Sotto's non-blocking conveyor belt.
//!
//! Raw audio uses bounded broadcast delivery. VAD and ASR drop oldest audio after lag,
//! because stale frames must never stall capture. The transcript and timeline seams are
//! bounded MPSC channels: recoverable VAD/partial updates use `try_send`, while finals
//! await capacity and are never discarded. Persistence reserves queue capacity before a
//! timeline checkpoint is drained, so transient saturation cannot lose the append-only log.
//!
//! Shutdown is deliberately producer-to-consumer: capture/audio, VAD+ASR, transcript
//! annotation, timeline, then persistence. Each consumer therefore observes upstream EOF
//! only after every producer has drained.
//!
//! The merged live view orders utterances by start, end, final before partial, system
//! before mic, then event id. This is a projection only; the append-only log is unchanged.

mod session;

use std::{
    num::NonZeroUsize,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::Duration,
};

use tokio::{
    sync::{broadcast, mpsc},
    task::JoinHandle,
};

use crate::{
    AudioFrame, BoxFuture, CaptureBackend, CaptureError, EventBus, EventPayload, PipelineError,
    ProsodyDelta, RagError, Session, Source, TimelineEvent, Transcriber, TranscriptUpdate,
    Utterance, VadSegment, VoiceActivityDetector,
};

pub use session::{CallSession, RecordingState, SessionView};

#[derive(Clone, Debug, Default)]
pub struct BackpressureCounters {
    vad_audio_dropped: Arc<AtomicU64>,
    asr_audio_dropped: Arc<AtomicU64>,
    transcript_updates_dropped: Arc<AtomicU64>,
    timeline_updates_dropped: Arc<AtomicU64>,
    transcript_queue_high_water: Arc<AtomicU64>,
    timeline_queue_high_water: Arc<AtomicU64>,
    persistence_saturated: Arc<AtomicU64>,
}

impl BackpressureCounters {
    #[must_use]
    pub fn vad_audio_dropped(&self) -> u64 {
        self.vad_audio_dropped.load(Ordering::Relaxed)
    }
    #[must_use]
    pub fn asr_audio_dropped(&self) -> u64 {
        self.asr_audio_dropped.load(Ordering::Relaxed)
    }
    #[must_use]
    pub fn transcript_updates_dropped(&self) -> u64 {
        self.transcript_updates_dropped.load(Ordering::Relaxed)
    }
    #[must_use]
    pub fn timeline_updates_dropped(&self) -> u64 {
        self.timeline_updates_dropped.load(Ordering::Relaxed)
    }
    #[must_use]
    pub fn transcript_queue_high_water(&self) -> u64 {
        self.transcript_queue_high_water.load(Ordering::Relaxed)
    }
    #[must_use]
    pub fn timeline_queue_high_water(&self) -> u64 {
        self.timeline_queue_high_water.load(Ordering::Relaxed)
    }
    #[must_use]
    pub fn persistence_saturated(&self) -> u64 {
        self.persistence_saturated.load(Ordering::Relaxed)
    }
}

#[derive(Clone, Copy, Debug)]
pub struct PipelineConfig {
    pub audio_capacity: NonZeroUsize,
    pub transcript_capacity: NonZeroUsize,
    pub timeline_capacity: NonZeroUsize,
    pub control_capacity: NonZeroUsize,
    pub event_capacity: NonZeroUsize,
    pub recent_event_capacity: NonZeroUsize,
    pub checkpoint_events: NonZeroUsize,
    /// Maximum not-yet-queued recording payloads retained during sustained storage failure.
    pub pending_persistence_events: NonZeroUsize,
    pub persistence_queue_capacity: NonZeroUsize,
    pub asr_poll_interval: Duration,
}

impl Default for PipelineConfig {
    fn default() -> Self {
        Self {
            audio_capacity: nonzero(256),
            transcript_capacity: nonzero(256),
            timeline_capacity: nonzero(512),
            control_capacity: nonzero(8),
            event_capacity: nonzero(512),
            recent_event_capacity: nonzero(512),
            checkpoint_events: nonzero(64),
            pending_persistence_events: nonzero(4_096),
            persistence_queue_capacity: nonzero(8),
            asr_poll_interval: Duration::from_millis(25),
        }
    }
}

const fn nonzero(value: usize) -> NonZeroUsize {
    match NonZeroUsize::new(value) {
        Some(value) => value,
        None => NonZeroUsize::MIN,
    }
}

pub trait TranscriptAnnotator: Send {
    fn observe_vad(&mut self, segment: &VadSegment);
    fn annotate(&mut self, utterance: &mut Utterance) -> Option<ProsodyDelta>;
}

#[derive(Default)]
pub struct NoopAnnotator;

impl TranscriptAnnotator for NoopAnnotator {
    fn observe_vad(&mut self, _segment: &VadSegment) {}
    fn annotate(&mut self, _utterance: &mut Utterance) -> Option<ProsodyDelta> {
        None
    }
}

pub trait PersistenceSink: Send + Sync {
    fn append<'a>(&'a self, events: Vec<TimelineEvent>) -> BoxFuture<'a, Result<(), RagError>>;
}

#[derive(Default)]
pub struct NoopPersistence;

impl PersistenceSink for NoopPersistence {
    fn append<'a>(&'a self, _events: Vec<TimelineEvent>) -> BoxFuture<'a, Result<(), RagError>> {
        Box::pin(async { Ok(()) })
    }
}

pub struct Pipeline;

impl Pipeline {
    #[must_use]
    pub fn builder(session: Session) -> PipelineBuilder {
        PipelineBuilder::new(session)
    }
}

pub struct PipelineBuilder {
    session: Session,
    capture: Option<Box<dyn CaptureBackend>>,
    mic_vad: Option<Box<dyn VoiceActivityDetector>>,
    system_vad: Option<Box<dyn VoiceActivityDetector>>,
    transcriber: Option<Box<dyn Transcriber>>,
    annotator: Option<Box<dyn TranscriptAnnotator>>,
    persistence: Arc<dyn PersistenceSink>,
    config: PipelineConfig,
}

impl PipelineBuilder {
    fn new(session: Session) -> Self {
        Self {
            session,
            capture: None,
            mic_vad: None,
            system_vad: None,
            transcriber: None,
            annotator: None,
            persistence: Arc::new(NoopPersistence),
            config: PipelineConfig::default(),
        }
    }
    #[must_use]
    pub fn capture(mut self, value: impl CaptureBackend + 'static) -> Self {
        self.capture = Some(Box::new(value));
        self
    }
    #[must_use]
    pub fn vad(
        mut self,
        mic: impl VoiceActivityDetector + 'static,
        system: impl VoiceActivityDetector + 'static,
    ) -> Self {
        self.mic_vad = Some(Box::new(mic));
        self.system_vad = Some(Box::new(system));
        self
    }
    #[must_use]
    pub fn transcriber(mut self, value: impl Transcriber + 'static) -> Self {
        self.transcriber = Some(Box::new(value));
        self
    }
    #[must_use]
    pub fn annotator(mut self, value: impl TranscriptAnnotator + 'static) -> Self {
        self.annotator = Some(Box::new(value));
        self
    }
    #[must_use]
    pub fn persistence(mut self, value: Arc<dyn PersistenceSink>) -> Self {
        self.persistence = value;
        self
    }
    #[must_use]
    pub const fn config(mut self, value: PipelineConfig) -> Self {
        self.config = value;
        self
    }

    pub fn start(self) -> Result<PipelineHandle, PipelineError> {
        let mut capture = self.capture.ok_or_else(missing_capture)?;
        let mic_vad = self.mic_vad.ok_or_else(missing_vad)?;
        let system_vad = self.system_vad.ok_or_else(missing_vad)?;
        let transcriber = self.transcriber.ok_or_else(missing_asr)?;
        let annotator = self.annotator.unwrap_or_else(|| Box::new(NoopAnnotator));
        let event_bus = EventBus::new(self.config.event_capacity);
        let counters = BackpressureCounters::default();
        let (audio_tx, _) = broadcast::channel(self.config.audio_capacity.get());
        let vad_audio = audio_tx.subscribe();
        let asr_audio = audio_tx.subscribe();
        let (transcript_tx, transcript_rx) = mpsc::channel(self.config.transcript_capacity.get());
        let (stage_tx, stage_rx) = mpsc::channel(self.config.timeline_capacity.get());
        let stage_control = Arc::new(Mutex::new(Some(stage_tx.clone())));
        let (control_tx, control_rx) = mpsc::channel(self.config.control_capacity.get());
        let (persistence_tx, persistence_rx) =
            mpsc::channel(self.config.persistence_queue_capacity.get());
        let (ack_tx, ack_rx) = mpsc::channel(self.config.persistence_queue_capacity.get());
        let recording_failed = Arc::new(AtomicBool::new(false));
        let call_session = CallSession::new(self.session, self.config.recent_event_capacity);

        capture.start(audio_tx.clone())?;
        let capture = Arc::new(Mutex::new(capture));
        let paused = Arc::new(AtomicBool::new(false));
        let vad_task = spawn_vad(
            vad_audio,
            mic_vad,
            system_vad,
            Arc::clone(&paused),
            transcript_tx.clone(),
            Arc::clone(&counters.vad_audio_dropped),
            Arc::clone(&counters.transcript_updates_dropped),
            Arc::clone(&counters.transcript_queue_high_water),
        );
        let asr_task = spawn_asr(
            asr_audio,
            transcriber,
            Arc::clone(&paused),
            transcript_tx,
            Arc::clone(&counters.asr_audio_dropped),
            Arc::clone(&counters.transcript_updates_dropped),
            Arc::clone(&counters.transcript_queue_high_water),
            self.config.asr_poll_interval,
        );
        let annotation_task = spawn_annotation(
            transcript_rx,
            annotator,
            stage_tx,
            Arc::clone(&counters.timeline_updates_dropped),
            Arc::clone(&counters.timeline_queue_high_water),
        );
        let timeline_task = call_session.spawn(
            stage_rx,
            control_rx,
            ack_rx,
            event_bus.sender(),
            persistence_tx,
            Arc::clone(&recording_failed),
            Arc::clone(&counters.persistence_saturated),
            self.config.checkpoint_events,
            self.config.pending_persistence_events,
        );
        let persistence_task = spawn_persistence(
            persistence_rx,
            ack_tx,
            self.persistence,
            Arc::clone(&recording_failed),
        );

        Ok(PipelineHandle {
            event_bus,
            counters,
            call_session,
            capture,
            audio_tx: Some(audio_tx),
            stage_tx: stage_control,
            control_tx: Some(control_tx),
            paused,
            vad_task,
            asr_task,
            annotation_task,
            timeline_task,
            persistence_task,
        })
    }
}

pub struct PipelineHandle {
    event_bus: EventBus,
    counters: BackpressureCounters,
    call_session: CallSession,
    capture: Arc<Mutex<Box<dyn CaptureBackend>>>,
    audio_tx: Option<broadcast::Sender<AudioFrame>>,
    stage_tx: Arc<Mutex<Option<mpsc::Sender<StageMessage>>>>,
    control_tx: Option<mpsc::Sender<StageMessage>>,
    paused: Arc<AtomicBool>,
    vad_task: JoinHandle<()>,
    asr_task: JoinHandle<()>,
    annotation_task: JoinHandle<()>,
    timeline_task: JoinHandle<()>,
    persistence_task: JoinHandle<()>,
}

impl PipelineHandle {
    #[must_use]
    pub fn events(&self) -> &EventBus {
        &self.event_bus
    }
    #[must_use]
    pub const fn counters(&self) -> &BackpressureCounters {
        &self.counters
    }
    #[must_use]
    pub fn session(&self) -> CallSession {
        self.call_session.clone()
    }
    pub fn pause(&self) {
        self.paused.store(true, Ordering::Release);
    }
    pub fn resume(&self) {
        self.paused.store(false, Ordering::Release);
    }
    #[must_use]
    pub fn is_paused(&self) -> bool {
        self.paused.load(Ordering::Acquire)
    }
    pub fn emit_screen(&self, timestamp: Duration, payload: crate::ScreenSnapshot) {
        self.try_stage(StageMessage::Payload(
            timestamp,
            EventPayload::ScreenSnapshot(payload),
        ));
    }
    pub async fn report_capture_error(&self, timestamp: Duration, error: CaptureError) {
        self.pause();
        if let Some(sender) = &self.control_tx {
            let _ = sender
                .send(StageMessage::Payload(
                    timestamp,
                    EventPayload::Error(PipelineError::Capture(error)),
                ))
                .await;
        }
    }
    fn try_stage(&self, message: StageMessage) {
        if let Ok(sender) = self.stage_tx.lock()
            && let Some(sender) = sender.as_ref()
            && sender.try_send(message).is_err()
        {
            self.counters
                .timeline_updates_dropped
                .fetch_add(1, Ordering::Relaxed);
        }
    }
    pub async fn stop(mut self) {
        if let Ok(mut capture) = self.capture.lock() {
            capture.stop();
        }
        self.audio_tx.take();
        let _ = self.vad_task.await;
        let _ = self.asr_task.await;
        let _ = self.annotation_task.await;
        if let Ok(mut sender) = self.stage_tx.lock() {
            sender.take();
        }
        self.control_tx.take();
        let _ = self.timeline_task.await;
        let _ = self.persistence_task.await;
    }
}

#[derive(Debug)]
pub(super) enum StageMessage {
    Payload(Duration, EventPayload),
    Annotated(Duration, Utterance, Option<ProsodyDelta>),
}

enum TranscriptMessage {
    Vad(Duration, VadSegment),
    Update(Duration, TranscriptUpdate),
}

#[expect(
    clippy::too_many_arguments,
    reason = "stage wiring keeps policies explicit"
)]
fn spawn_vad(
    mut audio: broadcast::Receiver<AudioFrame>,
    mut mic: Box<dyn VoiceActivityDetector>,
    mut system: Box<dyn VoiceActivityDetector>,
    paused: Arc<AtomicBool>,
    output: mpsc::Sender<TranscriptMessage>,
    audio_dropped: Arc<AtomicU64>,
    update_dropped: Arc<AtomicU64>,
    high_water: Arc<AtomicU64>,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            match audio.recv().await {
                Ok(frame) if !paused.load(Ordering::Acquire) => {
                    let segment = match frame.source {
                        Source::Mic => mic.push(&frame),
                        Source::System => system.push(&frame),
                    };
                    if let Some(segment) = segment {
                        if output
                            .try_send(TranscriptMessage::Vad(frame.stream_offset, segment))
                            .is_err()
                        {
                            update_dropped.fetch_add(1, Ordering::Relaxed);
                        } else {
                            observe_queue(&output, &high_water);
                        }
                    }
                }
                Ok(_) => {}
                Err(broadcast::error::RecvError::Lagged(count)) => {
                    audio_dropped.fetch_add(count, Ordering::Relaxed);
                }
                Err(broadcast::error::RecvError::Closed) => break,
            }
        }
    })
}

async fn send_transcripts(
    output: &mpsc::Sender<TranscriptMessage>,
    timestamp: Duration,
    updates: Vec<TranscriptUpdate>,
    dropped: &AtomicU64,
    high_water: &AtomicU64,
) -> bool {
    for update in updates {
        let message = TranscriptMessage::Update(timestamp, update);
        let sent = match &message {
            TranscriptMessage::Update(_, TranscriptUpdate::Final(_)) => {
                output.send(message).await.is_ok()
            }
            _ => output.try_send(message).is_ok(),
        };
        if !sent {
            dropped.fetch_add(1, Ordering::Relaxed);
            if output.is_closed() {
                return false;
            }
        } else {
            observe_queue(output, high_water);
        }
    }
    true
}

#[expect(
    clippy::too_many_arguments,
    reason = "stage wiring keeps policies explicit"
)]
fn spawn_asr(
    mut audio: broadcast::Receiver<AudioFrame>,
    mut transcriber: Box<dyn Transcriber>,
    paused: Arc<AtomicBool>,
    output: mpsc::Sender<TranscriptMessage>,
    audio_dropped: Arc<AtomicU64>,
    update_dropped: Arc<AtomicU64>,
    high_water: Arc<AtomicU64>,
    poll_interval: Duration,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(poll_interval);
        let mut latest = Duration::ZERO;
        loop {
            tokio::select! {
                result = audio.recv() => match result {
                    Ok(frame) if !paused.load(Ordering::Acquire) => {
                        latest = frame.stream_offset; transcriber.push(&frame);
                        if !send_transcripts(&output, latest, transcriber.poll(), &update_dropped, &high_water).await { break; }
                    }
                    Ok(_) => {}
                    Err(broadcast::error::RecvError::Lagged(count)) => { audio_dropped.fetch_add(count, Ordering::Relaxed); }
                    Err(broadcast::error::RecvError::Closed) => break,
                },
                _ = ticker.tick() => {
                    if !send_transcripts(&output, latest, transcriber.poll(), &update_dropped, &high_water).await { break; }
                }
            }
        }
        let updates = transcriber.poll();
        let _ = send_transcripts(&output, latest, updates, &update_dropped, &high_water).await;
    })
}

fn spawn_annotation(
    mut input: mpsc::Receiver<TranscriptMessage>,
    mut annotator: Box<dyn TranscriptAnnotator>,
    output: mpsc::Sender<StageMessage>,
    dropped: Arc<AtomicU64>,
    high_water: Arc<AtomicU64>,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        while let Some(message) = input.recv().await {
            let (stage, required) = match message {
                TranscriptMessage::Vad(ts, segment) => {
                    annotator.observe_vad(&segment);
                    (StageMessage::Payload(ts, EventPayload::Vad(segment)), false)
                }
                TranscriptMessage::Update(ts, TranscriptUpdate::Partial(utterance)) => (
                    StageMessage::Payload(ts, EventPayload::UtterancePartial(utterance)),
                    false,
                ),
                TranscriptMessage::Update(ts, TranscriptUpdate::Final(mut utterance)) => {
                    let delta = annotator.annotate(&mut utterance);
                    (StageMessage::Annotated(ts, utterance, delta), true)
                }
            };
            let sent = if required {
                output.send(stage).await.is_ok()
            } else {
                output.try_send(stage).is_ok()
            };
            if !sent {
                dropped.fetch_add(1, Ordering::Relaxed);
                if output.is_closed() {
                    break;
                }
            } else {
                observe_queue(&output, &high_water);
            }
        }
    })
}

fn observe_queue<T>(sender: &mpsc::Sender<T>, high_water: &AtomicU64) {
    let queued = sender.max_capacity().saturating_sub(sender.capacity());
    high_water.fetch_max(u64::try_from(queued).unwrap_or(u64::MAX), Ordering::Relaxed);
}

pub(super) struct PersistenceBatch {
    events: Vec<TimelineEvent>,
}
pub(super) struct PersistenceAck {
    count: usize,
    timestamp: Duration,
    result: Result<(), RagError>,
}

fn spawn_persistence(
    mut input: mpsc::Receiver<PersistenceBatch>,
    ack: mpsc::Sender<PersistenceAck>,
    persistence: Arc<dyn PersistenceSink>,
    failed: Arc<AtomicBool>,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        while let Some(batch) = input.recv().await {
            let count = batch.events.len();
            let timestamp = batch
                .events
                .last()
                .map_or(Duration::ZERO, TimelineEvent::ts);
            let result = persistence.append(batch.events).await;
            if result.is_err() {
                failed.store(true, Ordering::Release);
            }
            if ack
                .send(PersistenceAck {
                    count,
                    timestamp,
                    result,
                })
                .await
                .is_err()
            {
                break;
            }
        }
    })
}

fn missing_capture() -> PipelineError {
    CaptureError::StreamFailed("pipeline capture backend is required".to_owned()).into()
}
fn missing_vad() -> PipelineError {
    crate::VadError::ModelLoad("pipeline requires VAD for mic and system".to_owned()).into()
}
fn missing_asr() -> PipelineError {
    crate::AsrError::ModelLoad("pipeline transcriber is required".to_owned()).into()
}
