#[cfg(test)]
mod tests {
    use std::path::Path;

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
}
