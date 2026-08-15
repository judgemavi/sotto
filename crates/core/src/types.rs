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

use crate::{
    EventId, SessionId,
    timeline::{CaptureTarget, TargetKind},
};

/// Stable identity of one library entry.
///
/// An entry is the document-bearing object above recording sessions. Keeping its identity
/// distinct from [`SessionId`] prevents callers from accidentally hanging captured facts from a
/// document or treating a prepared entry as though a recording already exists.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, Ord, PartialEq, PartialOrd)]
#[cfg_attr(feature = "serde", derive(serde::Deserialize, serde::Serialize))]
pub struct EntryId(u128);

impl EntryId {
    #[must_use]
    pub const fn new(value: u128) -> Self {
        Self(value)
    }

    #[must_use]
    pub const fn get(self) -> u128 {
        self.0
    }
}

/// The library object that may exist before capture and may hold several recording sessions.
///
/// Captured facts remain on the referenced sessions. The title and future notes document belong
/// to this entry; the session ids are only the attachment relation between those two layers.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Entry {
    id: EntryId,
    created_at_unix_ms: u64,
    title: Option<RecordingTitle>,
    session_ids: Vec<SessionId>,
}

impl Entry {
    #[must_use]
    pub const fn new(id: EntryId, created_at_unix_ms: u64, title: Option<RecordingTitle>) -> Self {
        Self {
            id,
            created_at_unix_ms,
            title,
            session_ids: Vec::new(),
        }
    }

    #[must_use]
    pub const fn id(&self) -> EntryId {
        self.id
    }

    #[must_use]
    pub const fn created_at_unix_ms(&self) -> u64 {
        self.created_at_unix_ms
    }

    #[must_use]
    pub const fn title(&self) -> Option<&RecordingTitle> {
        self.title.as_ref()
    }

    #[must_use]
    pub fn session_ids(&self) -> &[SessionId] {
        &self.session_ids
    }

    pub fn attach_session(&mut self, session_id: SessionId) {
        if !self.session_ids.contains(&session_id) {
            self.session_ids.push(session_id);
        }
    }
}

/// Container written for one retained local meeting recording.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Deserialize, serde::Serialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "snake_case"))]
pub enum RecordingContainer {
    Mp4,
}

impl RecordingContainer {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Mp4 => "mp4",
        }
    }
}

/// Why a session no longer has locally addressable media.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Deserialize, serde::Serialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "snake_case"))]
pub enum RecordingMissingReason {
    Deleted,
    Pruned,
}

/// Exact affine mapping from session-relative nanoseconds to media presentation time.
///
/// V1 deliberately records the identity mapping. Keeping it explicit prevents later readers from
/// silently assuming that timeline and media clocks still coincide after a format change.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Deserialize, serde::Serialize))]
pub struct MediaTimeMapping {
    pub session_origin_ns: u64,
    pub media_origin_ns: u64,
    pub rate_numerator: u32,
    pub rate_denominator: u32,
}

impl MediaTimeMapping {
    pub const IDENTITY: Self = Self {
        session_origin_ns: 0,
        media_origin_ns: 0,
        rate_numerator: 1,
        rate_denominator: 1,
    };

    #[must_use]
    pub const fn is_identity(self) -> bool {
        self.session_origin_ns == 0
            && self.media_origin_ns == 0
            && self.rate_numerator == 1
            && self.rate_denominator == 1
    }

    #[must_use]
    pub fn media_time(self, session_time: Duration) -> Option<Duration> {
        if self.rate_denominator == 0 {
            return None;
        }
        let relative = session_time
            .as_nanos()
            .checked_sub(u128::from(self.session_origin_ns))?;
        let scaled = relative
            .checked_mul(u128::from(self.rate_numerator))?
            .checked_div(u128::from(self.rate_denominator))?;
        let media_ns = u128::from(self.media_origin_ns).checked_add(scaled)?;
        u64::try_from(media_ns).ok().map(Duration::from_nanos)
    }
}

/// Durable media state linked to a session without mutating its append-only timeline.
#[derive(Clone, Debug, Eq, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Deserialize, serde::Serialize))]
pub enum SessionRecording {
    Available {
        session_id: SessionId,
        path: String,
        container: RecordingContainer,
        duration: Duration,
        byte_size: u64,
        time_mapping: MediaTimeMapping,
    },
    Missing {
        session_id: SessionId,
        reason: RecordingMissingReason,
    },
}

impl SessionRecording {
    #[must_use]
    pub const fn session_id(&self) -> SessionId {
        match self {
            Self::Available { session_id, .. } | Self::Missing { session_id, .. } => *session_id,
        }
    }

    #[must_use]
    pub const fn byte_size(&self) -> u64 {
        match self {
            Self::Available { byte_size, .. } => *byte_size,
            Self::Missing { .. } => 0,
        }
    }

    #[must_use]
    pub const fn time_mapping(&self) -> Option<MediaTimeMapping> {
        match self {
            Self::Available { time_mapping, .. } => Some(*time_mapping),
            Self::Missing { .. } => None,
        }
    }
}

/// A person's chosen name for one recording.
///
/// This is deliberately **not** part of [`crate::CaptureTarget`]. The capture target is a captured
/// fact — which application or window the OS content filter was built from, and whether audio was
/// scoped to it — and ADR-0006 plus the timeline's append-only rule make that fact unrewritable.
/// A title is a label laid over the recording, so it is a separate value with a separate lifetime:
/// it can be chosen, changed, and cleared without any claim about what was recorded changing.
///
/// It also stands on its own. An imported recording (T071) has no capture target at all, so a
/// title modelled as a decoration on one would have nothing to decorate.
///
/// Construction normalizes rather than trusting the caller, because the value renders in a single
/// ellipsizing rail row: surrounding and interior whitespace collapses to single spaces, and a
/// name that is empty or entirely whitespace is refused so no surface can ever render blank.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct RecordingTitle(String);

impl RecordingTitle {
    /// Longest title retained, in characters.
    ///
    /// A title is navigation, not content: the rail ellipsizes it well before this. The bound
    /// exists so a paste of an entire document cannot become a durable session row.
    pub const MAX_CHARS: usize = 200;

    /// Normalizes `value` into a title, or `None` when nothing nameable remains.
    ///
    /// `None` is the caller's signal to fall back to the default name — it never means "store an
    /// empty title".
    #[must_use]
    pub fn new(value: &str) -> Option<Self> {
        let mut normalized = String::with_capacity(value.len());
        for word in value.split_whitespace() {
            if !normalized.is_empty() {
                normalized.push(' ');
            }
            normalized.push_str(word);
        }
        if normalized.is_empty() {
            return None;
        }
        if normalized.chars().count() > Self::MAX_CHARS {
            normalized = normalized.chars().take(Self::MAX_CHARS).collect();
            // Truncation can only remove characters, so the value stays non-empty here.
        }
        Some(Self(normalized))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for RecordingTitle {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

#[cfg(feature = "serde")]
impl<'de> serde::Deserialize<'de> for RecordingTitle {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::new(&value)
            .ok_or_else(|| serde::de::Error::custom("recording title must not be blank"))
    }
}

/// Builds the honest [`CaptureTarget`] for a session whose media arrived by import (T071,
/// ADR-0019) rather than a verified OS capture.
///
/// Every field states only what is actually known: `kind` is [`TargetKind::Imported`], the variant
/// that exists precisely because none of the capture-shaped variants may honestly stand in for it;
/// `display_name` is the imported file's own name (the one fact about its origin that is simply
/// true); `bundle_id` and `window_title` are `None` because nothing was scoped; and `audio_scoped`
/// is `false` because no scoped-audio claim can be supported. [`CaptureTarget::has_valid_scope`]
/// enforces exactly this shape for [`TargetKind::Imported`].
#[must_use]
pub fn imported_capture_target(display_name: String) -> CaptureTarget {
    CaptureTarget {
        bundle_id: None,
        display_name,
        window_title: None,
        kind: TargetKind::Imported,
        audio_scoped: false,
    }
}

/// True when `target` names a session produced by import rather than capture.
///
/// A thin, discoverable wrapper over [`CaptureTarget::is_imported`] kept alongside
/// [`imported_capture_target`] so every surface that would otherwise present a capture target or
/// an audio-scope claim — the library rail's marker and source label today, any future settings or
/// evidence surface — has one obvious name to call before treating `CaptureTarget`'s other fields
/// as a verified fact about how the session was recorded.
#[must_use]
pub fn is_imported_capture_target(target: &CaptureTarget) -> bool {
    target.is_imported()
}

/// The independently captured audio stream that produced an event.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Deserialize, serde::Serialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "snake_case"))]
pub enum Source {
    /// The local participant's microphone.
    Mic,
    /// Captured meeting audio carrying remote participants.
    System,
}

impl Source {
    /// Returns the human-readable speaker name used in inline transcripts.
    #[must_use]
    pub const fn speaker_name(self) -> &'static str {
        match self {
            Self::Mic => "local participant",
            Self::System => "meeting audio",
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

/// Meeting-general reason that Sotto may offer a proposal.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Deserialize, serde::Serialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "snake_case"))]
pub enum ProposalKind {
    ClarifyingQuestion,
    DecisionCheck,
    NextStep,
    FollowUp,
    RelevantContext,
}

/// Invalid proposal content rejected before it reaches the timeline.
#[derive(Clone, Debug, Eq, thiserror::Error, PartialEq)]
pub enum ProposalError {
    #[error("a proposal must have at least one meeting anchor")]
    MissingAnchor,
    #[error("proposal anchors and evidence ids must be unique")]
    DuplicateReference,
    #[error("proposal text must not be blank")]
    BlankText,
    #[error("proposal confidence must be finite and between zero and one")]
    InvalidConfidence,
    #[error("external evidence id must be 1..=256 safe opaque characters")]
    InvalidExternalEvidenceId,
}

/// Opaque reference to evidence retained outside `core` in a durable context bundle.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct ExternalEvidenceRef(String);

impl ExternalEvidenceRef {
    pub fn new(value: impl Into<String>) -> Result<Self, ProposalError> {
        let value = value.into();
        if value.is_empty()
            || value.len() > 256
            || !value.bytes().all(|byte| {
                byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b':')
            })
        {
            return Err(ProposalError::InvalidExternalEvidenceId);
        }
        Ok(Self(value))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[cfg(feature = "serde")]
impl<'de> serde::Deserialize<'de> for ExternalEvidenceRef {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::new(value).map_err(serde::de::Error::custom)
    }
}

/// Watcher decision that a particular meeting moment may benefit from a proposal.
#[derive(Clone, Debug, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct ProposalTrigger {
    kind: ProposalKind,
    anchors: Vec<EventId>,
    confidence: f32,
}

impl ProposalTrigger {
    pub fn new(
        kind: ProposalKind,
        anchors: Vec<EventId>,
        confidence: f32,
    ) -> Result<Self, ProposalError> {
        validate_event_refs(&anchors, true)?;
        if !confidence.is_finite() || !(0.0..=1.0).contains(&confidence) {
            return Err(ProposalError::InvalidConfidence);
        }
        Ok(Self {
            kind,
            anchors,
            confidence,
        })
    }

    #[must_use]
    pub const fn kind(&self) -> ProposalKind {
        self.kind
    }

    #[must_use]
    pub fn anchors(&self) -> &[EventId] {
        &self.anchors
    }

    #[must_use]
    pub const fn confidence(&self) -> f32 {
        self.confidence
    }
}

/// Streaming or final proposal with typed meeting and external evidence references.
#[derive(Clone, Debug, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct Proposal {
    kind: ProposalKind,
    anchors: Vec<EventId>,
    text: String,
    meeting_evidence: Vec<EventId>,
    external_evidence: Vec<ExternalEvidenceRef>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ProposalProvenance {
    pub(crate) kind: ProposalKind,
    pub(crate) anchors: Vec<EventId>,
    pub(crate) meeting_evidence: Vec<EventId>,
    pub(crate) external_evidence: Vec<ExternalEvidenceRef>,
}

impl Proposal {
    pub fn new(
        kind: ProposalKind,
        anchors: Vec<EventId>,
        text: impl Into<String>,
        meeting_evidence: Vec<EventId>,
        external_evidence: Vec<ExternalEvidenceRef>,
    ) -> Result<Self, ProposalError> {
        validate_event_refs(&anchors, true)?;
        validate_event_refs(&meeting_evidence, false)?;
        if has_duplicates(&external_evidence) {
            return Err(ProposalError::DuplicateReference);
        }
        let text = text.into();
        if text.trim().is_empty() {
            return Err(ProposalError::BlankText);
        }
        Ok(Self {
            kind,
            anchors,
            text,
            meeting_evidence,
            external_evidence,
        })
    }

    #[must_use]
    pub const fn kind(&self) -> ProposalKind {
        self.kind
    }

    #[must_use]
    pub fn anchors(&self) -> &[EventId] {
        &self.anchors
    }

    #[must_use]
    pub fn text(&self) -> &str {
        &self.text
    }

    #[must_use]
    pub fn meeting_evidence(&self) -> &[EventId] {
        &self.meeting_evidence
    }

    #[must_use]
    pub fn external_evidence(&self) -> &[ExternalEvidenceRef] {
        &self.external_evidence
    }

    pub(crate) fn provenance(&self) -> ProposalProvenance {
        ProposalProvenance {
            kind: self.kind,
            anchors: self.anchors.clone(),
            meeting_evidence: self.meeting_evidence.clone(),
            external_evidence: self.external_evidence.clone(),
        }
    }

    pub(crate) fn has_same_provenance(&self, other: &Self) -> bool {
        self.provenance() == other.provenance()
    }
}

fn validate_event_refs(values: &[EventId], require_one: bool) -> Result<(), ProposalError> {
    if require_one && values.is_empty() {
        return Err(ProposalError::MissingAnchor);
    }
    if has_duplicates(values) {
        return Err(ProposalError::DuplicateReference);
    }
    Ok(())
}

fn has_duplicates<T: Eq + std::hash::Hash>(values: &[T]) -> bool {
    let mut seen = std::collections::HashSet::with_capacity(values.len());
    values.iter().any(|value| !seen.insert(value))
}

#[cfg(feature = "serde")]
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct ProposalTriggerWire {
    kind: ProposalKind,
    anchors: Vec<EventId>,
    confidence: f32,
}

#[cfg(feature = "serde")]
impl<'de> serde::Deserialize<'de> for ProposalTrigger {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let wire = ProposalTriggerWire::deserialize(deserializer)?;
        Self::new(wire.kind, wire.anchors, wire.confidence).map_err(serde::de::Error::custom)
    }
}

#[cfg(feature = "serde")]
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct ProposalWire {
    kind: ProposalKind,
    anchors: Vec<EventId>,
    text: String,
    meeting_evidence: Vec<EventId>,
    external_evidence: Vec<ExternalEvidenceRef>,
}

#[cfg(feature = "serde")]
impl<'de> serde::Deserialize<'de> for Proposal {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let wire = ProposalWire::deserialize(deserializer)?;
        Self::new(
            wire.kind,
            wire.anchors,
            wire.text,
            wire.meeting_evidence,
            wire.external_evidence,
        )
        .map_err(serde::de::Error::custom)
    }
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

/// Provider-neutral constraint for the final model text.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Deserialize, serde::Serialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "snake_case"))]
pub enum ReasoningOutput {
    /// Unconstrained text, preserving the original completion behavior.
    #[default]
    Text,
    /// A syntactically valid JSON object without schema adherence.
    JsonObject,
    /// JSON constrained to the supplied schema.
    JsonSchema(JsonSchemaConstraint),
}

/// Caller-owned JSON Schema data. The JSON remains an opaque string in `core` so
/// the headless layer does not gain a mandatory JSON implementation dependency.
#[derive(Clone, Debug, Eq, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Deserialize, serde::Serialize))]
pub struct JsonSchemaConstraint {
    name: String,
    description: Option<String>,
    schema_json: String,
}

impl JsonSchemaConstraint {
    pub fn new(
        name: impl Into<String>,
        description: Option<String>,
        schema_json: impl Into<String>,
    ) -> Result<Self, ReasoningRequestError> {
        let value = Self {
            name: name.into(),
            description,
            schema_json: schema_json.into(),
        };
        value.validate()?;
        Ok(value)
    }

    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    #[must_use]
    pub fn description(&self) -> Option<&str> {
        self.description.as_deref()
    }

    #[must_use]
    pub fn schema_json(&self) -> &str {
        &self.schema_json
    }

    fn validate(&self) -> Result<(), ReasoningRequestError> {
        if self.name.is_empty()
            || self.name.len() > 64
            || !self
                .name
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
        {
            return Err(ReasoningRequestError::InvalidSchemaName);
        }
        if self.schema_json.trim().is_empty() {
            return Err(ReasoningRequestError::EmptySchema);
        }
        Ok(())
    }
}

/// Source-compatible extension of a text completion with output constraints.
/// Screen-authorized image evidence remains outside `core` and is dispatched by `providers`.
#[derive(Clone, Debug, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Deserialize, serde::Serialize))]
pub struct ReasoningRequest {
    pub completion: CompletionRequest,
    pub output: ReasoningOutput,
}

impl ReasoningRequest {
    #[must_use]
    pub fn text(completion: CompletionRequest) -> Self {
        Self {
            completion,
            output: ReasoningOutput::Text,
        }
    }

    #[must_use]
    pub fn json_object(completion: CompletionRequest) -> Self {
        Self {
            completion,
            output: ReasoningOutput::JsonObject,
        }
    }

    #[must_use]
    pub fn json_schema(completion: CompletionRequest, schema: JsonSchemaConstraint) -> Self {
        Self {
            completion,
            output: ReasoningOutput::JsonSchema(schema),
        }
    }

    pub fn validate(&self) -> Result<(), ReasoningRequestError> {
        if let ReasoningOutput::JsonSchema(schema) = &self.output {
            schema.validate()?;
        }
        Ok(())
    }
}

/// Invalid provider-neutral reasoning input, rejected before connector I/O.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum ReasoningRequestError {
    #[error("JSON Schema name must be 1..=64 ASCII letters, digits, underscores, or dashes")]
    InvalidSchemaName,
    #[error("JSON Schema must not be empty")]
    EmptySchema,
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
    use super::{
        Annotation, Entry, EntryId, MediaTimeMapping, RecordingTitle, Source, Utterance,
        imported_capture_target, is_imported_capture_target,
    };
    use crate::{SessionId, timeline::TargetKind};
    use std::time::Duration;

    #[test]
    fn an_imported_target_has_valid_scope_and_is_detected_as_imported() {
        let target = imported_capture_target("lecture.mp4".to_owned());

        assert!(
            target.has_valid_scope(),
            "an imported target must pass the same structural validation the store applies to a \
             captured one, or saving the session would fail"
        );
        assert!(
            is_imported_capture_target(&target),
            "the imported variant must round-trip through the predicate that detects it"
        );
        assert_eq!(target.kind, TargetKind::Imported);
        assert_eq!(target.display_name, "lecture.mp4");
        assert!(
            !target.audio_scoped,
            "an import must never claim a scoped-audio guarantee it cannot support"
        );
        assert_eq!(
            target.window_title, None,
            "an import has no scoped window to name"
        );
        assert_eq!(
            target.bundle_id, None,
            "an import has no OS-scoped application identity to name"
        );
    }

    #[test]
    fn a_genuine_capture_is_never_mistaken_for_an_import() {
        let captured = crate::CaptureTarget {
            bundle_id: Some("us.zoom.xos".to_owned()),
            display_name: "Zoom".to_owned(),
            window_title: Some("Standup".to_owned()),
            kind: TargetKind::Window,
            audio_scoped: true,
        };

        assert!(
            !is_imported_capture_target(&captured),
            "a captured target must never be mistaken for an import"
        );

        let no_bundle_id = crate::CaptureTarget {
            bundle_id: None,
            ..captured
        };
        assert!(
            !is_imported_capture_target(&no_bundle_id),
            "a captured target with no bundle id (e.g. a display) must not be mistaken for an \
             import either — the check is the variant, not the presence of a bundle id"
        );
    }

    #[test]
    fn entry_identity_and_session_attachments_stay_distinct() {
        let mut entry = Entry::new(EntryId::new(7), 1_700_000_000_000, None);
        entry.attach_session(SessionId::new(7));
        entry.attach_session(SessionId::new(8));
        entry.attach_session(SessionId::new(7));

        assert_eq!(entry.id(), EntryId::new(7));
        assert_eq!(entry.created_at_unix_ms(), 1_700_000_000_000);
        assert_eq!(entry.session_ids(), &[SessionId::new(7), SessionId::new(8)]);
    }

    #[test]
    fn a_chosen_title_is_normalized_and_never_blank() {
        assert_eq!(
            RecordingTitle::new("  Standup  ").map(|title| title.as_str().to_owned()),
            Some("Standup".to_owned()),
            "a title is trimmed rather than stored with the whitespace a field collects"
        );
        assert_eq!(
            RecordingTitle::new("BTU\n daily\tstandup").map(|title| title.as_str().to_owned()),
            Some("BTU daily standup".to_owned()),
            "a pasted multi-line name must collapse into the single rail row that renders it"
        );
        for blank in ["", "   ", "\t\n ", "\u{a0}"] {
            assert!(
                RecordingTitle::new(blank).is_none(),
                "{blank:?} names nothing, so it must be refused rather than rendered blank"
            );
        }
    }

    #[test]
    fn an_oversized_title_is_bounded_and_still_a_title() -> Result<(), Box<dyn std::error::Error>> {
        let pasted = "word ".repeat(400);
        let title = RecordingTitle::new(&pasted).ok_or("a long paste still names something")?;
        assert_eq!(
            title.as_str().chars().count(),
            RecordingTitle::MAX_CHARS,
            "a title is navigation, so it is bounded rather than unbounded"
        );
        assert!(
            !title.as_str().trim().is_empty(),
            "bounding must never produce a blank title"
        );
        Ok(())
    }

    #[cfg(feature = "serde")]
    #[test]
    fn a_blank_persisted_title_is_rejected_on_the_way_back_in()
    -> Result<(), Box<dyn std::error::Error>> {
        assert!(
            serde_json::from_str::<RecordingTitle>("\"  \"").is_err(),
            "a blank title must not survive a round trip through storage"
        );
        let title = RecordingTitle::new("Standup").ok_or("Standup is a title")?;
        let json = serde_json::to_string(&title)?;
        assert_eq!(
            serde_json::from_str::<RecordingTitle>(&json)?,
            title,
            "a chosen title must survive serialization unchanged"
        );
        Ok(())
    }

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
            "[meeting audio, hesitant, 2.5s pause] \"sure, sounds fine\""
        );
    }

    #[test]
    fn identity_media_clock_maps_citations_without_offset_or_scale() {
        let citation = Duration::from_secs(138) + Duration::from_millis(275);
        assert_eq!(
            MediaTimeMapping::IDENTITY.media_time(citation),
            Some(citation),
            "the recorded v1 identity mapping must address the same media presentation time"
        );
        assert!(
            MediaTimeMapping::IDENTITY.is_identity(),
            "the v1 clock equivalence must be asserted explicitly"
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
