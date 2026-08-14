//! Final-only, file-backed Whisper transcription in recording media time.

#![deny(warnings)]

mod finals;
mod recording;

pub mod model;

use std::{path::PathBuf, time::Duration};

use sotto_core::AsrError;

pub use finals::FinalWhisperTranscriber;
pub use recording::{
    DecodedRecordingChannel, LaggedRecordingTranscriber, LiveRecordingTranscriber, RecordingConfig,
    RecordingTranscriptionHandle, decode_recording_channel,
};

const SAMPLE_RATE: u32 = 16_000;

/// Routes whisper.cpp/GGML diagnostics away from stderr for normal product use.
///
/// Set `SOTTO_WHISPER_LOGS=1` before process start to retain the native backend inventory when
/// diagnosing Metal activation. ASR failures still travel through typed `AsrError` values.
pub fn configure_native_logging() {
    if std::env::var_os("SOTTO_WHISPER_LOGS").as_deref() != Some(std::ffi::OsStr::new("1")) {
        whisper_rs::install_logging_hooks();
    }
}

/// Bundled-weight-free model size selected by the user/download UI.
///
/// The default is chosen by measurement, not by size. On a 111-second captured recording scored
/// against an independent reference, `small.en` reached 4.75% WER against `base.en`'s 6.98% — about
/// a third fewer errors — and `medium.en` was both slower and *worse* at 9.22%, its errors
/// concentrated in outright hallucinations rather than diffuse degradation. The aggregate also
/// undersells the gain: `base.en` renders `Rust` as `us` and `GitLab` as `Gillab` on clean
/// single-speaker narration, which `small.en` gets right. Proper nouns are exactly the words that
/// reach a summary carrying a citation. See docs/experiments/asr-model-benchmark.md.
///
/// The costs are a 488 MB first-run download and roughly 653 MiB resident while transcribing,
/// against `base.en`'s 148 MB and 278 MiB. Throughput is not a constraint for either: transcription
/// runs behind a lagged recording and `small.en` still manages 12.3x realtime.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum ModelSize {
    BaseEn,
    #[default]
    SmallEn,
    MediumEn,
}

/// Accuracy techniques applied around each Whisper call.
///
/// [`Default`] is the behaviour Sotto shipped before any of these existed, so the benchmark's
/// baseline stays reproducible. [`Config::new`] is where the product's measured choices live, and
/// it deliberately differs. `crates/asr/examples/model_benchmark.rs` drives exactly this struct.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DecodingOptions {
    /// Already-committed audio re-encoded ahead of the window purely as acoustic context.
    ///
    /// Whisper pads its mel to thirty seconds whatever it is handed, so context up to that budget
    /// is close to free in encoder time. It exists so a window boundary is not also a knowledge
    /// boundary: without it the model sees a phrase that begins mid-word and completes it by
    /// invention.
    pub context: Duration,
    /// Searches this far back from the window edge for the quietest point and cuts there instead.
    ///
    /// A cut on the sample count lands mid-word roughly as often as not, and a word split across
    /// two windows is usually decoded by neither. Cutting at a trough moves the boundary into a
    /// pause the speaker already left.
    pub boundary_search: Duration,
    /// Carries the tail of the previous committed text as Whisper's initial prompt.
    ///
    /// Supplies lexical rather than acoustic context, which is what an ambiguous domain word needs
    /// — "Rust" and "us" are near-identical acoustically and separated only by what came before.
    pub prompt_carryover: bool,
    /// Denies whisper.cpp's non-speech vocabulary — brackets, parentheses, music notes — at decode
    /// time (`suppress_nst`). Whisper has no single token for `[BLANK_AUDIO]` or `(laughs)`; it
    /// assembles them from ordinary punctuation tokens, so denying those tokens denies the tags.
    ///
    /// This was formerly bundled as `safeguards` together with seven other parameters
    /// (`temperature`, `temperature_inc`, `entropy_thold`, `logprob_thold`, `no_speech_thold`,
    /// `suppress_blank`, `split_on_word`). All seven already equal whisper.cpp's own defaults
    /// (`whisper_full_default_params` in `whisper.cpp/src/whisper.cpp`) or, for `split_on_word`,
    /// have no effect without `max_len`, which Sotto never sets. Measured: the bundle was
    /// byte-identical to baseline on `base.en` and this field alone accounts for the entire
    /// effect. See docs/experiments/asr-model-benchmark.md, "Windowing and decoding sweep" and its
    /// correction, and the 2026-08-14 non-speech-token entry.
    ///
    /// This is a decode-time reduction in *likelihood*, not a guarantee — `finals.rs` also
    /// unconditionally drops any committed segment that is nothing but a bracketed or
    /// parenthesized annotation, regardless of this setting, so the timeline invariant does not
    /// depend on decoder behaviour alone.
    ///
    /// Measured worse than the unconditional filter alone: on real captured audio, denying
    /// whisper.cpp a null decode does not make windows with no clear speech silent, it makes them
    /// invent something plausible instead. On a two-party recording it dropped a real word ("You"
    /// from "You know...") and inserted "But", "like" and a hallucinated "Thank you." where the
    /// unfiltered decode had correctly produced nothing. [`Config::new`] leaves this off; see
    /// docs/experiments/asr-model-benchmark.md.
    pub suppress_non_speech: bool,
}

/// Runtime policy for final-only recording inference.
#[derive(Clone, Debug)]
pub struct Config {
    pub model_path: PathBuf,
    pub model_size: ModelSize,
    pub window: Duration,
    pub cadence: Duration,
    pub decoding: DecodingOptions,
}

impl Config {
    #[must_use]
    pub fn new(model_path: impl Into<PathBuf>) -> Self {
        Self {
            model_path: model_path.into(),
            model_size: ModelSize::default(),
            // Whisper emits nothing until a full window has accumulated, so this is the second
            // half of the transcript's delay behind live. Five seconds is a deliberate trade:
            // whisper.cpp encodes a mel padded to thirty seconds whatever it is handed, so halving
            // the window roughly doubles inference cost per second of audio, and the windows here
            // carry no acoustic context across their boundary. Shorter windows would keep buying
            // latency with accuracy at an increasingly poor rate.
            window: Duration::from_secs(5),
            cadence: Duration::from_millis(500),
            decoding: DecodingOptions {
                // Measured on a 111-second captured recording against an independent reference:
                // 9.50% WER to 6.98%, with deletions falling from 14 to 6. The three words that a
                // clock-aligned cut destroyed outright — "hirable", the "100" of "100 to 200K",
                // and "back-end" — all survive, and nothing is duplicated to recover them. See
                // docs/experiments/asr-model-benchmark.md.
                boundary_search: Duration::from_millis(500),
                // `context` and `prompt_carryover` both measured worse than doing nothing.
                // `suppress_non_speech` measured worse on real captured audio too: on the mic
                // channel of a two-party recording it dropped "You" from "You know...", and on
                // the meeting channel it inserted "But", "like" and a hallucinated "Thank you."
                // where the unfiltered decode had correctly produced nothing. Denying whisper.cpp
                // a null/non-speech decode does not make it silent, it makes it invent a plausible
                // sentence instead — the failure mode a cited transcript can least afford. All
                // three are kept off, and kept configurable so the sweeps that rejected them can be
                // re-run rather than repeated from memory. See
                // docs/experiments/asr-model-benchmark.md.
                ..DecodingOptions::default()
            },
        }
    }
}

pub(crate) struct Hypothesis {
    start: Duration,
    end: Duration,
    text: String,
    avg_logprob: f32,
}

fn validate_model_and_window(config: &Config) -> Result<(), AsrError> {
    if !config.model_path.is_file() {
        return Err(AsrError::ModelNotFound {
            path: config.model_path.display().to_string(),
        });
    }
    if config.window.is_zero() {
        return Err(AsrError::ModelLoad(
            "transcription window must be positive".to_owned(),
        ));
    }
    Ok(())
}

fn duration_samples(duration: Duration) -> usize {
    usize::try_from(duration.as_millis())
        .unwrap_or(usize::MAX)
        .saturating_mul(16_000)
        / 1_000
}

fn samples_duration(sample_count: usize) -> Duration {
    Duration::from_secs_f64(sample_count as f64 / f64::from(SAMPLE_RATE))
}

/// True when `text`, trimmed, is nothing but a single bracketed or parenthesized annotation —
/// Whisper's own convention for non-speech audio (`[BLANK_AUDIO]`, `(laughs)`, `(keyboard
/// clicking)`) rather than transcribed words. Matched on the *whole* segment, never a substring,
/// so a real sentence that merely contains a parenthetical aside is never touched.
///
/// This is the deterministic backstop for [`DecodingOptions::suppress_non_speech`]: that setting
/// asks the decoder not to emit these tokens, which is not the same as guaranteeing it never does.
/// Committed non-speech markers are eligible to be cited by the summarizer as if they were speech,
/// so the timeline must never receive one regardless of decoder behaviour.
fn is_non_speech_marker(text: &str) -> bool {
    let trimmed = text.trim().trim_end_matches(['.', ',', '!', '?']);
    let inner = trimmed
        .strip_prefix('[')
        .and_then(|rest| rest.strip_suffix(']'))
        .or_else(|| {
            trimmed
                .strip_prefix('(')
                .and_then(|rest| rest.strip_suffix(')'))
        });
    inner.is_some_and(|inner| !inner.is_empty() && !inner.contains(['[', ']', '(', ')']))
}

/// Drops empty hypotheses and non-speech markers before anything downstream can commit them.
///
/// Unconditional, mirroring [`bound_hypotheses_to_audio`]'s pattern: this is a timeline invariant
/// enforced once here, not an accuracy technique any [`DecodingOptions`] field gates.
fn retain_speech(hypotheses: Vec<Hypothesis>) -> Vec<Hypothesis> {
    hypotheses
        .into_iter()
        .filter(|hypothesis| !hypothesis.text.is_empty() && !is_non_speech_marker(&hypothesis.text))
        .collect()
}

fn bound_hypotheses_to_audio(
    hypotheses: Vec<Hypothesis>,
    audio_duration: Duration,
) -> Vec<Hypothesis> {
    hypotheses
        .into_iter()
        .filter_map(|mut hypothesis| {
            // whisper.cpp decodes short inputs in a padded context and can return a
            // timestamp token beyond the samples supplied by the caller. That padded
            // horizon is not session media time and must never enter the timeline.
            if hypothesis.start >= audio_duration {
                return None;
            }
            hypothesis.end = hypothesis.end.min(audio_duration);
            (hypothesis.end > hypothesis.start).then_some(hypothesis)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{Hypothesis, bound_hypotheses_to_audio, is_non_speech_marker, retain_speech};
    use std::time::Duration;

    fn hypothesis(start_ms: u64, end_ms: u64, text: &str) -> Hypothesis {
        Hypothesis {
            start: Duration::from_millis(start_ms),
            end: Duration::from_millis(end_ms),
            text: text.to_owned(),
            avg_logprob: -0.1,
        }
    }

    #[test]
    fn padded_decode_timestamps_cannot_exceed_supplied_audio() {
        let supplied_audio = Duration::from_millis(1_150);
        let bounded = bound_hypotheses_to_audio(
            vec![
                hypothesis(0, 8_000, "real speech"),
                hypothesis(7_000, 8_000, "padded horizon"),
            ],
            supplied_audio,
        );

        assert_eq!(bounded.len(), 1);
        assert_eq!(bounded[0].start, Duration::ZERO);
        assert_eq!(bounded[0].end, supplied_audio);
        assert_eq!(bounded[0].text, "real speech");
    }

    #[test]
    fn non_speech_markers_are_recognized_whole_segment_only() {
        assert!(is_non_speech_marker("[BLANK_AUDIO]"));
        assert!(is_non_speech_marker("(laughs)"));
        assert!(is_non_speech_marker("(keyboard clicking)"));
        // Surrounding whitespace and a trailing sentence-ending mark are real Whisper output
        // shapes, not a reason to miss the marker.
        assert!(is_non_speech_marker("  [BLANK_AUDIO]  "));
        assert!(is_non_speech_marker("(laughs)."));
    }

    #[test]
    fn real_speech_is_never_mistaken_for_a_marker() {
        assert!(!is_non_speech_marker("hello world"));
        // An aside inside real speech must survive: only a segment that is *nothing but* the
        // annotation is a marker, never a substring of one.
        assert!(!is_non_speech_marker("Well (laughs) that's funny"));
        assert!(!is_non_speech_marker("(laughs) that's funny"));
        assert!(!is_non_speech_marker(""));
        assert!(!is_non_speech_marker("[]"));
        assert!(!is_non_speech_marker("()"));
        // Mismatched or nested brackets must not be treated as a clean single annotation.
        assert!(!is_non_speech_marker("[BLANK_AUDIO)"));
        assert!(!is_non_speech_marker("[(nested)]"));
    }

    /// Exercises the exact function `finals.rs` calls on every window's hypotheses, proving the
    /// path the committed timeline actually goes through — not just the string predicate in
    /// isolation — drops non-speech markers while leaving real speech, including speech that sits
    /// in the same window as a marker, untouched.
    #[test]
    fn non_speech_markers_never_survive_the_committed_output_path() {
        let hypotheses = vec![
            hypothesis(0, 1_000, "Hey Emily, how you doing?"),
            hypothesis(1_000, 2_000, "[BLANK_AUDIO]"),
            hypothesis(2_000, 3_000, "(laughs)"),
            hypothesis(3_000, 4_000, ""),
            hypothesis(4_000, 5_000, "I guess that's just what happens over here."),
        ];

        let retained = retain_speech(hypotheses);

        assert_eq!(
            retained.iter().map(|h| h.text.as_str()).collect::<Vec<_>>(),
            vec![
                "Hey Emily, how you doing?",
                "I guess that's just what happens over here.",
            ],
            "non-speech markers and empty hypotheses must never reach committed output"
        );
    }
}
