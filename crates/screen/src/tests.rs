use std::{fs, time::Duration};

use sotto_core::{CaptureTarget, EventPayload, TargetKind};

use super::*;

struct FixtureOcr;

impl OcrEngine for FixtureOcr {
    fn recognize(&self, _frame: &Frame) -> Result<String, ScreenError> {
        Err(ScreenError::Ocr("screen ingestion invoked OCR".to_owned()))
    }
}

fn target() -> CaptureTarget {
    CaptureTarget {
        bundle_id: Some("com.apple.Keynote".to_owned()),
        display_name: "Keynote".to_owned(),
        window_title: Some("Pricing.key".to_owned()),
        kind: TargetKind::Window,
        audio_scoped: false,
    }
}

fn frame(at: Duration, red: u8) -> Frame {
    let pixel = [0, 0, red, 255];
    Frame {
        bgra: pixel.repeat(64 * 64),
        width: 64,
        height: 64,
        stride: 64 * 4,
        captured_at: at,
    }
}

fn temporary_directory(name: &str) -> PathBuf {
    std::env::temp_dir().join(format!("sotto-screen-{name}-{}", std::process::id()))
}

#[test]
fn five_minutes_of_static_frames_collapse_to_one_interval() -> Result<(), ScreenError> {
    let directory = temporary_directory("static");
    let mut sampler =
        ScreenSampler::new(SamplerConfig::default(), &directory, target(), FixtureOcr)?;
    for second in (0..=300).step_by(5) {
        assert!(
            sampler
                .push(
                    frame(Duration::from_secs(second), 10),
                    FrameMetadata::default()
                )?
                .is_none(),
            "static frame unexpectedly emitted an interval"
        );
    }
    let Some(EventPayload::ScreenSnapshot(snapshot)) = sampler.finish(Duration::from_secs(300))
    else {
        return Err(ScreenError::InvalidFrame(
            "missing final snapshot".to_owned(),
        ));
    };
    assert_eq!(snapshot.visible_from, Duration::ZERO);
    assert_eq!(snapshot.visible_to, Some(Duration::from_secs(300)));
    assert_eq!(snapshot.active_app.as_deref(), Some("Keynote"));
    assert_eq!(snapshot.window_title.as_deref(), Some("Pricing.key"));
    sampler.drop_session_frames()?;
    fs::remove_dir_all(directory)?;
    Ok(())
}

#[test]
fn slide_change_closes_previous_interval_without_running_ocr() -> Result<(), ScreenError> {
    let directory = temporary_directory("change");
    let mut sampler =
        ScreenSampler::new(SamplerConfig::default(), &directory, target(), FixtureOcr)?;
    sampler.push(frame(Duration::ZERO, 10), FrameMetadata::default())?;
    let Some(EventPayload::ScreenSnapshot(snapshot)) = sampler.push(
        frame(Duration::from_secs(10), 240),
        FrameMetadata::default(),
    )?
    else {
        return Err(ScreenError::InvalidFrame(
            "slide change did not emit".to_owned(),
        ));
    };
    assert!(
        snapshot.ocr_text.is_empty(),
        "ingestion must not persist eager OCR"
    );
    assert_eq!(snapshot.visible_to, Some(Duration::from_secs(10)));
    let Some(EventPayload::ScreenSnapshot(final_snapshot)) =
        sampler.finish(Duration::from_secs(20))
    else {
        return Err(ScreenError::InvalidFrame(
            "missing final snapshot".to_owned(),
        ));
    };
    assert!(
        final_snapshot.ocr_text.is_empty(),
        "final ingestion must remain OCR-free"
    );
    sampler.drop_session_frames()?;
    fs::remove_dir_all(directory)?;
    Ok(())
}

#[test]
fn cache_prunes_oldest_frames_to_hard_bound() -> Result<(), ScreenError> {
    let directory = temporary_directory("prune");
    let mut sampler = ScreenSampler::new(
        SamplerConfig {
            min_interval: Duration::ZERO,
            change_threshold: 1,
            max_cache_bytes: 200,
        },
        &directory,
        target(),
        FixtureOcr,
    )?;
    for index in 0..120 {
        sampler.push(
            frame(
                Duration::from_secs(index),
                u8::try_from(index).unwrap_or(u8::MAX),
            ),
            FrameMetadata::default(),
        )?;
        assert!(sampler.cached_bytes() <= 200, "cache exceeded hard bound");
    }
    sampler.drop_session_frames()?;
    fs::remove_dir_all(directory)?;
    Ok(())
}
