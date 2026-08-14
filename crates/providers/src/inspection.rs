//! Application-facing on-demand recording inspection.
//!
//! This path deliberately returns local evidence only. Provider transport continues to accept
//! only `AuthorizedReasoningImage`, so decoding a recording frame cannot itself disclose it.

use std::time::Duration;

use screen::{
    DecodedRecordingFrame, OcrEngine, RecordingFrameUnavailable, extract_recording_frame,
};
use sotto_core::{EventId, EventPayload, TimelineEvent, types::SessionRecording};

pub use screen::RecordingBackedScreenInspector;
pub use screen::RecordingFrameUnavailableReason;

/// A transcript coordinate selected by the user or an `inspect_screen` action.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RecordingScreenSelector {
    Timestamp(Duration),
    Event(EventId),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LocalRecordingEvidence {
    Frame,
    Ocr,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RecordingInspectionUnavailable {
    EventNotFound,
    EventHasNoMediaTimestamp,
    Recording(RecordingFrameUnavailable),
    OcrUnavailable,
    OcrFailed(String),
}

/// Local-only inspection result. It has no conversion to provider-authorized image evidence.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum LocalRecordingInspection {
    Frame(DecodedRecordingFrame),
    Ocr {
        frame: DecodedRecordingFrame,
        text: String,
    },
}

/// Resolves event selectors and performs no work until `inspect` is called explicitly.
pub struct RecordingInspector<O> {
    ocr: Option<O>,
}

impl<O> RecordingInspector<O> {
    #[must_use]
    pub const fn new(ocr: Option<O>) -> Self {
        Self { ocr }
    }

    pub fn inspect_frame(
        &self,
        recording: &SessionRecording,
        events: &[TimelineEvent],
        selector: RecordingScreenSelector,
    ) -> Result<LocalRecordingInspection, RecordingInspectionUnavailable> {
        let requested = resolve_time(events, selector)?;
        extract_recording_frame(recording, requested)
            .map(LocalRecordingInspection::Frame)
            .map_err(RecordingInspectionUnavailable::Recording)
    }
}

impl<O: OcrEngine> RecordingInspector<O> {
    pub fn inspect(
        &self,
        recording: &SessionRecording,
        events: &[TimelineEvent],
        selector: RecordingScreenSelector,
        evidence: LocalRecordingEvidence,
    ) -> Result<LocalRecordingInspection, RecordingInspectionUnavailable> {
        let LocalRecordingInspection::Frame(decoded) =
            self.inspect_frame(recording, events, selector)?
        else {
            unreachable!("frame-only inspection returns a frame")
        };
        match evidence {
            LocalRecordingEvidence::Frame => Ok(LocalRecordingInspection::Frame(decoded)),
            LocalRecordingEvidence::Ocr => {
                let Some(ocr) = self.ocr.as_ref() else {
                    return Err(RecordingInspectionUnavailable::OcrUnavailable);
                };
                let frame = decoded.decode_for_local_use().map_err(|error| {
                    RecordingInspectionUnavailable::OcrFailed(error.to_string())
                })?;
                let text = ocr.recognize(&frame).map_err(|error| {
                    RecordingInspectionUnavailable::OcrFailed(error.to_string())
                })?;
                Ok(LocalRecordingInspection::Ocr {
                    frame: decoded,
                    text,
                })
            }
        }
    }
}

fn resolve_time(
    events: &[TimelineEvent],
    selector: RecordingScreenSelector,
) -> Result<Duration, RecordingInspectionUnavailable> {
    match selector {
        RecordingScreenSelector::Timestamp(timestamp) => Ok(timestamp),
        RecordingScreenSelector::Event(id) => {
            let event = events
                .iter()
                .find(|event| event.id() == id)
                .ok_or(RecordingInspectionUnavailable::EventNotFound)?;
            match event.payload() {
                EventPayload::UtteranceFinal(utterance)
                | EventPayload::UtterancePartial(utterance) => Ok(utterance.start),
                EventPayload::ScreenSnapshot(snapshot) => Ok(snapshot.visible_from),
                _ => Err(RecordingInspectionUnavailable::EventHasNoMediaTimestamp),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };

    use screen::{Frame, ScreenError};
    use sotto_core::{
        CaptureTarget, EventPayload, Session, SessionId, Source, TargetKind, TimelineBuilder,
        Utterance, types::RecordingMissingReason,
    };

    use super::*;

    struct CountingOcr(Arc<AtomicUsize>);

    impl OcrEngine for CountingOcr {
        fn recognize(&self, _frame: &Frame) -> Result<String, ScreenError> {
            self.0.fetch_add(1, Ordering::Relaxed);
            Ok("Project Atlas".to_owned())
        }
    }

    fn timeline() -> (TimelineBuilder, EventId) {
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
                text: "Look at this".to_owned(),
                avg_logprob: -0.1,
                annotations: vec![],
            }),
        );
        (timeline, event.id())
    }

    #[test]
    fn default_construction_and_selector_resolution_do_not_run_ocr() {
        let calls = Arc::new(AtomicUsize::new(0));
        let inspector = RecordingInspector::new(Some(CountingOcr(Arc::clone(&calls))));
        let (timeline, event_id) = timeline();
        assert_eq!(
            resolve_time(timeline.events(), RecordingScreenSelector::Event(event_id)),
            Ok(Duration::from_secs(8))
        );
        assert_eq!(calls.load(Ordering::Relaxed), 0);
        drop(inspector);
        assert_eq!(calls.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn deleted_recording_stays_explicit_and_does_not_run_ocr() {
        let calls = Arc::new(AtomicUsize::new(0));
        let inspector = RecordingInspector::new(Some(CountingOcr(Arc::clone(&calls))));
        let (timeline, event_id) = timeline();
        let result = inspector.inspect(
            &SessionRecording::Missing {
                session_id: SessionId::new(59),
                reason: RecordingMissingReason::Deleted,
            },
            timeline.events(),
            RecordingScreenSelector::Event(event_id),
            LocalRecordingEvidence::Ocr,
        );
        assert!(matches!(
            result,
            Err(RecordingInspectionUnavailable::Recording(
                RecordingFrameUnavailable {
                    reason: screen::RecordingFrameUnavailableReason::Deleted,
                    ..
                }
            ))
        ));
        assert_eq!(calls.load(Ordering::Relaxed), 0);
    }
}
