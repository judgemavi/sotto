//! Transcript-first prompt assembly and the bounded `inspect_screen` second pass.

use std::{
    sync::{
        Arc, Mutex, MutexGuard, PoisonError,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use futures_util::StreamExt;
use screen::{
    AuthorizedReasoningImage, InspectScreenRequest, ScreenEvidence, ScreenInspection,
    ScreenInspectionSource, ScreenPrecision, ScreenSelector, ScreenUnavailableReason,
};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use sotto_core::{
    CancellationToken, CaptureTarget, CompletionMessage, CompletionRequest, EventPayload,
    MessageRole, ProviderError, ReasoningRequest, TimelineEvent, Usage,
};
use thiserror::Error;

use providers::{ReasoningProvider, backend::ObservedRequestNormalization};

pub const SCREEN_INSPECTION_BUDGET: usize = 3;

#[derive(Clone, Default)]
pub struct ScreenInspectionBudget(Arc<AtomicUsize>);

impl ScreenInspectionBudget {
    #[must_use]
    pub fn remaining(&self) -> usize {
        SCREEN_INSPECTION_BUDGET.saturating_sub(self.0.load(Ordering::Acquire))
    }

    fn try_spend(&self) -> bool {
        self.0
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |spent| {
                (spent < SCREEN_INSPECTION_BUDGET).then_some(spent.saturating_add(1))
            })
            .is_ok()
    }
}

const INSPECTION_INSTRUCTIONS: &str = r#"
The input intentionally contains only capture-target metadata and timestamped final transcript.
If screen evidence is materially necessary, return only one JSON action instead of a result:
{"action":"inspect_screen","timestamp_seconds":12.3,"evidence":"metadata|local_ocr|image","reason":"why this moment is necessary"}
or use "event_id" instead of "timestamp_seconds". Provide exactly one selector. You have a fixed
budget of three inspections for this run. After each result, either return the final requested JSON
or spend another inspection. Evidence reports its actual sampled or decoded media timestamp and
never implies greater precision than its source provides. An exhausted request is refused and does
not inspect or decode anything.
"#;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConsultedMoment {
    Timestamp(Duration),
    Event(sotto_core::EventId),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConsultedEvidence {
    Metadata,
    LocalOcr,
    Image,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConsultedPrecision {
    SampledChangeFrame,
    DecodedVideoFrame,
}

impl ConsultedPrecision {
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::SampledChangeFrame => "sampled change frame",
            Self::DecodedVideoFrame => "decoded video frame",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum ConsultationOutcome {
    Frame {
        requested_media_time: Duration,
        decoded_media_time: Duration,
        precision: ConsultedPrecision,
        ocr_characters: Option<usize>,
    },
    Unavailable {
        reason: String,
    },
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ScreenConsultation {
    pub session_id: sotto_core::SessionId,
    pub requested: ConsultedMoment,
    pub evidence: ConsultedEvidence,
    pub reason: String,
    pub outcome: ConsultationOutcome,
}

impl ScreenConsultation {
    #[must_use]
    pub fn describe(&self) -> String {
        let moment = match self.requested {
            ConsultedMoment::Timestamp(value) => format!("at {}", format_clock(value)),
            ConsultedMoment::Event(id) => format!("at transcript row {}", id.get()),
        };
        match &self.outcome {
            ConsultationOutcome::Frame {
                requested_media_time,
                decoded_media_time,
                precision,
                ocr_characters,
            } => {
                let text = ocr_characters.map_or_else(String::new, |count| {
                    format!(" · local text recognition returned {count} characters")
                });
                format!(
                    "Reasoning consulted the screen {moment} · {} at {} (requested {}){text} · no image was sent to the reasoning backend.",
                    precision.label(),
                    format_precise(*decoded_media_time),
                    format_precise(*requested_media_time),
                )
            }
            ConsultationOutcome::Unavailable { reason } => format!(
                "Reasoning asked for the screen {moment} · unavailable ({reason}) · the run continued from the transcript alone."
            ),
        }
    }
}

#[derive(Clone)]
pub struct ScreenConsultationLog {
    session_id: sotto_core::SessionId,
    entries: Arc<Mutex<Vec<ScreenConsultation>>>,
}

impl ScreenConsultationLog {
    #[must_use]
    pub fn new(session_id: sotto_core::SessionId) -> Self {
        Self {
            session_id,
            entries: Arc::new(Mutex::new(Vec::new())),
        }
    }

    fn locked(&self) -> MutexGuard<'_, Vec<ScreenConsultation>> {
        self.entries.lock().unwrap_or_else(PoisonError::into_inner)
    }

    pub fn record(&self, consultation: ScreenConsultation) {
        self.locked().push(consultation);
    }

    pub fn record_budget_exhausted(&self, request: &InspectScreenRequest) {
        self.record(ScreenConsultation {
            session_id: self.session_id,
            requested: consulted_moment(&request.selector),
            evidence: consulted_evidence(request.evidence),
            reason: request.reason.clone(),
            outcome: ConsultationOutcome::Unavailable {
                reason: "inspection_budget_exhausted".to_owned(),
            },
        });
    }

    #[must_use]
    pub fn entries(&self) -> Vec<ScreenConsultation> {
        self.locked().clone()
    }
}

pub fn consultation(
    session_id: sotto_core::SessionId,
    request: &InspectScreenRequest,
    inspection: &ScreenInspection,
) -> ScreenConsultation {
    ScreenConsultation {
        session_id,
        requested: consulted_moment(&request.selector),
        evidence: consulted_evidence(request.evidence),
        reason: request.reason.clone(),
        outcome: consultation_outcome(inspection),
    }
}

#[derive(Debug, Error)]
pub enum ReasoningContextError {
    #[error(transparent)]
    Provider(#[from] ProviderError),
    #[error("provider returned invalid reasoning JSON: {0}")]
    InvalidOutput(#[from] serde_json::Error),
    #[error("invalid inspect_screen action: {0}")]
    InvalidInspectionAction(String),
    #[error("provider requested more than one screen inspection")]
    RepeatedInspection,
}

pub(crate) struct ReasoningResult<T> {
    pub value: T,
    pub usage: Usage,
    pub calls: usize,
    pub normalizations: Vec<ObservedRequestNormalization>,
}

enum ReasoningTurn<T> {
    Complete(T),
    Inspect(InspectScreenRequest),
}

#[derive(Deserialize)]
struct InspectionWire {
    timestamp_seconds: Option<f64>,
    event_id: Option<u64>,
    evidence: Option<String>,
    reason: Option<String>,
    result: Option<serde_json::Value>,
}

/// Renders the only context allowed in an initial reasoning request.
pub(crate) fn render_transcript<'a>(
    target: &CaptureTarget,
    events: impl IntoIterator<Item = &'a TimelineEvent>,
) -> String {
    let mut lines = vec![format!(
        "Capture target: app={} window={}",
        target.display_name,
        target.window_title.as_deref().unwrap_or("unknown")
    )];
    lines.push("Timestamped final transcript:".to_owned());
    lines.extend(events.into_iter().filter_map(|event| {
        let EventPayload::UtteranceFinal(utterance) = event.payload() else {
            return None;
        };
        Some(format!(
            "[event:{} from:{:.3}s to:{:.3}s] {}",
            event.id().get(),
            utterance.start.as_secs_f64(),
            utterance.end.as_secs_f64(),
            utterance.render_inline()
        ))
    }));
    lines.join("\n")
}

pub(crate) async fn complete_with_optional_inspection<T: DeserializeOwned>(
    provider: &dyn ReasoningProvider,
    system: &str,
    transcript_context: String,
    events: &[TimelineEvent],
    inspector: Option<&dyn ScreenInspectionSource>,
) -> Result<ReasoningResult<T>, ReasoningContextError> {
    complete_with_optional_inspection_cancellable(
        provider,
        system,
        transcript_context,
        events,
        inspector,
        None,
        &ScreenInspectionBudget::default(),
        &CancellationToken::new(),
    )
    .await
}

#[expect(
    clippy::too_many_arguments,
    reason = "the reasoning boundary keeps provider, evidence authority, shared budget, audit log, and cancellation explicit"
)]
pub(crate) async fn complete_with_optional_inspection_cancellable<T: DeserializeOwned>(
    provider: &dyn ReasoningProvider,
    system: &str,
    transcript_context: String,
    events: &[TimelineEvent],
    inspector: Option<&dyn ScreenInspectionSource>,
    consultation_log: Option<&ScreenConsultationLog>,
    inspection_budget: &ScreenInspectionBudget,
    cancellation: &CancellationToken,
) -> Result<ReasoningResult<T>, ReasoningContextError> {
    let _stale = provider.take_request_normalizations();
    let system = format!(
        "{system}\n{INSPECTION_INSTRUCTIONS}\nThis run currently has {} screen inspection(s) remaining.",
        inspection_budget.remaining()
    );
    let (first, mut usage) = complete_raw(
        provider,
        &system,
        transcript_context.clone(),
        None,
        cancellation,
    )
    .await?;
    let mut turn = parse_turn(&first)?;
    let mut calls = 1_usize;
    let mut context = transcript_context;
    let mut exhausted_reported = false;
    loop {
        match turn {
            ReasoningTurn::Complete(value) => {
                return Ok(ReasoningResult {
                    value,
                    usage,
                    calls,
                    normalizations: provider.take_request_normalizations(),
                });
            }
            ReasoningTurn::Inspect(request) if inspection_budget.try_spend() => {
                let mut inspection = inspector.map_or_else(
                    || ScreenInspection::Unavailable {
                        requested: request.selector.clone(),
                        reason: ScreenUnavailableReason::InspectorUnavailable,
                    },
                    |source| source.inspect(events, &request),
                );
                if inspector.is_none()
                    && let Some(log) = consultation_log
                {
                    log.record(consultation(log.session_id, &request, &inspection));
                }
                let image = inspection.take_authorized_image_for(&request);
                context.push_str("\n\n");
                context.push_str(&render_inspection(&inspection, image.is_some()));
                context.push_str(&format!(
                    "\n{} screen inspection(s) remain. Return the final requested JSON or request another inspection only if materially necessary.",
                    inspection_budget.remaining()
                ));
                let (next, next_usage) =
                    complete_raw(provider, &system, context.clone(), image, cancellation).await?;
                calls = calls.saturating_add(1);
                add_usage(&mut usage, next_usage);
                turn = parse_turn(&next)?;
            }
            ReasoningTurn::Inspect(request) if !exhausted_reported => {
                if let Some(log) = consultation_log {
                    log.record_budget_exhausted(&request);
                }
                context.push_str(&format!(
                    "\n\nScreen inspection (derived evidence; does not amend the timeline): availability=unavailable requested={} reason=inspection_budget_exhausted\nThe fixed budget of {SCREEN_INSPECTION_BUDGET} inspections is exhausted. Return the final requested JSON now; no further inspection is possible.",
                    render_selector(&request.selector)
                ));
                let (next, next_usage) =
                    complete_raw(provider, &system, context.clone(), None, cancellation).await?;
                calls = calls.saturating_add(1);
                add_usage(&mut usage, next_usage);
                exhausted_reported = true;
                turn = parse_turn(&next)?;
            }
            ReasoningTurn::Inspect(_) => return Err(ReasoningContextError::RepeatedInspection),
        }
    }
}

async fn complete_raw(
    provider: &dyn ReasoningProvider,
    system: &str,
    input: String,
    image: Option<AuthorizedReasoningImage>,
    cancellation: &CancellationToken,
) -> Result<(String, Usage), ReasoningContextError> {
    let request = CompletionRequest {
        model: provider.model_id().to_owned(),
        system: Some(system.to_owned()),
        messages: vec![CompletionMessage {
            role: MessageRole::User,
            content: input,
            cache_boundary: false,
        }],
        max_tokens: Some(4_096),
        temperature: Some(0.0),
        stop: Vec::new(),
    };
    let mut stream = provider
        .stream_advanced_reasoning(
            ReasoningRequest::json_object(request),
            image,
            cancellation.clone(),
        )
        .await?;
    let mut output = String::new();
    let mut usage = Usage::default();
    while let Some(delta) = stream.next().await {
        let delta = delta?;
        output.push_str(&delta.text);
        if let Some(final_usage) = delta.usage {
            usage = final_usage;
        }
    }
    Ok((output, usage))
}

fn parse_turn<T: DeserializeOwned>(
    output: &str,
) -> Result<ReasoningTurn<T>, ReasoningContextError> {
    let value: serde_json::Value = serde_json::from_str(strip_fence(output))?;
    let Some(action) = value.get("action") else {
        return Ok(ReasoningTurn::Complete(serde_json::from_value(value)?));
    };
    let action = action
        .as_str()
        .ok_or_else(|| {
            ReasoningContextError::InvalidInspectionAction("action must be a string".to_owned())
        })?
        .to_owned();
    let wire: InspectionWire = serde_json::from_value(value)?;
    match action.as_str() {
        "complete" => wire
            .result
            .ok_or_else(|| {
                ReasoningContextError::InvalidInspectionAction(
                    "complete action requires result".to_owned(),
                )
            })
            .and_then(|result| {
                serde_json::from_value(result)
                    .map(ReasoningTurn::Complete)
                    .map_err(ReasoningContextError::from)
            }),
        "inspect_screen" => parse_inspection(wire).map(ReasoningTurn::Inspect),
        other => Err(ReasoningContextError::InvalidInspectionAction(format!(
            "unsupported action {other}"
        ))),
    }
}

fn parse_inspection(wire: InspectionWire) -> Result<InspectScreenRequest, ReasoningContextError> {
    let selector = match (wire.timestamp_seconds, wire.event_id) {
        (Some(seconds), None) => Duration::try_from_secs_f64(seconds)
            .map(ScreenSelector::Timestamp)
            .map_err(|_| {
                ReasoningContextError::InvalidInspectionAction(
                    "timestamp_seconds must be finite and non-negative".to_owned(),
                )
            })?,
        (None, Some(id)) => ScreenSelector::Event(sotto_core::EventId::new(id)),
        _ => {
            return Err(ReasoningContextError::InvalidInspectionAction(
                "exactly one of timestamp_seconds or event_id is required".to_owned(),
            ));
        }
    };
    let evidence = match wire.evidence.as_deref() {
        Some("metadata") => ScreenEvidence::Metadata,
        Some("local_ocr") => ScreenEvidence::LocalOcr,
        Some("image") => ScreenEvidence::Image,
        _ => {
            return Err(ReasoningContextError::InvalidInspectionAction(
                "evidence must be metadata, local_ocr, or image".to_owned(),
            ));
        }
    };
    let reason = wire.reason.unwrap_or_default();
    if reason.trim().is_empty() || reason.len() > 512 {
        return Err(ReasoningContextError::InvalidInspectionAction(
            "reason must contain 1..=512 bytes".to_owned(),
        ));
    }
    Ok(InspectScreenRequest {
        selector,
        evidence,
        reason,
    })
}

fn render_inspection(inspection: &ScreenInspection, image_attached: bool) -> String {
    match inspection {
        ScreenInspection::Available {
            provenance,
            ocr_text,
            authorized_image,
        } => {
            let requested = render_selector(&provenance.requested);
            let visible_to = provenance
                .visible_to
                .map(|value| format!("{:.3}s", value.as_secs_f64()))
                .unwrap_or_else(|| "session-end-unknown".to_owned());
            let precision = match provenance.precision {
                ScreenPrecision::SampledChangeFrame => "sampled_change_frame",
                ScreenPrecision::DecodedVideoFrame => "decoded_video_frame",
            };
            let mut line = format!(
                "Screen inspection (derived evidence; does not amend the timeline): availability=available requested={requested} captured_at={:.3}s visible_interval=[{:.3}s,{visible_to}) snapshot_event={} precision={precision}",
                provenance.captured_at.as_secs_f64(),
                provenance.visible_from.as_secs_f64(),
                provenance.snapshot_event_id.get(),
            );
            if let Some(text) = ocr_text {
                line.push_str("\nLocal OCR: ");
                line.push_str(text);
            }
            if authorized_image.is_some() && image_attached {
                line.push_str("\nImage evidence is attached under explicit local opt-in.");
            } else if authorized_image.is_some() {
                line.push_str("\nImage evidence became unavailable before bounded dispatch.");
            }
            line
        }
        ScreenInspection::RecordingAvailable {
            provenance,
            ocr_text,
        } => {
            let requested = render_selector(&provenance.requested);
            let mut line = format!(
                "Screen inspection (derived evidence; does not amend the timeline): availability=available requested={requested} requested_media_time={:.3}s decoded_media_time={:.3}s decode_offset_ms={:.3} recording_session={} precision=decoded_video_frame",
                provenance.requested_media_time.as_secs_f64(),
                provenance.decoded_media_time.as_secs_f64(),
                duration_offset_ms(
                    provenance.decoded_media_time,
                    provenance.requested_media_time
                ),
                provenance.recording_session_id.get(),
            );
            if let Some(text) = ocr_text {
                line.push_str("\nLocal OCR: ");
                line.push_str(text);
            }
            line
        }
        ScreenInspection::Unavailable { requested, reason } => format!(
            "Screen inspection (derived evidence; does not amend the timeline): availability=unavailable requested={} reason={}",
            render_selector(requested),
            render_unavailable(reason)
        ),
    }
}

fn render_selector(selector: &ScreenSelector) -> String {
    match selector {
        ScreenSelector::Timestamp(value) => format!("timestamp:{:.3}s", value.as_secs_f64()),
        ScreenSelector::Event(value) => format!("event:{}", value.get()),
    }
}

fn render_unavailable(reason: &ScreenUnavailableReason) -> &str {
    match reason {
        ScreenUnavailableReason::NoSnapshots => "no_snapshots",
        ScreenUnavailableReason::OutOfRange => "out_of_range",
        ScreenUnavailableReason::NoSnapshotForTimestamp => "no_snapshot_for_timestamp",
        ScreenUnavailableReason::EventNotFound => "event_not_found",
        ScreenUnavailableReason::EventIsNotSnapshot => "event_is_not_snapshot",
        ScreenUnavailableReason::FramePruned => "frame_pruned",
        ScreenUnavailableReason::FrameOutsideCache => "frame_outside_cache",
        ScreenUnavailableReason::OcrUnavailable => "ocr_unavailable",
        ScreenUnavailableReason::OcrFailed(_) => "ocr_failed",
        ScreenUnavailableReason::ImageOptInRequired => "image_opt_in_required",
        ScreenUnavailableReason::ImageAuthorizationMismatch => "image_authorization_mismatch",
        ScreenUnavailableReason::ImageUnreadable => "image_unreadable",
        ScreenUnavailableReason::ImageTooLarge => "image_too_large",
        ScreenUnavailableReason::InvalidImageMedia => "invalid_image_media",
        ScreenUnavailableReason::FrameSubstituted => "frame_substituted",
        ScreenUnavailableReason::InspectorUnavailable => "inspector_unavailable",
        ScreenUnavailableReason::RecordingDeleted => "recording_deleted",
        ScreenUnavailableReason::RecordingPruned => "recording_pruned",
        ScreenUnavailableReason::RecordingMissing => "recording_missing",
        ScreenUnavailableReason::InvalidTimeMapping => "invalid_time_mapping",
        ScreenUnavailableReason::RecordingDecodeFailed(_) => "recording_decode_failed",
        ScreenUnavailableReason::RecordingImageAuthorizationUnavailable => {
            "recording_image_authorization_unavailable"
        }
    }
}

fn duration_offset_ms(actual: Duration, requested: Duration) -> f64 {
    if actual >= requested {
        actual.saturating_sub(requested).as_secs_f64() * 1_000.0
    } else {
        -(requested.saturating_sub(actual).as_secs_f64() * 1_000.0)
    }
}

const fn consulted_moment(selector: &ScreenSelector) -> ConsultedMoment {
    match selector {
        ScreenSelector::Timestamp(value) => ConsultedMoment::Timestamp(*value),
        ScreenSelector::Event(value) => ConsultedMoment::Event(*value),
    }
}

const fn consulted_evidence(evidence: ScreenEvidence) -> ConsultedEvidence {
    match evidence {
        ScreenEvidence::Metadata => ConsultedEvidence::Metadata,
        ScreenEvidence::LocalOcr => ConsultedEvidence::LocalOcr,
        ScreenEvidence::Image => ConsultedEvidence::Image,
    }
}

fn consultation_outcome(inspection: &ScreenInspection) -> ConsultationOutcome {
    match inspection {
        ScreenInspection::RecordingAvailable {
            provenance,
            ocr_text,
        } => ConsultationOutcome::Frame {
            requested_media_time: provenance.requested_media_time,
            decoded_media_time: provenance.decoded_media_time,
            precision: consulted_precision(provenance.precision),
            ocr_characters: ocr_text.as_ref().map(|text| text.chars().count()),
        },
        ScreenInspection::Available {
            provenance,
            ocr_text,
            ..
        } => ConsultationOutcome::Frame {
            requested_media_time: provenance.captured_at,
            decoded_media_time: provenance.captured_at,
            precision: consulted_precision(provenance.precision),
            ocr_characters: ocr_text.as_ref().map(|text| text.chars().count()),
        },
        ScreenInspection::Unavailable { reason, .. } => ConsultationOutcome::Unavailable {
            reason: render_unavailable(reason).to_owned(),
        },
    }
}

const fn consulted_precision(precision: ScreenPrecision) -> ConsultedPrecision {
    match precision {
        ScreenPrecision::SampledChangeFrame => ConsultedPrecision::SampledChangeFrame,
        ScreenPrecision::DecodedVideoFrame => ConsultedPrecision::DecodedVideoFrame,
    }
}

fn format_clock(value: Duration) -> String {
    let seconds = value.as_secs();
    format!("{:02}:{:02}", seconds / 60, seconds % 60)
}

fn format_precise(value: Duration) -> String {
    let seconds = value.as_secs();
    format!(
        "{:02}:{:02}.{:03}",
        seconds / 60,
        seconds % 60,
        value.subsec_millis()
    )
}

fn add_usage(total: &mut Usage, increment: Usage) {
    total.input_tokens = total.input_tokens.saturating_add(increment.input_tokens);
    total.output_tokens = total.output_tokens.saturating_add(increment.output_tokens);
    total.cache_read_tokens = total
        .cache_read_tokens
        .saturating_add(increment.cache_read_tokens);
    total.cache_write_tokens = total
        .cache_write_tokens
        .saturating_add(increment.cache_write_tokens);
}

fn strip_fence(output: &str) -> &str {
    let trimmed = output.trim();
    trimmed
        .strip_prefix("```json")
        .and_then(|body| body.strip_suffix("```"))
        .map(str::trim)
        .unwrap_or(trimmed)
}

#[cfg(test)]
mod tests {
    use std::{
        collections::VecDeque,
        path::Path,
        sync::{
            Arc, Mutex,
            atomic::{AtomicUsize, Ordering},
        },
        time::Duration,
    };

    use providers::{
        AuthKind, AuthStatus, BackendCapabilities, BackendDescriptor, BackendId, ReasoningProvider,
        Registry, Role,
    };
    use screen::{
        Frame, ImageInspectionPolicy, OcrEngine, RecordingBackedScreenInspector,
        RecordingFrameDecoder, ScreenError,
    };
    use sotto_core::{
        BoxFuture, BoxStream, CancellationToken, CaptureTarget, CompletionProvider,
        CompletionRequest, Delta, EventPayload, ProviderError, Session, SessionId, Source,
        TargetKind, TimelineBuilder, Usage, Utterance,
        types::{MediaTimeMapping, RecordingContainer, SessionRecording},
    };

    use super::complete_with_optional_inspection;

    struct QueueProvider {
        outputs: Mutex<VecDeque<String>>,
        inputs: Arc<Mutex<Vec<String>>>,
    }

    impl CompletionProvider for QueueProvider {
        fn stream(
            &self,
            request: CompletionRequest,
            _cancellation: CancellationToken,
        ) -> BoxFuture<'_, Result<BoxStream<'static, Result<Delta, ProviderError>>, ProviderError>>
        {
            let output = self
                .outputs
                .lock()
                .map_err(|_| ProviderError::Network("output queue poisoned".to_owned()))
                .and_then(|mut outputs| {
                    outputs
                        .pop_front()
                        .ok_or_else(|| ProviderError::Network("missing queued output".to_owned()))
                });
            if let Some(message) = request.messages.first()
                && let Ok(mut inputs) = self.inputs.lock()
            {
                inputs.push(message.content.clone());
            }
            Box::pin(async move {
                let output = output?;
                Ok(Box::pin(futures_util::stream::iter([Ok(Delta {
                    text: output,
                    is_final: true,
                    usage: Some(Usage::default()),
                    stop_reason: None,
                })]))
                    as BoxStream<'static, Result<Delta, ProviderError>>)
            })
        }

        fn model_id(&self) -> &str {
            "recording-inspection-test"
        }
    }

    impl ReasoningProvider for QueueProvider {}

    struct PngDecoder(Arc<AtomicUsize>);

    impl RecordingFrameDecoder for PngDecoder {
        fn decode_png(
            &self,
            _path: &Path,
            requested: Duration,
        ) -> Result<(Vec<u8>, Duration), ScreenError> {
            self.0.fetch_add(1, Ordering::Relaxed);
            let mut bytes = Vec::new();
            {
                let mut encoder = png::Encoder::new(&mut bytes, 1, 1);
                encoder.set_color(png::ColorType::Rgba);
                encoder.set_depth(png::BitDepth::Eight);
                let mut writer = encoder.write_header()?;
                writer.write_image_data(&[9, 0, 0, 255])?;
                writer.finish()?;
            }
            Ok((bytes, requested + Duration::from_millis(25)))
        }
    }

    struct CountingOcr(Arc<AtomicUsize>);

    impl OcrEngine for CountingOcr {
        fn recognize(&self, _frame: &Frame) -> Result<String, ScreenError> {
            self.0.fetch_add(1, Ordering::Relaxed);
            Ok("Known slide text".to_owned())
        }
    }

    fn timeline() -> (TimelineBuilder, sotto_core::EventId) {
        let mut timeline = TimelineBuilder::new(Session::new(
            SessionId::new(59),
            CaptureTarget {
                bundle_id: None,
                display_name: "Meeting".to_owned(),
                window_title: None,
                kind: TargetKind::Application,
                audio_scoped: true,
            },
            0,
        ));
        let event = timeline.append(
            Duration::from_secs(8),
            EventPayload::UtteranceFinal(Utterance {
                source: Source::System,
                start: Duration::from_secs(8),
                end: Duration::from_secs(9),
                text: "Look at the slide".to_owned(),
                avg_logprob: -0.1,
                annotations: vec![],
            }),
        );
        (timeline, event.id())
    }

    #[tokio::test]
    async fn reasoning_event_selector_decodes_recording_and_runs_explicit_local_ocr()
    -> Result<(), Box<dyn std::error::Error>> {
        let path = std::env::temp_dir().join(format!(
            "sotto-reasoning-recording-inspection-{}.mp4",
            std::process::id()
        ));
        std::fs::write(&path, b"contract media handle")?;
        let decode_calls = Arc::new(AtomicUsize::new(0));
        let ocr_calls = Arc::new(AtomicUsize::new(0));
        let inspector = RecordingBackedScreenInspector::with_decoder(
            SessionRecording::Available {
                session_id: SessionId::new(59),
                path: path.to_string_lossy().into_owned(),
                container: RecordingContainer::Mp4,
                duration: Duration::from_secs(30),
                byte_size: 21,
                time_mapping: MediaTimeMapping::IDENTITY,
            },
            PngDecoder(Arc::clone(&decode_calls)),
            Some(CountingOcr(Arc::clone(&ocr_calls))),
            ImageInspectionPolicy::Deny,
        );
        let inputs = Arc::new(Mutex::new(Vec::new()));
        let (timeline, event_id) = timeline();
        let provider = QueueProvider {
            outputs: Mutex::new(VecDeque::from([
                format!(
                    r#"{{"action":"inspect_screen","event_id":{},"evidence":"local_ocr","reason":"read the cited slide"}}"#,
                    event_id.get()
                ),
                r#"{"answer":"done"}"#.to_owned(),
            ])),
            inputs: Arc::clone(&inputs),
        };

        assert_eq!(decode_calls.load(Ordering::Relaxed), 0);
        assert_eq!(ocr_calls.load(Ordering::Relaxed), 0);
        let result = complete_with_optional_inspection::<serde_json::Value>(
            &provider,
            "Return JSON",
            "transcript".to_owned(),
            timeline.events(),
            Some(&inspector),
        )
        .await?;
        assert_eq!(result.value["answer"], "done");
        assert_eq!(result.calls, 2);
        assert_eq!(decode_calls.load(Ordering::Relaxed), 1);
        assert_eq!(ocr_calls.load(Ordering::Relaxed), 1);
        let inputs = inputs.lock().map_err(|_| "input capture poisoned")?;
        assert!(inputs[1].contains("requested_media_time=8.000s"));
        assert!(inputs[1].contains("decoded_media_time=8.025s"));
        assert!(inputs[1].contains("decode_offset_ms=25.000"));
        assert!(inputs[1].contains("Local OCR: Known slide text"));
        assert!(!inputs[1].contains(path.to_string_lossy().as_ref()));
        std::fs::remove_file(path)?;
        Ok(())
    }

    #[tokio::test]
    async fn completed_first_pass_does_not_decode_or_run_default_ocr()
    -> Result<(), Box<dyn std::error::Error>> {
        let path = std::env::temp_dir().join(format!(
            "sotto-reasoning-no-default-inspection-{}.mp4",
            std::process::id()
        ));
        std::fs::write(&path, b"contract media handle")?;
        let decode_calls = Arc::new(AtomicUsize::new(0));
        let ocr_calls = Arc::new(AtomicUsize::new(0));
        let inspector = RecordingBackedScreenInspector::with_decoder(
            SessionRecording::Available {
                session_id: SessionId::new(59),
                path: path.to_string_lossy().into_owned(),
                container: RecordingContainer::Mp4,
                duration: Duration::from_secs(30),
                byte_size: 21,
                time_mapping: MediaTimeMapping::IDENTITY,
            },
            PngDecoder(Arc::clone(&decode_calls)),
            Some(CountingOcr(Arc::clone(&ocr_calls))),
            ImageInspectionPolicy::Deny,
        );
        let provider = QueueProvider {
            outputs: Mutex::new(VecDeque::from([r#"{"answer":"done"}"#.to_owned()])),
            inputs: Arc::new(Mutex::new(Vec::new())),
        };

        let result = complete_with_optional_inspection::<serde_json::Value>(
            &provider,
            "Return JSON",
            "transcript".to_owned(),
            &[],
            Some(&inspector),
        )
        .await?;
        assert_eq!(result.calls, 1);
        assert_eq!(decode_calls.load(Ordering::Relaxed), 0);
        assert_eq!(ocr_calls.load(Ordering::Relaxed), 0);
        std::fs::remove_file(path)?;
        Ok(())
    }

    #[tokio::test]
    async fn notes_context_dispatch_uses_the_resolved_backend_normalization_seam()
    -> Result<(), Box<dyn std::error::Error>> {
        let mut registry = Registry::default();
        let descriptor = BackendDescriptor::new(
            BackendId::new("example.notes-no-sampling")?,
            "Notes no sampling",
            "recording-inspection-test",
            1,
            BackendCapabilities::reasoning_baseline(),
            AuthKind::None,
            AuthStatus::Ready,
        )?;
        let id = descriptor.id().clone();
        registry.register_reasoning(
            descriptor,
            Arc::new(QueueProvider {
                outputs: Mutex::new(VecDeque::from([r#"{"answer":"done"}"#.to_owned()])),
                inputs: Arc::new(Mutex::new(Vec::new())),
            }),
        )?;
        registry.select(Role::Summarizer, Some(&id))?;
        let resolved = registry
            .resolve(Role::Summarizer)?
            .ok_or("notes backend must resolve")?;
        let provider = resolved.provider();
        let result = complete_with_optional_inspection::<serde_json::Value>(
            provider.as_ref(),
            "Return JSON",
            "transcript".to_owned(),
            &[],
            None,
        )
        .await?;
        assert_eq!(result.value["answer"], "done");
        let observations = result.normalizations;
        assert_eq!(observations.len(), 3);
        assert_eq!(observations[0].dispatch_id, observations[1].dispatch_id);
        assert_eq!(observations[0].dispatch_id, observations[2].dispatch_id);
        assert_eq!(
            observations[0].normalization.control.to_string(),
            "max_tokens"
        );
        assert_eq!(
            observations[1].normalization.control.to_string(),
            "temperature"
        );
        assert_eq!(
            observations[2].normalization.control.to_string(),
            "json_object output"
        );
        assert_eq!(resolved.normalization_observations().len(), 3);
        Ok(())
    }
}
