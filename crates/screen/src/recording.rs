//! Explicit, single-frame decoding from the retained session recording.

use std::{path::Path, time::Duration};

use sotto_core::{
    EventPayload, SessionId, TimelineEvent,
    types::{RecordingMissingReason, SessionRecording},
};
use thiserror::Error;

use crate::{
    Frame, OcrEngine, ScreenError,
    inspection::{
        ImageInspectionPolicy, InspectScreenRequest, RecordingScreenProvenance, ScreenEvidence,
        ScreenInspection, ScreenInspectionSource, ScreenPrecision, ScreenSelector,
        ScreenUnavailableReason,
    },
};

const ERROR_CAPACITY: usize = 2_048;

/// Honest media provenance for one explicitly decoded video frame.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RecordingFrameProvenance {
    pub requested_media_time: Duration,
    pub decoded_media_time: Duration,
    pub recording_session_id: SessionId,
}

impl RecordingFrameProvenance {
    /// Signed difference between the decoded presentation time and requested media time.
    #[must_use]
    pub fn decode_offset_ns(&self) -> i128 {
        duration_ns(self.decoded_media_time) - duration_ns(self.requested_media_time)
    }
}

/// PNG bytes decoded from the recording only after an explicit request.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DecodedRecordingFrame {
    pub png: Vec<u8>,
    pub provenance: RecordingFrameProvenance,
}

impl DecodedRecordingFrame {
    /// Decodes the local PNG into BGRA only for an explicit local consumer such as Vision OCR.
    pub fn decode_for_local_use(&self) -> Result<Frame, ScreenError> {
        let decoder = png::Decoder::new(std::io::Cursor::new(&self.png));
        let mut reader = decoder
            .read_info()
            .map_err(|error| ScreenError::PngDecode(error.to_string()))?;
        let size = reader.output_buffer_size().ok_or_else(|| {
            ScreenError::PngDecode("decoded recording frame exceeds PNG limits".to_owned())
        })?;
        let mut rgba = vec![0; size];
        let info = reader
            .next_frame(&mut rgba)
            .map_err(|error| ScreenError::PngDecode(error.to_string()))?;
        if info.color_type != png::ColorType::Rgba || info.bit_depth != png::BitDepth::Eight {
            return Err(ScreenError::PngDecode(
                "decoded recording frame is not 8-bit RGBA".to_owned(),
            ));
        }
        rgba.truncate(info.buffer_size());
        for pixel in rgba.chunks_exact_mut(4) {
            pixel.swap(0, 2);
        }
        let stride = info
            .width
            .checked_mul(4)
            .ok_or_else(|| ScreenError::InvalidFrame("decoded width overflow".to_owned()))?;
        Ok(Frame {
            bgra: rgba,
            width: info.width,
            height: info.height,
            stride,
            captured_at: self.provenance.decoded_media_time,
        })
    }
}

/// Missing recording state is data, not an opaque decoder failure.
#[derive(Clone, Debug, Eq, Error, PartialEq)]
#[error("screen frame unavailable at {requested_media_time:?}: {reason}")]
pub struct RecordingFrameUnavailable {
    pub requested_media_time: Duration,
    pub recording_session_id: SessionId,
    pub reason: RecordingFrameUnavailableReason,
}

#[derive(Clone, Debug, Eq, Error, PartialEq)]
pub enum RecordingFrameUnavailableReason {
    #[error("recording was deleted")]
    Deleted,
    #[error("recording was pruned")]
    Pruned,
    #[error("recording file is missing")]
    MissingFile,
    #[error("recording time mapping is invalid")]
    InvalidTimeMapping,
    #[error("decode failed: {0}")]
    DecodeFailed(String),
    #[error("recording frame extraction is unsupported on this platform")]
    UnsupportedPlatform,
}

/// Narrow decoder seam used by tests without pretending synthetic PNGs are real-media evidence.
pub trait RecordingFrameDecoder: Send + Sync {
    fn decode_png(
        &self,
        path: &Path,
        requested_media_time: Duration,
    ) -> Result<(Vec<u8>, Duration), ScreenError>;
}

#[derive(Clone, Copy, Debug, Default)]
pub struct PlatformFrameDecoder;

impl RecordingFrameDecoder for PlatformFrameDecoder {
    fn decode_png(
        &self,
        path: &Path,
        requested_media_time: Duration,
    ) -> Result<(Vec<u8>, Duration), ScreenError> {
        platform::decode_png(path, requested_media_time)
    }
}

/// Recording-backed implementation of the existing reasoning inspection seam.
pub struct RecordingBackedScreenInspector<D, O> {
    recording: SessionRecording,
    decoder: D,
    ocr: Option<O>,
    image_policy: ImageInspectionPolicy,
}

impl<O> RecordingBackedScreenInspector<PlatformFrameDecoder, O> {
    #[must_use]
    pub fn new(
        recording: SessionRecording,
        ocr: Option<O>,
        image_policy: ImageInspectionPolicy,
    ) -> Self {
        Self::with_decoder(recording, PlatformFrameDecoder, ocr, image_policy)
    }
}

impl<D, O> RecordingBackedScreenInspector<D, O> {
    #[must_use]
    pub const fn with_decoder(
        recording: SessionRecording,
        decoder: D,
        ocr: Option<O>,
        image_policy: ImageInspectionPolicy,
    ) -> Self {
        Self {
            recording,
            decoder,
            ocr,
            image_policy,
        }
    }
}

impl<D: RecordingFrameDecoder, O: OcrEngine> ScreenInspectionSource
    for RecordingBackedScreenInspector<D, O>
{
    fn inspect(
        &self,
        events: &[TimelineEvent],
        request: &InspectScreenRequest,
    ) -> ScreenInspection {
        if request.evidence == ScreenEvidence::Image
            && self.image_policy != ImageInspectionPolicy::Allow
        {
            return unavailable(request, ScreenUnavailableReason::ImageOptInRequired);
        }
        let requested_session_time = match resolve_selector_time(events, &request.selector) {
            Ok(value) => value,
            Err(reason) => return unavailable(request, reason),
        };
        let decoded = match extract_recording_frame_with(
            &self.decoder,
            &self.recording,
            requested_session_time,
        ) {
            Ok(value) => value,
            Err(unavailable_frame) => {
                return unavailable(request, map_unavailable(unavailable_frame.reason));
            }
        };
        let provenance = RecordingScreenProvenance {
            requested: request.selector.clone(),
            requested_media_time: decoded.provenance.requested_media_time,
            decoded_media_time: decoded.provenance.decoded_media_time,
            recording_session_id: decoded.provenance.recording_session_id,
            precision: ScreenPrecision::DecodedVideoFrame,
        };
        match request.evidence {
            ScreenEvidence::Metadata => ScreenInspection::RecordingAvailable {
                provenance,
                ocr_text: None,
            },
            ScreenEvidence::LocalOcr => {
                let Some(ocr) = self.ocr.as_ref() else {
                    return unavailable(request, ScreenUnavailableReason::OcrUnavailable);
                };
                match decoded
                    .decode_for_local_use()
                    .and_then(|frame| ocr.recognize(&frame))
                {
                    Ok(text) => ScreenInspection::RecordingAvailable {
                        provenance,
                        ocr_text: Some(text),
                    },
                    Err(error) => unavailable(
                        request,
                        ScreenUnavailableReason::OcrFailed(error.to_string()),
                    ),
                }
            }
            // Existing authorization provenance is snapshot-shaped. Even after user opt-in, do
            // not mint transport authority by inventing a snapshot id or visibility interval.
            ScreenEvidence::Image => unavailable(
                request,
                ScreenUnavailableReason::RecordingImageAuthorizationUnavailable,
            ),
        }
    }
}

fn resolve_selector_time(
    events: &[TimelineEvent],
    selector: &ScreenSelector,
) -> Result<Duration, ScreenUnavailableReason> {
    match selector {
        ScreenSelector::Timestamp(value) => Ok(*value),
        ScreenSelector::Event(id) => {
            let event = events
                .iter()
                .find(|event| event.id() == *id)
                .ok_or(ScreenUnavailableReason::EventNotFound)?;
            match event.payload() {
                EventPayload::UtteranceFinal(utterance)
                | EventPayload::UtterancePartial(utterance) => Ok(utterance.start),
                EventPayload::ScreenSnapshot(snapshot) => Ok(snapshot.visible_from),
                _ => Err(ScreenUnavailableReason::EventIsNotSnapshot),
            }
        }
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

fn map_unavailable(reason: RecordingFrameUnavailableReason) -> ScreenUnavailableReason {
    match reason {
        RecordingFrameUnavailableReason::Deleted => ScreenUnavailableReason::RecordingDeleted,
        RecordingFrameUnavailableReason::Pruned => ScreenUnavailableReason::RecordingPruned,
        RecordingFrameUnavailableReason::MissingFile => ScreenUnavailableReason::RecordingMissing,
        RecordingFrameUnavailableReason::InvalidTimeMapping => {
            ScreenUnavailableReason::InvalidTimeMapping
        }
        RecordingFrameUnavailableReason::DecodeFailed(error) => {
            ScreenUnavailableReason::RecordingDecodeFailed(error)
        }
        RecordingFrameUnavailableReason::UnsupportedPlatform => {
            ScreenUnavailableReason::RecordingDecodeFailed(
                "recording extraction is unsupported on this platform".to_owned(),
            )
        }
    }
}

/// Resolves session time through the durable mapping and decodes exactly one frame.
pub fn extract_recording_frame(
    recording: &SessionRecording,
    requested_session_time: Duration,
) -> Result<DecodedRecordingFrame, RecordingFrameUnavailable> {
    extract_recording_frame_with(&PlatformFrameDecoder, recording, requested_session_time)
}

pub(crate) fn extract_recording_frame_with(
    decoder: &dyn RecordingFrameDecoder,
    recording: &SessionRecording,
    requested_session_time: Duration,
) -> Result<DecodedRecordingFrame, RecordingFrameUnavailable> {
    let session_id = recording.session_id();
    let SessionRecording::Available {
        path, time_mapping, ..
    } = recording
    else {
        let SessionRecording::Missing { reason, .. } = recording else {
            unreachable!("session recording variants are exhaustive")
        };
        return Err(RecordingFrameUnavailable {
            requested_media_time: requested_session_time,
            recording_session_id: session_id,
            reason: match reason {
                RecordingMissingReason::Deleted => RecordingFrameUnavailableReason::Deleted,
                RecordingMissingReason::Pruned => RecordingFrameUnavailableReason::Pruned,
            },
        });
    };
    let Some(requested_media_time) = time_mapping.media_time(requested_session_time) else {
        return Err(RecordingFrameUnavailable {
            requested_media_time: requested_session_time,
            recording_session_id: session_id,
            reason: RecordingFrameUnavailableReason::InvalidTimeMapping,
        });
    };
    if !Path::new(path).is_file() {
        return Err(RecordingFrameUnavailable {
            requested_media_time,
            recording_session_id: session_id,
            reason: RecordingFrameUnavailableReason::MissingFile,
        });
    }
    decoder
        .decode_png(Path::new(path), requested_media_time)
        .map(|(png, decoded_media_time)| DecodedRecordingFrame {
            png,
            provenance: RecordingFrameProvenance {
                requested_media_time,
                decoded_media_time,
                recording_session_id: session_id,
            },
        })
        .map_err(|error| RecordingFrameUnavailable {
            requested_media_time,
            recording_session_id: session_id,
            reason: match error {
                ScreenError::UnsupportedPlatform => {
                    RecordingFrameUnavailableReason::UnsupportedPlatform
                }
                other => RecordingFrameUnavailableReason::DecodeFailed(other.to_string()),
            },
        })
}

fn duration_ns(value: Duration) -> i128 {
    i128::try_from(value.as_nanos()).unwrap_or(i128::MAX)
}

#[cfg(target_os = "macos")]
mod platform {
    use std::{ffi::CString, path::Path, time::Duration};

    use crate::ScreenError;

    use super::ERROR_CAPACITY;

    unsafe extern "C" {
        fn sotto_screen_decode_frame_png(
            path: *const std::ffi::c_char,
            requested_ns: u64,
            output: *mut u8,
            output_capacity: usize,
            output_length: *mut usize,
            actual_ns: *mut u64,
            error: *mut std::ffi::c_char,
            error_capacity: usize,
        ) -> bool;
    }

    pub(super) fn decode_png(
        path: &Path,
        requested: Duration,
    ) -> Result<(Vec<u8>, Duration), ScreenError> {
        let path = CString::new(path.to_string_lossy().as_bytes())
            .map_err(|_| ScreenError::RecordingDecode("recording path contains NUL".to_owned()))?;
        let requested_ns = u64::try_from(requested.as_nanos()).map_err(|_| {
            ScreenError::RecordingDecode("requested timestamp exceeds media range".to_owned())
        })?;
        let mut length = 0_usize;
        let mut actual_ns = 0_u64;
        let mut error = vec![0_i8; ERROR_CAPACITY];
        // SAFETY: the bridge only reads the path and writes within the supplied scalar/error
        // buffers. A null output asks for the exact PNG byte count.
        let measured = unsafe {
            sotto_screen_decode_frame_png(
                path.as_ptr(),
                requested_ns,
                std::ptr::null_mut(),
                0,
                &mut length,
                &mut actual_ns,
                error.as_mut_ptr(),
                error.len(),
            )
        };
        if !measured || length == 0 {
            return Err(ScreenError::RecordingDecode(error_message(&error)));
        }
        let mut png = vec![0_u8; length];
        // SAFETY: `png` is allocated to the exact length reported by the first bridge call.
        let decoded = unsafe {
            sotto_screen_decode_frame_png(
                path.as_ptr(),
                requested_ns,
                png.as_mut_ptr(),
                png.len(),
                &mut length,
                &mut actual_ns,
                error.as_mut_ptr(),
                error.len(),
            )
        };
        if !decoded || length > png.len() {
            return Err(ScreenError::RecordingDecode(error_message(&error)));
        }
        png.truncate(length);
        Ok((png, Duration::from_nanos(actual_ns)))
    }

    fn error_message(buffer: &[std::ffi::c_char]) -> String {
        let bytes = buffer
            .iter()
            .take_while(|byte| **byte != 0)
            .map(|byte| (*byte).cast_unsigned())
            .collect::<Vec<_>>();
        String::from_utf8_lossy(&bytes).into_owned()
    }
}

#[cfg(not(target_os = "macos"))]
mod platform {
    use std::{path::Path, time::Duration};

    use crate::ScreenError;

    pub(super) fn decode_png(
        _path: &Path,
        _requested: Duration,
    ) -> Result<(Vec<u8>, Duration), ScreenError> {
        Err(ScreenError::UnsupportedPlatform)
    }
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        path::Path,
        sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        },
        time::Duration,
    };

    use sotto_core::{
        SessionId,
        types::{MediaTimeMapping, RecordingContainer, RecordingMissingReason, SessionRecording},
    };

    use super::*;

    struct FixtureDecoder;

    impl RecordingFrameDecoder for FixtureDecoder {
        fn decode_png(
            &self,
            _path: &Path,
            requested: Duration,
        ) -> Result<(Vec<u8>, Duration), ScreenError> {
            Ok((
                vec![0x89, b'P', b'N', b'G'],
                requested + Duration::from_millis(17),
            ))
        }
    }

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
                writer.write_image_data(&[42, 0, 0, 255])?;
                writer.finish()?;
            }
            Ok((bytes, requested + Duration::from_millis(17)))
        }
    }

    struct CountingOcr(Arc<AtomicUsize>);

    impl OcrEngine for CountingOcr {
        fn recognize(&self, frame: &Frame) -> Result<String, ScreenError> {
            self.0.fetch_add(1, Ordering::Relaxed);
            Ok(format!("red={}", frame.bgra[2]))
        }
    }

    static NEXT_RECORDING: AtomicUsize = AtomicUsize::new(0);

    fn temporary_recording() -> Result<std::path::PathBuf, std::io::Error> {
        let path = std::env::temp_dir().join(format!(
            "sotto-screen-recording-contract-{}-{}.mp4",
            std::process::id(),
            NEXT_RECORDING.fetch_add(1, Ordering::Relaxed),
        ));
        fs::write(&path, b"contract-only; not real media")?;
        Ok(path)
    }

    fn available_recording(path: &Path) -> SessionRecording {
        SessionRecording::Available {
            session_id: SessionId::new(59),
            path: path.to_string_lossy().into_owned(),
            container: RecordingContainer::Mp4,
            duration: Duration::from_secs(60),
            byte_size: 24,
            time_mapping: MediaTimeMapping::IDENTITY,
        }
    }

    #[test]
    fn maps_session_time_and_reports_actual_decode_time() -> Result<(), Box<dyn std::error::Error>>
    {
        let path = temporary_recording()?;
        let recording = SessionRecording::Available {
            session_id: SessionId::new(59),
            path: path.to_string_lossy().into_owned(),
            container: RecordingContainer::Mp4,
            duration: Duration::from_secs(60),
            byte_size: 24,
            time_mapping: MediaTimeMapping {
                session_origin_ns: 1_000_000_000,
                media_origin_ns: 3_000_000_000,
                rate_numerator: 1,
                rate_denominator: 1,
            },
        };
        let decoded =
            extract_recording_frame_with(&FixtureDecoder, &recording, Duration::from_secs(11))?;
        assert_eq!(
            decoded.provenance.requested_media_time,
            Duration::from_secs(13)
        );
        assert_eq!(
            decoded.provenance.decoded_media_time,
            Duration::from_millis(13_017)
        );
        assert_eq!(decoded.provenance.decode_offset_ns(), 17_000_000);
        fs::remove_file(path)?;
        Ok(())
    }

    #[test]
    fn pruned_and_deleted_recordings_are_explicit_missing_states()
    -> Result<(), Box<dyn std::error::Error>> {
        for (reason, expected) in [
            (
                RecordingMissingReason::Pruned,
                RecordingFrameUnavailableReason::Pruned,
            ),
            (
                RecordingMissingReason::Deleted,
                RecordingFrameUnavailableReason::Deleted,
            ),
        ] {
            let recording = SessionRecording::Missing {
                session_id: SessionId::new(59),
                reason,
            };
            let unavailable =
                extract_recording_frame_with(&FixtureDecoder, &recording, Duration::from_secs(4))
                    .err()
                    .ok_or("missing media unexpectedly invoked the decoder")?;
            assert_eq!(unavailable.reason, expected);
        }
        Ok(())
    }

    #[test]
    fn reasoning_source_decodes_on_request_and_runs_ocr_only_when_explicit()
    -> Result<(), Box<dyn std::error::Error>> {
        let path = temporary_recording()?;
        let decode_calls = Arc::new(AtomicUsize::new(0));
        let ocr_calls = Arc::new(AtomicUsize::new(0));
        let inspector = RecordingBackedScreenInspector::with_decoder(
            available_recording(&path),
            PngDecoder(Arc::clone(&decode_calls)),
            Some(CountingOcr(Arc::clone(&ocr_calls))),
            ImageInspectionPolicy::Deny,
        );
        assert_eq!(decode_calls.load(Ordering::Relaxed), 0);
        assert_eq!(ocr_calls.load(Ordering::Relaxed), 0);

        let metadata_request = InspectScreenRequest {
            selector: ScreenSelector::Timestamp(Duration::from_secs(8)),
            evidence: ScreenEvidence::Metadata,
            reason: "inspect the cited moment".to_owned(),
        };
        let metadata = inspector.inspect(&[], &metadata_request);
        assert!(matches!(
            metadata,
            ScreenInspection::RecordingAvailable {
                provenance: RecordingScreenProvenance {
                    requested_media_time,
                    decoded_media_time,
                    recording_session_id,
                    precision: ScreenPrecision::DecodedVideoFrame,
                    ..
                },
                ocr_text: None,
            } if requested_media_time == Duration::from_secs(8)
                && decoded_media_time == Duration::from_millis(8_017)
                && recording_session_id == SessionId::new(59)
        ));
        assert_eq!(decode_calls.load(Ordering::Relaxed), 1);
        assert_eq!(ocr_calls.load(Ordering::Relaxed), 0);

        let ocr = inspector.inspect(
            &[],
            &InspectScreenRequest {
                evidence: ScreenEvidence::LocalOcr,
                ..metadata_request
            },
        );
        assert!(matches!(
            ocr,
            ScreenInspection::RecordingAvailable {
                ocr_text: Some(text),
                ..
            } if text == "red=42"
        ));
        assert_eq!(decode_calls.load(Ordering::Relaxed), 2);
        assert_eq!(ocr_calls.load(Ordering::Relaxed), 1);

        let denied_image = inspector.inspect(
            &[],
            &InspectScreenRequest {
                selector: ScreenSelector::Timestamp(Duration::from_secs(8)),
                evidence: ScreenEvidence::Image,
                reason: "attach the cited screen".to_owned(),
            },
        );
        assert!(matches!(
            denied_image,
            ScreenInspection::Unavailable {
                reason: ScreenUnavailableReason::ImageOptInRequired,
                ..
            }
        ));
        assert_eq!(decode_calls.load(Ordering::Relaxed), 2);

        let allowed_without_fabricated_authority = RecordingBackedScreenInspector::with_decoder(
            available_recording(&path),
            PngDecoder(Arc::clone(&decode_calls)),
            Some(CountingOcr(Arc::clone(&ocr_calls))),
            ImageInspectionPolicy::Allow,
        )
        .inspect(
            &[],
            &InspectScreenRequest {
                selector: ScreenSelector::Timestamp(Duration::from_secs(8)),
                evidence: ScreenEvidence::Image,
                reason: "attach the cited screen".to_owned(),
            },
        );
        assert!(matches!(
            allowed_without_fabricated_authority,
            ScreenInspection::Unavailable {
                reason: ScreenUnavailableReason::RecordingImageAuthorizationUnavailable,
                ..
            }
        ));
        assert_eq!(decode_calls.load(Ordering::Relaxed), 3);
        assert_eq!(ocr_calls.load(Ordering::Relaxed), 1);
        fs::remove_file(path)?;
        Ok(())
    }
}
