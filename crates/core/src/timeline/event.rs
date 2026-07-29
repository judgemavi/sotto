use std::time::Duration;

use crate::{Annotation, PipelineError, Source, Suggestion, Trigger, Utterance, VadSegment};

/// Monotonic event identity, unique within one session.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
#[cfg_attr(feature = "serde", derive(serde::Deserialize, serde::Serialize))]
pub struct EventId(u64);

impl EventId {
    #[must_use]
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }
}

/// Stable identity of one recorded call.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
#[cfg_attr(feature = "serde", derive(serde::Deserialize, serde::Serialize))]
pub struct SessionId(u128);

impl SessionId {
    #[must_use]
    pub const fn new(value: u128) -> Self {
        Self(value)
    }

    #[must_use]
    pub const fn get(self) -> u128 {
        self.0
    }
}

/// Content-addressed frame identifier or path, never inline image data.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Deserialize, serde::Serialize))]
pub struct FrameRef(String);

impl FrameRef {
    #[must_use]
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Low-rate screen context visible over a timeline interval.
#[derive(Clone, Debug, Eq, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Deserialize, serde::Serialize))]
pub struct ScreenSnapshot {
    pub frame_ref: FrameRef,
    pub ocr_text: String,
    pub active_app: Option<String>,
    pub window_title: Option<String>,
    pub visible_from: Duration,
    pub visible_to: Option<Duration>,
}

/// Independently emitted rolling prosody values.
#[derive(Clone, Debug, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Deserialize, serde::Serialize))]
pub struct ProsodyDelta {
    pub source: Source,
    pub speech_rate: Option<f32>,
    pub talk_time_ratio: f32,
    pub annotations: Vec<Annotation>,
}

/// Visual meaning of a user-created board annotation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Deserialize, serde::Serialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "snake_case"))]
pub enum MarkKind {
    Note,
    Important,
    FollowUp,
}

/// A rep-authored mark anchored to an existing timeline event.
#[derive(Clone, Debug, Eq, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Deserialize, serde::Serialize))]
pub struct UserAnnotation {
    pub anchor: EventId,
    pub text: String,
    pub mark: MarkKind,
}

/// Heterogeneous content carried by one timeline event.
#[derive(Clone, Debug, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Deserialize, serde::Serialize))]
#[cfg_attr(
    feature = "serde",
    serde(tag = "type", content = "value", rename_all = "snake_case")
)]
pub enum EventPayload {
    UtterancePartial(Utterance),
    UtteranceFinal(Utterance),
    Vad(VadSegment),
    Prosody(ProsodyDelta),
    ScreenSnapshot(ScreenSnapshot),
    Trigger(Trigger),
    SuggestionPartial(Suggestion),
    SuggestionFinal(Suggestion),
    UserAnnotation(UserAnnotation),
    Error(PipelineError),
}

/// Cheap event discriminant used for filtering and persistence indexes.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Deserialize, serde::Serialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "snake_case"))]
pub enum EventKind {
    UtterancePartial,
    UtteranceFinal,
    Vad,
    Prosody,
    ScreenSnapshot,
    Trigger,
    SuggestionPartial,
    SuggestionFinal,
    UserAnnotation,
    Error,
}

impl EventKind {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::UtterancePartial => "utterance.partial",
            Self::UtteranceFinal => "utterance.final",
            Self::Vad => "vad",
            Self::Prosody => "prosody",
            Self::ScreenSnapshot => "screen.snapshot",
            Self::Trigger => "trigger",
            Self::SuggestionPartial => "suggestion.partial",
            Self::SuggestionFinal => "suggestion.final",
            Self::UserAnnotation => "annotation.user",
            Self::Error => "error",
        }
    }
}

/// Immutable envelope in the canonical session log.
#[derive(Clone, Debug, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Deserialize, serde::Serialize))]
pub struct TimelineEvent {
    id: EventId,
    session_id: SessionId,
    ts: Duration,
    supersedes: Option<EventId>,
    payload: EventPayload,
}

impl TimelineEvent {
    pub(super) const fn new(
        id: EventId,
        session_id: SessionId,
        ts: Duration,
        supersedes: Option<EventId>,
        payload: EventPayload,
    ) -> Self {
        Self {
            id,
            session_id,
            ts,
            supersedes,
            payload,
        }
    }

    #[must_use]
    pub const fn id(&self) -> EventId {
        self.id
    }

    #[must_use]
    pub const fn session_id(&self) -> SessionId {
        self.session_id
    }

    #[must_use]
    pub const fn ts(&self) -> Duration {
        self.ts
    }

    #[must_use]
    pub const fn supersedes(&self) -> Option<EventId> {
        self.supersedes
    }

    #[must_use]
    pub const fn payload(&self) -> &EventPayload {
        &self.payload
    }

    #[must_use]
    pub const fn kind(&self) -> EventKind {
        match self.payload {
            EventPayload::UtterancePartial(_) => EventKind::UtterancePartial,
            EventPayload::UtteranceFinal(_) => EventKind::UtteranceFinal,
            EventPayload::Vad(_) => EventKind::Vad,
            EventPayload::Prosody(_) => EventKind::Prosody,
            EventPayload::ScreenSnapshot(_) => EventKind::ScreenSnapshot,
            EventPayload::Trigger(_) => EventKind::Trigger,
            EventPayload::SuggestionPartial(_) => EventKind::SuggestionPartial,
            EventPayload::SuggestionFinal(_) => EventKind::SuggestionFinal,
            EventPayload::UserAnnotation(_) => EventKind::UserAnnotation,
            EventPayload::Error(_) => EventKind::Error,
        }
    }
}

#[cfg(test)]
mod tests {
    #[cfg(feature = "serde")]
    use std::time::Duration;

    #[cfg(feature = "serde")]
    use crate::{
        Annotation, Citation, PipelineError, ProviderError, Source, SpeechState, Suggestion,
        Trigger, TriggerKind, Utterance, UtteranceSpan, VadSegment,
    };

    #[cfg(feature = "serde")]
    use super::{
        EventId, EventPayload, FrameRef, MarkKind, ProsodyDelta, ScreenSnapshot, UserAnnotation,
    };

    #[cfg(feature = "serde")]
    #[test]
    fn every_payload_variant_survives_json_round_trip() -> Result<(), Box<dyn std::error::Error>> {
        let utterance = Utterance {
            source: Source::System,
            start: Duration::from_secs(1),
            end: Duration::from_secs(2),
            text: "security review".to_owned(),
            avg_logprob: -0.2,
            annotations: vec![Annotation::Hesitant],
        };
        let trigger = Trigger {
            kind: TriggerKind::Objection,
            utterance_span: UtteranceSpan {
                source: Source::System,
                start: Duration::from_secs(1),
                end: Duration::from_secs(2),
            },
            confidence: 0.9,
        };
        let suggestion = Suggestion {
            anchors: vec![EventId::new(1)],
            trigger: trigger.clone(),
            text: "Offer the security brief".to_owned(),
            citations: vec![Citation {
                title: "Security".to_owned(),
                uri: None,
                excerpt: "SOC 2".to_owned(),
            }],
        };
        let payloads = vec![
            EventPayload::UtterancePartial(utterance.clone()),
            EventPayload::UtteranceFinal(utterance),
            EventPayload::Vad(VadSegment {
                source: Source::System,
                start: Duration::from_secs(1),
                end: Some(Duration::from_secs(2)),
                kind: SpeechState::SpeechEnd,
            }),
            EventPayload::Prosody(ProsodyDelta {
                source: Source::System,
                speech_rate: Some(120.0),
                talk_time_ratio: 0.6,
                annotations: vec![Annotation::Pause(Duration::from_millis(500))],
            }),
            EventPayload::ScreenSnapshot(ScreenSnapshot {
                frame_ref: FrameRef::new("sha256:abc"),
                ocr_text: "Pricing".to_owned(),
                active_app: Some("Keynote".to_owned()),
                window_title: Some("Pricing.key".to_owned()),
                visible_from: Duration::from_secs(3),
                visible_to: Some(Duration::from_secs(8)),
            }),
            EventPayload::Trigger(trigger),
            EventPayload::SuggestionPartial(suggestion.clone()),
            EventPayload::SuggestionFinal(suggestion),
            EventPayload::UserAnnotation(UserAnnotation {
                anchor: EventId::new(1),
                text: "follow up".to_owned(),
                mark: MarkKind::FollowUp,
            }),
            EventPayload::Error(PipelineError::Provider(ProviderError::Cancelled)),
        ];

        let encoded = serde_json::to_string(&payloads)?;
        let decoded: Vec<EventPayload> = serde_json::from_str(&encoded)?;
        assert_eq!(decoded, payloads, "every payload variant must round-trip");
        Ok(())
    }
}
