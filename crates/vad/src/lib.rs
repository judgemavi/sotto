//! Stateful, per-source voice activity detection.
//!
//! [`SileroVad`] embeds Silero VAD v5.1 from the
//! [upstream repository](https://github.com/snakers4/silero-vad/blob/v5.1/src/silero_vad/data/silero_vad.onnx)
//! (SHA-256 `2623a2953f6ff3d2c1e61740c6cdb7168133479b267dfef114a4a3cc5bdd788f`).
//! The model adds about 2.2 MiB before the ONNX Runtime binary itself; this is the
//! deliberate footprint cost of avoiding a system dependency and a first-run download.
//!
//! Silero's recurrent state belongs to one audio stream. Construction therefore
//! requires a [`Source`], and frames from another source are rejected rather than
//! accidentally contaminating speaker state. Input storage, recurrent state, and the
//! transition queue are fixed-size. After construction, the Rust portion of [`push`](SileroVad::push)
//! performs no heap allocation (ONNX Runtime manages its own inference buffers).

#![deny(warnings)]

use std::{collections::VecDeque, time::Duration};

use ndarray::ArrayView;
use ort::{
    inputs,
    session::{Session, builder::GraphOptimizationLevel},
    value::TensorRef,
};
use sotto_core::{AudioFrame, Source, SpeechState, VadError, VadSegment, VoiceActivityDetector};

const SAMPLE_RATE: u32 = 16_000;
const CHUNK_SAMPLES: usize = 512;
const MODEL: &[u8] = include_bytes!("silero_vad_v5.1.onnx");

/// Thresholds and debounce durations applied to raw model probabilities.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct VadConfig {
    pub speech_threshold: f32,
    pub silence_threshold: f32,
    pub min_speech_duration: Duration,
    pub min_silence_duration: Duration,
}

impl Default for VadConfig {
    fn default() -> Self {
        Self {
            speech_threshold: 0.5,
            silence_threshold: 0.35,
            min_speech_duration: Duration::from_millis(250),
            min_silence_duration: Duration::from_millis(500),
        }
    }
}

impl VadConfig {
    fn validate(self) -> Result<Self, VadError> {
        if !(0.0..=1.0).contains(&self.silence_threshold)
            || !(0.0..=1.0).contains(&self.speech_threshold)
            || self.silence_threshold >= self.speech_threshold
        {
            return Err(VadError::ModelLoad(
                "thresholds must satisfy 0 <= silence < speech <= 1".to_owned(),
            ));
        }
        Ok(self)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum DetectorState {
    Silent,
    MaybeSpeech {
        since: Duration,
    },
    Speaking {
        since: Duration,
    },
    MaybeSilence {
        speech_since: Duration,
        since: Duration,
    },
}

/// Silero VAD with an accumulator and recurrent state dedicated to one source.
pub struct SileroVad {
    source: Source,
    config: VadConfig,
    session: Session,
    input: [f32; CHUNK_SAMPLES],
    input_len: usize,
    input_start: Duration,
    recurrent: [f32; 256],
    state: DetectorState,
    pending: VecDeque<VadSegment>,
    last_decision_delay: Duration,
    last_error: Option<VadError>,
}

impl SileroVad {
    /// Loads the embedded model. Create one detector for each [`Source`].
    pub fn new(source: Source, config: VadConfig) -> Result<Self, VadError> {
        let config = config.validate()?;
        let session = Session::builder()
            .and_then(|builder| builder.with_optimization_level(GraphOptimizationLevel::Level3))
            .and_then(|builder| builder.commit_from_memory(MODEL))
            .map_err(|error| VadError::ModelLoad(error.to_string()))?;
        Ok(Self {
            source,
            config,
            session,
            input: [0.0; CHUNK_SAMPLES],
            input_len: 0,
            input_start: Duration::ZERO,
            recurrent: [0.0; 256],
            state: DetectorState::Silent,
            pending: VecDeque::with_capacity(4),
            last_decision_delay: Duration::ZERO,
            last_error: None,
        })
    }

    #[must_use]
    pub const fn source(&self) -> Source {
        self.source
    }

    /// Actual debounce delay observed for the most recent emitted transition.
    #[must_use]
    pub const fn last_decision_delay(&self) -> Duration {
        self.last_decision_delay
    }

    /// Returns and clears a frame-validation or inference error encountered by `push`.
    pub fn take_error(&mut self) -> Option<VadError> {
        self.last_error.take()
    }

    fn infer(&mut self) -> Result<f32, VadError> {
        let input_view = ArrayView::from_shape((1, CHUNK_SAMPLES), &self.input)
            .map_err(|error| VadError::Inference(error.to_string()))?;
        let input = TensorRef::from_array_view(input_view)
            .map_err(|error| VadError::Inference(error.to_string()))?;
        let state_view = ArrayView::from_shape((2, 1, 128), &self.recurrent)
            .map_err(|error| VadError::Inference(error.to_string()))?;
        let state = TensorRef::from_array_view(state_view)
            .map_err(|error| VadError::Inference(error.to_string()))?;
        let sample_rate = [i64::from(SAMPLE_RATE)];
        let sample_rate = TensorRef::from_array_view(([1_usize], &sample_rate[..]))
            .map_err(|error| VadError::Inference(error.to_string()))?;
        let outputs = self
            .session
            .run(inputs!["input" => input, "state" => state, "sr" => sample_rate])
            .map_err(|error| VadError::Inference(error.to_string()))?;
        let output = outputs
            .get("output")
            .ok_or_else(|| VadError::Inference("model omitted probability output".to_owned()))?;
        let (_, probabilities) = output
            .try_extract_tensor::<f32>()
            .map_err(|error| VadError::Inference(error.to_string()))?;
        let probability = probabilities
            .first()
            .copied()
            .ok_or_else(|| VadError::Inference("model returned no probability".to_owned()))?;
        let state_output = outputs
            .get("stateN")
            .ok_or_else(|| VadError::Inference("model omitted recurrent state".to_owned()))?;
        let (_, next_state) = state_output
            .try_extract_tensor::<f32>()
            .map_err(|error| VadError::Inference(error.to_string()))?;
        if next_state.len() != self.recurrent.len() {
            return Err(VadError::Inference(
                "model returned invalid recurrent state".to_owned(),
            ));
        }
        self.recurrent.copy_from_slice(next_state);
        Ok(probability)
    }

    fn observe(&mut self, probability: f32, chunk_start: Duration) {
        let chunk_end = chunk_start + chunk_duration();
        self.state = match self.state {
            DetectorState::Silent if probability >= self.config.speech_threshold => {
                DetectorState::MaybeSpeech { since: chunk_start }
            }
            DetectorState::MaybeSpeech { since }
                if probability >= self.config.speech_threshold
                    && chunk_end.saturating_sub(since) >= self.config.min_speech_duration =>
            {
                self.last_decision_delay = chunk_end.saturating_sub(since);
                self.enqueue(VadSegment {
                    source: self.source,
                    start: since,
                    end: None,
                    kind: SpeechState::SpeechStart,
                });
                DetectorState::Speaking { since }
            }
            DetectorState::MaybeSpeech { .. } if probability < self.config.silence_threshold => {
                DetectorState::Silent
            }
            DetectorState::Speaking { since } if probability < self.config.silence_threshold => {
                DetectorState::MaybeSilence {
                    speech_since: since,
                    since: chunk_start,
                }
            }
            DetectorState::MaybeSilence {
                speech_since,
                since,
            } if probability < self.config.silence_threshold
                && chunk_end.saturating_sub(since) >= self.config.min_silence_duration =>
            {
                self.last_decision_delay = chunk_end.saturating_sub(since);
                self.enqueue(VadSegment {
                    source: self.source,
                    start: speech_since,
                    end: Some(since),
                    kind: SpeechState::SpeechEnd,
                });
                DetectorState::Silent
            }
            DetectorState::MaybeSilence { speech_since, .. }
                if probability >= self.config.speech_threshold =>
            {
                DetectorState::Speaking {
                    since: speech_since,
                }
            }
            state => state,
        };
    }

    fn enqueue(&mut self, segment: VadSegment) {
        if self.pending.len() == self.pending.capacity() {
            self.last_error = Some(VadError::Inference(
                "transition queue exhausted; push frames more frequently".to_owned(),
            ));
            return;
        }
        self.pending.push_back(segment);
    }

    fn push_inner(&mut self, frame: &AudioFrame) -> Result<(), VadError> {
        if frame.source != self.source {
            return Err(VadError::Inference(format!(
                "detector for {:?} received {:?} frame",
                self.source, frame.source
            )));
        }
        if frame.sample_rate != SAMPLE_RATE {
            return Err(VadError::Inference(format!(
                "Silero VAD requires {SAMPLE_RATE} Hz audio, received {} Hz",
                frame.sample_rate
            )));
        }
        let mut consumed = 0;
        while consumed < frame.samples.len() {
            if self.input_len == 0 {
                self.input_start = frame.stream_offset + samples_duration(consumed);
            }
            let count = (CHUNK_SAMPLES - self.input_len).min(frame.samples.len() - consumed);
            self.input[self.input_len..self.input_len + count]
                .copy_from_slice(&frame.samples[consumed..consumed + count]);
            self.input_len += count;
            consumed += count;
            if self.input_len == CHUNK_SAMPLES {
                let probability = self.infer()?;
                self.observe(probability, self.input_start);
                self.input_len = 0;
            }
        }
        Ok(())
    }
}

impl VoiceActivityDetector for SileroVad {
    fn push(&mut self, frame: &AudioFrame) -> Option<VadSegment> {
        if let Err(error) = self.push_inner(frame) {
            self.last_error = Some(error);
        }
        self.pending.pop_front()
    }

    fn reset(&mut self) {
        self.input.fill(0.0);
        self.input_len = 0;
        self.input_start = Duration::ZERO;
        self.recurrent.fill(0.0);
        self.state = DetectorState::Silent;
        self.pending.clear();
        self.last_decision_delay = Duration::ZERO;
        self.last_error = None;
    }
}

const fn chunk_duration() -> Duration {
    Duration::from_millis(32)
}

fn samples_duration(samples: usize) -> Duration {
    Duration::from_secs_f64(samples as f64 / f64::from(SAMPLE_RATE))
}

#[cfg(test)]
mod tests {
    use super::{DetectorState, SileroVad, VadConfig, chunk_duration};
    use sotto_core::{Source, SpeechState};
    use std::time::Duration;

    fn detector(config: VadConfig) -> Result<SileroVad, sotto_core::VadError> {
        SileroVad::new(Source::Mic, config)
    }

    #[test]
    fn hysteresis_debounces_start_and_end() -> Result<(), Box<dyn std::error::Error>> {
        let config = VadConfig {
            min_speech_duration: Duration::from_millis(64),
            min_silence_duration: Duration::from_millis(64),
            ..VadConfig::default()
        };
        let mut vad = detector(config)?;
        vad.observe(0.8, Duration::ZERO);
        assert!(vad.pending.is_empty(), "one frame must not start speech");
        vad.observe(0.8, chunk_duration());
        let start = vad.pending.pop_front().ok_or("missing start transition")?;
        assert_eq!(start.kind, SpeechState::SpeechStart);
        vad.observe(0.1, Duration::from_millis(64));
        assert!(
            vad.pending.is_empty(),
            "one quiet frame must not end speech"
        );
        vad.observe(0.1, Duration::from_millis(96));
        let end = vad.pending.pop_front().ok_or("missing end transition")?;
        assert_eq!(end.kind, SpeechState::SpeechEnd);
        assert_eq!(end.end, Some(Duration::from_millis(64)));
        assert_eq!(vad.last_decision_delay(), Duration::from_millis(64));
        Ok(())
    }

    #[test]
    fn mid_sentence_breath_returns_to_speaking() -> Result<(), Box<dyn std::error::Error>> {
        let mut vad = detector(VadConfig::default())?;
        vad.state = DetectorState::Speaking {
            since: Duration::ZERO,
        };
        vad.observe(0.1, Duration::from_secs(1));
        vad.observe(0.8, Duration::from_millis(1_032));
        assert_eq!(
            vad.state,
            DetectorState::Speaking {
                since: Duration::ZERO
            }
        );
        assert!(
            vad.pending.is_empty(),
            "short breath must emit no transition"
        );
        Ok(())
    }

    #[test]
    fn thresholds_must_form_hysteresis_band() -> Result<(), Box<dyn std::error::Error>> {
        let error = SileroVad::new(
            Source::Mic,
            VadConfig {
                speech_threshold: 0.4,
                silence_threshold: 0.4,
                ..VadConfig::default()
            },
        )
        .err()
        .ok_or("invalid thresholds unexpectedly succeeded")?;
        assert!(error.to_string().contains("silence < speech"));
        Ok(())
    }
}
