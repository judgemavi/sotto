#[cfg(test)]
mod tests {
    use std::{path::PathBuf, time::Duration};

    use screen::{
        AuthorizedImageMediaType, ImageInspectionPolicy, InspectScreenRequest, OcrEngine,
        RetainedScreenInspector, ScreenError, ScreenEvidence, ScreenInspectionSource,
        ScreenSelector,
    };
    use sotto_core::{
        CaptureTarget, EventId, EventPayload, FrameRef, ScreenSnapshot, Session, SessionId,
        TargetKind, TimelineBuilder,
    };

    fn temporary_png() -> Result<PathBuf, Box<dyn std::error::Error>> {
        let directory = std::env::temp_dir().join(format!(
            "sotto-t031-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)?
                .as_nanos()
        ));
        std::fs::create_dir_all(&directory)?;
        let path = directory.join("frame.png");
        std::fs::write(
            &path,
            [0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a, 1, 2, 3],
        )?;
        Ok(path)
    }

    fn image_request() -> InspectScreenRequest {
        InspectScreenRequest {
            selector: ScreenSelector::Timestamp(Duration::from_secs(12)),
            evidence: ScreenEvidence::Image,
            reason: "pricing slide materially grounds the recap".to_owned(),
        }
    }

    struct NoOcr;

    impl OcrEngine for NoOcr {
        fn recognize(&self, _frame: &screen::Frame) -> Result<String, ScreenError> {
            Err(ScreenError::Ocr(
                "OCR must not run for image evidence".to_owned(),
            ))
        }
    }

    fn timeline(path: &std::path::Path) -> (TimelineBuilder, EventId) {
        let mut timeline = TimelineBuilder::new(Session::new(
            SessionId::new(31),
            CaptureTarget {
                bundle_id: Some("com.apple.Keynote".to_owned()),
                display_name: "Keynote".to_owned(),
                window_title: Some("Pricing".to_owned()),
                kind: TargetKind::Window,
                audio_scoped: true,
            },
            0,
        ));
        let event = timeline.append(
            Duration::from_secs(10),
            EventPayload::ScreenSnapshot(ScreenSnapshot {
                frame_ref: FrameRef::new(path.to_string_lossy()),
                ocr_text: String::new(),
                active_app: Some("Keynote".to_owned()),
                window_title: Some("Pricing".to_owned()),
                visible_from: Duration::from_secs(10),
                visible_to: Some(Duration::from_secs(20)),
            }),
        );
        let event_id = event.id();
        (timeline, event_id)
    }

    #[test]
    fn explicit_policy_mints_bounded_path_free_image_for_exact_request()
    -> Result<(), Box<dyn std::error::Error>> {
        let path = temporary_png()?;
        let cache_root = path.parent().ok_or("temporary frame has no parent")?;
        let (timeline, snapshot_event_id) = timeline(&path);
        let inspector =
            RetainedScreenInspector::new(cache_root, None::<NoOcr>, ImageInspectionPolicy::Allow);
        let request = image_request();
        let mut inspection = inspector.inspect(timeline.events(), &request);
        let image = inspection
            .take_authorized_image_for(&request)
            .ok_or("authorized image did not cross dispatch boundary")?;
        assert_eq!(image.media_type(), AuthorizedImageMediaType::Png);
        assert_eq!(image.bytes().len(), 11);
        assert_eq!(
            image.provenance().snapshot_event_id(),
            snapshot_event_id,
            "authorized provenance must cite the retained snapshot"
        );
        assert!(
            inspection.take_authorized_image_for(&request).is_none(),
            "screen authorization must be consumed at most once"
        );
        std::fs::remove_dir_all(cache_root)?;
        Ok(())
    }

    #[test]
    fn denied_policy_and_selector_mismatch_never_yield_image_bytes()
    -> Result<(), Box<dyn std::error::Error>> {
        let path = temporary_png()?;
        let cache_root = path.parent().ok_or("temporary frame has no parent")?;
        let (timeline, _) = timeline(&path);
        let request = image_request();
        let mut denied =
            RetainedScreenInspector::new(cache_root, None::<NoOcr>, ImageInspectionPolicy::Deny)
                .inspect(timeline.events(), &request);
        assert!(
            denied.take_authorized_image_for(&request).is_none(),
            "denied policy must not mint an image"
        );

        let mut allowed =
            RetainedScreenInspector::new(cache_root, None::<NoOcr>, ImageInspectionPolicy::Allow)
                .inspect(timeline.events(), &request);
        let mismatched = InspectScreenRequest {
            selector: ScreenSelector::Timestamp(Duration::from_secs(13)),
            ..request
        };
        assert!(
            allowed.take_authorized_image_for(&mismatched).is_none(),
            "dispatch cannot reuse authorization for a different selector"
        );
        std::fs::remove_dir_all(cache_root)?;
        Ok(())
    }
}
