//! Turns a file the user already has into a session.
//!
//! ADR-0018 made transcription a consumer of a retained recording rather than of a live stream, so
//! everything downstream of capture is already file-driven; importing only has to produce a session
//! whose recording came from somewhere else. This module reuses exactly the reader T058 built for a
//! captured recording's own finalized-tail read ([`LaggedRecordingTranscriber`]) — there is no
//! second decode path and no second transcription implementation here, only the orchestration that
//! turns one already-recorded file into the same session shape a capture produces.
//!
//! The provenance an import cannot honestly claim — a capture target, a scoped-audio guarantee, a
//! known channel layout — is recorded as an explicit absence via
//! [`sotto_core::types::imported_capture_target`] rather than defaulted to a plausible-looking
//! value: `TargetKind::Imported` names the state directly, and every field that would otherwise
//! carry a captured fact is `None` or `false`.

use std::{
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use asr::{Config as AsrConfig, LaggedRecordingTranscriber, RecordingConfig};
use rag::Store;
use sotto_core::{
    AsrError, RecordingStatus, RecordingTranscriber, Session, SessionId, TranscriptUpdate,
    types::{MediaTimeMapping, RecordingContainer, SessionRecording, imported_capture_target},
};

/// Containers the AVFoundation-backed recording reader can be relied on to open, kept to exactly
/// the common audio and video cases the plan asks for rather than everything `AVURLAsset`
/// technically accepts. Short enough to state plainly in a rejection message, which is the point:
/// an unsupported file is refused here, before any file is copied or any model is loaded, rather
/// than failing obscurely partway through transcription.
const SUPPORTED_EXTENSIONS: &[&str] = &[
    "mp4", "mov", "m4v", "m4a", "mp3", "wav", "aif", "aiff", "caf",
];

/// The name every import falls back to when the source path carries none, which the OS file picker
/// should never actually hand back — this only guards `imported_capture_target`'s non-empty-name
/// requirement against that impossible case rather than ever being the expected path.
const UNNAMED_IMPORT: &str = "Imported file";

pub struct ImportOutcome {
    pub session_id: SessionId,
    pub utterance_count: usize,
}

/// Rejects an unsupported container up front with the reason stated plainly, rather than letting an
/// arbitrary file reach the model load and the native reader.
fn validate_container(source: &Path) -> Result<(), String> {
    let extension = source
        .extension()
        .and_then(|extension| extension.to_str())
        .map(str::to_lowercase);
    if extension.is_some_and(|extension| SUPPORTED_EXTENSIONS.contains(&extension.as_str())) {
        return Ok(());
    }
    let name = source.file_name().map_or_else(
        || "This file".to_owned(),
        |name| name.to_string_lossy().into_owned(),
    );
    Err(format!(
        "Sotto can import {} files. {name} isn't one of them.",
        SUPPORTED_EXTENSIONS.join(", ")
    ))
}

/// Turns the native recording reader's own diagnostic into the stated reason the plan asks for.
///
/// `crates/asr`'s reader already distinguishes "no audio track" from "not stereo" when it reads a
/// captured recording's own tail (see `sotto_asr_read_stereo` in the recording bridge); those exact
/// native diagnostics are what surfaces here, translated into language about the imported file
/// rather than about a capture. No second check is implemented — the native read is the check, and
/// it runs before any Whisper inference: `transcribe_available` calls it first and only pushes
/// decoded samples to the transcriber once it succeeds.
fn describe_asr_failure(error: &AsrError) -> String {
    let message = error.to_string();
    if message.contains("no committed audio track") {
        "This file has no audio track, so it can't be transcribed.".to_owned()
    } else if message.contains("must be stereo") {
        "This file's audio track is mono. Sotto's import currently supports stereo audio only; \
         many voice memos and some podcasts are recorded mono and aren't supported yet."
            .to_owned()
    } else {
        format!("Sotto could not read this file: {message}")
    }
}

/// Imports `source` as a new session: transcribes it, copies it into the managed recording
/// directory, and persists a session whose provenance honestly states that it was imported.
///
/// Transcription runs first, against the original file, before anything is copied or written to the
/// store — an unsupported or broken file costs nothing beyond the read attempt itself, and nothing
/// durable exists until the file is known to produce a transcript.
pub(super) async fn import_recording(
    source: &Path,
    database: &Path,
    recording_directory: &Path,
    model_path: &Path,
) -> Result<ImportOutcome, String> {
    validate_container(source)?;

    // Reused unmodified, per the plan: this is exactly the layout a captured recording's own tail
    // read uses. An import has no verified channel convention to apply (T061's left=meeting,
    // right=mic split is a fact about Sotto's own writer, not about an arbitrary file), so every
    // channel the file carries is read rather than assuming either channel is disposable — which is
    // what selecting `.microphone_only()` here would claim without evidence.
    let config = RecordingConfig::new(AsrConfig::new(model_path));
    let mut transcriber = LaggedRecordingTranscriber::new(source, config)
        .map_err(|error| describe_asr_failure(&error))?;
    let updates = transcriber
        .transcribe_available(RecordingStatus::Complete)
        .map_err(|error| describe_asr_failure(&error))?;
    let utterances = updates
        .into_iter()
        .filter_map(|update| match update {
            TranscriptUpdate::Final(utterance) => Some(utterance),
            TranscriptUpdate::Partial(_) => None,
        })
        .collect::<Vec<_>>();
    let duration = transcriber.media_cursor();

    std::fs::create_dir_all(recording_directory)
        .map_err(|error| format!("Could not create the recording directory: {error}"))?;
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| format!("System clock precedes Unix epoch: {error}"))?;
    let session_id = SessionId::new(now.as_nanos());
    let destination = destination_path(recording_directory, session_id);
    std::fs::copy(source, &destination)
        .map_err(|error| format!("Could not copy this file into the recording library: {error}"))?;
    let byte_size = std::fs::metadata(&destination)
        .map_err(|error| format!("Could not measure the imported recording: {error}"))?
        .len();

    let display_name = source
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .filter(|name| !name.trim().is_empty())
        .unwrap_or_else(|| UNNAMED_IMPORT.to_owned());
    let started_at_unix_ms = u64::try_from(now.as_millis()).unwrap_or(u64::MAX);
    let ended_at_unix_ms =
        started_at_unix_ms.saturating_add(u64::try_from(duration.as_millis()).unwrap_or(u64::MAX));
    let mut session = Session::new(
        session_id,
        imported_capture_target(display_name),
        started_at_unix_ms,
    );
    session.end(ended_at_unix_ms);

    let store = Store::open(database)
        .await
        .map_err(|error| error.to_string())?;
    store
        .save_session(&session)
        .await
        .map_err(|error| error.to_string())?;
    store
        .save_recording(&SessionRecording::Available {
            session_id,
            path: destination.to_string_lossy().into_owned(),
            container: RecordingContainer::Mp4,
            duration,
            byte_size,
            time_mapping: MediaTimeMapping::IDENTITY,
        })
        .await
        .map_err(|error| error.to_string())?;
    let events = store
        .append_final_utterances(session_id, &utterances)
        .await
        .map_err(|error| error.to_string())?;
    // Non-fatal and last, mirroring the captured-session finalize path: the session, its timeline
    // and its media are already durable by this point, so a budget or index failure degrades
    // retention accounting or cross-recording search rather than losing the import.
    if let Err(error) = store.enforce_recording_budget(recording_directory).await {
        eprintln!("Imported recording saved, but retention budget enforcement failed: {error}");
    }
    if let Err(error) = store.index_prior_meeting(session_id, None).await {
        eprintln!(
            "Imported recording saved, but it could not be added to cross-recording search: \
             {error}. It stays reviewable, and Sotto retries when you ask across recordings."
        );
    }
    Ok(ImportOutcome {
        session_id,
        utterance_count: events.len(),
    })
}

/// The on-disk name and container tag every managed recording carries, imported or captured alike.
///
/// `application_recording_directory`'s own contract is "session-id-named MP4 recordings", the
/// growing-recording write path hardcodes `container='mp4'`, and crash recovery
/// (`Store::recover_quarantined_media`) reconstructs a quarantined file's original name as literally
/// `<session_id>.mp4` — none of that is owned by this task. Naming the copy anything else would
/// silently corrupt recovery for exactly this recording after a future crash mid-delete: the
/// tombstone would be restored under the wrong extension.
///
/// The copy keeps the source file's real bytes; only the name changes. This is safe to decode:
/// verified locally with `afinfo` (the same on-device content-detection stack AVFoundation draws
/// on) against real AIFF, WAVE, M4A and MP3 audio each renamed to `.mp4` — every one was still
/// identified and read correctly by its actual content, never by the misleading extension.
fn destination_path(recording_directory: &Path, session_id: SessionId) -> PathBuf {
    recording_directory.join(format!("{}.mp4", session_id.get()))
}

#[cfg(test)]
mod tests {
    use super::{describe_asr_failure, validate_container};
    use sotto_core::AsrError;
    use std::path::Path;

    #[test]
    fn a_supported_extension_is_accepted_case_insensitively() {
        for name in ["call.MP4", "voice.m4a", "lecture.MOV", "clip.wav"] {
            assert!(
                validate_container(Path::new(name)).is_ok(),
                "{name} should be an accepted container"
            );
        }
    }

    #[test]
    fn an_unsupported_extension_is_named_in_the_rejection() {
        let error = validate_container(Path::new("call.mkv"))
            .err()
            .unwrap_or_else(|| "MISSING REJECTION".to_owned());
        assert!(
            error.contains("call.mkv"),
            "the rejection must name the file it refused, got {error}"
        );
        assert!(
            error.contains("mp4"),
            "the rejection must state what is supported, got {error}"
        );
    }

    #[test]
    fn a_missing_extension_is_rejected_rather_than_guessed() {
        assert!(validate_container(Path::new("README")).is_err());
    }

    #[test]
    fn no_audio_track_and_mono_audio_produce_distinct_stated_reasons() {
        let no_track = describe_asr_failure(&AsrError::Inference(
            "recording has no committed audio track".to_owned(),
        ));
        assert!(no_track.contains("no audio track"), "got {no_track}");
        let mono = describe_asr_failure(&AsrError::Inference(
            "recording audio must be stereo: left=meeting audio, right=microphone".to_owned(),
        ));
        assert!(mono.contains("mono"), "got {mono}");
        assert!(
            no_track != mono,
            "a missing audio track and a mono audio track are different reasons and must not \
             collapse into the same message"
        );
    }

    // The house verification rule: a crate wrapping a model, a device, or an OS API needs at
    // least one test that runs real input through it and asserts on a known real answer. These
    // run real `whisper.cpp` inference over real speech synthesized with `say`/`afconvert`, both
    // stock macOS tools; the video variant additionally uses `ffmpeg` to assemble a fixture with a
    // genuine video track, which is not a Sotto or system dependency and is only present because
    // this verification pass installed it locally. All are `#[ignore]`d, matching
    // `crates/cli/tests/headless_harness.rs::real_asr_covers_ground_truth...`, because CI has
    // neither model weights nor guaranteed audio-synthesis tooling.
    #[cfg(target_os = "macos")]
    mod real_media {
        use super::super::import_recording;
        use screen::{
            ImageInspectionPolicy, InspectScreenRequest, RecordingBackedScreenInspector,
            ScreenEvidence, ScreenInspection, ScreenInspectionSource, ScreenSelector, VisionOcr,
        };
        use sotto_core::{EventPayload, types::is_imported_capture_target};
        use std::{path::PathBuf, process::Command, time::Duration};

        /// Runs `say` to produce a short spoken AIFF, then re-encodes it to stereo AAC with
        /// `afconvert`: the recording reader requires two channels and `say`'s own output is
        /// mono. `None` (never a hard failure) if either tool is missing, which only happens off
        /// a real macOS host.
        fn synthesize_stereo_speech(
            directory: &std::path::Path,
            text: &str,
            name: &str,
        ) -> Option<PathBuf> {
            let aiff = directory.join("say-source.aiff");
            if !Command::new("say")
                .args(["-o", &aiff.to_string_lossy(), text])
                .status()
                .ok()?
                .success()
            {
                return None;
            }
            let destination = directory.join(name);
            Command::new("afconvert")
                .args([
                    "-f",
                    "m4af",
                    "-d",
                    "aac",
                    "-c",
                    "2",
                    &aiff.to_string_lossy(),
                    &destination.to_string_lossy(),
                ])
                .status()
                .ok()?
                .success()
                .then_some(destination)
        }

        /// Assembles a short real MP4 carrying a genuine H.264 video track alongside `audio`'s
        /// stereo AAC track, via `ffmpeg`.
        fn synthesize_video_with_speech(
            directory: &std::path::Path,
            audio: &std::path::Path,
            name: &str,
        ) -> Option<PathBuf> {
            let destination = directory.join(name);
            Command::new("ffmpeg")
                .args([
                    "-y",
                    "-f",
                    "lavfi",
                    "-i",
                    "testsrc2=size=640x360:duration=3:rate=25",
                ])
                .arg("-i")
                .arg(audio)
                .args([
                    "-c:v",
                    "libx264",
                    "-pix_fmt",
                    "yuv420p",
                    "-c:a",
                    "aac",
                    "-ac",
                    "2",
                    "-shortest",
                ])
                .arg(&destination)
                .status()
                .ok()?
                .success()
                .then_some(destination)
        }

        async fn transcript_of(store: &rag::Store, session_id: sotto_core::SessionId) -> String {
            store
                .load_session(session_id)
                .await
                .map(|events| {
                    events
                        .iter()
                        .filter_map(|event| match event.payload() {
                            EventPayload::UtteranceFinal(utterance) => {
                                Some(utterance.text.to_lowercase())
                            }
                            _ => None,
                        })
                        .collect::<Vec<_>>()
                        .join(" ")
                })
                .unwrap_or_default()
        }

        #[tokio::test(flavor = "multi_thread")]
        #[ignore = "requires SOTTO_WHISPER_MODEL, real on-device inference, and macOS say/afconvert"]
        async fn imports_a_real_audio_file_and_transcribes_known_words()
        -> Result<(), Box<dyn std::error::Error>> {
            let model = std::env::var_os("SOTTO_WHISPER_MODEL")
                .ok_or("SOTTO_WHISPER_MODEL must point to ggml Whisper weights")?;
            let directory = tempfile::tempdir()?;
            let Some(source) = synthesize_stereo_speech(
                directory.path(),
                "The quarterly roadmap review starts on Monday morning",
                "voice-memo.m4a",
            ) else {
                return Err("this host is missing say or afconvert".into());
            };
            let model = PathBuf::from(model);
            let database = directory.path().join("sotto.sqlite3");
            let recordings = directory.path().join("recordings");
            let outcome = import_recording(&source, &database, &recordings, &model).await?;

            assert!(
                outcome.utterance_count > 0,
                "a real spoken file must produce at least one transcribed utterance"
            );
            let store = rag::Store::open(&database).await?;
            let transcript = transcript_of(&store, outcome.session_id).await;
            assert!(
                transcript.contains("roadmap") || transcript.contains("monday"),
                "transcript must contain recognizable words from the known spoken text, got \
                 {transcript:?}"
            );
            let session = store.load_session_record(outcome.session_id).await?;
            assert!(
                is_imported_capture_target(session.capture_target()),
                "an imported session must record itself as imported, not as a capture"
            );
            assert!(
                !session.capture_target().audio_scoped,
                "an import must never claim a scoped-audio guarantee it cannot support"
            );
            Ok(())
        }

        #[tokio::test(flavor = "multi_thread")]
        #[ignore = "requires SOTTO_WHISPER_MODEL, real on-device inference, macOS say/afconvert, \
                    and ffmpeg to assemble the video fixture"]
        async fn imports_a_real_video_file_transcribes_it_and_supports_screen_inspection()
        -> Result<(), Box<dyn std::error::Error>> {
            let model = std::env::var_os("SOTTO_WHISPER_MODEL")
                .ok_or("SOTTO_WHISPER_MODEL must point to ggml Whisper weights")?;
            let directory = tempfile::tempdir()?;
            let Some(audio) = synthesize_stereo_speech(
                directory.path(),
                "Screen sharing starts now and the roadmap slide is on screen",
                "narration.m4a",
            ) else {
                return Err("this host is missing say or afconvert".into());
            };
            let Some(source) =
                synthesize_video_with_speech(directory.path(), &audio, "meeting.mp4")
            else {
                return Err(
                    "this host is missing ffmpeg, used only to assemble the test fixture; \
                             it is not a Sotto dependency"
                        .into(),
                );
            };
            let model = PathBuf::from(model);
            let database = directory.path().join("sotto.sqlite3");
            let recordings = directory.path().join("recordings");
            // Awaited on this test's own runtime rather than nested inside a second one. The two
            // sibling rejection tests below are synchronous and legitimately build their own
            // runtime; this one is `async` because it awaits the store afterwards, and
            // `Runtime::block_on` panics outright when a runtime is already driving the thread.
            let outcome = import_recording(&source, &database, &recordings, &model).await?;

            let store = rag::Store::open(&database).await?;
            let transcript = transcript_of(&store, outcome.session_id).await;
            assert!(
                transcript.contains("roadmap") || transcript.contains("screen"),
                "the video's real speech must transcribe to recognizable words, got {transcript:?}"
            );

            let recording = store
                .load_recording(outcome.session_id)
                .await?
                .ok_or("the imported video must have a settled recording row")?;
            let inspector = RecordingBackedScreenInspector::new(
                recording,
                None::<VisionOcr>,
                ImageInspectionPolicy::Deny,
            );
            let inspection = inspector.inspect(
                &[],
                &InspectScreenRequest {
                    selector: ScreenSelector::Timestamp(Duration::from_millis(500)),
                    evidence: ScreenEvidence::Metadata,
                    reason: "T071 verification".to_owned(),
                },
            );
            assert!(
                matches!(inspection, ScreenInspection::RecordingAvailable { .. }),
                "a video import must support decoding a screen frame at a cited moment, got \
                 {inspection:?}"
            );
            Ok(())
        }

        #[test]
        #[ignore = "requires SOTTO_WHISPER_MODEL, real on-device inference, and macOS say"]
        fn a_real_mono_audio_file_is_rejected_with_a_stated_reason()
        -> Result<(), Box<dyn std::error::Error>> {
            let model = std::env::var_os("SOTTO_WHISPER_MODEL")
                .ok_or("SOTTO_WHISPER_MODEL must point to ggml Whisper weights")?;
            let model = PathBuf::from(model);
            let directory = tempfile::tempdir()?;
            let source = directory.path().join("mono-voice-memo.m4a");
            let produced = Command::new("say")
                .args([
                    "-o",
                    &source.to_string_lossy(),
                    "This voice memo has only one channel",
                ])
                .status()?
                .success();
            if !produced {
                return Err("this host is missing say".into());
            }
            let database = directory.path().join("sotto.sqlite3");
            let recordings = directory.path().join("recordings");
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()?;
            let error = runtime
                .block_on(import_recording(&source, &database, &recordings, &model))
                .err()
                .ok_or("a genuinely mono recording must be rejected, not silently accepted")?;
            assert!(
                error.contains("mono"),
                "the rejection must state the real reason, got {error}"
            );
            assert!(
                !database.exists(),
                "a rejected import must not leave a durable session behind"
            );
            Ok(())
        }

        #[test]
        #[ignore = "requires SOTTO_WHISPER_MODEL and ffmpeg to assemble the video fixture"]
        fn a_real_video_with_no_audio_track_is_rejected_before_transcription()
        -> Result<(), Box<dyn std::error::Error>> {
            let model = std::env::var_os("SOTTO_WHISPER_MODEL")
                .ok_or("SOTTO_WHISPER_MODEL must point to ggml Whisper weights")?;
            let model = PathBuf::from(model);
            let directory = tempfile::tempdir()?;
            let source = directory.path().join("silent-screen-share.mp4");
            let produced = Command::new("ffmpeg")
                .args([
                    "-y",
                    "-f",
                    "lavfi",
                    "-i",
                    "testsrc2=size=640x360:duration=2:rate=25",
                    "-c:v",
                    "libx264",
                    "-pix_fmt",
                    "yuv420p",
                    "-an",
                ])
                .arg(&source)
                .status()?
                .success();
            if !produced {
                return Err("this host is missing ffmpeg".into());
            }
            // Model resolution happens before the native reader runs (`LaggedRecordingTranscriber`
            // needs a loaded model to construct at all), so this cannot prove the audio-track check
            // precedes *model loading* — only that it precedes any Whisper inference, which is what
            // "before transcription begins" means here: `transcribe_available` calls the native
            // read first and only pushes decoded samples to Whisper once it succeeds.
            let database = directory.path().join("sotto.sqlite3");
            let recordings = directory.path().join("recordings");
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()?;
            let error = runtime
                .block_on(import_recording(&source, &database, &recordings, &model))
                .err()
                .ok_or("a video with no audio track must be rejected, not silently accepted")?;
            assert!(
                error.contains("no audio track"),
                "the rejection must state the real reason, got {error}"
            );
            assert!(
                !database.exists(),
                "a rejected import must not leave a durable session behind"
            );
            Ok(())
        }
    }
}
