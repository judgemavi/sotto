use std::path::{Path, PathBuf};

use asr::{Config as AsrConfig, LaggedRecordingTranscriber, RecordingConfig};
use rag::{RecordingReference, RecordingUsage, Store};
use sotto_core::types::{RecordingMissingReason, SessionRecording};
use sotto_core::{RagError, RecordingStatus, RecordingTranscriber, SessionId, TranscriptUpdate};

/// One settings-library row. Missing media remains visible instead of disappearing ambiguously.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RecordingLibraryItem {
    pub session_id: SessionId,
    pub session_label: String,
    pub recording: SessionRecording,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RecordingLibrarySnapshot {
    pub items: Vec<RecordingLibraryItem>,
    pub growing: Vec<RecordingReference>,
    pub usage: RecordingUsage,
}

/// App-facing recording controls kept separate from transcript deletion.
#[derive(Clone, Debug)]
pub struct RecordingLibrary {
    database: PathBuf,
    directory: PathBuf,
}

impl RecordingLibrary {
    #[must_use]
    pub fn new(database: impl Into<PathBuf>, directory: impl Into<PathBuf>) -> Self {
        Self {
            database: database.into(),
            directory: directory.into(),
        }
    }

    #[must_use]
    pub fn application_default() -> Self {
        Self::new(
            super::application_database_path(),
            super::application_recording_directory(),
        )
    }

    pub fn snapshot(&self) -> Result<RecordingLibrarySnapshot, RagError> {
        let store = Store::open(&self.database)?;
        let sessions = store.list_sessions()?;
        let references = store.list_recording_references()?;
        let recordings = references.iter().filter_map(|reference| match reference {
            RecordingReference::Settled(recording) => Some(recording.clone()),
            RecordingReference::Growing { .. } => None,
        });
        let items = recordings
            .into_iter()
            .map(|recording| {
                let session_id = recording.session_id();
                let session_label = sessions
                    .iter()
                    .find(|session| session.id == session_id)
                    .map_or_else(
                        || format!("Meeting {}", session_id.get()),
                        |session| {
                            session
                                .capture_target
                                .window_title
                                .clone()
                                .unwrap_or_else(|| session.capture_target.display_name.clone())
                        },
                    );
                RecordingLibraryItem {
                    session_id,
                    session_label,
                    recording,
                }
            })
            .collect();
        Ok(RecordingLibrarySnapshot {
            items,
            growing: references
                .into_iter()
                .filter(|reference| matches!(reference, RecordingReference::Growing { .. }))
                .collect(),
            usage: store.recording_usage()?,
        })
    }

    pub fn delete(&self, session_id: SessionId) -> Result<bool, RagError> {
        Store::open(&self.database)?.remove_recording_media(
            session_id,
            RecordingMissingReason::Deleted,
            &self.directory,
        )
    }

    pub fn set_budget(&self, budget_bytes: u64) -> Result<Vec<SessionId>, RagError> {
        let store = Store::open(&self.database)?;
        store.set_recording_budget(budget_bytes)?;
        store.enforce_recording_budget(&self.directory)
    }

    /// Rebuilds the selected meeting's derived transcript from retained media.
    ///
    /// The append-only captured timeline is not modified. A successful rerun atomically replaces
    /// the prior derived projection for this session.
    pub fn retranscribe(&self, session_id: SessionId, model_path: &Path) -> Result<usize, String> {
        let store = Store::open(&self.database).map_err(|error| error.to_string())?;
        let recording = store
            .load_recording_reference(session_id)
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "This meeting has no recording metadata.".to_owned())?;
        let path = match recording {
            RecordingReference::Settled(SessionRecording::Available { path, .. }) => path,
            RecordingReference::Settled(SessionRecording::Missing { .. }) => {
                return Err("This meeting's recording is no longer retained.".to_owned());
            }
            RecordingReference::Growing {
                finalization_error: Some(error),
                ..
            } => return Err(format!("This recording did not finalize: {error}")),
            RecordingReference::Growing { .. } => {
                return Err(
                    "Wait for this meeting's recording to finish before re-transcribing."
                        .to_owned(),
                );
            }
        };
        let model = model_path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("local-whisper-model")
            .to_owned();
        let mut transcriber =
            LaggedRecordingTranscriber::new(path, RecordingConfig::new(AsrConfig::new(model_path)))
                .map_err(|error| error.to_string())?;
        let utterances = transcriber
            .transcribe_available(RecordingStatus::Complete)
            .map_err(|error| error.to_string())?
            .into_iter()
            .filter_map(|update| match update {
                TranscriptUpdate::Final(utterance) => Some(utterance),
                TranscriptUpdate::Partial(_) => None,
            })
            .collect::<Vec<_>>();
        store
            .replace_derived_transcript(session_id, &model, &utterances)
            .map_err(|error| error.to_string())?;
        Ok(utterances.len())
    }

    #[must_use]
    pub fn recording_directory(&self) -> &Path {
        &self.directory
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use sotto_core::types::{MediaTimeMapping, RecordingContainer};
    use sotto_core::{CaptureTarget, Session, TargetKind};

    use super::*;

    #[test]
    fn library_reports_usage_and_delete_without_deleting_meeting()
    -> Result<(), Box<dyn std::error::Error>> {
        let directory = tempfile::tempdir()?;
        let database = directory.path().join("sotto.sqlite3");
        let recordings = directory.path().join("recordings");
        std::fs::create_dir_all(&recordings)?;
        let store = Store::open(&database)?;
        let session_id = SessionId::new(27);
        let mut session = Session::new(
            session_id,
            CaptureTarget {
                bundle_id: Some("us.zoom.xos".to_owned()),
                display_name: "Zoom".to_owned(),
                window_title: Some("Retention review".to_owned()),
                kind: TargetKind::Window,
                audio_scoped: true,
            },
            1_000,
        );
        session.end(2_000);
        store.save_session(&session)?;
        let path = recordings.join("27.mp4");
        std::fs::write(&path, vec![1_u8; 12])?;
        store.save_recording(&SessionRecording::Available {
            session_id,
            path: path.to_string_lossy().into_owned(),
            container: RecordingContainer::Mp4,
            duration: Duration::from_secs(1),
            byte_size: 12,
            time_mapping: MediaTimeMapping::IDENTITY,
        })?;
        let library = RecordingLibrary::new(&database, &recordings);

        let before = library.snapshot()?;
        assert_eq!(
            before.usage.used_bytes, 12,
            "library must report retained bytes"
        );
        assert_eq!(before.items.len(), 1, "library must show one recording row");
        assert_eq!(
            before.items[0].session_label, "Retention review",
            "recording rows must retain a recognizable meeting label"
        );
        assert!(
            library.delete(session_id)?,
            "delete must remove available media"
        );
        assert_eq!(
            Store::open(&database)?
                .load_session_record(session_id)?
                .id(),
            session_id,
            "recording delete must not delete the meeting"
        );
        assert_eq!(
            library.snapshot()?.usage.used_bytes,
            0,
            "delete must immediately free accounted media bytes"
        );
        Ok(())
    }
}
