//! App-side assembly for the markdown vault projection.
//!
//! `rag` owns file safety and markdown diffs. This module is the intentional upward seam which
//! composes `insight` documents, translates external changes to overlay operations, and supplies
//! the append-only record projection.

use std::{
    collections::BTreeMap,
    fmt,
    fs::{self, OpenOptions},
    hash::{Hash, Hasher},
    io::Write as _,
    path::{Path, PathBuf},
    sync::mpsc,
    thread,
    time::{SystemTime, UNIX_EPOCH},
};

use chrono::{DateTime, Utc};
use gpui::Context;
use insight::{
    NotesOverlayOperation, OverlayTarget, PresentedNotesBlock, PresentedNotesBlockId,
    PresentedNotesDocument, RecordingNotes, RecordingNotesSectionKind,
    append_notes_overlay_operation, load_latest_grounded_notes_status,
    load_presented_notes_document,
};
use notify::{RecursiveMode, Watcher as _};
use rag::{
    Store, VaultBlock, VaultCitation, VaultEdit, VaultEntry, VaultError, VaultMirror, VaultSync,
};
use serde::{Deserialize, Serialize};
use sotto_core::{Entry, EntryId, EventPayload, RagError, SessionId};

use crate::persistence_runtime::block_on;

const VAULT_SETTINGS_FILE: &str = "vault-settings.json";

/// The only persisted vault choices. The folder is retained while mirroring is off so disabling
/// never implies deleting either the preference or any markdown already written there.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct VaultPreferences {
    pub enabled: bool,
    pub folder: Option<PathBuf>,
}

/// A durable status receipt shared by the background mirror and the lazily opened settings sheet.
/// Keeping the last failure outside the workspace means a missing folder is still visible after a
/// relaunch instead of becoming a transient stderr line.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum VaultRuntimeStatus {
    Disabled {
        folder: Option<PathBuf>,
    },
    Synced {
        folder: PathBuf,
        files: usize,
    },
    Syncing {
        folder: PathBuf,
    },
    Failed {
        folder: Option<PathBuf>,
        message: String,
    },
}

impl Default for VaultRuntimeStatus {
    fn default() -> Self {
        Self::Disabled { folder: None }
    }
}

impl VaultRuntimeStatus {
    #[must_use]
    pub fn summary(&self) -> String {
        match self {
            Self::Disabled {
                folder: Some(folder),
            } => format!(
                "Off · files already in {} remain on disk.",
                folder.display()
            ),
            Self::Disabled { folder: None } => "Off · choose a local folder to begin.".to_owned(),
            Self::Synced { folder, files } => format!(
                "On · {files} markdown file(s) mirrored in {}.",
                folder.display()
            ),
            Self::Syncing { folder } => {
                format!("On · syncing markdown in {}.", folder.display())
            }
            Self::Failed { folder, message } => folder.as_ref().map_or_else(
                || format!("Mirror unavailable · {message}"),
                |folder| format!("Mirror paused at {} · {message}", folder.display()),
            ),
        }
    }
}

#[must_use]
pub fn vault_settings_path(database: &Path) -> PathBuf {
    database.with_file_name(VAULT_SETTINGS_FILE)
}

pub fn load_vault_preferences(database: &Path) -> Result<VaultPreferences, String> {
    read_json(&vault_settings_path(database))
}

pub fn save_vault_preferences(
    database: &Path,
    preferences: &VaultPreferences,
) -> Result<(), String> {
    atomic_write_json(&vault_settings_path(database), preferences)
}

enum MirrorSignal {
    Changed(Result<(), String>),
    Stop,
}

struct VaultMirrorWorker {
    sender: mpsc::Sender<MirrorSignal>,
    handle: Option<thread::JoinHandle<()>>,
}

impl VaultMirrorWorker {
    fn stop(&mut self) {
        let _ = self.sender.send(MirrorSignal::Stop);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

impl Drop for VaultMirrorWorker {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Owns the mirror lifecycle. A disabled controller has no worker and performs no periodic work.
/// When enabled, `notify` blocks on native filesystem events and the worker pushes status changes
/// back to this entity over a channel; neither preferences nor status are polled.
pub struct VaultMirrorController {
    database: PathBuf,
    preferences: VaultPreferences,
    status: VaultRuntimeStatus,
    worker: Option<VaultMirrorWorker>,
    generation: u64,
}

impl VaultMirrorController {
    #[must_use]
    pub fn new(database: PathBuf, cx: &mut Context<Self>) -> Self {
        let (preferences, status) = match load_vault_preferences(&database) {
            Ok(preferences) => {
                let status = VaultRuntimeStatus::Disabled {
                    folder: preferences.folder.clone(),
                };
                (preferences, status)
            }
            Err(message) => (
                VaultPreferences::default(),
                VaultRuntimeStatus::Failed {
                    folder: None,
                    message,
                },
            ),
        };
        let mut controller = Self {
            database,
            preferences,
            status,
            worker: None,
            generation: 0,
        };
        if controller.preferences.enabled {
            controller.start(cx);
        }
        controller
    }

    #[must_use]
    pub fn preferences(&self) -> &VaultPreferences {
        &self.preferences
    }

    #[must_use]
    pub fn status(&self) -> &VaultRuntimeStatus {
        &self.status
    }

    pub fn set_folder(&mut self, folder: PathBuf, cx: &mut Context<Self>) -> Result<(), String> {
        let previous = self.preferences.clone();
        self.preferences.folder = Some(folder);
        if let Err(error) = save_vault_preferences(&self.database, &self.preferences) {
            self.preferences = previous;
            return Err(error);
        }
        if self.preferences.enabled {
            self.start(cx);
        } else {
            self.status = VaultRuntimeStatus::Disabled {
                folder: self.preferences.folder.clone(),
            };
            cx.notify();
        }
        Ok(())
    }

    pub fn set_enabled(&mut self, enabled: bool, cx: &mut Context<Self>) -> Result<(), String> {
        if enabled && self.preferences.folder.is_none() {
            return Err("Choose a local vault folder first.".to_owned());
        }
        let previous = self.preferences.clone();
        self.preferences.enabled = enabled;
        if let Err(error) = save_vault_preferences(&self.database, &self.preferences) {
            self.preferences = previous;
            return Err(error);
        }
        if enabled {
            self.start(cx);
        } else {
            self.stop();
            self.status = VaultRuntimeStatus::Disabled {
                folder: self.preferences.folder.clone(),
            };
            cx.notify();
        }
        Ok(())
    }

    fn stop(&mut self) {
        self.generation = self.generation.wrapping_add(1);
        if let Some(mut worker) = self.worker.take() {
            worker.stop();
        }
    }

    fn start(&mut self, cx: &mut Context<Self>) {
        self.stop();
        let Some(folder) = self.preferences.folder.clone() else {
            return;
        };
        self.status = VaultRuntimeStatus::Syncing {
            folder: folder.clone(),
        };
        cx.notify();
        let generation = self.generation;
        let (status_sender, mut status_receiver) = tokio::sync::mpsc::unbounded_channel();
        match spawn_mirror_worker(self.database.clone(), folder.clone(), status_sender) {
            Ok(worker) => self.worker = Some(worker),
            Err(message) => {
                self.status = VaultRuntimeStatus::Failed {
                    folder: Some(folder),
                    message,
                };
                cx.notify();
                return;
            }
        }
        let controller = cx.entity().downgrade();
        cx.spawn(async move |_, cx| {
            while let Some(status) = status_receiver.recv().await {
                if controller
                    .update(cx, |controller, cx| {
                        if controller.generation == generation {
                            controller.status = status;
                            cx.notify();
                        }
                    })
                    .is_err()
                {
                    return;
                }
            }
        })
        .detach();
    }
}

fn spawn_mirror_worker(
    database: PathBuf,
    folder: PathBuf,
    status_sender: tokio::sync::mpsc::UnboundedSender<VaultRuntimeStatus>,
) -> Result<VaultMirrorWorker, String> {
    let (signal_sender, signal_receiver) = mpsc::channel();
    let watcher_sender = signal_sender.clone();
    let mut watcher = notify::recommended_watcher(move |event: notify::Result<notify::Event>| {
        let _ = watcher_sender.send(MirrorSignal::Changed(
            event.map(|_| ()).map_err(|error| error.to_string()),
        ));
    })
    .map_err(|error| format!("Could not watch the vault: {error}"))?;
    watcher
        .watch(&folder, RecursiveMode::NonRecursive)
        .map_err(|error| format!("Could not watch {}: {error}", folder.display()))?;
    let database_parent = database
        .parent()
        .ok_or_else(|| format!("{} has no parent folder", database.display()))?;
    if database_parent != folder {
        watcher
            .watch(database_parent, RecursiveMode::NonRecursive)
            .map_err(|error| format!("Could not watch {}: {error}", database_parent.display()))?;
    }
    let handle = thread::Builder::new()
        .name("sotto-vault-mirror".to_owned())
        .spawn(move || mirror_worker(database, folder, watcher, signal_receiver, status_sender))
        .map_err(|error| format!("Could not start the vault mirror: {error}"))?;
    Ok(VaultMirrorWorker {
        sender: signal_sender,
        handle: Some(handle),
    })
}

fn mirror_worker(
    database: PathBuf,
    folder: PathBuf,
    _watcher: notify::RecommendedWatcher,
    signal_receiver: mpsc::Receiver<MirrorSignal>,
    status_sender: tokio::sync::mpsc::UnboundedSender<VaultRuntimeStatus>,
) {
    push_sync_status(&database, &folder, &status_sender);
    let mut fingerprint = mirror_fingerprint(&database, &folder);
    while let Ok(signal) = signal_receiver.recv() {
        match signal {
            MirrorSignal::Stop => return,
            MirrorSignal::Changed(Err(message)) => {
                let _ = status_sender.send(VaultRuntimeStatus::Failed {
                    folder: Some(folder.clone()),
                    message,
                });
            }
            MirrorSignal::Changed(Ok(())) => {
                let changed = mirror_fingerprint(&database, &folder);
                if changed != fingerprint {
                    push_sync_status(&database, &folder, &status_sender);
                    fingerprint = mirror_fingerprint(&database, &folder);
                }
            }
        }
    }
}

fn push_sync_status(
    database: &Path,
    folder: &Path,
    sender: &tokio::sync::mpsc::UnboundedSender<VaultRuntimeStatus>,
) {
    let status = match sync_vault(database, folder) {
        Ok(VaultSync::Synced { files }) => VaultRuntimeStatus::Synced {
            folder: folder.to_path_buf(),
            files: files.len(),
        },
        Ok(VaultSync::NeedsIngestion { .. }) => VaultRuntimeStatus::Failed {
            folder: Some(folder.to_path_buf()),
            message: "An external edit is still changing; Sotto will retry on its next filesystem change."
                .to_owned(),
        },
        Err(error) => VaultRuntimeStatus::Failed {
            folder: Some(folder.to_path_buf()),
            message: error.to_string(),
        },
    };
    let _ = sender.send(status);
}

fn mirror_fingerprint(database: &Path, folder: &Path) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    for path in [
        database.to_path_buf(),
        PathBuf::from(format!("{}-wal", database.display())),
    ] {
        hash_file_metadata(&path, &mut hasher);
    }
    if let Ok(entries) = fs::read_dir(folder) {
        let mut paths = entries
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|path| path.extension().is_some_and(|extension| extension == "md"))
            .collect::<Vec<_>>();
        paths.sort();
        for path in paths {
            path.hash(&mut hasher);
            hash_file_metadata(&path, &mut hasher);
        }
    } else {
        folder.hash(&mut hasher);
    }
    hasher.finish()
}

fn hash_file_metadata(path: &Path, hasher: &mut impl Hasher) {
    path.hash(hasher);
    match fs::metadata(path) {
        Ok(metadata) => {
            metadata.len().hash(hasher);
            metadata
                .modified()
                .ok()
                .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
                .map(|duration| duration.as_nanos())
                .hash(hasher);
        }
        Err(error) => error.kind().hash(hasher),
    }
}

fn read_json<T>(path: &Path) -> Result<T, String>
where
    T: for<'de> Deserialize<'de> + Default,
{
    match fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .map_err(|error| format!("Could not read {}: {error}", path.display())),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(T::default()),
        Err(error) => Err(format!("Could not read {}: {error}", path.display())),
    }
}

fn atomic_write_json(path: &Path, value: &impl Serialize) -> Result<(), String> {
    let parent = path
        .parent()
        .ok_or_else(|| format!("{} has no parent folder", path.display()))?;
    fs::create_dir_all(parent)
        .map_err(|error| format!("Could not create {}: {error}", parent.display()))?;
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let temporary = parent.join(format!(
        ".{}.{}-{nonce}.tmp",
        path.file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("vault"),
        std::process::id()
    ));
    let result = (|| {
        let bytes = serde_json::to_vec_pretty(value)
            .map_err(|error| format!("Could not encode vault state: {error}"))?;
        let mut file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&temporary)
            .map_err(|error| format!("Could not create {}: {error}", temporary.display()))?;
        file.write_all(&bytes)
            .map_err(|error| format!("Could not write {}: {error}", temporary.display()))?;
        file.sync_all()
            .map_err(|error| format!("Could not sync {}: {error}", temporary.display()))?;
        fs::rename(&temporary, path)
            .map_err(|error| format!("Could not replace {}: {error}", path.display()))
    })();
    if result.is_err() {
        let _ = fs::remove_file(temporary);
    }
    result
}

#[derive(Debug)]
pub enum VaultControllerError {
    Projection(VaultError),
    Persistence(RagError),
    Notes(String),
}

impl fmt::Display for VaultControllerError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Projection(error) => error.fmt(formatter),
            Self::Persistence(error) => error.fmt(formatter),
            Self::Notes(error) => formatter.write_str(error),
        }
    }
}

impl std::error::Error for VaultControllerError {}

impl From<VaultError> for VaultControllerError {
    fn from(value: VaultError) -> Self {
        Self::Projection(value)
    }
}

impl From<RagError> for VaultControllerError {
    fn from(value: RagError) -> Self {
        Self::Persistence(value)
    }
}

struct ComposedEntry {
    projection: VaultEntry,
    artifact: Option<RecordingNotes>,
    document: Option<PresentedNotesDocument>,
}

/// Rebuilds the complete vault and ingests at most one observed external edit batch.
///
/// `VaultMirror` returns before writing when an editor raced Sotto. The controller appends those
/// changes to the canonical overlay, recomposes from SQLite, and retries; the retry is what proves
/// neither the external edit nor a concurrent in-app edit is overwritten.
pub fn sync_vault(database: &Path, root: &Path) -> Result<VaultSync, VaultControllerError> {
    block_on(async {
        let store = Store::open(database).await?;
        let composed = compose_library(&store).await?;
        let inputs = composed
            .values()
            .map(|entry| entry.projection.clone())
            .collect::<Vec<_>>();
        let mirror = VaultMirror::new(root);
        match mirror.sync(&inputs)? {
            VaultSync::Synced { files } => Ok(VaultSync::Synced { files }),
            VaultSync::NeedsIngestion {
                entry_id,
                revision,
                edits,
                ..
            } => {
                let entry = composed.get(&entry_id).ok_or_else(|| {
                    VaultControllerError::Notes(
                        "vault edit refers to an entry no longer in the library".to_owned(),
                    )
                })?;
                let artifact = entry.artifact.as_ref().ok_or_else(|| {
                    VaultControllerError::Notes(
                        "vault note has no generated artifact to receive an overlay".to_owned(),
                    )
                })?;
                let document = entry.document.as_ref().ok_or_else(|| {
                    VaultControllerError::Notes(
                        "vault note has no composed document to receive an overlay".to_owned(),
                    )
                })?;
                append_external_edits(&store, entry_id, artifact, document, &edits).await?;
                mirror.acknowledge_external_edit(entry_id, &revision)?;
                let recomposed = compose_library(&store).await?;
                let inputs = recomposed
                    .values()
                    .map(|entry| entry.projection.clone())
                    .collect::<Vec<_>>();
                Ok(mirror.sync(&inputs)?)
            }
        }
    })
}

async fn compose_library(
    store: &Store,
) -> Result<BTreeMap<EntryId, ComposedEntry>, VaultControllerError> {
    let entries = store.list_entries().await?;
    let sessions = store
        .list_sessions()
        .await?
        .into_iter()
        .map(|session| (session.id, session))
        .collect::<BTreeMap<_, _>>();
    let mut output = BTreeMap::new();
    for entry in entries {
        let mut artifact = None;
        let mut document = None;
        let mut artifact_session = None;
        for session_id in entry.session_ids().iter().rev() {
            let cached = load_latest_grounded_notes_status(store, *session_id)
                .await
                .map_err(|error| VaultControllerError::Notes(error.to_string()))?;
            let Some(cached) = cached else {
                continue;
            };
            let presented =
                load_presented_notes_document(store, entry.id(), &cached.report.artifact)
                    .await
                    .map_err(|error| VaultControllerError::Notes(error.to_string()))?;
            artifact_session = Some(*session_id);
            artifact = Some(cached.report.artifact);
            document = Some(presented);
            break;
        }
        let title = entry.title().map_or_else(
            || {
                entry
                    .session_ids()
                    .first()
                    .and_then(|id| sessions.get(id))
                    .map_or_else(
                        || format!("Entry {}", entry.id().get()),
                        |session| session.capture_target.display_name.clone(),
                    )
            },
            ToString::to_string,
        );
        let capture_targets = entry
            .session_ids()
            .iter()
            .filter_map(|id| sessions.get(id))
            .map(|session| session.capture_target.display_name.clone())
            .fold(Vec::<String>::new(), |mut targets, target| {
                if !targets.contains(&target) {
                    targets.push(target);
                }
                targets
            });
        let date = entry_date(&entry, &sessions);
        let blocks = document.as_ref().map_or_else(Vec::new, |document| {
            document
                .blocks
                .iter()
                .map(|block| vault_block(block, artifact_session))
                .collect()
        });
        let mut transcript = Vec::new();
        for session_id in entry.session_ids() {
            transcript.push(format!("### Recording {}", session_id.get()));
            let mut events = store.load_session(*session_id).await?;
            events.sort_by_key(sotto_core::TimelineEvent::id);
            let replayed = sotto_core::replay(&events)
                .map_err(|error| RagError::Storage(error.to_string()))?;
            for event in replayed.active().values() {
                if let EventPayload::UtteranceFinal(utterance) = event.payload() {
                    transcript.push(format!(
                        "[{}] {}: {} ^event-{}-{}",
                        timecode(utterance.start),
                        utterance.source.speaker_name(),
                        utterance.text,
                        session_id.get(),
                        event.id().get(),
                    ));
                }
            }
        }
        output.insert(
            entry.id(),
            ComposedEntry {
                projection: VaultEntry {
                    id: entry.id(),
                    title,
                    date,
                    participants: Vec::new(),
                    capture_targets,
                    series: entry.series().map(str::to_owned),
                    linked_entries: Vec::new(),
                    blocks,
                    transcript,
                },
                artifact,
                document,
            },
        );
    }
    Ok(output)
}

fn entry_date(entry: &Entry, sessions: &BTreeMap<SessionId, rag::SessionSummary>) -> String {
    let millis = entry
        .session_ids()
        .iter()
        .filter_map(|id| sessions.get(id))
        .map(|session| session.started_at_unix_ms)
        .min()
        .unwrap_or_else(|| entry.created_at_unix_ms());
    i64::try_from(millis)
        .ok()
        .and_then(DateTime::<Utc>::from_timestamp_millis)
        .map_or_else(
            || "unknown".to_owned(),
            |date| date.format("%Y-%m-%d").to_string(),
        )
}

fn vault_block(block: &PresentedNotesBlock, session_id: Option<SessionId>) -> VaultBlock {
    let id = presented_id(&block.id);
    let citations = session_id.map_or_else(Vec::new, |session_id| {
        block
            .meeting_citations
            .iter()
            .map(|event_id| VaultCitation {
                label: format!("event {}", event_id.get()),
                session_id,
                event_id: *event_id,
            })
            .collect()
    });
    VaultBlock {
        id,
        section: section_title(block.section).to_owned(),
        text: block.text.clone(),
        action: block.action,
        checked: block.checked,
        owner: block.owner.clone(),
        due_date: block.due_date.clone(),
        citations,
    }
}

async fn append_external_edits(
    store: &Store,
    entry_id: EntryId,
    artifact: &RecordingNotes,
    document: &PresentedNotesDocument,
    edits: &[VaultEdit],
) -> Result<(), VaultControllerError> {
    let mut targets = document
        .blocks
        .iter()
        .map(|block| (presented_id(&block.id), target(block)))
        .collect::<BTreeMap<_, _>>();
    let base_time = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_err(|error| VaultControllerError::Notes(error.to_string()))?
        .as_millis();
    let base_time =
        u64::try_from(base_time).map_err(|error| VaultControllerError::Notes(error.to_string()))?;
    let mut operations = Vec::new();
    for edit in edits {
        match edit {
            VaultEdit::Add {
                block_id,
                section,
                text,
                action,
                checked,
            } => {
                let section = parse_section(section).ok_or_else(|| {
                    VaultControllerError::Notes(format!(
                        "external block uses unknown section {section:?}"
                    ))
                })?;
                operations.push(NotesOverlayOperation::Add {
                    user_block_id: block_id.clone(),
                    section,
                    text: text.clone(),
                    action: *action,
                    owner: None,
                    due_date: None,
                });
                let added = OverlayTarget {
                    block_id: block_id.clone(),
                    section,
                    action: *action,
                    meeting_citations: Vec::new(),
                    external_citations: Vec::new(),
                };
                targets.insert(block_id.clone(), added.clone());
                if *action && *checked {
                    operations.push(NotesOverlayOperation::SetChecked {
                        target: added,
                        checked: true,
                    });
                }
            }
            VaultEdit::Reword { block_id, text } => {
                let target = require_target(&targets, block_id)?;
                let current = document
                    .blocks
                    .iter()
                    .find(|block| presented_id(&block.id) == *block_id);
                operations.push(NotesOverlayOperation::Reword {
                    target,
                    text: text.clone(),
                    owner: current.and_then(|block| block.owner.clone()),
                    due_date: current.and_then(|block| block.due_date.clone()),
                });
            }
            VaultEdit::SetChecked { block_id, checked } => {
                operations.push(NotesOverlayOperation::SetChecked {
                    target: require_target(&targets, block_id)?,
                    checked: *checked,
                });
            }
            VaultEdit::Hide { block_id } => operations.push(NotesOverlayOperation::Hide {
                target: require_target(&targets, block_id)?,
            }),
            VaultEdit::Reorder { block_id, after } => {
                operations.push(NotesOverlayOperation::Reorder {
                    target: require_target(&targets, block_id)?,
                    after: after
                        .as_ref()
                        .map(|id| require_target(&targets, id))
                        .transpose()?,
                });
            }
        }
    }
    for (index, operation) in operations.iter().enumerate() {
        append_notes_overlay_operation(
            store,
            entry_id,
            artifact,
            operation,
            base_time.saturating_add(index as u64),
        )
        .await
        .map_err(|error| VaultControllerError::Notes(error.to_string()))?;
    }
    Ok(())
}

fn require_target(
    targets: &BTreeMap<String, OverlayTarget>,
    id: &str,
) -> Result<OverlayTarget, VaultControllerError> {
    targets.get(id).cloned().ok_or_else(|| {
        VaultControllerError::Notes(format!("external edit names unknown block {id:?}"))
    })
}

fn target(block: &PresentedNotesBlock) -> OverlayTarget {
    OverlayTarget {
        block_id: presented_id(&block.id),
        section: block.section,
        action: block.action,
        meeting_citations: block.meeting_citations.clone(),
        external_citations: block.external_citations.clone(),
    }
}

fn presented_id(id: &PresentedNotesBlockId) -> String {
    match id {
        PresentedNotesBlockId::Generated(id) => id.as_str().to_owned(),
        PresentedNotesBlockId::User(id) => id.clone(),
    }
}

const fn section_title(section: RecordingNotesSectionKind) -> &'static str {
    match section {
        RecordingNotesSectionKind::Overview => "Overview",
        RecordingNotesSectionKind::Topics => "Topics",
        RecordingNotesSectionKind::Explanations => "Explanations",
        RecordingNotesSectionKind::Findings => "Findings",
        RecordingNotesSectionKind::Decisions => "Decisions",
        RecordingNotesSectionKind::ActionItems => "Action items",
        RecordingNotesSectionKind::OpenQuestions => "Open questions",
        RecordingNotesSectionKind::Risks => "Risks",
        RecordingNotesSectionKind::FollowUps => "Follow-ups",
    }
}

fn parse_section(value: &str) -> Option<RecordingNotesSectionKind> {
    [
        RecordingNotesSectionKind::Overview,
        RecordingNotesSectionKind::Topics,
        RecordingNotesSectionKind::Explanations,
        RecordingNotesSectionKind::Findings,
        RecordingNotesSectionKind::Decisions,
        RecordingNotesSectionKind::ActionItems,
        RecordingNotesSectionKind::OpenQuestions,
        RecordingNotesSectionKind::Risks,
        RecordingNotesSectionKind::FollowUps,
    ]
    .into_iter()
    .find(|section| section_title(*section) == value)
}

fn timecode(value: std::time::Duration) -> String {
    let seconds = value.as_secs();
    let minutes = seconds / 60;
    format!("{minutes:02}:{:02}", seconds % 60)
}

#[cfg(test)]
mod tests {
    use gpui::{AppContext as _, TestAppContext};
    use insight::{compose_notes_document, load_presented_notes_document};

    use super::*;

    #[test]
    fn disabled_vault_starts_no_worker() -> Result<(), Box<dyn std::error::Error>> {
        let directory = tempfile::tempdir()?;
        let database = directory.path().join("sotto.sqlite3");
        let mut cx = TestAppContext::single();
        let controller = cx.new(|cx| VaultMirrorController::new(database, cx));

        assert!(!cx.read(|cx| {
            let controller = controller.read(cx);
            controller.preferences.enabled || controller.worker.is_some()
        }));
        Ok(())
    }

    #[test]
    fn enabling_starts_one_worker_and_disabling_stops_it() -> Result<(), Box<dyn std::error::Error>>
    {
        let directory = tempfile::tempdir()?;
        let database = directory.path().join("sotto.sqlite3");
        let folder = directory.path().join("vault");
        fs::create_dir_all(&folder)?;
        let mut cx = TestAppContext::single();
        let controller = cx.new(|cx| VaultMirrorController::new(database, cx));

        cx.update(|cx| {
            controller.update(cx, |controller, cx| {
                controller.set_folder(folder, cx)?;
                controller.set_enabled(true, cx)
            })
        })?;
        assert!(cx.read(|cx| controller.read(cx).worker.is_some()));

        cx.update(|cx| controller.update(cx, |controller, cx| controller.set_enabled(false, cx)))?;
        assert!(cx.read(|cx| {
            let controller = controller.read(cx);
            controller.worker.is_none()
                && matches!(controller.status, VaultRuntimeStatus::Disabled { .. })
        }));
        Ok(())
    }

    #[test]
    fn vault_preferences_and_visible_status_round_trip_without_deleting_files()
    -> Result<(), Box<dyn std::error::Error>> {
        let directory = tempfile::tempdir()?;
        let database = directory.path().join("sotto.sqlite3");
        let folder = directory.path().join("notes");
        fs::create_dir_all(&folder)?;
        fs::write(folder.join("kept.md"), "kept")?;
        let enabled = VaultPreferences {
            enabled: true,
            folder: Some(folder.clone()),
        };
        save_vault_preferences(&database, &enabled)?;
        assert_eq!(load_vault_preferences(&database)?, enabled);

        let disabled = VaultPreferences {
            enabled: false,
            folder: Some(folder.clone()),
        };
        save_vault_preferences(&database, &disabled)?;
        assert_eq!(load_vault_preferences(&database)?, disabled);
        assert_eq!(fs::read_to_string(folder.join("kept.md"))?, "kept");
        assert!(
            VaultRuntimeStatus::Disabled {
                folder: Some(folder)
            }
            .summary()
            .contains("remain on disk")
        );
        Ok(())
    }

    #[test]
    fn mirror_fingerprint_observes_external_markdown_edits()
    -> Result<(), Box<dyn std::error::Error>> {
        let directory = tempfile::tempdir()?;
        let database = directory.path().join("sotto.sqlite3");
        let folder = directory.path().join("notes");
        fs::create_dir_all(&folder)?;
        let note = folder.join("entry.md");
        fs::write(&note, "before")?;
        let before = mirror_fingerprint(&database, &folder);
        fs::write(&note, "after, with a different byte length")?;
        let after = mirror_fingerprint(&database, &folder);
        assert_ne!(before, after);
        Ok(())
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn external_reword_and_check_append_verbatim_overlay_operations()
    -> Result<(), Box<dyn std::error::Error>> {
        let store = Store::open_in_memory().await?;
        let entry_id = EntryId::new(88);
        store.create_entry(&Entry::new(entry_id, 1, None)).await?;
        let artifact: RecordingNotes = serde_json::from_value(serde_json::json!({
            "sections": [{
                "kind": "action_items",
                "blocks": [{
                    "type": "action",
                    "id": "action-1",
                    "text": "Draft wording",
                    "meeting_citations": [],
                    "external_citations": [],
                    "owner": null,
                    "owner_meeting_citations": [],
                    "owner_external_citations": [],
                    "due_date": null,
                    "due_date_meeting_citations": [],
                    "due_date_external_citations": []
                }]
            }]
        }))?;
        let document = compose_notes_document(&artifact, &[])?;
        append_external_edits(
            &store,
            entry_id,
            &artifact,
            &document,
            &[
                VaultEdit::Reword {
                    block_id: "action-1".to_owned(),
                    text: "The person's exact words".to_owned(),
                },
                VaultEdit::SetChecked {
                    block_id: "action-1".to_owned(),
                    checked: true,
                },
            ],
        )
        .await?;
        let presented = load_presented_notes_document(&store, entry_id, &artifact).await?;
        assert_eq!(presented.blocks[0].text, "The person's exact words");
        assert!(presented.blocks[0].checked);
        assert_eq!(
            presented.blocks[0].provenance,
            insight::NotesBlockProvenance::EditedFromDraft
        );
        Ok(())
    }
}
