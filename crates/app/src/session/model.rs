//! Persisted, explicit selection and launch-time availability for local Whisper weights.

use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use asr::ModelSize;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

const SETTINGS_FILE_NAME: &str = "transcription-settings.json";
const SETTINGS_VERSION: u32 = 1;

/// The three pinned English Whisper artifacts Sotto offers.
pub const MODEL_CHOICES: [ModelSize; 3] =
    [ModelSize::BaseEn, ModelSize::SmallEn, ModelSize::MediumEn];

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
enum PersistedModelSize {
    #[serde(rename = "base-en")]
    Base,
    #[serde(rename = "small-en")]
    Small,
    #[serde(rename = "medium-en")]
    Medium,
}

impl From<ModelSize> for PersistedModelSize {
    fn from(value: ModelSize) -> Self {
        match value {
            ModelSize::BaseEn => Self::Base,
            ModelSize::SmallEn => Self::Small,
            ModelSize::MediumEn => Self::Medium,
        }
    }
}

impl From<PersistedModelSize> for ModelSize {
    fn from(value: PersistedModelSize) -> Self {
        match value {
            PersistedModelSize::Base => Self::BaseEn,
            PersistedModelSize::Small => Self::SmallEn,
            PersistedModelSize::Medium => Self::MediumEn,
        }
    }
}

#[derive(Debug, Deserialize, Serialize)]
struct PersistedSettings {
    version: u32,
    selected: PersistedModelSize,
}

/// Facts needed by Home, Settings, and every action which needs Whisper.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ModelAvailability {
    /// The launch-time integrity check is running off the UI thread.
    Checking,
    /// No verified selected model exists. Non-transcription features remain usable.
    Missing,
    /// The explicitly selected model is being downloaded and verified.
    Provisioning(asr::model::ProvisionProgress),
    /// The selected model passed its pinned length and SHA-256 checks.
    Ready(PathBuf),
    /// Selection or provisioning failed; the action remains unavailable and says why.
    Error(String),
}

impl ModelAvailability {
    #[must_use]
    pub fn ready_path(&self) -> Option<&Path> {
        match self {
            Self::Ready(path) => Some(path),
            Self::Checking | Self::Missing | Self::Provisioning(_) | Self::Error(_) => None,
        }
    }
}

/// User-owned transcription preference plus the verified state of its artifact.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TranscriptionModel {
    selected: ModelSize,
    availability: ModelAvailability,
    settings_path: PathBuf,
    models_directory: PathBuf,
    override_path: Option<PathBuf>,
}

impl TranscriptionModel {
    #[must_use]
    pub fn load_default() -> Self {
        let support = application_support_directory();
        Self::load(
            support.join(SETTINGS_FILE_NAME),
            support.join("models"),
            std::env::var_os("SOTTO_WHISPER_MODEL").map(PathBuf::from),
        )
    }

    fn load(
        settings_path: PathBuf,
        models_directory: PathBuf,
        override_path: Option<PathBuf>,
    ) -> Self {
        let (selected, availability) = match load_selection(&settings_path) {
            Ok(selected) => (selected, ModelAvailability::Checking),
            Err(error) => (ModelSize::default(), ModelAvailability::Error(error)),
        };
        Self {
            selected,
            availability,
            settings_path,
            models_directory,
            override_path,
        }
    }

    #[must_use]
    pub const fn selected(&self) -> ModelSize {
        self.selected
    }

    #[must_use]
    pub const fn availability(&self) -> &ModelAvailability {
        &self.availability
    }

    #[must_use]
    pub fn ready_path(&self) -> Option<&Path> {
        self.availability.ready_path()
    }

    #[must_use]
    pub fn unavailable_reason(&self) -> Option<String> {
        match &self.availability {
            ModelAvailability::Ready(_) => None,
            ModelAvailability::Checking => Some(
                "Checking the selected transcription model before transcription becomes available."
                    .to_owned(),
            ),
            ModelAvailability::Missing => Some(format!(
                "Choose and download a transcription model on Home before using transcription. {} is selected but is not available yet.",
                model_label(self.selected)
            )),
            ModelAvailability::Provisioning(progress) => {
                Some(super::progress_label(self.selected, *progress))
            }
            ModelAvailability::Error(error) => Some(format!(
                "The selected transcription model is unavailable: {error} Choose a model on Home and retry."
            )),
        }
    }

    /// The reason a Home card presents, where the choice panel is on the same surface.
    ///
    /// Deliberately not [`Self::unavailable_reason`]. That sentence has to name the surface to go
    /// to, and while a download runs it carries the percentage, the byte counts and the cancel
    /// instruction — all of which belong beside the progress bar and its Cancel control. Repeating
    /// it under every start card put the same sentence on Home four times, three of them next to a
    /// control that cannot act on it.
    #[must_use]
    pub fn home_card_reason(&self) -> Option<String> {
        match &self.availability {
            ModelAvailability::Ready(_) => None,
            ModelAvailability::Checking => {
                Some("Checking the selected transcription model.".to_owned())
            }
            ModelAvailability::Missing => Some("Choose a transcription model above.".to_owned()),
            ModelAvailability::Provisioning(_) => Some(format!(
                "{} is still downloading.",
                model_label(self.selected)
            )),
            ModelAvailability::Error(_) => Some(
                "The selected transcription model is unavailable — choose one above.".to_owned(),
            ),
        }
    }

    pub fn choose(&mut self, selected: ModelSize) -> Result<(), String> {
        save_selection(&self.settings_path, selected)?;
        self.selected = selected;
        self.override_path = None;
        self.availability = ModelAvailability::Missing;
        Ok(())
    }

    pub(super) fn inspect(&self) -> ModelAvailability {
        self.override_path.as_deref().map_or_else(
            || inspect_managed_model(&self.models_directory, self.selected),
            inspect_override,
        )
    }

    pub(super) fn mark_missing(&mut self) {
        self.availability = ModelAvailability::Missing;
    }

    pub(super) fn set_provisioning(&mut self, progress: asr::model::ProvisionProgress) {
        self.availability = ModelAvailability::Provisioning(progress);
    }

    pub(super) fn set_ready(&mut self, path: PathBuf) {
        self.availability = ModelAvailability::Ready(path);
    }

    pub(super) fn set_error(&mut self, error: String) {
        self.availability = ModelAvailability::Error(error);
    }

    pub(super) fn set_availability(&mut self, availability: ModelAvailability) {
        self.availability = availability;
    }

    #[cfg(test)]
    pub(super) fn for_test(
        settings_path: PathBuf,
        models_directory: PathBuf,
        override_path: Option<PathBuf>,
    ) -> Self {
        Self::load(settings_path, models_directory, override_path)
    }
}

#[must_use]
pub const fn model_label(size: ModelSize) -> &'static str {
    match size {
        ModelSize::BaseEn => "base.en",
        ModelSize::SmallEn => "small.en",
        ModelSize::MediumEn => "medium.en",
    }
}

#[must_use]
pub fn download_size_label(size: ModelSize) -> String {
    let bytes = size.spec().byte_len;
    if bytes >= 1_000_000_000 {
        format!("{:.2} GB", bytes as f64 / 1_000_000_000.0)
    } else {
        format!("{:.0} MB", bytes as f64 / 1_000_000.0)
    }
}

fn application_support_directory() -> PathBuf {
    application_support_directory_from(std::env::var_os("HOME").filter(|home| !home.is_empty()))
}

fn application_support_directory_from(home: Option<std::ffi::OsString>) -> PathBuf {
    home.map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
        .join("Library")
        .join("Application Support")
        .join("Sotto")
}

fn load_selection(path: &Path) -> Result<ModelSize, String> {
    match fs::read(path) {
        Ok(bytes) => {
            let settings: PersistedSettings = serde_json::from_slice(&bytes)
                .map_err(|error| format!("invalid transcription settings: {error}"))?;
            if settings.version != SETTINGS_VERSION {
                return Err("unsupported transcription settings version".to_owned());
            }
            Ok(settings.selected.into())
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(ModelSize::default()),
        Err(error) => Err(format!("could not read {}: {error}", path.display())),
    }
}

fn save_selection(path: &Path, selected: ModelSize) -> Result<(), String> {
    let parent = path
        .parent()
        .ok_or_else(|| "transcription settings path has no parent".to_owned())?;
    fs::create_dir_all(parent)
        .map_err(|error| format!("could not create {}: {error}", parent.display()))?;
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
            .map_err(|error| format!("could not create {}: {error}", temporary.display()))?;
        let bytes = serde_json::to_vec_pretty(&PersistedSettings {
            version: SETTINGS_VERSION,
            selected: selected.into(),
        })
        .map_err(|error| format!("could not encode transcription settings: {error}"))?;
        file.write_all(&bytes)
            .map_err(|error| format!("could not write {}: {error}", temporary.display()))?;
        file.sync_all()
            .map_err(|error| format!("could not sync {}: {error}", temporary.display()))?;
        fs::rename(&temporary, path)
            .map_err(|error| format!("could not replace {}: {error}", path.display()))
    })();
    if result.is_err() {
        let _ = fs::remove_file(temporary);
    }
    result
}

fn inspect_override(path: &Path) -> ModelAvailability {
    match fs::metadata(path) {
        Ok(metadata) if metadata.is_file() && metadata.len() > 0 => {
            ModelAvailability::Ready(path.to_owned())
        }
        Ok(_) => ModelAvailability::Error(format!(
            "SOTTO_WHISPER_MODEL is not a non-empty file: {}",
            path.display()
        )),
        Err(error) => ModelAvailability::Error(format!(
            "could not read SOTTO_WHISPER_MODEL at {}: {error}",
            path.display()
        )),
    }
}

fn inspect_managed_model(directory: &Path, selected: ModelSize) -> ModelAvailability {
    let spec = selected.spec();
    let path = directory.join(spec.file_name);
    let metadata = match fs::metadata(&path) {
        Ok(metadata) if metadata.is_file() => metadata,
        Ok(_) => return ModelAvailability::Missing,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return ModelAvailability::Missing;
        }
        Err(error) => return ModelAvailability::Error(error.to_string()),
    };
    if metadata.len() != spec.byte_len {
        return ModelAvailability::Missing;
    }
    match sha256_file(&path) {
        Ok(digest) if digest == spec.sha256 => ModelAvailability::Ready(path),
        Ok(_) => ModelAvailability::Missing,
        Err(error) => ModelAvailability::Error(error),
    }
}

fn sha256_file(path: &Path) -> Result<String, String> {
    let mut file =
        File::open(path).map_err(|error| format!("could not open {}: {error}", path.display()))?;
    let mut digest = Sha256::new();
    let mut buffer = vec![0_u8; 1024 * 1024];
    loop {
        let read = file
            .read(&mut buffer)
            .map_err(|error| format!("could not read {}: {error}", path.display()))?;
        if read == 0 {
            break;
        }
        digest.update(&buffer[..read]);
    }
    Ok(hex::encode(digest.finalize()))
}

#[cfg(test)]
mod tests {
    use super::{
        ModelAvailability, TranscriptionModel, download_size_label, load_selection, model_label,
    };
    use asr::ModelSize;

    #[test]
    fn every_choice_uses_the_pinned_spec_for_its_download_cost() {
        assert_eq!(download_size_label(ModelSize::BaseEn), "148 MB");
        assert_eq!(download_size_label(ModelSize::SmallEn), "488 MB");
        assert_eq!(download_size_label(ModelSize::MediumEn), "1.53 GB");
        assert_eq!(model_label(ModelSize::SmallEn), "small.en");
    }

    /// A start card says it is waiting; it does not restate the download's own progress line.
    ///
    /// Home draws the choice panel and then one card per way of starting. Handing every card
    /// `unavailable_reason` put the identical `Downloading small.en… 56% (276 of 488 MB).
    /// Cancelling keeps a resumable partial.` sentence on screen four times — three of them beside
    /// a control that can neither cancel nor resume it.
    #[test]
    fn a_start_card_does_not_repeat_the_downloads_own_progress_line()
    -> Result<(), Box<dyn std::error::Error>> {
        let directory = tempfile::tempdir()?;
        let mut state = TranscriptionModel::for_test(
            directory.path().join("transcription.json"),
            directory.path().join("models"),
            None,
        );
        state.set_provisioning(asr::model::ProvisionProgress {
            phase: asr::model::ProvisionPhase::Downloading,
            downloaded: 276_000_000,
            total: 487_614_201,
        });

        let panel = state
            .unavailable_reason()
            .ok_or_else(|| std::io::Error::other("the panel must state progress"))?;
        let card = state
            .home_card_reason()
            .ok_or_else(|| std::io::Error::other("the card must state it is blocked"))?;
        assert!(
            panel.contains("276") && panel.contains('%'),
            "the panel beside Cancel keeps the byte counts and the percentage, got {panel:?}"
        );
        assert_ne!(
            card, panel,
            "a card must not restate the sentence the panel above it already carries"
        );
        assert!(
            !card.contains('%') && !card.to_lowercase().contains("cancel"),
            "a card offers neither the percentage nor the cancel instruction, got {card:?}"
        );
        assert!(
            card.contains("small.en"),
            "the card still names the model it is waiting on, got {card:?}"
        );
        Ok(())
    }

    #[test]
    fn choice_persists_without_downloading_an_unselected_default()
    -> Result<(), Box<dyn std::error::Error>> {
        let directory = tempfile::tempdir()?;
        let settings = directory.path().join("transcription.json");
        let models = directory.path().join("models");
        let mut state = TranscriptionModel::for_test(settings.clone(), models.clone(), None);
        assert_eq!(state.selected(), ModelSize::SmallEn);
        assert_eq!(state.availability(), &ModelAvailability::Checking);

        state.choose(ModelSize::MediumEn)?;
        assert_eq!(load_selection(&settings), Ok(ModelSize::MediumEn));
        assert_eq!(state.selected(), ModelSize::MediumEn);
        assert_eq!(state.availability(), &ModelAvailability::Missing);
        assert!(!models.join(ModelSize::SmallEn.spec().file_name).exists());
        Ok(())
    }

    #[test]
    fn a_nonempty_override_is_available_without_managed_weights()
    -> Result<(), Box<dyn std::error::Error>> {
        let directory = tempfile::tempdir()?;
        let override_path = directory.path().join("custom.bin");
        std::fs::write(&override_path, b"custom")?;
        let state = TranscriptionModel::for_test(
            directory.path().join("settings.json"),
            directory.path().join("models"),
            Some(override_path.clone()),
        );
        assert_eq!(state.availability(), &ModelAvailability::Checking);
        assert_eq!(state.inspect(), ModelAvailability::Ready(override_path));
        Ok(())
    }
}
