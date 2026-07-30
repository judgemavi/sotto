//! Explicit, picker-scoped live session wiring.

use std::{
    path::{Path, PathBuf},
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};

use asr::{Config as AsrConfig, WhisperTranscriber};
use capture::macos::{CaptureStatus, MacCapture, PickedTarget};
use rag::{Store, TimelinePersistence};
use sotto_core::{Pipeline, Session, SessionId, Source};
use vad::{SileroVad, VadConfig};

use crate::devwindow::TimelineIngress;

/// Result of an explicitly requested picker-first start.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum StartOutcome {
    Cancelled,
    Ended(CaptureStatus),
}

/// Presents the system picker, then runs one capture session to a terminal status.
pub(crate) async fn pick_and_run(ingress: TimelineIngress) -> Result<StartOutcome, String> {
    let model_path = whisper_model_path()?;
    let Some(target) = MacCapture::pick_target().await else {
        return Ok(StartOutcome::Cancelled);
    };
    run_on_worker(target, model_path, ingress).await
}

async fn run_on_worker(
    target: PickedTarget,
    model_path: PathBuf,
    ingress: TimelineIngress,
) -> Result<StartOutcome, String> {
    let (sender, receiver) = tokio::sync::oneshot::channel();
    std::thread::Builder::new()
        .name("sotto-live-session".to_owned())
        .spawn(move || {
            let result = tokio::runtime::Builder::new_current_thread()
                .enable_time()
                .build()
                .map_err(|error| format!("could not create session runtime: {error}"))
                .and_then(|runtime| runtime.block_on(run(target, &model_path, ingress)));
            let _ = sender.send(result);
        })
        .map_err(|error| format!("could not start session worker: {error}"))?;
    receiver
        .await
        .map_err(|_| "session worker stopped unexpectedly".to_owned())?
}

async fn run(
    target: PickedTarget,
    model_path: &Path,
    ingress: TimelineIngress,
) -> Result<StartOutcome, String> {
    let capture_target = target.description().clone();
    let capture = target.into_capture();
    let mut statuses = capture.subscribe_status();
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| format!("system clock precedes Unix epoch: {error}"))?;
    let session = Session::new(
        SessionId::new(now.as_nanos()),
        capture_target,
        u64::try_from(now.as_millis()).unwrap_or(u64::MAX),
    );
    let persistence = persistence()?;
    let mut asr_config = AsrConfig::new(model_path);
    // Silero is a separate pipeline producer. The object-safe transcriber seam does
    // not carry its speech state into Whisper's concrete optional gate.
    asr_config.vad_gating = false;
    let pipeline = Pipeline::builder(session)
        .capture(capture)
        .vad(
            SileroVad::new(Source::Mic, VadConfig::default()).map_err(|error| error.to_string())?,
            SileroVad::new(Source::System, VadConfig::default())
                .map_err(|error| error.to_string())?,
        )
        .transcriber(WhisperTranscriber::new(asr_config).map_err(|error| error.to_string())?)
        .annotator(prosody::Annotator::default())
        .persistence(persistence)
        .start()
        .map_err(|error| error.to_string())?;
    let mut events = pipeline.events().subscribe("app-timeline-ingress");

    let terminal = loop {
        tokio::select! {
            event = events.recv() => {
                match event {
                    Ok(event) => {
                        if ingress.send(event).await.is_err() {
                            break CaptureStatus::Failed;
                        }
                    }
                    Err(_) => break CaptureStatus::Failed,
                }
            }
            status = statuses.recv() => {
                match status {
                    Ok(status @ (CaptureStatus::TargetEnded | CaptureStatus::UserStopped | CaptureStatus::Failed)) => break status,
                    Ok(CaptureStatus::Stopped | CaptureStatus::Starting | CaptureStatus::Running | CaptureStatus::Stopping) => {}
                    Err(_) => break CaptureStatus::Failed,
                }
            }
        }
    };
    let stopping = tokio::spawn(pipeline.stop());
    while let Ok(event) = events.recv().await {
        if ingress.send(event).await.is_err() {
            break;
        }
    }
    stopping
        .await
        .map_err(|error| format!("session shutdown failed: {error}"))?;
    Ok(StartOutcome::Ended(terminal))
}

fn whisper_model_path() -> Result<PathBuf, String> {
    let path = std::env::var_os("SOTTO_WHISPER_MODEL")
        .map(PathBuf::from)
        .ok_or_else(|| {
            "Whisper model is missing. Set SOTTO_WHISPER_MODEL before starting capture.".to_owned()
        })?;
    if !path.is_file() {
        return Err(format!("Whisper model is missing: {}", path.display()));
    }
    Ok(path)
}

fn persistence() -> Result<Arc<TimelinePersistence>, String> {
    let path = match std::env::var_os("SOTTO_DATABASE") {
        Some(path) => PathBuf::from(path),
        None => {
            let home = std::env::var_os("HOME")
                .filter(|home| !home.is_empty())
                .ok_or_else(|| "could not locate the user's home directory".to_owned())?;
            let directory = PathBuf::from(home)
                .join("Library")
                .join("Application Support")
                .join("Sotto");
            std::fs::create_dir_all(&directory).map_err(|error| {
                format!(
                    "could not create application support directory {}: {error}",
                    directory.display()
                )
            })?;
            directory.join("sotto.sqlite3")
        }
    };
    let store = Store::open(&path).map_err(|error| {
        format!(
            "could not open session database {}: {error}",
            path.display()
        )
    })?;
    Ok(Arc::new(TimelinePersistence::new(Arc::new(store))))
}
