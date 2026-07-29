use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result};
use asr::{Config as AsrConfig, WhisperTranscriber};
use prosody::{Annotator, Event as ProsodyEvent};
use serde::Serialize;
use sotto_core::{
    CaptureTarget, EventPayload, Session, SessionId, Source, SpeechState, TargetKind,
    TimelineBuilder, TimelineEvent, Transcriber, TranscriptUpdate, VoiceActivityDetector,
};
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
    let mode = if options.realtime {
        FileCaptureMode::Realtime
    } else {
        FileCaptureMode::Fast
    };
    let mut frames = FileCapture::new(&options.mic, Source::Mic, mode).frames()?;
    if let Some(system) = &options.system {
        frames.extend(FileCapture::new(system, Source::System, mode).frames()?);
    }
    frames.sort_by_key(|frame| (frame.stream_offset, source_order(frame.source), frame.seq));

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
    let mut timeline = TimelineBuilder::new(session);
    let screen_payloads = options
        .frames
        .as_deref()
        .map(load_screen_payloads)
        .transpose()?
        .unwrap_or_default();
    let mut screen_payloads = screen_payloads.into_iter().peekable();
    let mut mic_vad = SileroVad::new(Source::Mic, VadConfig::default())?;
    let mut system_vad = options
        .system
        .as_ref()
        .map(|_| SileroVad::new(Source::System, VadConfig::default()))
        .transpose()?;
    let mut transcriber = options
        .model
        .as_ref()
        .map(|path| {
            let mut config = AsrConfig::new(path);
            if !options.realtime {
                // A fast fixture run feeds audio faster than the worker can observe live VAD
                // controls. Treat the completed files as one batch and commit on the first pass.
                config.vad_gating = false;
                config.agreement_passes = 1;
                config.unstable_tail = Duration::ZERO;
            }
            WhisperTranscriber::new(config)
        })
        .transpose()?;
    let mut prosody = Annotator::default();
    let mut latest_partial = HashMap::new();
    let mut speech_ended = HashMap::new();
    let mut frame_vad = Vec::new();
    let mut partial_latency = Vec::new();
    let mut final_latency = Vec::new();

    let started = Instant::now();
    for mut frame in frames {
        if options.realtime {
            let due = started + frame.stream_offset;
            if let Some(delay) = due.checked_duration_since(Instant::now()) {
                std::thread::sleep(delay);
            }
        }
        frame.capture_ts = Instant::now();
        let ts = frame.stream_offset;
        while screen_payloads
            .peek()
            .is_some_and(|(screen_ts, _)| *screen_ts <= ts)
        {
            if let Some((screen_ts, payload)) = screen_payloads.next() {
                timeline.append(screen_ts, payload);
            }
        }
        let segment = match frame.source {
            Source::Mic => {
                let segment = mic_vad.push(&frame);
                if let Some(error) = mic_vad.take_error() {
                    anyhow::bail!("mic VAD failed: {error}");
                }
                segment
            }
            Source::System => {
                let segment = system_vad
                    .as_mut()
                    .and_then(|detector| detector.push(&frame));
                if let Some(error) = system_vad.as_mut().and_then(vad::SileroVad::take_error) {
                    anyhow::bail!("system VAD failed: {error}");
                }
                segment
            }
        };
        if let Some(segment) = segment {
            frame_vad.push(frame.capture_ts.elapsed());
            if segment.kind == SpeechState::SpeechEnd {
                speech_ended.insert(segment.source, Instant::now());
            }
            if let Some(asr) = &transcriber {
                asr.set_speech_state(segment.source, segment.kind);
            }
            prosody.observe(ProsodyEvent::Vad(&segment));
            timeline.append(ts, EventPayload::Vad(segment));
        }
        if let Some(asr) = &mut transcriber {
            asr.push(&frame);
            append_updates(
                asr.poll(),
                ts,
                &mut timeline,
                &mut prosody,
                &mut latest_partial,
                &speech_ended,
                &mut partial_latency,
                &mut final_latency,
            )?;
        }
    }
    for (screen_ts, payload) in screen_payloads {
        timeline.append(screen_ts, payload);
    }
    if let Some(asr) = &mut transcriber {
        std::thread::sleep(Duration::from_millis(550));
        let ts = timeline
            .events()
            .last()
            .map_or(Duration::ZERO, TimelineEvent::ts);
        append_updates(
            asr.poll(),
            ts,
            &mut timeline,
            &mut prosody,
            &mut latest_partial,
            &speech_ended,
            &mut partial_latency,
            &mut final_latency,
        )?;
        if let Some(error) = asr.poll_errors().into_iter().next() {
            anyhow::bail!("ASR worker failed: {error}");
        }
    }

    Ok(PipelineRun {
        events: timeline.into_events(),
        latency: LatencyReport {
            frame_to_vad: percentiles(&mut frame_vad),
            speech_end_to_partial: percentiles(&mut partial_latency),
            speech_end_to_final: percentiles(&mut final_latency),
            speech_end_to_suggestion: None,
        },
    })
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

#[expect(
    clippy::too_many_arguments,
    reason = "stage state is explicit at the pipeline seam"
)]
fn append_updates(
    updates: Vec<TranscriptUpdate>,
    ts: Duration,
    timeline: &mut TimelineBuilder,
    prosody: &mut Annotator,
    latest_partial: &mut HashMap<Source, TimelineEvent>,
    speech_ended: &HashMap<Source, Instant>,
    partial_latency: &mut Vec<Duration>,
    final_latency: &mut Vec<Duration>,
) -> Result<()> {
    for update in updates {
        let source = update.utterance().source;
        let ended_latency = speech_ended.get(&source).map(Instant::elapsed);
        match update {
            TranscriptUpdate::Partial(utterance) => {
                if let Some(latency) = ended_latency {
                    partial_latency.push(latency);
                }
                let payload = EventPayload::UtterancePartial(utterance);
                let event = if let Some(previous) = latest_partial.remove(&source) {
                    timeline.supersede(ts, payload, &previous)?
                } else {
                    timeline.append(ts, payload)
                };
                latest_partial.insert(source, event);
            }
            TranscriptUpdate::Final(mut utterance) => {
                if let Some(latency) = ended_latency {
                    final_latency.push(latency);
                }
                let annotations = prosody.observe(ProsodyEvent::Utterance(&utterance));
                utterance.annotations.extend(annotations);
                let payload = EventPayload::UtteranceFinal(utterance);
                if let Some(previous) = latest_partial.remove(&source) {
                    timeline.supersede(ts, payload, &previous)?;
                } else {
                    timeline.append(ts, payload);
                }
                if let Some(delta) = prosody.last_delta().cloned() {
                    timeline.append(ts, EventPayload::Prosody(delta));
                }
            }
        }
    }
    Ok(())
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

const fn source_order(source: Source) -> u8 {
    match source {
        Source::Mic => 0,
        Source::System => 1,
    }
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
