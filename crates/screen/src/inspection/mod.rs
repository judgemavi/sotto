//! Read-only, timestamp-addressed access to retained change frames.

use std::{fs, io::BufReader, path::PathBuf, time::Duration};

use sotto_core::{EventId, EventPayload, FrameRef, SessionId, TimelineEvent};

use crate::{Frame, OcrEngine, ScreenError};

mod authorized;
use authorized::authorize_retained_image;
pub use authorized::{
    AuthorizedImageMediaType, AuthorizedImageProvenance, AuthorizedReasoningImage,
    MAX_AUTHORIZED_IMAGE_BYTES,
};

/// The timeline coordinate a reasoning pass wants to inspect.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ScreenSelector {
    Timestamp(Duration),
    Event(EventId),
}

/// Evidence explicitly requested by a reasoning pass.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ScreenEvidence {
    Metadata,
    LocalOcr,
    Image,
}

/// Typed `inspect_screen` action emitted by a first reasoning pass.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InspectScreenRequest {
    pub selector: ScreenSelector,
    pub evidence: ScreenEvidence,
    pub reason: String,
}

/// A retained frame is always a sampled change frame, never an exact video frame.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ScreenPrecision {
    SampledChangeFrame,
    /// A video frame decoded on demand. `captured_at` is its actual presentation timestamp.
    DecodedVideoFrame,
}

/// Provenance carried with every available inspection result.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ScreenProvenance {
    pub requested: ScreenSelector,
    pub captured_at: Duration,
    pub visible_from: Duration,
    pub visible_to: Option<Duration>,
    pub snapshot_event_id: EventId,
    pub frame_ref: FrameRef,
    pub precision: ScreenPrecision,
}

/// Recording-native provenance without a fabricated snapshot event or visibility interval.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RecordingScreenProvenance {
    pub requested: ScreenSelector,
    pub requested_media_time: Duration,
    pub decoded_media_time: Duration,
    pub recording_session_id: SessionId,
    pub precision: ScreenPrecision,
}

/// Why requested screen evidence could not be produced.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ScreenUnavailableReason {
    NoSnapshots,
    OutOfRange,
    NoSnapshotForTimestamp,
    EventNotFound,
    EventIsNotSnapshot,
    FramePruned,
    FrameOutsideCache,
    OcrUnavailable,
    OcrFailed(String),
    ImageOptInRequired,
    ImageAuthorizationMismatch,
    ImageUnreadable,
    ImageTooLarge,
    InvalidImageMedia,
    FrameSubstituted,
    InspectorUnavailable,
    RecordingDeleted,
    RecordingPruned,
    RecordingMissing,
    InvalidTimeMapping,
    RecordingDecodeFailed(String),
    RecordingImageAuthorizationUnavailable,
}

/// Derived screen evidence. It never amends the append-only timeline.
#[derive(Debug, Eq, PartialEq)]
pub enum ScreenInspection {
    Available {
        provenance: ScreenProvenance,
        ocr_text: Option<String>,
        authorized_image: Option<AuthorizedReasoningImage>,
    },
    RecordingAvailable {
        provenance: RecordingScreenProvenance,
        ocr_text: Option<String>,
    },
    Unavailable {
        requested: ScreenSelector,
        reason: ScreenUnavailableReason,
    },
}

impl ScreenInspection {
    /// Returns an opaque image only when the final dispatch request still matches the
    /// policy-authorized inspection and its path-free provenance exactly.
    #[must_use]
    pub fn take_authorized_image_for(
        &mut self,
        request: &InspectScreenRequest,
    ) -> Option<AuthorizedReasoningImage> {
        if request.evidence != ScreenEvidence::Image {
            return None;
        }
        let Self::Available {
            provenance,
            authorized_image,
            ..
        } = self
        else {
            return None;
        };
        let image = authorized_image.as_ref()?;
        let image_provenance = image.provenance();
        let matches = request.selector == provenance.requested
            && image_provenance.requested() == &request.selector
            && image_provenance.captured_at() == provenance.captured_at
            && image_provenance.visible_from() == provenance.visible_from
            && image_provenance.visible_to() == provenance.visible_to
            && image_provenance.snapshot_event_id() == provenance.snapshot_event_id;
        matches.then(|| authorized_image.take()).flatten()
    }
}

/// Read-only boundary consumed by reasoning code.
pub trait ScreenInspectionSource: Send + Sync {
    fn inspect(&self, events: &[TimelineEvent], request: &InspectScreenRequest)
    -> ScreenInspection;
}

/// User-owned policy for allowing a retained frame to proceed toward an image-capable backend.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ImageInspectionPolicy {
    Deny,
    Allow,
}

/// Resolves retained frames under one cache root and performs OCR only when asked.
pub struct RetainedScreenInspector<O> {
    cache_root: PathBuf,
    ocr: Option<O>,
    image_policy: ImageInspectionPolicy,
}

impl<O> RetainedScreenInspector<O> {
    #[must_use]
    pub fn new(
        cache_root: impl Into<PathBuf>,
        ocr: Option<O>,
        image_policy: ImageInspectionPolicy,
    ) -> Self {
        Self {
            cache_root: cache_root.into(),
            ocr,
            image_policy,
        }
    }
}

impl<O: OcrEngine> ScreenInspectionSource for RetainedScreenInspector<O> {
    fn inspect(
        &self,
        events: &[TimelineEvent],
        request: &InspectScreenRequest,
    ) -> ScreenInspection {
        let Some((event, snapshot)) = resolve(events, &request.selector) else {
            return unavailable_for_resolution(events, &request.selector);
        };
        let path = std::path::Path::new(snapshot.frame_ref.as_str());
        if !path.is_file() {
            return unavailable(request, ScreenUnavailableReason::FramePruned);
        }
        if !is_within_cache(path, &self.cache_root) {
            return unavailable(request, ScreenUnavailableReason::FrameOutsideCache);
        }
        if request.evidence == ScreenEvidence::Image
            && self.image_policy != ImageInspectionPolicy::Allow
        {
            return unavailable(request, ScreenUnavailableReason::ImageOptInRequired);
        }

        let provenance = ScreenProvenance {
            requested: request.selector.clone(),
            captured_at: snapshot.visible_from,
            visible_from: snapshot.visible_from,
            visible_to: snapshot.visible_to,
            snapshot_event_id: event.id(),
            frame_ref: snapshot.frame_ref.clone(),
            precision: ScreenPrecision::SampledChangeFrame,
        };
        match request.evidence {
            ScreenEvidence::Metadata => ScreenInspection::Available {
                provenance,
                ocr_text: None,
                authorized_image: None,
            },
            ScreenEvidence::Image => match authorize_retained_image(
                request,
                &provenance,
                event.id(),
                snapshot,
                &self.cache_root,
                self.image_policy,
            ) {
                Ok(authorized_image) => ScreenInspection::Available {
                    provenance,
                    ocr_text: None,
                    authorized_image: Some(authorized_image),
                },
                Err(reason) => unavailable(request, reason),
            },
            ScreenEvidence::LocalOcr => {
                let Some(ocr) = self.ocr.as_ref() else {
                    return unavailable(request, ScreenUnavailableReason::OcrUnavailable);
                };
                match load_retained_frame(path, snapshot.visible_from)
                    .and_then(|frame| ocr.recognize(&frame))
                {
                    Ok(text) => ScreenInspection::Available {
                        provenance,
                        ocr_text: Some(text),
                        authorized_image: None,
                    },
                    Err(error) => unavailable(
                        request,
                        ScreenUnavailableReason::OcrFailed(error.to_string()),
                    ),
                }
            }
        }
    }
}

fn resolve<'a>(
    events: &'a [TimelineEvent],
    selector: &ScreenSelector,
) -> Option<(&'a TimelineEvent, &'a sotto_core::ScreenSnapshot)> {
    match selector {
        ScreenSelector::Event(id) => events.iter().find_map(|event| {
            (event.id() == *id)
                .then(|| match event.payload() {
                    EventPayload::ScreenSnapshot(snapshot) => Some((event, snapshot)),
                    _ => None,
                })
                .flatten()
        }),
        ScreenSelector::Timestamp(timestamp) => events.iter().find_map(|event| {
            let EventPayload::ScreenSnapshot(snapshot) = event.payload() else {
                return None;
            };
            let contains = snapshot.visible_from <= *timestamp
                && snapshot.visible_to.is_none_or(|end| *timestamp < end);
            contains.then_some((event, snapshot))
        }),
    }
}

fn unavailable_for_resolution(
    events: &[TimelineEvent],
    selector: &ScreenSelector,
) -> ScreenInspection {
    let snapshots: Vec<_> = events
        .iter()
        .filter_map(|event| match event.payload() {
            EventPayload::ScreenSnapshot(snapshot) => Some((event, snapshot)),
            _ => None,
        })
        .collect();
    let reason = match selector {
        ScreenSelector::Event(id) => match events.iter().find(|event| event.id() == *id) {
            Some(_) => ScreenUnavailableReason::EventIsNotSnapshot,
            None => ScreenUnavailableReason::EventNotFound,
        },
        ScreenSelector::Timestamp(timestamp) if snapshots.is_empty() => {
            let _ = timestamp;
            ScreenUnavailableReason::NoSnapshots
        }
        ScreenSelector::Timestamp(timestamp) => {
            let first = snapshots
                .iter()
                .map(|(_, snapshot)| snapshot.visible_from)
                .min()
                .unwrap_or_default();
            let last = snapshots
                .iter()
                .filter_map(|(_, snapshot)| snapshot.visible_to)
                .max();
            if *timestamp < first || last.is_some_and(|end| *timestamp >= end) {
                ScreenUnavailableReason::OutOfRange
            } else {
                ScreenUnavailableReason::NoSnapshotForTimestamp
            }
        }
    };
    ScreenInspection::Unavailable {
        requested: selector.clone(),
        reason,
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

fn is_within_cache(path: &std::path::Path, cache_root: &std::path::Path) -> bool {
    path.canonicalize()
        .ok()
        .zip(cache_root.canonicalize().ok())
        .is_some_and(|(path, root)| path.starts_with(root))
}

fn load_retained_frame(
    path: &std::path::Path,
    captured_at: Duration,
) -> Result<Frame, ScreenError> {
    let decoder = png::Decoder::new(BufReader::new(fs::File::open(path)?));
    let mut reader = decoder
        .read_info()
        .map_err(|error| ScreenError::PngDecode(error.to_string()))?;
    let size = reader
        .output_buffer_size()
        .ok_or_else(|| ScreenError::PngDecode("decoded frame exceeds PNG limits".to_owned()))?;
    let mut rgba = vec![0; size];
    let info = reader
        .next_frame(&mut rgba)
        .map_err(|error| ScreenError::PngDecode(error.to_string()))?;
    if info.color_type != png::ColorType::Rgba || info.bit_depth != png::BitDepth::Eight {
        return Err(ScreenError::PngDecode(
            "retained frame is not 8-bit RGBA".to_owned(),
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
        captured_at,
    })
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        io::BufWriter,
        path::Path,
        sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        },
    };

    use sotto_core::{
        CaptureTarget, EventPayload, FrameRef, ScreenSnapshot, Session, SessionId, TargetKind,
        TimelineBuilder,
    };

    use super::*;

    struct CountingOcr(Arc<AtomicUsize>);

    impl OcrEngine for CountingOcr {
        fn recognize(&self, frame: &Frame) -> Result<String, ScreenError> {
            self.0.fetch_add(1, Ordering::Relaxed);
            Ok(format!("red={}", frame.bgra[2]))
        }
    }

    fn timeline() -> TimelineBuilder {
        TimelineBuilder::new(Session::new(
            SessionId::new(28),
            CaptureTarget {
                bundle_id: Some("com.apple.Keynote".to_owned()),
                display_name: "Keynote".to_owned(),
                window_title: Some("Pricing".to_owned()),
                kind: TargetKind::Window,
                audio_scoped: true,
            },
            0,
        ))
    }

    fn directory(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("sotto-inspection-{name}-{}", std::process::id()))
    }

    fn write_png(directory: &Path, name: &str, red: u8) -> Result<PathBuf, ScreenError> {
        fs::create_dir_all(directory)?;
        let path = directory.join(name);
        let file = fs::File::create(&path)?;
        let mut encoder = png::Encoder::new(BufWriter::new(file), 2, 2);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder.write_header()?;
        writer.write_image_data(&[red, 0, 0, 255].repeat(4))?;
        writer.finish()?;
        Ok(path)
    }

    fn snapshot(path: &Path, from: u64, to: u64) -> EventPayload {
        EventPayload::ScreenSnapshot(ScreenSnapshot {
            frame_ref: FrameRef::new(path.to_string_lossy()),
            ocr_text: String::new(),
            active_app: Some("Keynote".to_owned()),
            window_title: Some("Pricing".to_owned()),
            visible_from: Duration::from_secs(from),
            visible_to: Some(Duration::from_secs(to)),
        })
    }

    #[test]
    fn timestamp_request_returns_sampled_interval_provenance_and_runs_ocr_once()
    -> Result<(), Box<dyn std::error::Error>> {
        let directory = directory("timestamp");
        let path = write_png(&directory, "frame.png", 42)?;
        let mut timeline = timeline();
        let event = timeline.append(Duration::from_secs(10), snapshot(&path, 10, 20));
        let calls = Arc::new(AtomicUsize::new(0));
        let inspector = RetainedScreenInspector::new(
            &directory,
            Some(CountingOcr(Arc::clone(&calls))),
            ImageInspectionPolicy::Deny,
        );
        let result = inspector.inspect(
            timeline.events(),
            &InspectScreenRequest {
                selector: ScreenSelector::Timestamp(Duration::from_secs(15)),
                evidence: ScreenEvidence::LocalOcr,
                reason: "read price".to_owned(),
            },
        );

        let ScreenInspection::Available {
            provenance,
            ocr_text,
            authorized_image,
        } = result
        else {
            return Err("expected retained screen evidence".into());
        };
        assert_eq!(
            provenance.snapshot_event_id,
            event.id(),
            "inspection must cite the resolved snapshot event"
        );
        assert_eq!(
            provenance.captured_at,
            Duration::from_secs(10),
            "inspection must expose the sampled capture time"
        );
        assert_eq!(
            provenance.visible_from,
            Duration::from_secs(10),
            "inspection must expose the interval start"
        );
        assert_eq!(
            provenance.visible_to,
            Some(Duration::from_secs(20)),
            "inspection must expose the interval end"
        );
        assert_eq!(
            provenance.precision,
            ScreenPrecision::SampledChangeFrame,
            "inspection must not claim exact-video precision"
        );
        assert_eq!(
            ocr_text.as_deref(),
            Some("red=42"),
            "explicit OCR must run against the retained frame"
        );
        assert!(
            authorized_image.is_none(),
            "local OCR must not authorize image evidence"
        );
        assert_eq!(
            calls.load(Ordering::Relaxed),
            1,
            "one local OCR request must invoke OCR exactly once"
        );

        let by_event = inspector.inspect(
            timeline.events(),
            &InspectScreenRequest {
                selector: ScreenSelector::Event(event.id()),
                evidence: ScreenEvidence::Metadata,
                reason: "inspect cited snapshot".to_owned(),
            },
        );
        assert!(
            matches!(
                by_event,
                ScreenInspection::Available {
                    provenance: ScreenProvenance {
                        snapshot_event_id,
                        precision: ScreenPrecision::SampledChangeFrame,
                        ..
                    },
                    ocr_text: None,
                    authorized_image: None,
                } if snapshot_event_id == event.id()
            ),
            "event-id metadata inspection must resolve the cited sampled frame"
        );
        assert_eq!(
            calls.load(Ordering::Relaxed),
            1,
            "metadata-only inspection must not run OCR"
        );
        fs::remove_dir_all(directory)?;
        Ok(())
    }

    #[test]
    fn gaps_out_of_range_and_pruned_frames_are_not_replaced_by_nearest_frame()
    -> Result<(), Box<dyn std::error::Error>> {
        let directory = directory("missing");
        let path = write_png(&directory, "frame.png", 10)?;
        let mut timeline = timeline();
        timeline.append(Duration::ZERO, snapshot(&path, 0, 5));
        timeline.append(Duration::from_secs(10), snapshot(&path, 10, 20));
        let inspector = RetainedScreenInspector::new(
            &directory,
            Some(CountingOcr(Arc::new(AtomicUsize::new(0)))),
            ImageInspectionPolicy::Deny,
        );
        let inspect_at = |seconds| {
            inspector.inspect(
                timeline.events(),
                &InspectScreenRequest {
                    selector: ScreenSelector::Timestamp(Duration::from_secs(seconds)),
                    evidence: ScreenEvidence::Metadata,
                    reason: "test absence".to_owned(),
                },
            )
        };

        assert!(
            matches!(
                inspect_at(7),
                ScreenInspection::Unavailable {
                    reason: ScreenUnavailableReason::NoSnapshotForTimestamp,
                    ..
                }
            ),
            "an interval gap must not select the nearest retained frame"
        );
        assert!(
            matches!(
                inspect_at(25),
                ScreenInspection::Unavailable {
                    reason: ScreenUnavailableReason::OutOfRange,
                    ..
                }
            ),
            "a timestamp after all closed intervals must be out of range"
        );
        fs::remove_file(&path)?;
        assert!(
            matches!(
                inspect_at(12),
                ScreenInspection::Unavailable {
                    reason: ScreenUnavailableReason::FramePruned,
                    ..
                }
            ),
            "a deleted retained frame must be reported as pruned"
        );
        fs::remove_dir_all(directory)?;
        Ok(())
    }

    #[test]
    fn image_request_requires_user_owned_opt_in() -> Result<(), Box<dyn std::error::Error>> {
        let directory = directory("image-opt-in");
        let path = write_png(&directory, "frame.png", 10)?;
        let mut timeline = timeline();
        timeline.append(Duration::ZERO, snapshot(&path, 0, 5));
        let inspector = RetainedScreenInspector::new(
            &directory,
            Some(CountingOcr(Arc::new(AtomicUsize::new(0)))),
            ImageInspectionPolicy::Deny,
        );
        let result = inspector.inspect(
            timeline.events(),
            &InspectScreenRequest {
                selector: ScreenSelector::Timestamp(Duration::from_secs(2)),
                evidence: ScreenEvidence::Image,
                reason: "inspect chart".to_owned(),
            },
        );
        assert!(
            matches!(
                result,
                ScreenInspection::Unavailable {
                    reason: ScreenUnavailableReason::ImageOptInRequired,
                    ..
                }
            ),
            "image evidence must remain unavailable without user opt-in"
        );
        fs::remove_dir_all(directory)?;
        Ok(())
    }

    #[test]
    fn authorized_image_is_minted_only_for_matching_opted_in_screen_evidence()
    -> Result<(), Box<dyn std::error::Error>> {
        let directory = directory("authorized-image");
        let path = write_png(&directory, "frame.png", 10)?;
        let frame_ref = FrameRef::new(path.to_string_lossy());
        let snapshot = ScreenSnapshot {
            frame_ref: frame_ref.clone(),
            ocr_text: String::new(),
            active_app: None,
            window_title: Some("Pricing".to_owned()),
            visible_from: Duration::ZERO,
            visible_to: Some(Duration::from_secs(5)),
        };
        let request = InspectScreenRequest {
            selector: ScreenSelector::Timestamp(Duration::from_secs(2)),
            evidence: ScreenEvidence::Image,
            reason: "inspect chart".to_owned(),
        };
        let provenance = ScreenProvenance {
            requested: request.selector.clone(),
            captured_at: Duration::ZERO,
            visible_from: Duration::ZERO,
            visible_to: Some(Duration::from_secs(5)),
            snapshot_event_id: EventId::new(4),
            frame_ref,
            precision: ScreenPrecision::SampledChangeFrame,
        };

        let authorized = authorize_retained_image(
            &request,
            &provenance,
            EventId::new(4),
            &snapshot,
            &directory,
            ImageInspectionPolicy::Allow,
        )
        .map_err(|reason| format!("authorized image unexpectedly unavailable: {reason:?}"))?;
        assert_eq!(
            authorized.media_type(),
            AuthorizedImageMediaType::Png,
            "media type must be established from retained bytes"
        );
        assert_eq!(
            authorized.provenance().snapshot_event_id(),
            EventId::new(4),
            "path-free authorization must retain the snapshot identity"
        );

        assert_eq!(
            authorize_retained_image(
                &request,
                &provenance,
                EventId::new(4),
                &snapshot,
                &directory,
                ImageInspectionPolicy::Deny,
            ),
            Err(ScreenUnavailableReason::ImageOptInRequired),
            "a caller cannot forge consent by supplying matching provenance"
        );

        let mut mismatched_selector = provenance.clone();
        mismatched_selector.requested = ScreenSelector::Timestamp(Duration::from_secs(3));
        assert_eq!(
            authorize_retained_image(
                &request,
                &mismatched_selector,
                EventId::new(4),
                &snapshot,
                &directory,
                ImageInspectionPolicy::Allow,
            ),
            Err(ScreenUnavailableReason::ImageAuthorizationMismatch),
            "the authorized image must cite the exact requested selector"
        );

        assert_eq!(
            authorize_retained_image(
                &request,
                &provenance,
                EventId::new(99),
                &snapshot,
                &directory,
                ImageInspectionPolicy::Allow,
            ),
            Err(ScreenUnavailableReason::ImageAuthorizationMismatch),
            "the authorized image must cite the exact snapshot event"
        );

        let other_snapshot = ScreenSnapshot {
            frame_ref: FrameRef::new(directory.join("other.png").to_string_lossy()),
            ..snapshot
        };
        assert_eq!(
            authorize_retained_image(
                &request,
                &provenance,
                EventId::new(4),
                &other_snapshot,
                &directory,
                ImageInspectionPolicy::Allow,
            ),
            Err(ScreenUnavailableReason::ImageAuthorizationMismatch),
            "the authorized image must match the frame recorded in provenance"
        );
        fs::remove_dir_all(directory)?;
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn symlink_path_substitution_cannot_escape_the_cache_root()
    -> Result<(), Box<dyn std::error::Error>> {
        use std::os::unix::fs::symlink;

        let cache = directory("path-substitution-cache");
        let outside = directory("path-substitution-outside");
        fs::create_dir_all(&cache)?;
        let outside_frame = write_png(&outside, "outside.png", 10)?;
        let substituted = cache.join("frame.png");
        symlink(&outside_frame, &substituted)?;
        let mut timeline = timeline();
        timeline.append(Duration::ZERO, snapshot(&substituted, 0, 5));
        let inspector =
            RetainedScreenInspector::new(&cache, None::<CountingOcr>, ImageInspectionPolicy::Allow);

        let result = inspector.inspect(
            timeline.events(),
            &InspectScreenRequest {
                selector: ScreenSelector::Timestamp(Duration::from_secs(2)),
                evidence: ScreenEvidence::Image,
                reason: "inspect chart".to_owned(),
            },
        );
        assert!(
            matches!(
                result,
                ScreenInspection::Unavailable {
                    reason: ScreenUnavailableReason::FrameOutsideCache,
                    ..
                }
            ),
            "a cache path substituted with an external symlink must not mint authorization"
        );
        fs::remove_dir_all(cache)?;
        fs::remove_dir_all(outside)?;
        Ok(())
    }

    #[test]
    fn invalid_or_oversized_retained_bytes_never_receive_authorization()
    -> Result<(), Box<dyn std::error::Error>> {
        let directory = directory("invalid-image-bytes");
        fs::create_dir_all(&directory)?;
        let invalid = directory.join("invalid.png");
        fs::write(&invalid, b"not an image")?;
        let oversized = directory.join("oversized.png");
        let mut oversized_bytes = vec![0_u8; MAX_AUTHORIZED_IMAGE_BYTES + 1];
        oversized_bytes[..8].copy_from_slice(&[0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a]);
        fs::write(&oversized, oversized_bytes)?;
        let inspect = |path: &Path| {
            let mut timeline = timeline();
            timeline.append(Duration::ZERO, snapshot(path, 0, 5));
            RetainedScreenInspector::new(
                &directory,
                None::<CountingOcr>,
                ImageInspectionPolicy::Allow,
            )
            .inspect(
                timeline.events(),
                &InspectScreenRequest {
                    selector: ScreenSelector::Timestamp(Duration::from_secs(2)),
                    evidence: ScreenEvidence::Image,
                    reason: "inspect chart".to_owned(),
                },
            )
        };

        assert!(
            matches!(
                inspect(&invalid),
                ScreenInspection::Unavailable {
                    reason: ScreenUnavailableReason::InvalidImageMedia,
                    ..
                }
            ),
            "magic sniffing must reject a mislabeled retained file"
        );
        assert!(
            matches!(
                inspect(&oversized),
                ScreenInspection::Unavailable {
                    reason: ScreenUnavailableReason::ImageTooLarge,
                    ..
                }
            ),
            "the screen boundary must enforce the image payload cap before dispatch"
        );
        fs::remove_dir_all(directory)?;
        Ok(())
    }
}
