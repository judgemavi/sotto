use std::{
    ffi::{CStr, CString, c_char, c_float, c_void},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use sotto_core::{
    AsrError, AudioFrame, RecordingStatus, RecordingTranscriber, Source, Transcriber,
    TranscriptUpdate,
};

use crate::{Config, FinalWhisperTranscriber};

const ERROR_CAPACITY: usize = 2_048;
/// The capture writer's `preferredOutputSegmentInterval`, which Rust cannot read from Swift.
///
/// A growing recording only exposes whole committed segments, so no reader can ever be closer to
/// live than this. Changing `preferredOutputSegmentInterval` in `CaptureBridge.swift` without
/// changing this value here would let the transcriber read into an uncommitted tail.
const SEGMENT_COMMIT_INTERVAL: Duration = Duration::from_secs(2);
/// Sits exactly on the commit interval: the transcript follows the recording as closely as the
/// writer allows, and no closer.
const DEFAULT_RECORDING_LAG: Duration = SEGMENT_COMMIT_INTERVAL;

/// One decoded mono channel from Sotto's retained stereo recording.
#[derive(Debug)]
pub struct DecodedRecordingChannel {
    pub samples: Vec<f32>,
    pub sample_rate: u32,
}

/// Decodes one deterministically attributed channel from a retained recording.
///
/// Sotto recordings map the selected target's meeting audio to [`Source::System`] (left/channel
/// zero) and the local microphone to [`Source::Mic`] (right/channel one).
pub fn decode_recording_channel(
    path: impl AsRef<Path>,
    source: Source,
) -> Result<DecodedRecordingChannel, AsrError> {
    let path = path.as_ref();
    let duration = platform::duration(path)?;
    let mut sequence = 0;
    let frames = platform::read_stereo(path, Duration::ZERO, duration, &mut sequence)?;
    let mut samples = Vec::new();
    for frame in frames.into_iter().filter(|frame| frame.source == source) {
        samples.extend_from_slice(&frame.samples);
    }
    Ok(DecodedRecordingChannel {
        samples,
        sample_rate: 16_000,
    })
}

#[derive(Clone, Debug)]
pub struct RecordingConfig {
    pub transcription: Config,
    pub lag: Duration,
    layout: RecordingLayout,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RecordingLayout {
    MeetingStereo,
    MicrophoneOnly,
}

impl RecordingConfig {
    #[must_use]
    pub fn new(transcription: Config) -> Self {
        Self {
            transcription,
            lag: DEFAULT_RECORDING_LAG,
            layout: RecordingLayout::MeetingStereo,
        }
    }

    /// Reads channel 0 once as the microphone and ignores channel 1's intentional silence.
    #[must_use]
    pub const fn microphone_only(mut self) -> Self {
        self.layout = RecordingLayout::MicrophoneOnly;
        self
    }
}

/// Reopens the committed prefix of a growing fragmented MP4 and transcribes it in media time.
pub struct LaggedRecordingTranscriber {
    reader: Box<dyn RecordingMediaReader>,
    config: RecordingConfig,
    inner: FinalWhisperTranscriber,
    cursor: Duration,
    sequence: u64,
    complete: bool,
}

/// Pipeline adapter and retained completion handle for a growing session recording.
///
/// The pipeline owns the adapter while the session worker retains the handle. Once capture has
/// finalized the MP4, the handle can consume the exact remaining media prefix even though the
/// capture-audio pipeline has already shut down.
pub struct LiveRecordingTranscriber {
    shared: Arc<Mutex<LaggedRecordingTranscriber>>,
    last_poll: Option<Instant>,
    poll_interval: Duration,
}

#[derive(Clone)]
pub struct RecordingTranscriptionHandle {
    shared: Arc<Mutex<LaggedRecordingTranscriber>>,
}

impl LiveRecordingTranscriber {
    pub fn new(
        path: impl Into<PathBuf>,
        config: RecordingConfig,
    ) -> Result<(Self, RecordingTranscriptionHandle), AsrError> {
        let poll_interval = config.transcription.cadence;
        let shared = Arc::new(Mutex::new(LaggedRecordingTranscriber::new(path, config)?));
        Ok((
            Self {
                shared: Arc::clone(&shared),
                last_poll: None,
                poll_interval,
            },
            RecordingTranscriptionHandle { shared },
        ))
    }
}

impl RecordingTranscriptionHandle {
    /// Reads through finalized EOF and flushes the incomplete final Whisper window exactly once.
    pub fn transcribe_complete(&self) -> Result<Vec<TranscriptUpdate>, AsrError> {
        self.shared
            .lock()
            .map_err(|_| AsrError::Inference("recording transcriber lock was poisoned".to_owned()))?
            .transcribe_available(RecordingStatus::Complete)
    }

    #[must_use]
    pub fn media_cursor(&self) -> Duration {
        self.shared
            .lock()
            .map_or(Duration::ZERO, |transcriber| transcriber.media_cursor())
    }
}

impl Transcriber for LiveRecordingTranscriber {
    fn push(&mut self, _frame: &AudioFrame) {
        // Capture audio is intentionally not an ASR input. It only wakes the pipeline poll path;
        // committed recording media is the single transcription source of truth.
    }

    fn poll(&mut self) -> Vec<TranscriptUpdate> {
        let now = Instant::now();
        if self
            .last_poll
            .is_some_and(|last| now.duration_since(last) < self.poll_interval)
        {
            return Vec::new();
        }
        self.last_poll = Some(now);
        self.shared
            .lock()
            .ok()
            .and_then(|mut transcriber| {
                transcriber
                    .transcribe_available(RecordingStatus::Growing)
                    .ok()
            })
            .unwrap_or_default()
    }
}

impl LaggedRecordingTranscriber {
    pub fn new(path: impl Into<PathBuf>, config: RecordingConfig) -> Result<Self, AsrError> {
        Self::with_reader(
            Box::new(PlatformRecordingReader { path: path.into() }),
            config,
        )
    }

    fn with_reader(
        reader: Box<dyn RecordingMediaReader>,
        config: RecordingConfig,
    ) -> Result<Self, AsrError> {
        if config.lag < SEGMENT_COMMIT_INTERVAL {
            return Err(AsrError::ModelLoad(format!(
                "recording transcription lag must be at least the {}-second segment commit interval",
                SEGMENT_COMMIT_INTERVAL.as_secs()
            )));
        }
        let inner = FinalWhisperTranscriber::new(config.transcription.clone())?;
        Ok(Self {
            reader,
            config,
            inner,
            cursor: Duration::ZERO,
            sequence: 0,
            complete: false,
        })
    }

    fn read_to(&mut self, end: Duration) -> Result<(), AsrError> {
        if end <= self.cursor {
            return Ok(());
        }
        let frames = self
            .reader
            .read_stereo(self.cursor, end, &mut self.sequence)?;
        for frame in frames {
            if let Some(frame) = route_recording_frame(self.config.layout, frame) {
                self.inner.push(&frame);
            }
        }
        self.cursor = end;
        Ok(())
    }
}

fn route_recording_frame(layout: RecordingLayout, mut frame: AudioFrame) -> Option<AudioFrame> {
    match (layout, frame.source) {
        (RecordingLayout::MeetingStereo, _) => Some(frame),
        (RecordingLayout::MicrophoneOnly, Source::System) => {
            frame.source = Source::Mic;
            Some(frame)
        }
        (RecordingLayout::MicrophoneOnly, Source::Mic) => None,
    }
}

impl RecordingTranscriber for LaggedRecordingTranscriber {
    fn transcribe_available(
        &mut self,
        status: RecordingStatus,
    ) -> Result<Vec<TranscriptUpdate>, AsrError> {
        if self.complete {
            return Ok(Vec::new());
        }
        let duration = self.reader.duration()?;
        let readable_end = match status {
            RecordingStatus::Growing => duration.saturating_sub(self.config.lag),
            RecordingStatus::Complete => duration,
        };
        self.read_to(readable_end)?;
        if status == RecordingStatus::Complete {
            self.complete = true;
            self.inner.finish();
        }
        let updates = self.inner.poll();
        if let Some(error) = self.inner.poll_errors().into_iter().next() {
            return Err(error);
        }
        Ok(updates)
    }

    fn media_cursor(&self) -> Duration {
        self.cursor
    }
}

trait RecordingMediaReader: Send {
    fn duration(&self) -> Result<Duration, AsrError>;
    fn read_stereo(
        &self,
        start: Duration,
        end: Duration,
        sequence: &mut u64,
    ) -> Result<Vec<AudioFrame>, AsrError>;
}

struct PlatformRecordingReader {
    path: PathBuf,
}

impl RecordingMediaReader for PlatformRecordingReader {
    fn duration(&self) -> Result<Duration, AsrError> {
        platform::duration(&self.path)
    }

    fn read_stereo(
        &self,
        start: Duration,
        end: Duration,
        sequence: &mut u64,
    ) -> Result<Vec<AudioFrame>, AsrError> {
        platform::read_stereo(&self.path, start, end, sequence)
    }
}

#[cfg(target_os = "macos")]
mod platform {
    use super::*;

    type AudioCallback =
        unsafe extern "C" fn(*mut c_void, *const c_float, *const c_float, usize, u64);

    unsafe extern "C" {
        fn sotto_asr_recording_duration(
            path: *const c_char,
            duration_ns: *mut u64,
            error: *mut c_char,
            error_capacity: usize,
        ) -> bool;
        fn sotto_asr_read_stereo(
            path: *const c_char,
            start_ns: u64,
            end_ns: u64,
            callback: AudioCallback,
            context: *mut c_void,
            error: *mut c_char,
            error_capacity: usize,
        ) -> bool;
    }

    pub(super) struct CallbackState {
        pub(super) frames: Vec<AudioFrame>,
        pub(super) sequence: u64,
    }

    pub(super) fn duration(path: &Path) -> Result<Duration, AsrError> {
        let path = path_string(path)?;
        let mut duration_ns = 0_u64;
        let mut error = [0_i8; ERROR_CAPACITY];
        // SAFETY: all pointers remain valid for this synchronous native call.
        let success = unsafe {
            sotto_asr_recording_duration(
                path.as_ptr(),
                &mut duration_ns,
                error.as_mut_ptr(),
                error.len(),
            )
        };
        if success {
            Ok(Duration::from_nanos(duration_ns))
        } else {
            Err(native_error(&error))
        }
    }

    pub(super) fn read_stereo(
        path: &Path,
        start: Duration,
        end: Duration,
        sequence: &mut u64,
    ) -> Result<Vec<AudioFrame>, AsrError> {
        let path = path_string(path)?;
        let mut error = [0_i8; ERROR_CAPACITY];
        let mut state = CallbackState {
            frames: Vec::new(),
            sequence: *sequence,
        };
        // SAFETY: the callback state and buffers remain valid for the synchronous read. The
        // native bridge invokes `receive_audio` only before this function returns.
        let success = unsafe {
            sotto_asr_read_stereo(
                path.as_ptr(),
                duration_ns(start),
                duration_ns(end),
                receive_audio,
                (&mut state as *mut CallbackState).cast(),
                error.as_mut_ptr(),
                error.len(),
            )
        };
        if !success {
            return Err(native_error(&error));
        }
        *sequence = state.sequence;
        Ok(state.frames)
    }

    pub(super) unsafe extern "C" fn receive_audio(
        context: *mut c_void,
        left: *const c_float,
        right: *const c_float,
        count: usize,
        timestamp_ns: u64,
    ) {
        if context.is_null() || left.is_null() || right.is_null() || count == 0 {
            return;
        }
        // SAFETY: validated above; Swift guarantees both channel buffers contain `count` samples
        // and retains them for the duration of this callback.
        let state = unsafe { &mut *context.cast::<CallbackState>() };
        // SAFETY: same native callback contract described above.
        let meeting = unsafe { std::slice::from_raw_parts(left, count) };
        // SAFETY: same native callback contract described above.
        let microphone = unsafe { std::slice::from_raw_parts(right, count) };
        let timestamp = Duration::from_nanos(timestamp_ns);
        for (source, samples) in [(Source::System, meeting), (Source::Mic, microphone)] {
            state.frames.push(AudioFrame {
                source,
                samples: Arc::from(samples),
                sample_rate: 16_000,
                seq: state.sequence,
                capture_ts: Instant::now(),
                stream_offset: timestamp,
            });
            state.sequence = state.sequence.saturating_add(1);
        }
    }

    fn path_string(path: &Path) -> Result<CString, AsrError> {
        CString::new(path.to_string_lossy().as_bytes())
            .map_err(|_| AsrError::Inference("recording path contains a NUL byte".to_owned()))
    }

    fn native_error(buffer: &[c_char; ERROR_CAPACITY]) -> AsrError {
        // SAFETY: the zero-initialized fixed buffer remains NUL-terminated even if native code
        // does not write a diagnostic.
        let detail = unsafe { CStr::from_ptr(buffer.as_ptr()) }
            .to_string_lossy()
            .into_owned();
        AsrError::Inference(if detail.is_empty() {
            "recording reader failed without a native diagnostic".to_owned()
        } else {
            detail
        })
    }

    fn duration_ns(duration: Duration) -> u64 {
        u64::try_from(duration.as_nanos()).unwrap_or(u64::MAX)
    }
}

#[cfg(test)]
#[cfg(target_os = "macos")]
mod tests {
    use std::{
        sync::{Arc, Mutex},
        time::Duration,
    };

    use sotto_core::{RecordingStatus, RecordingTranscriber, Source, Transcriber};
    use tempfile::NamedTempFile;

    use super::platform::{CallbackState, receive_audio};
    use super::{
        DEFAULT_RECORDING_LAG, LaggedRecordingTranscriber, LiveRecordingTranscriber,
        RecordingConfig, RecordingLayout, RecordingMediaReader, RecordingTranscriptionHandle,
        SEGMENT_COMMIT_INTERVAL, route_recording_frame,
    };
    use crate::Config;

    #[test]
    fn native_left_and_right_channels_map_to_meeting_and_microphone() {
        let left = [0.25_f32, 0.5];
        let right = [-0.25_f32, -0.5];
        let mut state = CallbackState {
            frames: Vec::new(),
            sequence: 7,
        };

        // SAFETY: all callback pointers address fixed local storage for the synchronous call.
        unsafe {
            receive_audio(
                (&mut state as *mut CallbackState).cast(),
                left.as_ptr(),
                right.as_ptr(),
                left.len(),
                3_000_000_000,
            );
        }

        assert_eq!(state.frames.len(), 2);
        assert_eq!(state.frames[0].source, Source::System);
        assert_eq!(&*state.frames[0].samples, left);
        assert_eq!(state.frames[1].source, Source::Mic);
        assert_eq!(&*state.frames[1].samples, right);
        assert_eq!(state.frames[0].stream_offset, Duration::from_secs(3));
        assert_eq!(state.frames[0].seq, 7);
        assert_eq!(state.frames[1].seq, 8);
    }

    #[test]
    fn microphone_only_routes_channel_zero_once_as_mic() -> Result<(), Box<dyn std::error::Error>> {
        let left = sotto_core::AudioFrame {
            source: Source::System,
            samples: Arc::from([0.25_f32, 0.5]),
            sample_rate: 16_000,
            seq: 1,
            capture_ts: std::time::Instant::now(),
            stream_offset: Duration::ZERO,
        };
        let right = sotto_core::AudioFrame {
            source: Source::Mic,
            samples: Arc::from([0.0_f32, 0.0]),
            sample_rate: 16_000,
            seq: 2,
            capture_ts: std::time::Instant::now(),
            stream_offset: Duration::ZERO,
        };

        let routed = route_recording_frame(RecordingLayout::MicrophoneOnly, left)
            .ok_or("channel zero was not retained")?;
        assert_eq!(routed.source, Source::Mic);
        assert_eq!(&*routed.samples, &[0.25, 0.5]);
        assert!(route_recording_frame(RecordingLayout::MicrophoneOnly, right).is_none());
        Ok(())
    }

    struct FakeReader {
        duration: Duration,
        reads: Arc<Mutex<Vec<(Duration, Duration)>>>,
    }

    impl RecordingMediaReader for FakeReader {
        fn duration(&self) -> Result<Duration, sotto_core::AsrError> {
            Ok(self.duration)
        }

        fn read_stereo(
            &self,
            start: Duration,
            end: Duration,
            _sequence: &mut u64,
        ) -> Result<Vec<sotto_core::AudioFrame>, sotto_core::AsrError> {
            self.reads
                .lock()
                .map_err(|error| sotto_core::AsrError::Inference(error.to_string()))?
                .push((start, end));
            Ok(Vec::new())
        }
    }

    #[test]
    fn growing_reader_holds_configured_lag_then_complete_catches_up_once()
    -> Result<(), Box<dyn std::error::Error>> {
        let model = NamedTempFile::new()?;
        let reads = Arc::new(Mutex::new(Vec::new()));
        let reader = FakeReader {
            duration: Duration::from_secs(25),
            reads: Arc::clone(&reads),
        };
        let mut config = RecordingConfig::new(Config::new(model.path()));
        config.lag = Duration::from_secs(10);
        let mut transcriber = LaggedRecordingTranscriber::with_reader(Box::new(reader), config)?;

        assert!(
            transcriber
                .transcribe_available(RecordingStatus::Growing)?
                .is_empty()
        );
        assert_eq!(transcriber.media_cursor(), Duration::from_secs(15));
        assert!(
            transcriber
                .transcribe_available(RecordingStatus::Complete)?
                .is_empty()
        );
        assert_eq!(transcriber.media_cursor(), Duration::from_secs(25));
        assert!(
            transcriber
                .transcribe_available(RecordingStatus::Complete)?
                .is_empty()
        );
        let recorded_reads = reads.lock().map_err(|error| error.to_string())?.clone();
        assert_eq!(
            recorded_reads,
            [
                (Duration::ZERO, Duration::from_secs(15)),
                (Duration::from_secs(15), Duration::from_secs(25)),
            ]
        );
        Ok(())
    }

    #[test]
    fn live_adapter_retains_the_same_cursor_for_post_finalize_tail()
    -> Result<(), Box<dyn std::error::Error>> {
        let model = NamedTempFile::new()?;
        let reads = Arc::new(Mutex::new(Vec::new()));
        let inner = LaggedRecordingTranscriber::with_reader(
            Box::new(FakeReader {
                duration: Duration::from_secs(25),
                reads: Arc::clone(&reads),
            }),
            RecordingConfig::new(Config::new(model.path())),
        )?;
        let shared = Arc::new(Mutex::new(inner));
        let mut live = LiveRecordingTranscriber {
            shared: Arc::clone(&shared),
            last_poll: None,
            poll_interval: Duration::ZERO,
        };
        let completion = RecordingTranscriptionHandle { shared };
        // Expressed against the default rather than a literal, so that changing how far the
        // transcript trails live cannot leave this test asserting the previous product behaviour.
        let growing_end = Duration::from_secs(25) - DEFAULT_RECORDING_LAG;

        assert!(live.poll().is_empty());
        assert_eq!(completion.media_cursor(), growing_end);
        drop(live);
        assert!(completion.transcribe_complete()?.is_empty());
        assert_eq!(completion.media_cursor(), Duration::from_secs(25));
        assert_eq!(
            reads.lock().map_err(|error| error.to_string())?.as_slice(),
            [
                (Duration::ZERO, growing_end),
                (growing_end, Duration::from_secs(25)),
            ]
        );
        Ok(())
    }

    #[test]
    fn lag_below_the_segment_commit_interval_is_rejected() -> Result<(), Box<dyn std::error::Error>>
    {
        let model = NamedTempFile::new()?;
        let mut config = RecordingConfig::new(Config::new(model.path()));
        config.lag = SEGMENT_COMMIT_INTERVAL
            .checked_sub(Duration::from_millis(1))
            .ok_or("segment commit interval must be positive")?;

        let error = LaggedRecordingTranscriber::with_reader(
            Box::new(FakeReader {
                duration: Duration::from_secs(25),
                reads: Arc::new(Mutex::new(Vec::new())),
            }),
            config,
        )
        .err()
        .ok_or("reading into the uncommitted tail must be refused")?;

        assert!(
            error.to_string().contains("segment commit interval"),
            "unexpected error: {error}"
        );
        Ok(())
    }
}

#[cfg(not(target_os = "macos"))]
mod platform {
    use super::*;

    pub(super) fn duration(_path: &Path) -> Result<Duration, AsrError> {
        Err(AsrError::Inference(
            "recording transcription is currently available only on macOS".to_owned(),
        ))
    }

    pub(super) fn read_stereo(
        _path: &Path,
        _start: Duration,
        _end: Duration,
        _sequence: &mut u64,
    ) -> Result<Vec<AudioFrame>, AsrError> {
        Err(AsrError::Inference(
            "recording transcription is currently available only on macOS".to_owned(),
        ))
    }
}
