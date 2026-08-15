//! The app's assembly of the recording-backed `inspect_screen` seam.
//!
//! ADR-0009 lets a reasoning pass ask for one frame at a timestamp or event. `screen` implements
//! the inspector over the durable session recording and `insight` accepts one; this module is the
//! joint. It resolves the recording per session **at request time**, so a recording that was
//! pruned, deleted or is still growing produces the contract's explicit unavailable state and the
//! run continues from the transcript alone.
//!
//! Two boundaries are deliberate here:
//!
//! * Local decode and local OCR are not consent to transport an image. The policy stays
//!   [`ImageInspectionPolicy::Deny`], so an `image` request is refused before any decode and no
//!   image can reach a backend without the separate opt-in that Phase 5 still owns.
//! * Every consultation is recorded. A model that silently looked at the screen is a worse product
//!   than one that did not look, so each request lands in a [`ScreenConsultationLog`] the UI can
//!   show back to the user.

use std::{
    path::PathBuf,
    sync::{Arc, Mutex, MutexGuard, PoisonError},
    time::Duration,
};

use screen::{
    Frame, ImageInspectionPolicy, InspectScreenRequest, OcrEngine, PlatformFrameDecoder,
    RecordingBackedScreenInspector, RecordingFrameDecoder, ScreenError, ScreenEvidence,
    ScreenInspection, ScreenInspectionSource, ScreenPrecision, ScreenSelector,
    ScreenUnavailableReason,
};
use sotto_core::{EventId, SessionId, TimelineEvent, types::SessionRecording};

/// The meeting moment a reasoning pass asked to see.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ConsultedMoment {
    Timestamp(Duration),
    Event(EventId),
}

/// The evidence kind the pass asked for, never widened by the app.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ConsultedEvidence {
    Metadata,
    LocalOcr,
    Image,
}

/// Honest precision of what the pass received. A sampled change frame is never a video frame.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
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

/// What the consultation produced.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ConsultationOutcome {
    Frame {
        requested_media_time: Duration,
        decoded_media_time: Duration,
        precision: ConsultedPrecision,
        /// Characters returned by local OCR, present only when the pass asked for text.
        ocr_characters: Option<usize>,
    },
    Unavailable {
        reason: &'static str,
    },
}

/// One disclosed screen consultation performed on behalf of a reasoning run.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ScreenConsultation {
    pub session_id: SessionId,
    pub requested: ConsultedMoment,
    pub evidence: ConsultedEvidence,
    pub reason: String,
    pub outcome: ConsultationOutcome,
}

impl ScreenConsultation {
    /// Renders the sentence the user reads. It always names the moment and the decode precision.
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
                let text = match ocr_characters {
                    Some(count) => {
                        format!(" · local text recognition returned {count} characters")
                    }
                    None => String::new(),
                };
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

/// Run-scoped, shared record of every consultation an inspector performed.
#[derive(Clone, Default)]
pub struct ScreenConsultationLog(Arc<Mutex<Vec<ScreenConsultation>>>);

impl ScreenConsultationLog {
    fn locked(&self) -> MutexGuard<'_, Vec<ScreenConsultation>> {
        // A poisoned log must not hide the fact that the model looked at the screen.
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn record(&self, consultation: ScreenConsultation) {
        self.locked().push(consultation);
    }

    #[must_use]
    pub fn entries(&self) -> Vec<ScreenConsultation> {
        self.locked().clone()
    }
}

/// Builds the inspector a reasoning run for one session may use, plus its disclosure log.
pub trait ScreenInspectorAssembly: Send + Sync {
    fn assemble(
        &self,
        session_id: SessionId,
    ) -> (Arc<dyn ScreenInspectionSource>, ScreenConsultationLog);
}

/// Resolves the durable recording for one session, at request time rather than at assembly time.
pub trait RecordingSource: Send + Sync {
    fn recording(&self, session_id: SessionId) -> Result<Option<SessionRecording>, String>;
}

/// The product source: the same SQLite file that holds the meeting record.
pub struct StoredRecordings {
    database: PathBuf,
}

impl StoredRecordings {
    #[must_use]
    pub const fn new(database: PathBuf) -> Self {
        Self { database }
    }
}

impl RecordingSource for StoredRecordings {
    fn recording(&self, session_id: SessionId) -> Result<Option<SessionRecording>, String> {
        // `load_recording` deliberately excludes a still-growing recording, so a live capture
        // resolves to `None` and becomes an explicit unavailable state below.
        crate::persistence_runtime::block_on(async {
            rag::Store::open(&self.database)
                .await?
                .load_recording(session_id)
                .await
        })
        .map_err(|error| error.to_string())
    }
}

/// Local Apple Vision OCR, invoked only when a pass explicitly asks for screen text.
#[derive(Clone, Copy, Debug, Default)]
pub struct LocalOcr;

#[cfg(target_os = "macos")]
impl OcrEngine for LocalOcr {
    fn recognize(&self, frame: &Frame) -> Result<String, ScreenError> {
        screen::VisionOcr::new().recognize(frame)
    }
}

#[cfg(not(target_os = "macos"))]
impl OcrEngine for LocalOcr {
    fn recognize(&self, _frame: &Frame) -> Result<String, ScreenError> {
        Err(ScreenError::UnsupportedPlatform)
    }
}

/// Assembles recording-backed inspectors without deciding anything at construction time.
pub struct RecordingScreenInspectors<R, D, O> {
    recordings: Arc<R>,
    decoder: D,
    ocr: Option<O>,
    image_policy: ImageInspectionPolicy,
}

impl<R, D, O> RecordingScreenInspectors<R, D, O> {
    #[must_use]
    pub fn new(recordings: R, decoder: D, ocr: Option<O>) -> Self {
        Self {
            recordings: Arc::new(recordings),
            decoder,
            ocr,
            // Phase 5 owns the image opt-in. Until it exists, local decode and local OCR must
            // never be mistaken for permission to transport an image.
            image_policy: ImageInspectionPolicy::Deny,
        }
    }
}

impl<R, D, O> ScreenInspectorAssembly for RecordingScreenInspectors<R, D, O>
where
    R: RecordingSource + 'static,
    D: RecordingFrameDecoder + Clone + 'static,
    O: OcrEngine + Clone + Send + Sync + 'static,
{
    fn assemble(
        &self,
        session_id: SessionId,
    ) -> (Arc<dyn ScreenInspectionSource>, ScreenConsultationLog) {
        let log = ScreenConsultationLog::default();
        let inspector = SessionScreenInspector {
            session_id,
            recordings: Arc::clone(&self.recordings),
            decoder: self.decoder.clone(),
            ocr: self.ocr.clone(),
            image_policy: self.image_policy,
            log: log.clone(),
        };
        (Arc::new(inspector), log)
    }
}

/// The product assembly over the meeting database.
#[must_use]
pub fn product_screen_inspectors(
    database: PathBuf,
) -> RecordingScreenInspectors<StoredRecordings, PlatformFrameDecoder, LocalOcr> {
    RecordingScreenInspectors::new(
        StoredRecordings::new(database),
        PlatformFrameDecoder,
        // Off-macOS there is no local OCR engine; the contract reports that rather than failing.
        cfg!(target_os = "macos").then_some(LocalOcr),
    )
}

struct SessionScreenInspector<R, D, O> {
    session_id: SessionId,
    recordings: Arc<R>,
    decoder: D,
    ocr: Option<O>,
    image_policy: ImageInspectionPolicy,
    log: ScreenConsultationLog,
}

impl<R, D, O> ScreenInspectionSource for SessionScreenInspector<R, D, O>
where
    R: RecordingSource,
    D: RecordingFrameDecoder + Clone,
    O: OcrEngine + Clone + Send + Sync,
{
    fn inspect(
        &self,
        events: &[TimelineEvent],
        request: &InspectScreenRequest,
    ) -> ScreenInspection {
        let inspection = match self.recordings.recording(self.session_id) {
            Ok(Some(recording)) => RecordingBackedScreenInspector::with_decoder(
                recording,
                self.decoder.clone(),
                self.ocr.clone(),
                self.image_policy,
            )
            .inspect(events, request),
            Ok(None) => unavailable(request, ScreenUnavailableReason::RecordingMissing),
            Err(error) => unavailable(
                request,
                ScreenUnavailableReason::RecordingDecodeFailed(format!(
                    "recording lookup failed: {error}"
                )),
            ),
        };
        self.log
            .record(consultation(self.session_id, request, &inspection));
        inspection
    }
}

fn unavailable(
    request: &InspectScreenRequest,
    reason: ScreenUnavailableReason,
) -> ScreenInspection {
    ScreenInspection::Unavailable {
        requested: request.selector.clone(),
        reason,
    }
}

fn consultation(
    session_id: SessionId,
    request: &InspectScreenRequest,
    inspection: &ScreenInspection,
) -> ScreenConsultation {
    ScreenConsultation {
        session_id,
        requested: match &request.selector {
            ScreenSelector::Timestamp(value) => ConsultedMoment::Timestamp(*value),
            ScreenSelector::Event(id) => ConsultedMoment::Event(*id),
        },
        evidence: match request.evidence {
            ScreenEvidence::Metadata => ConsultedEvidence::Metadata,
            ScreenEvidence::LocalOcr => ConsultedEvidence::LocalOcr,
            ScreenEvidence::Image => ConsultedEvidence::Image,
        },
        reason: request.reason.clone(),
        outcome: outcome(inspection),
    }
}

fn outcome(inspection: &ScreenInspection) -> ConsultationOutcome {
    match inspection {
        ScreenInspection::RecordingAvailable {
            provenance,
            ocr_text,
        } => ConsultationOutcome::Frame {
            requested_media_time: provenance.requested_media_time,
            decoded_media_time: provenance.decoded_media_time,
            precision: precision(provenance.precision),
            ocr_characters: ocr_text.as_ref().map(|text| text.chars().count()),
        },
        // A retained change frame carries no separate decode time; reporting the sampled capture
        // time for both keeps the disclosure from implying precision the source does not have.
        ScreenInspection::Available {
            provenance,
            ocr_text,
            ..
        } => ConsultationOutcome::Frame {
            requested_media_time: provenance.captured_at,
            decoded_media_time: provenance.captured_at,
            precision: precision(provenance.precision),
            ocr_characters: ocr_text.as_ref().map(|text| text.chars().count()),
        },
        ScreenInspection::Unavailable { reason, .. } => ConsultationOutcome::Unavailable {
            reason: unavailable_token(reason),
        },
    }
}

const fn precision(value: ScreenPrecision) -> ConsultedPrecision {
    match value {
        ScreenPrecision::SampledChangeFrame => ConsultedPrecision::SampledChangeFrame,
        ScreenPrecision::DecodedVideoFrame => ConsultedPrecision::DecodedVideoFrame,
    }
}

/// Mirrors the tokens the reasoning prompt already uses, so the user and the model see one word.
const fn unavailable_token(reason: &ScreenUnavailableReason) -> &'static str {
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

#[cfg(test)]
mod tests {
    use std::{
        path::Path,
        sync::atomic::{AtomicUsize, Ordering},
    };

    use sotto_core::types::{
        MediaTimeMapping, RecordingContainer, RecordingMissingReason, SessionRecording,
    };

    use super::*;

    /// One 1x1 8-bit RGBA PNG. Embedding it keeps the app free of an encoder dependency while
    /// still exercising the real local-decode path `decode_for_local_use` takes before OCR.
    const ONE_PIXEL_PNG: &[u8] = &[
        0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44,
        0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1f,
        0x15, 0xc4, 0x89, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x44, 0x41, 0x54, 0x78, 0xda, 0x63, 0x60,
        0x67, 0x60, 0xf8, 0x0f, 0x00, 0x01, 0x20, 0x01, 0x07, 0x45, 0xfa, 0xc7, 0x0d, 0x00, 0x00,
        0x00, 0x00, 0x49, 0x45, 0x4e, 0x44, 0xae, 0x42, 0x60, 0x82,
    ];

    #[derive(Clone)]
    struct CountingDecoder(Arc<AtomicUsize>);

    impl RecordingFrameDecoder for CountingDecoder {
        fn decode_png(
            &self,
            _path: &Path,
            requested: Duration,
        ) -> Result<(Vec<u8>, Duration), ScreenError> {
            self.0.fetch_add(1, Ordering::Relaxed);
            Ok((
                ONE_PIXEL_PNG.to_vec(),
                requested + Duration::from_millis(25),
            ))
        }
    }

    #[derive(Clone)]
    struct CountingOcr(Arc<AtomicUsize>);

    impl OcrEngine for CountingOcr {
        fn recognize(&self, _frame: &Frame) -> Result<String, ScreenError> {
            self.0.fetch_add(1, Ordering::Relaxed);
            Ok("Pricing".to_owned())
        }
    }

    struct FixedRecording(Option<SessionRecording>);

    impl RecordingSource for FixedRecording {
        fn recording(&self, _session_id: SessionId) -> Result<Option<SessionRecording>, String> {
            Ok(self.0.clone())
        }
    }

    fn available(path: &Path) -> SessionRecording {
        SessionRecording::Available {
            session_id: SessionId::new(59),
            path: path.to_string_lossy().into_owned(),
            container: RecordingContainer::Mp4,
            duration: Duration::from_secs(60),
            byte_size: 24,
            time_mapping: MediaTimeMapping::IDENTITY,
        }
    }

    fn request(evidence: ScreenEvidence) -> InspectScreenRequest {
        InspectScreenRequest {
            selector: ScreenSelector::Timestamp(Duration::from_secs(8)),
            evidence,
            reason: "read the cited slide".to_owned(),
        }
    }

    #[test]
    fn image_evidence_is_refused_before_any_decode() -> Result<(), Box<dyn std::error::Error>> {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("recording.mp4");
        std::fs::write(&path, b"contract-only; not real media")?;
        let decodes = Arc::new(AtomicUsize::new(0));
        let assembly = RecordingScreenInspectors::new(
            FixedRecording(Some(available(&path))),
            CountingDecoder(Arc::clone(&decodes)),
            Some(CountingOcr(Arc::new(AtomicUsize::new(0)))),
        );
        let (inspector, log) = assembly.assemble(SessionId::new(59));

        let inspection = inspector.inspect(&[], &request(ScreenEvidence::Image));

        assert!(
            matches!(
                inspection,
                ScreenInspection::Unavailable {
                    reason: ScreenUnavailableReason::ImageOptInRequired,
                    ..
                }
            ),
            "the app must not widen the image opt-in on the reasoning path"
        );
        assert_eq!(
            decodes.load(Ordering::Relaxed),
            0,
            "a refused image request must not decode the recording"
        );
        let entries = log.entries();
        assert_eq!(entries.len(), 1, "a refusal is still a disclosed request");
        assert_eq!(
            entries[0].outcome,
            ConsultationOutcome::Unavailable {
                reason: "image_opt_in_required"
            }
        );
        Ok(())
    }

    #[test]
    fn local_ocr_decodes_once_and_discloses_the_moment_and_precision()
    -> Result<(), Box<dyn std::error::Error>> {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("recording.mp4");
        std::fs::write(&path, b"contract-only; not real media")?;
        let decodes = Arc::new(AtomicUsize::new(0));
        let ocr = Arc::new(AtomicUsize::new(0));
        let assembly = RecordingScreenInspectors::new(
            FixedRecording(Some(available(&path))),
            CountingDecoder(Arc::clone(&decodes)),
            Some(CountingOcr(Arc::clone(&ocr))),
        );
        let (inspector, log) = assembly.assemble(SessionId::new(59));

        let inspection = inspector.inspect(&[], &request(ScreenEvidence::LocalOcr));

        assert!(
            matches!(inspection, ScreenInspection::RecordingAvailable { .. }),
            "an available recording must serve the requested frame"
        );
        assert_eq!(decodes.load(Ordering::Relaxed), 1, "exactly one decode");
        assert_eq!(ocr.load(Ordering::Relaxed), 1, "exactly one OCR pass");
        let entries = log.entries();
        assert_eq!(
            entries[0].outcome,
            ConsultationOutcome::Frame {
                requested_media_time: Duration::from_secs(8),
                decoded_media_time: Duration::from_millis(8_025),
                precision: ConsultedPrecision::DecodedVideoFrame,
                ocr_characters: Some(7),
            }
        );
        let described = entries[0].describe();
        assert!(described.contains("00:08"), "{described}");
        assert!(
            described.contains("decoded video frame at 00:08.025"),
            "{described}"
        );
        assert!(
            described.contains("no image was sent to the reasoning backend"),
            "{described}"
        );
        Ok(())
    }

    #[test]
    fn deleted_pruned_and_growing_recordings_stay_explicit_and_never_decode()
    -> Result<(), Box<dyn std::error::Error>> {
        for (source, expected) in [
            (
                FixedRecording(Some(SessionRecording::Missing {
                    session_id: SessionId::new(59),
                    reason: RecordingMissingReason::Deleted,
                })),
                "recording_deleted",
            ),
            (
                FixedRecording(Some(SessionRecording::Missing {
                    session_id: SessionId::new(59),
                    reason: RecordingMissingReason::Pruned,
                })),
                "recording_pruned",
            ),
            // A still-growing recording has no settled row, which is how capture keeps a live
            // meeting out of reasoning's reach.
            (FixedRecording(None), "recording_missing"),
        ] {
            let decodes = Arc::new(AtomicUsize::new(0));
            let assembly = RecordingScreenInspectors::new(
                source,
                CountingDecoder(Arc::clone(&decodes)),
                Some(CountingOcr(Arc::new(AtomicUsize::new(0)))),
            );
            let (inspector, log) = assembly.assemble(SessionId::new(59));

            let inspection = inspector.inspect(&[], &request(ScreenEvidence::LocalOcr));

            assert!(
                matches!(inspection, ScreenInspection::Unavailable { .. }),
                "{expected} must remain an explicit unavailable state"
            );
            assert_eq!(
                decodes.load(Ordering::Relaxed),
                0,
                "{expected} must not reach the decoder"
            );
            assert_eq!(
                log.entries()[0].outcome,
                ConsultationOutcome::Unavailable { reason: expected }
            );
        }
        Ok(())
    }

    #[test]
    fn assembling_an_inspector_performs_no_work_until_a_pass_asks()
    -> Result<(), Box<dyn std::error::Error>> {
        let decodes = Arc::new(AtomicUsize::new(0));
        let ocr = Arc::new(AtomicUsize::new(0));
        let assembly = RecordingScreenInspectors::new(
            FixedRecording(None),
            CountingDecoder(Arc::clone(&decodes)),
            Some(CountingOcr(Arc::clone(&ocr))),
        );

        let (_inspector, log) = assembly.assemble(SessionId::new(59));

        assert_eq!(decodes.load(Ordering::Relaxed), 0);
        assert_eq!(ocr.load(Ordering::Relaxed), 0);
        assert!(
            log.entries().is_empty(),
            "assembly alone is not a consultation"
        );
        Ok(())
    }
}
