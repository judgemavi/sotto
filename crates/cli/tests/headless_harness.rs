#[cfg(test)]
mod tests {
    use std::{collections::HashMap, path::Path, process::Command};

    use cli::{PipelineOptions, run_files};
    use serde::Deserialize;
    use sotto_core::{EventKind, EventPayload, Source};

    #[derive(Deserialize)]
    struct GroundTruth {
        #[serde(default)]
        transcript: Vec<ExpectedUtterance>,
    }

    #[derive(Deserialize)]
    struct ExpectedUtterance {
        source: Source,
        text: String,
    }

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

    #[test]
    #[ignore = "requires SOTTO_WHISPER_MODEL and runs real on-device inference"]
    fn real_asr_covers_ground_truth_in_order_and_pacing_modes_agree()
    -> Result<(), Box<dyn std::error::Error>> {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures");
        let model = std::env::var_os("SOTTO_WHISPER_MODEL")
            .ok_or("SOTTO_WHISPER_MODEL must point to ggml Whisper weights")?;
        let corpora = [
            ("call-01", "call-01-mic.wav", Some("call-01-sys.wav")),
            (
                "crosstalk",
                "crosstalk-mic.wav",
                Some("crosstalk-system.wav"),
            ),
            ("long-silence", "long-silence.wav", None),
            (
                "objection",
                "objection-mic.wav",
                Some("objection-system.wav"),
            ),
        ];

        for (name, mic, system) in corpora {
            let truth: GroundTruth = serde_json::from_slice(&std::fs::read(
                root.join(format!("{name}-ground-truth.json")),
            )?)?;
            let options = |realtime| PipelineOptions {
                mic: root.join(mic),
                system: system.map(|path| root.join(path)),
                frames: None,
                model: Some(model.clone().into()),
                realtime,
            };
            let fast = run_files(&options(false))?;
            assert_ground_truth(name, &truth.transcript, &fast.events)?;

            if name == "call-01" {
                let realtime = run_files(&options(true))?;
                assert_ground_truth("call-01 --realtime", &truth.transcript, &realtime.events)?;
                assert_eq!(
                    final_texts(&fast.events),
                    final_texts(&realtime.events),
                    "audio-time inference must not depend on fixture wall-clock pacing"
                );
            }
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

    fn assert_ground_truth(
        corpus: &str,
        expected: &[ExpectedUtterance],
        events: &[sotto_core::TimelineEvent],
    ) -> Result<(), Box<dyn std::error::Error>> {
        let actual = final_texts(events);
        let mut next = HashMap::from([(Source::Mic, 0_usize), (Source::System, 0_usize)]);
        for utterance in expected {
            let candidates = actual
                .get(&utterance.source)
                .map(Vec::as_slice)
                .unwrap_or_default();
            let start = *next.get(&utterance.source).unwrap_or(&0);
            let Some((offset, _)) = candidates[start..]
                .iter()
                .enumerate()
                .find(|(_, text)| word_overlap(&utterance.text, text) >= 0.55)
            else {
                return Err(format!(
                    "{corpus}: missing ordered {:?} utterance {:?}; finals: {:?}",
                    utterance.source, utterance.text, candidates
                )
                .into());
            };
            next.insert(utterance.source, start + offset + 1);
        }
        Ok(())
    }

    fn final_texts(events: &[sotto_core::TimelineEvent]) -> HashMap<Source, Vec<String>> {
        let mut output = HashMap::<Source, Vec<String>>::new();
        for event in events {
            if let EventPayload::UtteranceFinal(value) = event.payload() {
                output
                    .entry(value.source)
                    .or_default()
                    .push(value.text.clone());
            }
        }
        output
    }

    fn word_overlap(expected: &str, actual: &str) -> f64 {
        let expected = words(expected);
        let actual = words(actual);
        if expected.is_empty() {
            return 1.0;
        }
        let matched = expected.iter().filter(|word| actual.contains(word)).count();
        matched as f64 / expected.len() as f64
    }

    fn words(text: &str) -> Vec<String> {
        text.split_whitespace()
            .map(|word| {
                word.chars()
                    .filter(|character| character.is_alphanumeric())
                    .flat_map(char::to_lowercase)
                    .collect::<String>()
            })
            .filter(|word| !word.is_empty())
            .collect()
    }
}
