//! Rebuildable, conflict-aware markdown projection of entry documents.
//!
//! This module deliberately knows nothing about `insight`'s generated-note schema. `rag` is below
//! the reasoning layer, so callers hand it the already-composed document and translate returned
//! [`VaultEdit`] values into their append-only overlay operations. The markdown file is therefore
//! a projection, never a second persistence authority.

use std::{
    collections::{BTreeMap, BTreeSet},
    fmt,
    fs::{self, OpenOptions},
    io::Write as _,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

use serde::{Deserialize, Serialize};
use sotto_core::{EntryId, EventId, SessionId};

const MANIFEST: &str = ".sotto-vault.json";
const NOTES_START: &str = "<!-- sotto:notes:start -->";
const NOTES_END: &str = "<!-- sotto:notes:end -->";
const RECORD_START: &str = "<!-- sotto:record:read-only:start -->";
const RECORD_END: &str = "<!-- sotto:record:read-only:end -->";
static NEXT_TEMP: AtomicU64 = AtomicU64::new(1);

/// One meeting-record citation attached to a projected notes block.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VaultCitation {
    pub label: String,
    pub session_id: SessionId,
    pub event_id: EventId,
}

/// One composed notes block. Its id is the same stable id used by the overlay.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VaultBlock {
    pub id: String,
    pub section: String,
    pub text: String,
    pub action: bool,
    pub checked: bool,
    pub owner: Option<String>,
    pub due_date: Option<String>,
    pub citations: Vec<VaultCitation>,
}

/// Complete input for one entry markdown file.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VaultEntry {
    pub id: EntryId,
    pub title: String,
    /// ISO calendar date, kept as text so this projection does not invent timezone policy.
    pub date: String,
    pub participants: Vec<String>,
    pub capture_targets: Vec<String>,
    pub series: Option<String>,
    pub linked_entries: Vec<String>,
    pub blocks: Vec<VaultBlock>,
    /// Canonical, read-only record lines. Each line should already carry its timestamp/speaker.
    pub transcript: Vec<String>,
}

/// A markdown change which the caller can append to the notes overlay.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum VaultEdit {
    Add {
        block_id: String,
        section: String,
        text: String,
        action: bool,
        checked: bool,
    },
    Reword {
        block_id: String,
        text: String,
    },
    SetChecked {
        block_id: String,
        checked: bool,
    },
    Hide {
        block_id: String,
    },
    Reorder {
        block_id: String,
        after: Option<String>,
    },
}

/// Non-fatal mirror state suitable for presenting in settings.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum VaultStatus {
    Off,
    Ready,
    FolderMissing,
    PermissionDenied,
    ExternalEditNeedsIngestion,
    ExternalEditNeedsAttention,
}

/// Result of one all-entry synchronization attempt.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum VaultSync {
    Synced {
        files: BTreeMap<EntryId, PathBuf>,
    },
    NeedsIngestion {
        entry_id: EntryId,
        path: PathBuf,
        revision: String,
        edits: Vec<VaultEdit>,
    },
}

#[derive(Debug)]
pub enum VaultError {
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    InvalidManifest(String),
    UnparseableExternalEdit {
        path: PathBuf,
        reason: String,
    },
    InvalidDeepLink(String),
}

impl VaultError {
    #[must_use]
    pub fn status(&self) -> VaultStatus {
        match self {
            Self::Io { source, .. } if source.kind() == std::io::ErrorKind::NotFound => {
                VaultStatus::FolderMissing
            }
            Self::Io { source, .. } if source.kind() == std::io::ErrorKind::PermissionDenied => {
                VaultStatus::PermissionDenied
            }
            Self::UnparseableExternalEdit { .. } | Self::InvalidManifest(_) => {
                VaultStatus::ExternalEditNeedsAttention
            }
            Self::Io { .. } | Self::InvalidDeepLink(_) => VaultStatus::ExternalEditNeedsAttention,
        }
    }
}

impl fmt::Display for VaultError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io { path, source } => write!(formatter, "{}: {source}", path.display()),
            Self::InvalidManifest(reason) => write!(formatter, "invalid vault manifest: {reason}"),
            Self::UnparseableExternalEdit { path, reason } => {
                write!(formatter, "could not ingest {}: {reason}", path.display())
            }
            Self::InvalidDeepLink(link) => write!(formatter, "invalid Sotto link {link:?}"),
        }
    }
}

impl std::error::Error for VaultError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io { source, .. } => Some(source),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SottoLink {
    pub entry_id: EntryId,
    pub session_id: SessionId,
    pub event_id: EventId,
}

impl SottoLink {
    #[must_use]
    pub fn render(self) -> String {
        format!(
            "sotto://entry/{}/session/{}/event/{}",
            self.entry_id.get(),
            self.session_id.get(),
            self.event_id.get()
        )
    }

    pub fn parse(value: &str) -> Result<Self, VaultError> {
        let Some(path) = value.strip_prefix("sotto://entry/") else {
            return Err(VaultError::InvalidDeepLink(value.to_owned()));
        };
        let fields = path.split('/').collect::<Vec<_>>();
        let [entry, "session", session, "event", event] = fields.as_slice() else {
            return Err(VaultError::InvalidDeepLink(value.to_owned()));
        };
        let entry_id = entry
            .parse()
            .map(EntryId::new)
            .map_err(|_| VaultError::InvalidDeepLink(value.to_owned()))?;
        let session_id = session
            .parse()
            .map(SessionId::new)
            .map_err(|_| VaultError::InvalidDeepLink(value.to_owned()))?;
        let event_id = event
            .parse()
            .map(EventId::new)
            .map_err(|_| VaultError::InvalidDeepLink(value.to_owned()))?;
        Ok(Self {
            entry_id,
            session_id,
            event_id,
        })
    }
}

#[derive(Debug)]
pub struct VaultMirror {
    root: PathBuf,
}

impl VaultMirror {
    #[must_use]
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Synchronizes the complete library, detecting and returning external edits before writing.
    ///
    /// The caller must persist returned edits and call `sync` again with the newly composed
    /// document. No projected file is touched on the conflict return path.
    pub fn sync(&self, entries: &[VaultEntry]) -> Result<VaultSync, VaultError> {
        if !self.root.is_dir() {
            return Err(io_error(
                self.root.clone(),
                std::io::Error::new(std::io::ErrorKind::NotFound, "vault folder is missing"),
            ));
        }
        let mut manifest = self.load_manifest()?;
        for entry in entries {
            let key = entry.id.get().to_string();
            let proposed = render_entry(entry);
            let Some(previous) = manifest.entries.get(&key) else {
                continue;
            };
            let path = self.root.join(&previous.file);
            let actual = read_optional(&path)?;
            if actual.as_deref() != Some(previous.projection.as_str()) {
                let Some(actual) = actual else {
                    return Err(VaultError::UnparseableExternalEdit {
                        path,
                        reason: "a projected note was removed outside Sotto".to_owned(),
                    });
                };
                if editable_notes_equal(&proposed, &actual).map_err(|reason| {
                    VaultError::UnparseableExternalEdit {
                        path: path.clone(),
                        reason,
                    }
                })? {
                    // The caller incorporated the edit, or only one-way material changed. The
                    // write phase accepts the new notes and restores canonical record content.
                    continue;
                }
                let edits = diff_external(&previous.projection, &actual).map_err(|reason| {
                    VaultError::UnparseableExternalEdit {
                        path: path.clone(),
                        reason,
                    }
                })?;
                return Ok(VaultSync::NeedsIngestion {
                    entry_id: entry.id,
                    path,
                    revision: actual,
                    edits,
                });
            }
        }

        let live = entries
            .iter()
            .map(|entry| entry.id.get().to_string())
            .collect::<BTreeSet<_>>();
        let stale = manifest
            .entries
            .iter()
            .filter(|(id, _)| !live.contains(*id))
            .map(|(id, state)| (id.clone(), state.file.clone()))
            .collect::<Vec<_>>();
        for (_, file) in &stale {
            let path = self.root.join(file);
            if path.exists() {
                fs::remove_file(&path).map_err(|source| io_error(path, source))?;
            }
        }
        for (id, _) in stale {
            manifest.entries.remove(&id);
        }

        let live_series = entries
            .iter()
            .filter_map(|entry| entry.series.as_ref())
            .cloned()
            .collect::<BTreeSet<_>>();
        let stale_series = manifest
            .series
            .iter()
            .filter(|(series, _)| !live_series.contains(*series))
            .map(|(series, state)| (series.clone(), state.file.clone()))
            .collect::<Vec<_>>();
        for (_, file) in &stale_series {
            let path = self.root.join(file);
            if path.exists() {
                fs::remove_file(&path).map_err(|source| io_error(path, source))?;
            }
        }
        for (series, _) in stale_series {
            manifest.series.remove(&series);
        }

        let mut reserved = manifest
            .entries
            .iter()
            .filter(|(id, _)| live.contains(*id))
            .map(|(_, state)| state.file.clone())
            .collect::<BTreeSet<_>>();
        reserved.extend(manifest.series.values().map(|state| state.file.clone()));
        let mut ordered = entries.iter().collect::<Vec<_>>();
        ordered.sort_by_key(|entry| entry.id);
        let mut files = BTreeMap::new();
        for entry in ordered {
            let key = entry.id.get().to_string();
            let desired = allocate_filename(&entry.title, entry.id, &reserved);
            let old = manifest.entries.get(&key).map(|state| state.file.clone());
            let file = match old {
                Some(old) if filename_stem(&old) == filename_stem(&desired) => old,
                Some(old) => {
                    reserved.remove(&old);
                    let next = allocate_filename(&entry.title, entry.id, &reserved);
                    let old_path = self.root.join(&old);
                    let next_path = self.root.join(&next);
                    if old_path.exists() {
                        fs::rename(&old_path, &next_path)
                            .map_err(|source| io_error(next_path.clone(), source))?;
                    }
                    next
                }
                None => desired,
            };
            reserved.insert(file.clone());
            let projection = render_entry(entry);
            let path = self.root.join(&file);
            if read_optional(&path)?.as_deref() != Some(projection.as_str()) {
                atomic_write(&path, projection.as_bytes())?;
            }
            manifest.entries.insert(
                key,
                ManifestEntry {
                    file: file.clone(),
                    projection,
                },
            );
            files.insert(entry.id, path);
        }
        for series in live_series {
            let previous = manifest.series.get(&series).map(|state| state.file.clone());
            if let Some(previous) = &previous {
                reserved.remove(previous);
            }
            let file = allocate_filename(&series, EntryId::new(0), &reserved);
            if let Some(previous) = previous.filter(|previous| previous != &file) {
                let old_path = self.root.join(previous);
                let next_path = self.root.join(&file);
                if old_path.exists() {
                    fs::rename(&old_path, &next_path)
                        .map_err(|source| io_error(next_path, source))?;
                }
            }
            reserved.insert(file.clone());
            let projection = render_series(&series, entries);
            let path = self.root.join(&file);
            if read_optional(&path)?.as_deref() != Some(projection.as_str()) {
                atomic_write(&path, projection.as_bytes())?;
            }
            manifest
                .series
                .insert(series, ManifestEntry { file, projection });
        }
        let encoded = serde_json::to_vec_pretty(&manifest)
            .map_err(|error| VaultError::InvalidManifest(error.to_string()))?;
        atomic_write(&self.root.join(MANIFEST), &encoded)?;
        Ok(VaultSync::Synced { files })
    }

    /// Advances one entry's conflict baseline after its returned edits are durably appended.
    ///
    /// This is intentionally separate from [`Self::sync`]. Advancing before the overlay commit
    /// would let a failed ingestion overwrite the person's file on retry. The current bytes are
    /// reread here so an editor racing the ingestion cannot be acknowledged accidentally.
    pub fn acknowledge_external_edit(
        &self,
        entry_id: EntryId,
        revision: &str,
    ) -> Result<(), VaultError> {
        let mut manifest = self.load_manifest()?;
        let key = entry_id.get().to_string();
        let state = manifest.entries.get_mut(&key).ok_or_else(|| {
            VaultError::InvalidManifest(format!("entry {} is not projected", entry_id.get()))
        })?;
        let path = self.root.join(&state.file);
        let current = fs::read_to_string(&path).map_err(|source| io_error(path.clone(), source))?;
        if current != revision {
            return Err(VaultError::UnparseableExternalEdit {
                path,
                reason: "the note changed again while its previous edit was being ingested"
                    .to_owned(),
            });
        }
        state.projection = revision.to_owned();
        let encoded = serde_json::to_vec_pretty(&manifest)
            .map_err(|error| VaultError::InvalidManifest(error.to_string()))?;
        atomic_write(&self.root.join(MANIFEST), &encoded)
    }

    fn load_manifest(&self) -> Result<VaultManifest, VaultError> {
        let path = self.root.join(MANIFEST);
        let Some(contents) = read_optional(&path)? else {
            return Ok(VaultManifest::default());
        };
        let manifest: VaultManifest = serde_json::from_str(&contents)
            .map_err(|error| VaultError::InvalidManifest(error.to_string()))?;
        if manifest.version != 1 {
            return Err(VaultError::InvalidManifest(format!(
                "unsupported version {}",
                manifest.version
            )));
        }
        Ok(manifest)
    }
}

#[derive(Debug, Deserialize, Serialize)]
struct VaultManifest {
    version: u8,
    entries: BTreeMap<String, ManifestEntry>,
    #[serde(default)]
    series: BTreeMap<String, ManifestEntry>,
}

impl Default for VaultManifest {
    fn default() -> Self {
        Self {
            version: 1,
            entries: BTreeMap::new(),
            series: BTreeMap::new(),
        }
    }
}

#[derive(Debug, Deserialize, Serialize)]
struct ManifestEntry {
    file: String,
    projection: String,
}

fn render_entry(entry: &VaultEntry) -> String {
    let mut out = String::new();
    out.push_str("---\n");
    yaml_scalar(&mut out, "sotto_entry_id", &entry.id.get().to_string());
    yaml_scalar(&mut out, "title", &entry.title);
    yaml_scalar(&mut out, "date", &entry.date);
    yaml_list(&mut out, "participants", &entry.participants);
    yaml_list(&mut out, "capture_targets", &entry.capture_targets);
    if let Some(series) = &entry.series {
        yaml_scalar(&mut out, "series", &format!("[[{series}]]"));
    } else {
        out.push_str("series: null\n");
    }
    out.push_str("---\n\n# ");
    out.push_str(&entry.title);
    out.push_str("\n\n");
    if !entry.linked_entries.is_empty() {
        out.push_str("Related: ");
        for (index, link) in entry.linked_entries.iter().enumerate() {
            if index > 0 {
                out.push_str(", ");
            }
            out.push_str("[[");
            out.push_str(link);
            out.push_str("]]");
        }
        out.push_str("\n\n");
    }
    out.push_str(NOTES_START);
    out.push('\n');
    let mut section = None::<&str>;
    for block in &entry.blocks {
        if section != Some(block.section.as_str()) {
            section = Some(&block.section);
            out.push_str("\n## ");
            out.push_str(&block.section);
            out.push_str("\n\n");
        }
        if block.action {
            out.push_str(if block.checked { "- [x] " } else { "- [ ] " });
        }
        out.push_str(&block.text);
        if let Some(owner) = &block.owner {
            out.push_str(" — Owner: ");
            out.push_str(owner);
        }
        if let Some(due) = &block.due_date {
            out.push_str(" — Due: ");
            out.push_str(due);
        }
        for citation in &block.citations {
            out.push_str(" [");
            out.push_str(&citation.label);
            out.push_str("](");
            out.push_str(
                &SottoLink {
                    entry_id: entry.id,
                    session_id: citation.session_id,
                    event_id: citation.event_id,
                }
                .render(),
            );
            out.push(')');
        }
        out.push_str(" ^");
        out.push_str(&block.id);
        out.push_str("\n\n");
    }
    out.push_str(NOTES_END);
    out.push_str("\n\n");
    out.push_str(RECORD_START);
    out.push_str("\n> [!IMPORTANT] Read-only meeting record\n");
    out.push_str("> Sotto restores this section from its append-only local record. External edits here are never imported.\n\n");
    out.push_str("## Transcript (read-only)\n\n");
    for line in &entry.transcript {
        out.push_str(line);
        out.push('\n');
    }
    out.push_str(RECORD_END);
    out.push('\n');
    out
}

fn render_series(series: &str, entries: &[VaultEntry]) -> String {
    let mut occurrences = entries
        .iter()
        .filter(|entry| entry.series.as_deref() == Some(series))
        .collect::<Vec<_>>();
    occurrences.sort_by(|left, right| {
        left.date
            .cmp(&right.date)
            .then_with(|| left.id.cmp(&right.id))
    });
    let mut out = String::from("---\nsotto_derived: series\n---\n\n# ");
    out.push_str(series);
    out.push_str("\n\n> [!IMPORTANT] Derived, read-only series page\n");
    out.push_str("> Sotto regenerates this page from its entry documents.\n\n## Occurrences\n\n");
    for entry in &occurrences {
        out.push_str("- [[");
        out.push_str(&entry.title);
        out.push_str("]] — ");
        out.push_str(&entry.date);
        out.push('\n');
    }
    out.push_str("\n## Open items\n\n");
    for entry in occurrences {
        for block in entry
            .blocks
            .iter()
            .filter(|block| block.action && !block.checked)
        {
            out.push_str("- [ ] ");
            out.push_str(&block.text);
            out.push_str(" — [[");
            out.push_str(&entry.title);
            out.push_str("]]\n");
        }
    }
    out
}

fn yaml_scalar(out: &mut String, key: &str, value: &str) {
    out.push_str(key);
    out.push_str(": \"");
    for character in value.chars() {
        match character {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            other => out.push(other),
        }
    }
    out.push_str("\"\n");
}

fn yaml_list(out: &mut String, key: &str, values: &[String]) {
    out.push_str(key);
    if values.is_empty() {
        out.push_str(": []\n");
        return;
    }
    out.push_str(":\n");
    for value in values {
        out.push_str("  - \"");
        out.push_str(&escape_yaml(value));
        out.push_str("\"\n");
    }
}

fn escape_yaml(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
}

#[derive(Debug, Eq, PartialEq)]
struct ParsedBlock {
    id: String,
    section: String,
    text: String,
    action: bool,
    checked: bool,
}

fn diff_external(expected: &str, actual: &str) -> Result<Vec<VaultEdit>, String> {
    // The markers are mandatory, but record contents never become overlay operations. A later
    // successful projection restores that section from SQLite.
    let _ = between(expected, RECORD_START, RECORD_END)?;
    let _ = between(actual, RECORD_START, RECORD_END)?;
    let expected_blocks = parse_blocks(between(expected, NOTES_START, NOTES_END)?)?;
    let actual_blocks = parse_blocks(between(actual, NOTES_START, NOTES_END)?)?;
    let expected_by_id = expected_blocks
        .iter()
        .map(|block| (block.id.as_str(), block))
        .collect::<BTreeMap<_, _>>();
    let actual_by_id = actual_blocks
        .iter()
        .map(|block| (block.id.as_str(), block))
        .collect::<BTreeMap<_, _>>();
    let mut edits = Vec::new();
    for block in &expected_blocks {
        let Some(actual) = actual_by_id.get(block.id.as_str()) else {
            edits.push(VaultEdit::Hide {
                block_id: block.id.clone(),
            });
            continue;
        };
        if actual.text != block.text {
            edits.push(VaultEdit::Reword {
                block_id: block.id.clone(),
                text: actual.text.clone(),
            });
        }
        if block.action && actual.checked != block.checked {
            edits.push(VaultEdit::SetChecked {
                block_id: block.id.clone(),
                checked: actual.checked,
            });
        }
    }
    for block in &actual_blocks {
        if !expected_by_id.contains_key(block.id.as_str()) {
            edits.push(VaultEdit::Add {
                block_id: block.id.clone(),
                section: block.section.clone(),
                text: block.text.clone(),
                action: block.action,
                checked: block.checked,
            });
        }
    }
    let expected_order = expected_blocks
        .iter()
        .filter(|block| actual_by_id.contains_key(block.id.as_str()))
        .map(|block| block.id.as_str())
        .collect::<Vec<_>>();
    let actual_order = actual_blocks
        .iter()
        .filter(|block| expected_by_id.contains_key(block.id.as_str()))
        .map(|block| block.id.as_str())
        .collect::<Vec<_>>();
    if expected_order != actual_order {
        for (index, id) in actual_order.iter().enumerate() {
            if expected_order.get(index).copied() != Some(*id) {
                edits.push(VaultEdit::Reorder {
                    block_id: (*id).to_owned(),
                    after: index
                        .checked_sub(1)
                        .map(|previous| actual_order[previous].to_owned()),
                });
            }
        }
    }
    Ok(edits)
}

fn editable_notes_equal(expected: &str, actual: &str) -> Result<bool, String> {
    let expected = parse_blocks(between(expected, NOTES_START, NOTES_END)?)?;
    let actual = parse_blocks(between(actual, NOTES_START, NOTES_END)?)?;
    Ok(expected == actual)
}

fn between<'a>(input: &'a str, start: &str, end: &str) -> Result<&'a str, String> {
    let start_at = input
        .find(start)
        .ok_or_else(|| format!("missing marker {start}"))?
        + start.len();
    let tail = &input[start_at..];
    let end_at = tail
        .find(end)
        .ok_or_else(|| format!("missing marker {end}"))?;
    Ok(&tail[..end_at])
}

fn parse_blocks(input: &str) -> Result<Vec<ParsedBlock>, String> {
    let mut blocks = Vec::new();
    let mut section = None::<String>;
    for chunk in input.split("\n\n") {
        let chunk = chunk.trim();
        if chunk.is_empty() {
            continue;
        }
        if let Some(title) = chunk.strip_prefix("## ") {
            section = Some(title.trim().to_owned());
            continue;
        }
        let section = section
            .clone()
            .ok_or_else(|| "a notes block appears before its section heading".to_owned())?;
        let anchor_at = chunk
            .rfind(" ^")
            .ok_or_else(|| format!("notes block has no ^blockid anchor: {chunk:?}"))?;
        let id = chunk[anchor_at + 2..].trim();
        if id.is_empty() || id.chars().any(char::is_whitespace) {
            return Err(format!("invalid block anchor {id:?}"));
        }
        if blocks.iter().any(|block: &ParsedBlock| block.id == id) {
            return Err(format!("duplicate block anchor {id:?}"));
        }
        let mut body = chunk[..anchor_at].trim().to_owned();
        while let Some(open) = body.rfind(" [") {
            let suffix = &body[open..];
            let Some(link_start) = suffix.find("](sotto://") else {
                break;
            };
            if !suffix.ends_with(')')
                || SottoLink::parse(&suffix[link_start + 2..suffix.len() - 1]).is_err()
            {
                break;
            }
            body.truncate(open);
        }
        let (action, checked, text) = if let Some(text) = body.strip_prefix("- [ ] ") {
            (true, false, text.to_owned())
        } else if let Some(text) = body
            .strip_prefix("- [x] ")
            .or_else(|| body.strip_prefix("- [X] "))
        {
            (true, true, text.to_owned())
        } else {
            (false, false, body)
        };
        blocks.push(ParsedBlock {
            id: id.to_owned(),
            section,
            text,
            action,
            checked,
        });
    }
    Ok(blocks)
}

fn allocate_filename(title: &str, id: EntryId, reserved: &BTreeSet<String>) -> String {
    let stem = sanitized_title(title, id);
    let direct = format!("{stem}.md");
    if !reserved.contains(&direct) {
        return direct;
    }
    for suffix in 2_u64.. {
        let candidate = format!("{stem} ({suffix}).md");
        if !reserved.contains(&candidate) {
            return candidate;
        }
    }
    unreachable!("the filename suffix space is not finite")
}

fn sanitized_title(title: &str, id: EntryId) -> String {
    let stem = title
        .trim()
        .chars()
        .map(|character| match character {
            '/' | ':' | '\0' => '-',
            other => other,
        })
        .collect::<String>();
    let stem = stem.trim_matches(['.', ' ']);
    if stem.is_empty() {
        format!("Entry {}", id.get())
    } else {
        stem.to_owned()
    }
}

fn filename_stem(file: &str) -> &str {
    file.strip_suffix(".md").unwrap_or(file)
}

fn read_optional(path: &Path) -> Result<Option<String>, VaultError> {
    match fs::read_to_string(path) {
        Ok(contents) => Ok(Some(contents)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(source) => Err(io_error(path.to_owned(), source)),
    }
}

fn atomic_write(path: &Path, contents: &[u8]) -> Result<(), VaultError> {
    let parent = path.parent().ok_or_else(|| {
        io_error(
            path.to_owned(),
            std::io::Error::new(std::io::ErrorKind::InvalidInput, "path has no parent"),
        )
    })?;
    let mut attempt = 0_u8;
    loop {
        let nonce = NEXT_TEMP.fetch_add(1, Ordering::Relaxed);
        let temp = parent.join(format!(".sotto-write-{}-{nonce}.tmp", std::process::id()));
        match OpenOptions::new().write(true).create_new(true).open(&temp) {
            Ok(mut file) => {
                let result = (|| {
                    file.write_all(contents)?;
                    file.sync_all()?;
                    fs::rename(&temp, path)
                })();
                if let Err(source) = result {
                    let _ = fs::remove_file(&temp);
                    return Err(io_error(path.to_owned(), source));
                }
                return Ok(());
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists && attempt < 8 => {
                attempt = attempt.saturating_add(1);
            }
            Err(source) => return Err(io_error(path.to_owned(), source)),
        }
    }
}

fn io_error(path: PathBuf, source: std::io::Error) -> VaultError {
    VaultError::Io { path, source }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(id: u128, title: &str) -> VaultEntry {
        VaultEntry {
            id: EntryId::new(id),
            title: title.to_owned(),
            date: "2026-08-16".to_owned(),
            participants: vec!["Ari".to_owned(), "Bo".to_owned()],
            capture_targets: vec!["Zoom".to_owned()],
            series: Some("Weekly standup".to_owned()),
            linked_entries: vec!["Previous standup".to_owned()],
            blocks: vec![
                VaultBlock {
                    id: "decision-1".to_owned(),
                    section: "Decisions".to_owned(),
                    text: "Ship the local projection.".to_owned(),
                    action: false,
                    checked: false,
                    owner: None,
                    due_date: None,
                    citations: vec![VaultCitation {
                        label: "00:12".to_owned(),
                        session_id: SessionId::new(9),
                        event_id: EventId::new(4),
                    }],
                },
                VaultBlock {
                    id: "action-1".to_owned(),
                    section: "Action items".to_owned(),
                    text: "Verify it in Obsidian.".to_owned(),
                    action: true,
                    checked: false,
                    owner: Some("Ari".to_owned()),
                    due_date: Some("Friday".to_owned()),
                    citations: Vec::new(),
                },
            ],
            transcript: vec!["[00:12] Meeting audio: Ship it.".to_owned()],
        }
    }

    #[test]
    fn deep_links_round_trip() -> Result<(), VaultError> {
        let link = SottoLink {
            entry_id: EntryId::new(7),
            session_id: SessionId::new(9),
            event_id: EventId::new(11),
        };
        assert_eq!(SottoLink::parse(&link.render())?, link);
        Ok(())
    }

    #[test]
    fn rebuild_is_byte_identical_and_collisions_are_stable()
    -> Result<(), Box<dyn std::error::Error>> {
        let directory = tempfile::tempdir()?;
        let mirror = VaultMirror::new(directory.path());
        let entries = vec![entry(1, "Standup"), entry(2, "Standup")];
        let VaultSync::Synced { files: first } = mirror.sync(&entries)? else {
            return Err("fresh projection unexpectedly needed ingestion".into());
        };
        let bytes = first
            .iter()
            .map(|(id, path)| Ok((*id, fs::read(path)?)))
            .collect::<Result<BTreeMap<_, _>, std::io::Error>>()?;
        let VaultSync::Synced { files: second } = mirror.sync(&entries)? else {
            return Err("unchanged rebuild unexpectedly needed ingestion".into());
        };
        for (id, path) in second {
            assert_eq!(fs::read(path)?, bytes[&id]);
        }
        assert_ne!(first[&EntryId::new(1)], first[&EntryId::new(2)]);
        Ok(())
    }

    #[test]
    fn external_check_and_reword_are_returned_before_any_write()
    -> Result<(), Box<dyn std::error::Error>> {
        let directory = tempfile::tempdir()?;
        let mirror = VaultMirror::new(directory.path());
        let source = entry(1, "Standup");
        let VaultSync::Synced { files } = mirror.sync(std::slice::from_ref(&source))? else {
            return Err("fresh projection unexpectedly needed ingestion".into());
        };
        let path = &files[&source.id];
        let external = fs::read_to_string(path)?
            .replace(
                "Ship the local projection.",
                "Ship the person's exact wording.",
            )
            .replace("- [ ] Verify it", "- [x] Verify it");
        fs::write(path, &external)?;
        let VaultSync::NeedsIngestion { edits, .. } = mirror.sync(&[source])? else {
            return Err("external edit was overwritten".into());
        };
        assert!(edits.contains(&VaultEdit::Reword {
            block_id: "decision-1".to_owned(),
            text: "Ship the person's exact wording.".to_owned(),
        }));
        assert!(edits.contains(&VaultEdit::SetChecked {
            block_id: "action-1".to_owned(),
            checked: true,
        }));
        assert_eq!(fs::read_to_string(path)?, external);
        Ok(())
    }

    #[test]
    fn acknowledged_external_and_concurrent_in_app_edits_both_survive()
    -> Result<(), Box<dyn std::error::Error>> {
        let directory = tempfile::tempdir()?;
        let mirror = VaultMirror::new(directory.path());
        let source = entry(1, "Standup");
        let VaultSync::Synced { files } = mirror.sync(std::slice::from_ref(&source))? else {
            return Err("fresh projection unexpectedly needed ingestion".into());
        };
        let path = &files[&source.id];
        let external = fs::read_to_string(path)?.replace("- [ ] Verify it", "- [x] Verify it");
        fs::write(path, external)?;
        let mut in_app = source.clone();
        in_app.blocks[0].text = "In-app wording survives too.".to_owned();
        let VaultSync::NeedsIngestion {
            edits, revision, ..
        } = mirror.sync(std::slice::from_ref(&in_app))?
        else {
            return Err("external check must be offered for ingestion".into());
        };
        assert!(edits.contains(&VaultEdit::SetChecked {
            block_id: "action-1".to_owned(),
            checked: true,
        }));
        in_app.blocks[1].checked = true;
        mirror.acknowledge_external_edit(source.id, &revision)?;
        let VaultSync::Synced { .. } = mirror.sync(&[in_app])? else {
            return Err("acknowledged edit must permit the merged projection".into());
        };
        let merged = fs::read_to_string(path)?;
        assert!(merged.contains("In-app wording survives too."));
        assert!(merged.contains("- [x] Verify it"));
        Ok(())
    }

    #[test]
    fn transcript_edits_are_restored_but_never_ingested() -> Result<(), Box<dyn std::error::Error>>
    {
        let directory = tempfile::tempdir()?;
        let mirror = VaultMirror::new(directory.path());
        let source = entry(1, "Standup");
        let VaultSync::Synced { files } = mirror.sync(std::slice::from_ref(&source))? else {
            return Err("fresh projection unexpectedly needed ingestion".into());
        };
        let path = &files[&source.id];
        let external = fs::read_to_string(path)?.replace("Ship it.", "Changed record.");
        fs::write(path, &external)?;
        let VaultSync::Synced { .. } = mirror.sync(&[source])? else {
            return Err("record text must never become an overlay edit".into());
        };
        let restored = fs::read_to_string(path)?;
        assert!(!restored.contains("Changed record."));
        assert!(restored.contains("Ship it."));
        Ok(())
    }

    #[test]
    fn renaming_moves_the_projection_and_deleting_removes_it()
    -> Result<(), Box<dyn std::error::Error>> {
        let directory = tempfile::tempdir()?;
        let mirror = VaultMirror::new(directory.path());
        let mut source = entry(1, "Before");
        let VaultSync::Synced { files } = mirror.sync(std::slice::from_ref(&source))? else {
            return Err("fresh projection unexpectedly needed ingestion".into());
        };
        let before = files[&source.id].clone();
        source.title = "After".to_owned();
        let VaultSync::Synced { files } = mirror.sync(std::slice::from_ref(&source))? else {
            return Err("rename unexpectedly needed ingestion".into());
        };
        assert!(!before.exists());
        assert!(files[&source.id].exists());
        let VaultSync::Synced { files } = mirror.sync(&[])? else {
            return Err("delete unexpectedly needed ingestion".into());
        };
        assert!(files.is_empty());
        assert_eq!(
            fs::read_dir(directory.path())?.count(),
            1,
            "only manifest remains"
        );
        Ok(())
    }

    #[test]
    fn series_page_is_derived_with_occurrences_and_open_items()
    -> Result<(), Box<dyn std::error::Error>> {
        let directory = tempfile::tempdir()?;
        let mirror = VaultMirror::new(directory.path());
        let source = entry(1, "Standup 2026-08-16");
        let VaultSync::Synced { .. } = mirror.sync(&[source])? else {
            return Err("fresh projection unexpectedly needed ingestion".into());
        };
        let page = fs::read_to_string(directory.path().join("Weekly standup.md"))?;
        assert!(page.contains("sotto_derived: series"));
        assert!(page.contains("[[Standup 2026-08-16]]"));
        assert!(page.contains("- [ ] Verify it in Obsidian."));
        Ok(())
    }
}
