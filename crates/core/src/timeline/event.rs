use std::time::Duration;

use crate::{
    Annotation, PipelineError, Proposal, ProposalTrigger, Source, Usage, Utterance, VadSegment,
};

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
///
/// Deserialization is hand-written rather than derived because `serde_json::Value` cannot yield a
/// `u128`: its number type holds `u64`/`i64`/`f64`, so any `from_value` into this type fails with
/// "u128 is not supported" regardless of how small the value is. Reasoning replies are parsed
/// through exactly that path — the model's JSON becomes a `Value` first so an `inspect_screen`
/// action can be told apart from a result — which made every Ask answer carrying a citation fail.
///
/// Ids are nanosecond timestamps and fit a `u64` for the next several centuries, so accepting the
/// narrower widths costs nothing and keeps the stored representation a plain number.
/// `Default` is the zero id, which names no recording. It exists so a wire format can omit the
/// field where the value is unknowable to the sender; a zero reaching a lookup simply misses, and
/// callers resolve it deliberately rather than trusting it.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, Ord, PartialEq, PartialOrd)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct SessionId(u128);

#[cfg(feature = "serde")]
impl<'de> serde::Deserialize<'de> for SessionId {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct Visitor;

        impl serde::de::Visitor<'_> for Visitor {
            type Value = SessionId;

            fn expecting(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
                formatter.write_str("a session id as a non-negative integer or decimal string")
            }

            fn visit_u64<E: serde::de::Error>(self, value: u64) -> Result<Self::Value, E> {
                Ok(SessionId(u128::from(value)))
            }

            fn visit_u128<E: serde::de::Error>(self, value: u128) -> Result<Self::Value, E> {
                Ok(SessionId(value))
            }

            fn visit_i64<E: serde::de::Error>(self, value: i64) -> Result<Self::Value, E> {
                u64::try_from(value)
                    .map(Self::Value::from)
                    .map_err(|_| E::invalid_value(serde::de::Unexpected::Signed(value), &self))
            }

            /// Accepted so a stored or transported id can widen to a string later without
            /// stranding the records already written as numbers.
            fn visit_str<E: serde::de::Error>(self, value: &str) -> Result<Self::Value, E> {
                value
                    .parse::<u128>()
                    .map(SessionId)
                    .map_err(|_| E::invalid_value(serde::de::Unexpected::Str(value), &self))
            }
        }

        deserializer.deserialize_any(Visitor)
    }
}

impl From<u64> for SessionId {
    fn from(value: u64) -> Self {
        Self(u128::from(value))
    }
}

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

/// Whether a proposal payload is a watcher trigger, streaming draft, or final card.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Deserialize, serde::Serialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "snake_case"))]
pub enum ProposalPhase {
    Trigger,
    Partial,
    Final,
}

/// Opaque proposal timeline payload. Only checked `TimelineBuilder` methods create one.
#[derive(Clone, Debug, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Deserialize, serde::Serialize))]
#[cfg_attr(feature = "serde", serde(transparent))]
pub struct ProposalEvent {
    content: ProposalEventContent,
}

#[derive(Clone, Debug, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Deserialize, serde::Serialize))]
#[cfg_attr(
    feature = "serde",
    serde(tag = "phase", content = "content", rename_all = "snake_case")
)]
enum ProposalEventContent {
    Trigger(ProposalTrigger),
    Partial(Proposal),
    Final(Proposal),
}

impl ProposalEvent {
    pub(super) const fn from_trigger(value: ProposalTrigger) -> Self {
        Self {
            content: ProposalEventContent::Trigger(value),
        }
    }

    pub(super) const fn partial(value: Proposal) -> Self {
        Self {
            content: ProposalEventContent::Partial(value),
        }
    }

    pub(super) const fn final_value(value: Proposal) -> Self {
        Self {
            content: ProposalEventContent::Final(value),
        }
    }

    #[must_use]
    pub const fn phase(&self) -> ProposalPhase {
        match self.content {
            ProposalEventContent::Trigger(_) => ProposalPhase::Trigger,
            ProposalEventContent::Partial(_) => ProposalPhase::Partial,
            ProposalEventContent::Final(_) => ProposalPhase::Final,
        }
    }

    #[must_use]
    pub fn anchors(&self) -> &[EventId] {
        match &self.content {
            ProposalEventContent::Trigger(value) => value.anchors(),
            ProposalEventContent::Partial(value) | ProposalEventContent::Final(value) => {
                value.anchors()
            }
        }
    }

    #[must_use]
    pub const fn trigger(&self) -> Option<&ProposalTrigger> {
        match &self.content {
            ProposalEventContent::Trigger(value) => Some(value),
            ProposalEventContent::Partial(_) | ProposalEventContent::Final(_) => None,
        }
    }

    #[must_use]
    pub const fn proposal(&self) -> Option<&Proposal> {
        match &self.content {
            ProposalEventContent::Partial(value) | ProposalEventContent::Final(value) => {
                Some(value)
            }
            ProposalEventContent::Trigger(_) => None,
        }
    }

    pub(super) fn has_same_provenance(&self, other: &Self) -> bool {
        match (self.proposal(), other.proposal()) {
            (Some(left), Some(right)) => left.has_same_provenance(right),
            _ => false,
        }
    }
}

/// User interaction with a final proposal; it is not a captured meeting fact.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Deserialize, serde::Serialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "snake_case"))]
pub enum ProposalDispositionKind {
    Dismissed,
    Copied,
    Accepted,
}

/// Append-only proposal interaction referring to the final card the user acted on.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Deserialize, serde::Serialize))]
pub struct ProposalDisposition {
    proposal: EventId,
    kind: ProposalDispositionKind,
}

impl ProposalDisposition {
    pub(super) const fn new(proposal: EventId, kind: ProposalDispositionKind) -> Self {
        Self { proposal, kind }
    }

    #[must_use]
    pub const fn proposal(self) -> EventId {
        self.proposal
    }

    #[must_use]
    pub const fn kind(self) -> ProposalDispositionKind {
        self.kind
    }
}

/// Terminal state of one provider-neutral proposal generation run.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Deserialize, serde::Serialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "snake_case"))]
pub enum ProposalRunOutcome {
    Completed,
    Cancelled,
    Failed,
}

/// Invalid proposal run audit metadata.
#[derive(Clone, Debug, Eq, thiserror::Error, PartialEq)]
pub enum ProposalRunAuditError {
    #[error("proposal run audit requires at least one anchor")]
    MissingAnchor,
    #[error("proposal run audit anchors must be unique")]
    DuplicateAnchor,
    #[error("backend fingerprint must be 1..=256 safe identifier characters")]
    InvalidBackendFingerprint,
}

/// Provider-neutral audit record; contains no SDK values, credentials, or failure text.
#[derive(Clone, Debug, Eq, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct ProposalRunAudit {
    proposal: EventId,
    anchors: Vec<EventId>,
    backend_fingerprint: String,
    usage: Option<Usage>,
    outcome: ProposalRunOutcome,
}

impl ProposalRunAudit {
    pub fn new(
        proposal: EventId,
        anchors: Vec<EventId>,
        backend_fingerprint: impl Into<String>,
        usage: Option<Usage>,
        outcome: ProposalRunOutcome,
    ) -> Result<Self, ProposalRunAuditError> {
        if anchors.is_empty() {
            return Err(ProposalRunAuditError::MissingAnchor);
        }
        let mut seen = std::collections::HashSet::with_capacity(anchors.len());
        if anchors.iter().any(|anchor| !seen.insert(anchor)) {
            return Err(ProposalRunAuditError::DuplicateAnchor);
        }
        let backend_fingerprint = backend_fingerprint.into();
        if backend_fingerprint.is_empty()
            || backend_fingerprint.len() > 256
            || !backend_fingerprint.bytes().all(|byte| {
                byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b':' | b'/')
            })
        {
            return Err(ProposalRunAuditError::InvalidBackendFingerprint);
        }
        Ok(Self {
            proposal,
            anchors,
            backend_fingerprint,
            usage,
            outcome,
        })
    }

    #[must_use]
    pub const fn proposal(&self) -> EventId {
        self.proposal
    }

    #[must_use]
    pub fn anchors(&self) -> &[EventId] {
        &self.anchors
    }

    #[must_use]
    pub fn backend_fingerprint(&self) -> &str {
        &self.backend_fingerprint
    }

    #[must_use]
    pub const fn usage(&self) -> Option<Usage> {
        self.usage
    }

    #[must_use]
    pub const fn outcome(&self) -> ProposalRunOutcome {
        self.outcome
    }
}

#[cfg(feature = "serde")]
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct ProposalRunAuditWire {
    proposal: EventId,
    anchors: Vec<EventId>,
    backend_fingerprint: String,
    usage: Option<Usage>,
    outcome: ProposalRunOutcome,
}

#[cfg(feature = "serde")]
impl<'de> serde::Deserialize<'de> for ProposalRunAudit {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let wire = ProposalRunAuditWire::deserialize(deserializer)?;
        Self::new(
            wire.proposal,
            wire.anchors,
            wire.backend_fingerprint,
            wire.usage,
            wire.outcome,
        )
        .map_err(serde::de::Error::custom)
    }
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
    Proposal(ProposalEvent),
    ProposalDisposition(ProposalDisposition),
    ProposalRunAudit(ProposalRunAudit),
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
    ProposalTrigger,
    ProposalPartial,
    ProposalFinal,
    ProposalDisposition,
    ProposalRunAudit,
    UserAnnotation,
    Error,
}

/// Trust classification used to keep captured facts separate from derived output.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum EventClass {
    CapturedMeetingFact,
    SystemOutput,
    UserInteraction,
    Diagnostic,
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
            Self::ProposalTrigger => "proposal.trigger",
            Self::ProposalPartial => "proposal.partial",
            Self::ProposalFinal => "proposal.final",
            Self::ProposalDisposition => "proposal.disposition",
            Self::ProposalRunAudit => "proposal.run_audit",
            Self::UserAnnotation => "annotation.user",
            Self::Error => "error",
        }
    }

    #[must_use]
    pub const fn class(self) -> EventClass {
        match self {
            Self::UtterancePartial
            | Self::UtteranceFinal
            | Self::Vad
            | Self::Prosody
            | Self::ScreenSnapshot => EventClass::CapturedMeetingFact,
            Self::ProposalTrigger
            | Self::ProposalPartial
            | Self::ProposalFinal
            | Self::ProposalRunAudit => EventClass::SystemOutput,
            Self::ProposalDisposition | Self::UserAnnotation => EventClass::UserInteraction,
            Self::Error => EventClass::Diagnostic,
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
    pub fn kind(&self) -> EventKind {
        match &self.payload {
            EventPayload::UtterancePartial(_) => EventKind::UtterancePartial,
            EventPayload::UtteranceFinal(_) => EventKind::UtteranceFinal,
            EventPayload::Vad(_) => EventKind::Vad,
            EventPayload::Prosody(_) => EventKind::Prosody,
            EventPayload::ScreenSnapshot(_) => EventKind::ScreenSnapshot,
            EventPayload::Proposal(value) => match value.phase() {
                ProposalPhase::Trigger => EventKind::ProposalTrigger,
                ProposalPhase::Partial => EventKind::ProposalPartial,
                ProposalPhase::Final => EventKind::ProposalFinal,
            },
            EventPayload::ProposalDisposition(_) => EventKind::ProposalDisposition,
            EventPayload::ProposalRunAudit(_) => EventKind::ProposalRunAudit,
            EventPayload::UserAnnotation(_) => EventKind::UserAnnotation,
            EventPayload::Error(_) => EventKind::Error,
        }
    }
}

#[cfg(test)]
mod tests {
    #[cfg(feature = "serde")]
    use std::time::Duration;

    /// A session id must survive the `Value` round trip that every reasoning reply takes.
    ///
    /// The model's JSON is parsed to a `serde_json::Value` first so an `inspect_screen` action can
    /// be told apart from a result, and only then into the reply type. `Value` cannot produce a
    /// `u128`, so a derived `Deserialize` failed there with "u128 is not supported" and took every
    /// Ask answer carrying a citation down with it — while `from_str` on the same bytes succeeded,
    /// which is what made it hard to see.
    #[cfg(feature = "serde")]
    #[test]
    fn a_session_id_survives_the_value_round_trip_reasoning_replies_take()
    -> Result<(), Box<dyn std::error::Error>> {
        use crate::SessionId;

        // A real id: nanoseconds since the epoch, comfortably inside u64.
        let id = SessionId::new(1_786_652_195_282_693_000);
        let value = serde_json::to_value(id)?;
        let parsed: SessionId = serde_json::from_value(value)?;
        assert_eq!(parsed, id, "a session id must survive from_value");

        let from_str: SessionId = serde_json::from_str("1786652195282693000")?;
        assert_eq!(from_str, id, "a bare number still parses");

        let from_string: SessionId = serde_json::from_str("\"1786652195282693000\"")?;
        assert_eq!(
            from_string, id,
            "accepting a string leaves room to widen the wire format without stranding records \
             already written as numbers"
        );

        assert!(
            serde_json::from_str::<SessionId>("-1").is_err(),
            "a negative id is not a session"
        );
        Ok(())
    }

    #[cfg(feature = "serde")]
    use crate::{
        Annotation, ExternalEvidenceRef, PipelineError, Proposal, ProposalKind, ProposalTrigger,
        ProviderError, Source, SpeechState, Utterance, VadSegment,
    };

    #[cfg(feature = "serde")]
    use super::{
        EventId, EventPayload, FrameRef, MarkKind, ProposalDisposition, ProposalDispositionKind,
        ProposalEvent, ProposalRunAudit, ProposalRunOutcome, ProsodyDelta, ScreenSnapshot,
        UserAnnotation,
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
        let trigger =
            ProposalTrigger::new(ProposalKind::DecisionCheck, vec![EventId::new(1)], 0.9)?;
        let proposal = Proposal::new(
            ProposalKind::DecisionCheck,
            vec![EventId::new(1)],
            "Confirm the decision",
            vec![EventId::new(1)],
            vec![ExternalEvidenceRef::new("mcp-evidence-v1-abc")?],
        )?;
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
            EventPayload::Proposal(ProposalEvent::from_trigger(trigger)),
            EventPayload::Proposal(ProposalEvent::partial(proposal.clone())),
            EventPayload::Proposal(ProposalEvent::final_value(proposal)),
            EventPayload::ProposalDisposition(ProposalDisposition::new(
                EventId::new(3),
                ProposalDispositionKind::Copied,
            )),
            EventPayload::ProposalRunAudit(ProposalRunAudit::new(
                EventId::new(3),
                vec![EventId::new(1)],
                "openai:gpt-5:fingerprint",
                None,
                ProposalRunOutcome::Completed,
            )?),
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

    #[cfg(feature = "serde")]
    #[test]
    fn proposal_deserialization_rejects_invalid_shape() {
        let blank = r#"{"kind":"next_step","anchors":[1],"text":" ","meeting_evidence":[],"external_evidence":[]}"#;
        let unsafe_evidence = r#"{"kind":"next_step","anchors":[1],"text":"Ask","meeting_evidence":[],"external_evidence":["token?secret"]}"#;

        assert!(
            serde_json::from_str::<Proposal>(blank).is_err(),
            "blank proposal text must fail closed"
        );
        assert!(
            serde_json::from_str::<Proposal>(unsafe_evidence).is_err(),
            "unsafe external evidence ids must fail closed"
        );
        let credential_shaped_fingerprint = r#"{"proposal":3,"anchors":[1],"backend_fingerprint":"openai?key=secret","usage":null,"outcome":"failed"}"#;
        assert!(
            serde_json::from_str::<ProposalRunAudit>(credential_shaped_fingerprint).is_err(),
            "run audit fingerprints must reject query-shaped secret material"
        );
    }
}
