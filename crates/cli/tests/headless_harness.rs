#[cfg(test)]
mod tests {
    use std::{path::Path, process::Command};

    use cli::{PipelineOptions, run_files};
    use sotto_core::EventKind;

    #[test]
    fn paired_fixture_runs_without_display_or_audio_hardware()
    -> Result<(), Box<dyn std::error::Error>> {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures");
        let run = run_files(&PipelineOptions {
            mic: root.join("call-01-mic.wav"),
            system: Some(root.join("call-01-sys.wav")),
            frames: Some(root.join("call-01-frames")),
            model: None,
            realtime: false,
        })?;
        assert!(
            run.events
                .iter()
                .any(|event| event.kind() == EventKind::Vad),
            "speech fixture must emit VAD transitions"
        );
        assert!(
            run.events
                .iter()
                .any(|event| event.kind() == EventKind::ScreenSnapshot),
            "fused fixture must emit screen context"
        );
        let encoded = run
            .events
            .iter()
            .map(serde_json::to_string)
            .collect::<Result<Vec<_>, _>>()?;
        assert_eq!(
            encoded.len(),
            run.events.len(),
            "every event must serialize as one JSONL record"
        );
        Ok(())
    }

    #[test]
    fn silence_reports_empty_stages_on_stderr() -> Result<(), Box<dyn std::error::Error>> {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures");
        let output = Command::new(env!("CARGO_BIN_EXE_sotto-cli"))
            .arg("run")
            .arg(root.join("silence.wav"))
            .output()?;
        assert!(output.status.success());
        let stderr = String::from_utf8(output.stderr)?;
        assert!(stderr.contains("VAD stage emitted no events"), "{stderr}");
        assert!(stderr.contains("no model configured"), "{stderr}");
        assert!(stderr.contains("no frames provided"), "{stderr}");
        Ok(())
    }

    #[test]
    fn actual_run_reference_contains_all_local_map_stages() -> Result<(), Box<dyn std::error::Error>>
    {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures");
        let timeline = std::fs::read_to_string(root.join("timelines/call-01.jsonl"))?;
        let kinds = timeline
            .lines()
            .map(serde_json::from_str::<sotto_core::TimelineEvent>)
            .collect::<Result<Vec<_>, _>>()?;
        for expected in [
            EventKind::Vad,
            EventKind::UtteranceFinal,
            EventKind::Prosody,
            EventKind::ScreenSnapshot,
        ] {
            assert!(
                kinds.iter().any(|event| event.kind() == expected),
                "reference timeline is missing {expected:?}"
            );
        }
        Ok(())
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn pricing_frame_contains_text_readable_by_vision() -> Result<(), Box<dyn std::error::Error>> {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures");
        let verifier =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("examples/verify_fixture_vision.swift");
        let output = std::process::Command::new("swift")
            .env(
                "CLANG_MODULE_CACHE_PATH",
                std::env::temp_dir().join("sotto-swift-module-cache"),
            )
            .arg(verifier)
            .arg(root.join("call-01-frames/15000-pricing.png"))
            .arg("Enterprise Pricing")
            .output()?;
        assert!(
            output.status.success(),
            "Vision OCR fixture verification failed:\nstdout: {}\nstderr: {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        );
        Ok(())
    }
}
