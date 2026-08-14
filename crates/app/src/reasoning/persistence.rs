//! Identifier-only reasoning settings persisted beside Sotto's session database.

use std::{
    fs::{self, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use serde::{Deserialize, Serialize};

use super::ReasoningError;

pub(super) const SETTINGS_FILE_NAME: &str = "reasoning-settings.json";

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(super) struct PersistedSettings {
    pub version: u32,
    pub backends: PersistedBackends,
    pub roles: PersistedRoles,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(super) struct PersistedBackends {
    pub openai_responses_model_id: String,
    #[serde(default = "default_codex_model_id")]
    pub codex_model_id: String,
    #[serde(default)]
    pub codex_experimental_consent_version: Option<u32>,
}

fn default_codex_model_id() -> String {
    super::DEFAULT_CODEX_MODEL.to_owned()
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub(super) struct PersistedRoles {
    pub watcher: Option<String>,
    pub suggester: Option<String>,
    pub summarizer: Option<String>,
}

pub(super) fn application_support_path() -> Result<PathBuf, ReasoningError> {
    let home = std::env::var_os("HOME")
        .filter(|home| !home.is_empty())
        .ok_or_else(|| ReasoningError::Persistence("could not locate the home directory".into()))?;
    Ok(PathBuf::from(home)
        .join("Library")
        .join("Application Support")
        .join("Sotto")
        .join(SETTINGS_FILE_NAME))
}

pub(super) fn load(path: &Path) -> Result<Option<PersistedSettings>, ReasoningError> {
    match fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes).map(Some).map_err(|error| {
            ReasoningError::Persistence(format!("invalid settings file: {error}"))
        }),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(ReasoningError::Persistence(format!(
            "could not read {}: {error}",
            path.display()
        ))),
    }
}

pub(super) fn save(path: &Path, settings: &PersistedSettings) -> Result<(), ReasoningError> {
    let parent = path.parent().ok_or_else(|| {
        ReasoningError::Persistence("reasoning settings path has no parent directory".into())
    })?;
    fs::create_dir_all(parent).map_err(|error| {
        ReasoningError::Persistence(format!("could not create {}: {error}", parent.display()))
    })?;
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let temporary = parent.join(format!(
        ".{SETTINGS_FILE_NAME}.{}-{nonce}.tmp",
        std::process::id()
    ));
    let result = (|| {
        let mut file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&temporary)
            .map_err(|error| {
                ReasoningError::Persistence(format!(
                    "could not create {}: {error}",
                    temporary.display()
                ))
            })?;
        let bytes = serde_json::to_vec_pretty(settings).map_err(|error| {
            ReasoningError::Persistence(format!("could not encode settings: {error}"))
        })?;
        file.write_all(&bytes).map_err(|error| {
            ReasoningError::Persistence(format!("could not write {}: {error}", temporary.display()))
        })?;
        file.sync_all().map_err(|error| {
            ReasoningError::Persistence(format!("could not sync {}: {error}", temporary.display()))
        })?;
        fs::rename(&temporary, path).map_err(|error| {
            ReasoningError::Persistence(format!(
                "could not atomically replace {}: {error}",
                path.display()
            ))
        })?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(temporary);
    }
    result
}
