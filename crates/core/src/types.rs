//! Domain values shared by every stage of the real-time pipeline.
//!
//! [`Utterance::render_inline`] exclusively owns the canonical LLM-ready transcript
//! format. The prosody crate selects annotations and manages their token budget; it
//! does not implement a competing renderer.

use std::{
    collections::BTreeMap,
    sync::Arc,
    time::{Duration, Instant},
};

use crate::EventId;

/// The independently captured audio stream that produced an event.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Deserialize, serde::Serialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "snake_case"))]
pub enum Source {
    /// The sales representative's microphone.
    Mic,
    /// System audio carrying the customer's voice.
    System,
}

impl Source {
    /// Returns the human-readable speaker name used in inline transcripts.
    #[must_use]
    pub const fn speaker_name(self) -> &'static str {
        match self {
            Self::Mic => "rep",
            Self::System => "customer",
        }
    }
}

/// A cheap-to-clone frame of mono audio from one capture stream.
#[derive(Clone, Debug)]
#[cfg_attr(feature = "serde", derive(serde::Deserialize, serde::Serialize))]
pub struct AudioFrame {
    pub source: Source,
    pub samples: Arc<[f32]>,
    pub sample_rate: u32,
    pub seq: u64,
    /// Local monotonic timestamp used only to measure processing latency.
    #[cfg_attr(feature = "serde", serde(skip, default = "instant_now"))]
    pub capture_ts: Instant,
    /// Monotonic position from the start of this audio stream.
    pub stream_offset: Duration,
}

#[cfg(feature = "serde")]
fn instant_now() -> Instant {
    Instant::now()
}

/// A VAD transition aligned to a capture stream.
#[derive(Clone, Debug, Eq, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Deserialize, serde::Serialize))]
pub struct VadSegment {
    pub source: Source,
    pub start: Duration,
    pub end: Option<Duration>,
    pub kind: SpeechState,
}

/// A transition emitted by voice activity detection.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Deserialize, serde::Serialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "snake_case"))]
pub enum SpeechState {
    SpeechStart,
    SpeechEnd,
}

/// ASR content carried by a partial or final timeline payload.
#[derive(Clone, Debug, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Deserialize, serde::Serialize))]
pub struct Utterance {
    pub source: Source,
    pub start: Duration,
    pub end: Duration,
    pub text: String,
    pub avg_logprob: f32,
    pub annotations: Vec<Annotation>,
}

impl Utterance {
    /// Renders the speaker and local prosody as an LLM-ready transcript line.
    #[must_use]
    pub fn render_inline(&self) -> String {
        let mut context = self.source.speaker_name().to_owned();
        for annotation in &self.annotations {
            context.push_str(", ");
            context.push_str(&annotation.render_inline());
        }
        format!("[{context}] \"{}\"", self.text)
    }
}

/// Locally derived conversational context attached to an utterance.
#[derive(Clone, Debug, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Deserialize, serde::Serialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "snake_case"))]
pub enum Annotation {
    Pause(Duration),
    Interruption { by: Source },
    SpeechRate(f32),
    TalkTimeRatio(f32),
    Hesitant,
    Emphatic,
}

impl Annotation {
    /// Renders this annotation as a compact inline transcript fragment.
    #[must_use]
    pub fn render_inline(&self) -> String {
        match self {
            Self::Pause(duration) => {
                format!("{:.1}s pause", duration.as_secs_f64())
            }
            Self::Interruption { by } => {
                format!("interrupted by {}", by.speaker_name())
            }
            Self::SpeechRate(words_per_minute) => {
                format!("{words_per_minute:.0} wpm")
            }
            Self::TalkTimeRatio(ratio) => {
                format!("{:.0}% talk time", ratio * 100.0)
            }
            Self::Hesitant => "hesitant".to_owned(),
            Self::Emphatic => "emphatic".to_owned(),
        }
    }
}

/// The v1 reason that the watcher requested a suggestion.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Deserialize, serde::Serialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "snake_case"))]
pub enum TriggerKind {
    CompetitorMention,
    PricingQuestion,
    Objection,
    DiscoveryGap,
}

/// Stable location of the utterance that prompted a trigger.
#[derive(Clone, Debug, Eq, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Deserialize, serde::Serialize))]
pub struct UtteranceSpan {
    pub source: Source,
    pub start: Duration,
    pub end: Duration,
}

/// A watcher decision that warrants invoking the suggestion model.
#[derive(Clone, Debug, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Deserialize, serde::Serialize))]
pub struct Trigger {
    pub kind: TriggerKind,
    pub utterance_span: UtteranceSpan,
    pub confidence: f32,
}

/// Evidence supporting a generated suggestion.
#[derive(Clone, Debug, Eq, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Deserialize, serde::Serialize))]
pub struct Citation {
    pub title: String,
    pub uri: Option<String>,
    pub excerpt: String,
}

/// Suggestion content carried by a partial or final timeline payload.
#[derive(Clone, Debug, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Deserialize, serde::Serialize))]
pub struct Suggestion {
    /// Timeline events whose context caused this suggestion.
    pub anchors: Vec<EventId>,
    pub trigger: Trigger,
    pub text: String,
    pub citations: Vec<Citation>,
}

/// A locally indexed passage returned by retrieval.
#[derive(Clone, Debug, Eq, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Deserialize, serde::Serialize))]
pub struct Chunk {
    pub id: String,
    pub text: String,
    pub source: String,
    pub metadata: BTreeMap<String, String>,
}

/// The role of one message in a provider-neutral completion request.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Deserialize, serde::Serialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "snake_case"))]
pub enum MessageRole {
    System,
    User,
    Assistant,
}

/// One message in a provider-neutral completion request.
#[derive(Clone, Debug, Eq, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Deserialize, serde::Serialize))]
pub struct CompletionMessage {
    pub role: MessageRole,
    pub content: String,
    /// Everything through this message is static and may be provider-cached.
    ///
    /// At most one message in a request may set this. Providers without prompt
    /// caching ignore the boundary.
    pub cache_boundary: bool,
}

/// Provider-neutral input for a streaming completion.
#[derive(Clone, Debug, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Deserialize, serde::Serialize))]
pub struct CompletionRequest {
    pub model: String,
    pub system: Option<String>,
    pub messages: Vec<CompletionMessage>,
    pub max_tokens: Option<u32>,
    pub temperature: Option<f32>,
    pub stop: Vec<String>,
}

/// One provider-neutral update from a streaming completion.
#[derive(Clone, Debug, Eq, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Deserialize, serde::Serialize))]
pub struct Delta {
    pub text: String,
    pub is_final: bool,
    /// Token accounting, populated on the final delta.
    pub usage: Option<Usage>,
    /// Why generation ended, populated on the final delta.
    pub stop_reason: Option<StopReason>,
}

/// Provider-reported token accounting for a completion.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Deserialize, serde::Serialize))]
pub struct Usage {
    pub input_tokens: u32,
    pub output_tokens: u32,
    pub cache_read_tokens: u32,
    pub cache_write_tokens: u32,
}

/// Provider-neutral reason a completion stream ended.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Deserialize, serde::Serialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "snake_case"))]
pub enum StopReason {
    EndTurn,
    MaxTokens,
    StopSequence,
    Aborted,
}

/// Current macOS capture permission state.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Deserialize, serde::Serialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "snake_case"))]
pub enum PermissionStatus {
    NotDetermined,
    Authorized,
    Denied,
    Restricted,
}

#[cfg(test)]
mod tests {
    use super::{Annotation, Source, Utterance};
    use std::time::Duration;

    #[test]
    fn renders_llm_ready_inline_annotation_context() {
        let utterance = Utterance {
            source: Source::System,
            start: Duration::ZERO,
            end: Duration::from_secs(3),
            text: "sure, sounds fine".to_owned(),
            avg_logprob: -0.2,
            annotations: vec![
                Annotation::Hesitant,
                Annotation::Pause(Duration::from_millis(2_500)),
            ],
        };

        assert_eq!(
            utterance.render_inline(),
            "[customer, hesitant, 2.5s pause] \"sure, sounds fine\""
        );
    }

    #[cfg(feature = "serde")]
    #[test]
    fn annotation_json_round_trip_preserves_inline_rendering()
    -> Result<(), Box<dyn std::error::Error>> {
        let original = Annotation::Interruption { by: Source::Mic };
        let json = serde_json::to_string(&original)?;
        let decoded: Annotation = serde_json::from_str(&json)?;

        assert_eq!(decoded.render_inline(), original.render_inline());
        Ok(())
    }

    #[test]
    fn shared_types_satisfy_pipeline_bounds() {
        fn assert_bounds<T: Clone + Send + Sync + 'static>() {}

        assert_bounds::<super::Utterance>();
        assert_bounds::<crate::TimelineEvent>();
    }
}
