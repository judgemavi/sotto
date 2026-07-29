use std::{
    fs,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

use hound::{SampleFormat, WavReader};
use sotto_core::{AudioFrame, CaptureBackend, CaptureError, PermissionStatus, Source};
use tokio::sync::broadcast;

const FRAME_SAMPLES: usize = 512;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FileCaptureMode {
    Fast,
    Realtime,
}

/// WAV-backed implementation of the production audio capture contract.
pub struct FileCapture {
    path: PathBuf,
    source: Source,
    mode: FileCaptureMode,
    stop: Arc<AtomicBool>,
    worker: Option<thread::JoinHandle<()>>,
}

impl FileCapture {
    #[must_use]
    pub fn new(path: impl Into<PathBuf>, source: Source, mode: FileCaptureMode) -> Self {
        Self {
            path: path.into(),
            source,
            mode,
            stop: Arc::new(AtomicBool::new(false)),
            worker: None,
        }
    }

    pub fn frames(&self) -> Result<Vec<AudioFrame>, CaptureError> {
        read_wav(&self.path, self.source)
    }
}

impl CaptureBackend for FileCapture {
    fn start(&mut self, sink: broadcast::Sender<AudioFrame>) -> Result<(), CaptureError> {
        if self.worker.is_some() {
            return Err(CaptureError::StreamFailed(
                "file capture already started".to_owned(),
            ));
        }
        let frames = self.frames()?;
        self.stop.store(false, Ordering::Release);
        let stop = Arc::clone(&self.stop);
        let mode = self.mode;
        self.worker = Some(
            thread::Builder::new()
                .name("sotto-file-capture".to_owned())
                .spawn(move || {
                    let started = Instant::now();
                    for frame in frames {
                        if stop.load(Ordering::Acquire) {
                            break;
                        }
                        if mode == FileCaptureMode::Realtime {
                            let due = started + frame.stream_offset;
                            if let Some(delay) = due.checked_duration_since(Instant::now()) {
                                thread::sleep(delay);
                            }
                        }
                        if sink.send(frame).is_err() {
                            break;
                        }
                    }
                })
                .map_err(|error| CaptureError::StreamFailed(error.to_string()))?,
        );
        Ok(())
    }

    fn stop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }

    fn permission_status(&self) -> PermissionStatus {
        PermissionStatus::Authorized
    }
}

impl Drop for FileCapture {
    fn drop(&mut self) {
        self.stop();
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FrameFile {
    pub timestamp: Duration,
    pub path: PathBuf,
}

/// Timestamped PNG files named `<milliseconds>-anything.png`.
pub struct TimestampedFrames;

impl TimestampedFrames {
    pub fn read(directory: &Path) -> Result<Vec<FrameFile>, CaptureError> {
        let entries = fs::read_dir(directory).map_err(|error| {
            CaptureError::StreamFailed(format!("{}: {error}", directory.display()))
        })?;
        let mut frames = Vec::new();
        for entry in entries {
            let entry = entry.map_err(|error| CaptureError::StreamFailed(error.to_string()))?;
            let path = entry.path();
            if path.extension().and_then(|value| value.to_str()) != Some("png") {
                continue;
            }
            let stem = path
                .file_stem()
                .and_then(|value| value.to_str())
                .ok_or_else(|| {
                    CaptureError::Unsupported(format!("invalid frame name: {}", path.display()))
                })?;
            let millis = stem
                .split(['-', '_'])
                .next()
                .ok_or_else(|| CaptureError::Unsupported(format!("missing timestamp: {stem}")))?
                .parse::<u64>()
                .map_err(|error| {
                    CaptureError::Unsupported(format!("invalid timestamp {stem}: {error}"))
                })?;
            frames.push(FrameFile {
                timestamp: Duration::from_millis(millis),
                path,
            });
        }
        frames.sort_by_key(|frame| frame.timestamp);
        Ok(frames)
    }
}

fn read_wav(path: &Path, source: Source) -> Result<Vec<AudioFrame>, CaptureError> {
    let mut reader = WavReader::open(path)
        .map_err(|error| CaptureError::StreamFailed(format!("{}: {error}", path.display())))?;
    let spec = reader.spec();
    if spec.channels != 1 {
        return Err(CaptureError::Unsupported(format!(
            "{} must be mono; found {} channels",
            path.display(),
            spec.channels
        )));
    }
    let samples = match spec.sample_format {
        SampleFormat::Float => reader
            .samples::<f32>()
            .map(|sample| sample.map_err(|error| CaptureError::StreamFailed(error.to_string())))
            .collect::<Result<Vec<_>, _>>()?,
        SampleFormat::Int => {
            let scale = 2_f32.powi(i32::from(spec.bits_per_sample).saturating_sub(1));
            reader
                .samples::<i32>()
                .map(|sample| {
                    sample
                        .map(|value| value as f32 / scale)
                        .map_err(|error| CaptureError::StreamFailed(error.to_string()))
                })
                .collect::<Result<Vec<_>, _>>()?
        }
    };
    let mut frames = Vec::with_capacity(samples.len().div_ceil(FRAME_SAMPLES));
    for (seq, chunk) in samples.chunks(FRAME_SAMPLES).enumerate() {
        let offset_samples = seq.saturating_mul(FRAME_SAMPLES);
        frames.push(AudioFrame {
            source,
            samples: Arc::from(chunk),
            sample_rate: spec.sample_rate,
            seq: u64::try_from(seq).unwrap_or(u64::MAX),
            capture_ts: Instant::now(),
            stream_offset: Duration::from_secs_f64(
                offset_samples as f64 / f64::from(spec.sample_rate),
            ),
        });
    }
    Ok(frames)
}

#[cfg(test)]
mod tests {
    use super::{FileCapture, FileCaptureMode, TimestampedFrames};
    use sotto_core::Source;

    #[test]
    fn shared_fixture_is_read_without_audio_hardware() -> Result<(), Box<dyn std::error::Error>> {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures");
        let fixture = FileCapture::new(
            root.join("call-01-mic.wav"),
            Source::Mic,
            FileCaptureMode::Fast,
        );
        let frames = fixture.frames()?;
        assert!(!frames.is_empty(), "fixture must contain audio frames");
        assert_eq!(frames[0].source, Source::Mic, "source must be retained");
        Ok(())
    }

    #[test]
    fn timestamped_frames_sort_by_fixture_clock() -> Result<(), Box<dyn std::error::Error>> {
        let frames = TimestampedFrames::read(
            &std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/call-01-frames"),
        )?;
        assert!(
            frames.len() >= 2,
            "paired fixture must contain screen frames"
        );
        assert!(
            frames
                .windows(2)
                .all(|pair| pair[0].timestamp <= pair[1].timestamp),
            "frames must follow the fixture clock"
        );
        Ok(())
    }
}
