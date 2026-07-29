use std::{
    collections::HashMap,
    num::NonZeroUsize,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result};
use asr::{Config as AsrConfig, WhisperTranscriber};
use serde::Serialize;
use sotto_core::{
    AudioFrame, CaptureBackend, CaptureError, CaptureTarget, EventPayload, PermissionStatus,
    Pipeline, PipelineConfig, Session, SessionId, Source, SpeechState, TargetKind, TimelineEvent,
    Transcriber, TranscriptUpdate,
};
use tokio::sync::{Notify, broadcast};
use vad::{SileroVad, VadConfig};

use crate::file_capture::{FileCapture, FileCaptureMode, TimestampedFrames};

#[derive(Clone, Debug)]
pub struct PipelineOptions {
    pub mic: PathBuf,
    pub system: Option<PathBuf>,
    pub frames: Option<PathBuf>,
    pub model: Option<PathBuf>,
    pub realtime: bool,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct Percentiles {
    pub count: usize,
    pub p50_ms: f64,
    pub p95_ms: f64,
    pub p99_ms: f64,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct LatencyReport {
    pub frame_to_vad: Percentiles,
    pub speech_end_to_partial: Percentiles,
    pub speech_end_to_final: Percentiles,
    pub speech_end_to_suggestion: Option<Percentiles>,
}

pub struct PipelineRun {
    pub events: Vec<TimelineEvent>,
    pub latency: LatencyReport,
}

pub fn run_files(options: &PipelineOptions) -> Result<PipelineRun> {
    tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()?
        .block_on(run_files_async(options))
}

async fn run_files_async(options: &PipelineOptions) -> Result<PipelineRun> {
    let mode = if options.realtime {
        FileCaptureMode::Realtime
    } else {
        FileCaptureMode::Fast
    };
    let metrics = Arc::new(CaptureMetrics::default());
    let (capture, completion, frame_count) = MergedFileCapture::load(
        &options.mic,
        options.system.as_deref(),
        mode,
        Arc::clone(&metrics),
    )?;
    let transcriber = if let Some(path) = &options.model {
        let mut config = AsrConfig::new(path);
        if !options.realtime {
            config.vad_gating = false;
            config.agreement_passes = 1;
            config.unstable_tail = Duration::ZERO;
        }
        HarnessTranscriber::Real(DrainingTranscriber::new(
            WhisperTranscriber::new(config)?,
            frame_count,
        ))
    } else {
        HarnessTranscriber::Noop
    };

    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("system clock precedes Unix epoch")?;
    let session = Session::new(
        SessionId::new(now.as_nanos()),
        CaptureTarget {
            bundle_id: None,
            display_name: "file fixture".to_owned(),
            window_title: options
                .frames
                .as_ref()
                .map(|path| path.display().to_string()),
            kind: TargetKind::Window,
            audio_scoped: options.system.is_some(),
        },
        u64::try_from(now.as_millis()).unwrap_or(u64::MAX),
    );
    let capacity = NonZeroUsize::new(65_536).unwrap_or(NonZeroUsize::MIN);
    let pipeline = Pipeline::builder(session)
        .capture(capture)
        .vad(
            SileroVad::new(Source::Mic, VadConfig::default())?,
            SileroVad::new(Source::System, VadConfig::default())?,
        )
        .transcriber(transcriber)
        .annotator(prosody::Annotator::default())
        .config(PipelineConfig {
            audio_capacity: NonZeroUsize::new(frame_count.saturating_add(1))
                .unwrap_or(NonZeroUsize::MIN),
            transcript_capacity: capacity,
            timeline_capacity: capacity,
            event_capacity: capacity,
            recent_event_capacity: capacity,
            pending_persistence_events: capacity,
            ..PipelineConfig::default()
        })
        .start()?;
    let session = pipeline.session();
    let mut receiver = pipeline.events().subscribe("cli-latency");
    let collector_metrics = Arc::clone(&metrics);
    let collector = tokio::spawn(async move {
        let mut latency = LatencyCollector::new(collector_metrics);
        while let Ok(event) = receiver.recv().await {
            latency.observe(&event);
        }
        latency.finish()
    });

    if let Some(directory) = options.frames.as_deref() {
        for (timestamp, payload) in load_screen_payloads(directory)? {
            if let EventPayload::ScreenSnapshot(snapshot) = payload {
                pipeline.emit_screen(timestamp, snapshot);
            }
        }
    }
    completion.wait().await;
    pipeline.stop().await;
    let latency = collector.await.context("latency collector failed")?;
    let events = session.view().recent_append_order;
    Ok(PipelineRun { events, latency })
}

#[derive(Default)]
struct CaptureMetrics {
    sent: Mutex<HashMap<(Source, Duration), Instant>>,
}

impl CaptureMetrics {
    fn record(&self, frame: &AudioFrame) {
        if let Ok(mut sent) = self.sent.lock() {
            sent.insert((frame.source, frame.stream_offset), frame.capture_ts);
        }
    }

    fn elapsed(&self, source: Source, timestamp: Duration) -> Option<Duration> {
        self.sent
            .lock()
            .ok()?
            .remove(&(source, timestamp))
            .map(|sent| sent.elapsed())
    }
}

#[derive(Clone, Default)]
struct CaptureCompletion {
    done: Arc<AtomicBool>,
    notify: Arc<Notify>,
}

impl CaptureCompletion {
    async fn wait(&self) {
        loop {
            let notified = self.notify.notified();
            if self.done.load(Ordering::Acquire) {
                return;
            }
            notified.await;
        }
    }
}

struct MergedFileCapture {
    frames: Vec<AudioFrame>,
    mode: FileCaptureMode,
    completion: CaptureCompletion,
    metrics: Arc<CaptureMetrics>,
    worker: Option<thread::JoinHandle<()>>,
}

impl MergedFileCapture {
    fn load(
        mic: &Path,
        system: Option<&Path>,
        mode: FileCaptureMode,
        metrics: Arc<CaptureMetrics>,
    ) -> Result<(Self, CaptureCompletion, usize)> {
        let mut frames = FileCapture::new(mic, Source::Mic, mode).frames()?;
        if let Some(path) = system {
            frames.extend(FileCapture::new(path, Source::System, mode).frames()?);
        }
        // This reconstructs fixture capture arrival only. Conversation ordering belongs
        // exclusively to `core::CallSession`.
        frames.sort_by_key(|frame| (frame.stream_offset, frame.seq));
        let count = frames.len();
        let completion = CaptureCompletion::default();
        Ok((
            Self {
                frames,
                mode,
                completion: completion.clone(),
                metrics,
                worker: None,
            },
            completion,
            count,
        ))
    }
}

impl CaptureBackend for MergedFileCapture {
    fn start(&mut self, sink: broadcast::Sender<AudioFrame>) -> Result<(), CaptureError> {
        let frames = std::mem::take(&mut self.frames);
        let mode = self.mode;
        let completion = self.completion.clone();
        let metrics = Arc::clone(&self.metrics);
        self.worker = Some(
            thread::Builder::new()
                .name("sotto-cli-fixture".to_owned())
                .spawn(move || {
                    let started = Instant::now();
                    for mut frame in frames {
                        if mode == FileCaptureMode::Realtime {
                            let due = started + frame.stream_offset;
                            if let Some(delay) = due.checked_duration_since(Instant::now()) {
                                thread::sleep(delay);
                            }
                        }
                        frame.capture_ts = Instant::now();
                        metrics.record(&frame);
                        if sink.send(frame).is_err() {
                            break;
                        }
                    }
                    completion.done.store(true, Ordering::Release);
                    completion.notify.notify_waiters();
                })
                .map_err(|error| CaptureError::StreamFailed(error.to_string()))?,
        );
        Ok(())
    }

    fn stop(&mut self) {
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }

    fn permission_status(&self) -> PermissionStatus {
        PermissionStatus::Authorized
    }
}

impl Drop for MergedFileCapture {
    fn drop(&mut self) {
        self.stop();
    }
}

enum HarnessTranscriber {
    Real(DrainingTranscriber),
    Noop,
}

impl Transcriber for HarnessTranscriber {
    fn push(&mut self, frame: &AudioFrame) {
        if let Self::Real(inner) = self {
            inner.push(frame);
        }
    }

    fn poll(&mut self) -> Vec<TranscriptUpdate> {
        match self {
            Self::Real(inner) => inner.poll(),
            Self::Noop => Vec::new(),
        }
    }
}

struct DrainingTranscriber {
    inner: WhisperTranscriber,
    expected_frames: usize,
    pushed_frames: usize,
    drained: bool,
}

impl DrainingTranscriber {
    fn new(inner: WhisperTranscriber, expected_frames: usize) -> Self {
        Self {
            inner,
            expected_frames,
            pushed_frames: 0,
            drained: false,
        }
    }
}

impl Transcriber for DrainingTranscriber {
    fn push(&mut self, frame: &AudioFrame) {
        self.pushed_frames = self.pushed_frames.saturating_add(1);
        self.inner.push(frame);
    }

    fn poll(&mut self) -> Vec<TranscriptUpdate> {
        let mut updates = self.inner.poll();
        if self.drained || self.pushed_frames < self.expected_frames {
            return updates;
        }
        self.drained = true;
        let started = Instant::now();
        let mut last_output = None;
        while started.elapsed() < Duration::from_secs(180) {
            thread::sleep(Duration::from_millis(100));
            let next = self.inner.poll();
            if !next.is_empty() {
                last_output = Some(Instant::now());
                updates.extend(next);
            }
            if last_output.is_some_and(|last| last.elapsed() >= Duration::from_millis(750)) {
                break;
            }
            if !self.inner.poll_errors().is_empty() {
                break;
            }
        }
        updates
    }
}

struct LatencyCollector {
    metrics: Arc<CaptureMetrics>,
    speech_ended: HashMap<Source, Instant>,
    frame_vad: Vec<Duration>,
    partial: Vec<Duration>,
    final_updates: Vec<Duration>,
}

impl LatencyCollector {
    fn new(metrics: Arc<CaptureMetrics>) -> Self {
        Self {
            metrics,
            speech_ended: HashMap::new(),
            frame_vad: Vec::new(),
            partial: Vec::new(),
            final_updates: Vec::new(),
        }
    }
    fn observe(&mut self, event: &TimelineEvent) {
        match event.payload() {
            EventPayload::Vad(segment) => {
                if let Some(value) = self.metrics.elapsed(segment.source, event.ts()) {
                    self.frame_vad.push(value);
                }
                if segment.kind == SpeechState::SpeechEnd {
                    self.speech_ended.insert(segment.source, Instant::now());
                }
            }
            EventPayload::UtterancePartial(value) => {
                if let Some(started) = self.speech_ended.get(&value.source) {
                    self.partial.push(started.elapsed());
                }
            }
            EventPayload::UtteranceFinal(value) => {
                if let Some(started) = self.speech_ended.get(&value.source) {
                    self.final_updates.push(started.elapsed());
                }
            }
            _ => {}
        }
    }
    fn finish(mut self) -> LatencyReport {
        LatencyReport {
            frame_to_vad: percentiles(&mut self.frame_vad),
            speech_end_to_partial: percentiles(&mut self.partial),
            speech_end_to_final: percentiles(&mut self.final_updates),
            speech_end_to_suggestion: None,
        }
    }
}

pub fn load_screen_payloads(directory: &Path) -> Result<Vec<(Duration, EventPayload)>> {
    let frame_files = TimestampedFrames::read(directory)?;
    let target = CaptureTarget {
        bundle_id: None,
        display_name: "fixture frames".to_owned(),
        window_title: directory
            .file_name()
            .map(|name| name.to_string_lossy().into_owned()),
        kind: TargetKind::Window,
        audio_scoped: false,
    };
    let cache = std::env::temp_dir().join(format!("sotto-cli-screen-{}", std::process::id()));
    let mut sampler = screen::ScreenSampler::new(
        screen::SamplerConfig {
            min_interval: Duration::ZERO,
            ..screen::SamplerConfig::default()
        },
        cache,
        target,
        platform_ocr(),
    )?;
    let mut payloads = Vec::new();
    let mut last = Duration::ZERO;
    for item in frame_files {
        last = item.timestamp;
        let frame = decode_png(&item.path, item.timestamp)?;
        if let Some(payload) = sampler.push(frame, screen::FrameMetadata::default())? {
            payloads.push((item.timestamp, payload));
        }
    }
    if let Some(payload) = sampler.finish(last + Duration::from_secs(5)) {
        payloads.push((last + Duration::from_secs(5), payload));
    }
    Ok(payloads)
}

fn decode_png(path: &Path, captured_at: Duration) -> Result<screen::Frame> {
    let decoder = png::Decoder::new(std::io::BufReader::new(std::fs::File::open(path)?));
    let mut reader = decoder.read_info()?;
    let size = reader
        .output_buffer_size()
        .context("PNG dimensions exceed decoder limits")?;
    let mut bytes = vec![0; size];
    let info = reader.next_frame(&mut bytes)?;
    let source = &bytes[..info.buffer_size()];
    let mut bgra = Vec::with_capacity(info.width as usize * info.height as usize * 4);
    match info.color_type {
        png::ColorType::Rgba => {
            for pixel in source.chunks_exact(4) {
                bgra.extend_from_slice(&[pixel[2], pixel[1], pixel[0], pixel[3]]);
            }
        }
        png::ColorType::Rgb => {
            for pixel in source.chunks_exact(3) {
                bgra.extend_from_slice(&[pixel[2], pixel[1], pixel[0], 255]);
            }
        }
        other => anyhow::bail!(
            "{} uses unsupported PNG color type {other:?}",
            path.display()
        ),
    }
    Ok(screen::Frame {
        bgra,
        width: info.width,
        height: info.height,
        stride: info.width.saturating_mul(4),
        captured_at,
    })
}

#[cfg(target_os = "macos")]
struct BestEffortVisionOcr(screen::VisionOcr);
#[cfg(target_os = "macos")]
impl screen::OcrEngine for BestEffortVisionOcr {
    fn recognize(&self, frame: &screen::Frame) -> Result<String, screen::ScreenError> {
        match self.0.recognize(frame) {
            Ok(text) => Ok(text),
            Err(error) => {
                eprintln!("warning: Vision OCR failed; preserving snapshot without text: {error}");
                Ok(String::new())
            }
        }
    }
}
#[cfg(target_os = "macos")]
fn platform_ocr() -> BestEffortVisionOcr {
    BestEffortVisionOcr(screen::VisionOcr::new())
}

#[cfg(not(target_os = "macos"))]
struct UnavailableOcr;
#[cfg(not(target_os = "macos"))]
impl screen::OcrEngine for UnavailableOcr {
    fn recognize(&self, _frame: &screen::Frame) -> Result<String, screen::ScreenError> {
        Ok(String::new())
    }
}
#[cfg(not(target_os = "macos"))]
fn platform_ocr() -> UnavailableOcr {
    UnavailableOcr
}

fn percentiles(values: &mut [Duration]) -> Percentiles {
    values.sort_unstable();
    Percentiles {
        count: values.len(),
        p50_ms: percentile(values, 50, 100),
        p95_ms: percentile(values, 95, 100),
        p99_ms: percentile(values, 99, 100),
    }
}
fn percentile(values: &[Duration], numerator: usize, denominator: usize) -> f64 {
    if values.is_empty() {
        return 0.0;
    }
    let index = (values.len() - 1)
        .saturating_mul(numerator)
        .div_ceil(denominator);
    values[index].as_secs_f64() * 1_000.0
}
pub fn ingest_file(store_path: &Path, input: &Path) -> Result<bool> {
    let text = std::fs::read_to_string(input)
        .with_context(|| format!("read document {}", input.display()))?;
    let store = rag::Store::open(store_path)?;
    let metadata = rag::IngestMetadata {
        title: input
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("document")
            .to_owned(),
        source_path: Some(input.display().to_string()),
        ..rag::IngestMetadata::default()
    };
    store
        .ingest_text(&text, rag::DocumentKind::ProductDocument, metadata)
        .map_err(Into::into)
}
