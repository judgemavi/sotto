use std::{
    collections::{BTreeMap, HashMap},
    hash::{Hash, Hasher},
    io,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::{SystemTime, UNIX_EPOCH},
};

use fastembed::{EmbeddingModel, InitOptions, TextEmbedding};
use rusqlite::{Connection, OpenFlags, OptionalExtension, params, types::Type};
use serde_json::{Value, json};
use sotto_core::types::{
    MediaTimeMapping, RecordingContainer, RecordingMissingReason, RecordingTitle, SessionRecording,
};
use sotto_core::{
    BoxFuture, CaptureTarget, Chunk, Entry, EntryId, EventPayload, PersistenceSink, RagError,
    Retriever, Session, SessionId, Source, TargetKind, TimelineEvent, Utterance,
};

use crate::schema::{configure, migrate, storage};

mod annotations;

const MAX_FILTERED_CANDIDATES: usize = 256;
pub const DEFAULT_RECORDING_BUDGET_BYTES: u64 = 20_000_000_000;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DocumentKind {
    ResourceDocument,
    ProjectNote,
    MeetingNote,
    PriorMeeting,
}

impl DocumentKind {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ResourceDocument => "resource_document",
            Self::ProjectNote => "project_note",
            Self::MeetingNote => "meeting_note",
            Self::PriorMeeting => "prior_meeting",
        }
    }

    const fn requires_source_session(self) -> bool {
        matches!(self, Self::MeetingNote | Self::PriorMeeting)
    }

    fn parse(value: &str) -> Result<Self, RagError> {
        match value {
            "resource_document" => Ok(Self::ResourceDocument),
            "project_note" => Ok(Self::ProjectNote),
            "meeting_note" => Ok(Self::MeetingNote),
            "prior_meeting" => Ok(Self::PriorMeeting),
            unknown => Err(RagError::Storage(format!(
                "unknown local document kind {unknown:?}"
            ))),
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct IngestMetadata {
    pub title: String,
    pub source_path: Option<String>,
    pub collection_id: Option<String>,
    pub source_session_id: Option<SessionId>,
    pub fields: BTreeMap<String, String>,
}

#[derive(Clone, Debug, Default)]
pub struct SearchFilter {
    pub kind: Option<DocumentKind>,
    pub collection_id: Option<String>,
    pub source_session_id: Option<SessionId>,
}

/// What one pass of [`Store::index_missing_prior_meetings`] did, per recording.
///
/// A backfill that returned only a count could not distinguish "nothing to do" from "three
/// timelines would not load", and the caller has to be able to say which without re-deriving it.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct PriorMeetingIndexReport {
    /// Recordings newly added to the cross-recording index.
    pub indexed: Vec<SessionId>,
    /// Completed recordings that captured no speech, so have nothing to index.
    pub skipped: Vec<SessionId>,
    /// Recordings whose indexing failed, each with the reason. The rest still indexed.
    pub failed: Vec<(SessionId, String)>,
}

/// Whether local evidence has exact current provenance or a conservative legacy marker.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LocalProvenance {
    Native,
    LegacyUnlinked,
}

impl LocalProvenance {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Native => "native",
            Self::LegacyUnlinked => "legacy_unlinked",
        }
    }

    fn parse(value: &str) -> Result<Self, RagError> {
        match value {
            "native" => Ok(Self::Native),
            "legacy_unlinked" => Ok(Self::LegacyUnlinked),
            unknown => Err(RagError::Storage(format!(
                "unknown local provenance status {unknown:?}"
            ))),
        }
    }
}

/// Exact, locally persisted provenance for a retrieved chunk.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LocalEvidenceReceipt {
    pub evidence_id: String,
    pub document_id: String,
    pub ordinal: usize,
    pub text: String,
    pub kind: DocumentKind,
    pub title: String,
    pub source_path: Option<String>,
    pub collection_id: Option<String>,
    pub source_session_id: Option<SessionId>,
    pub provenance: LocalProvenance,
    pub metadata: BTreeMap<String, String>,
}

/// Lightweight persisted-session metadata for the recording catalogue.
///
/// `capture_target` and `title` are separate fields because they are separate kinds of claim:
/// the target is what was recorded, the title is what a person decided to call it. A summary
/// carries both so a caller can name the recording without losing the ability to state its scope.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SessionSummary {
    pub id: SessionId,
    pub capture_target: CaptureTarget,
    /// The chosen name, absent until someone renames the recording.
    pub title: Option<RecordingTitle>,
    pub started_at_unix_ms: u64,
    pub ended_at_unix_ms: Option<u64>,
}

/// Recording-library totals shown independently from transcript retention.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RecordingUsage {
    pub used_bytes: u64,
    pub budget_bytes: u64,
}

/// Durable recording reference, including a writer-owned file whose measurements are not final.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RecordingReference {
    Growing {
        session_id: SessionId,
        path: String,
        finalization_error: Option<String>,
    },
    Settled(SessionRecording),
}

impl RecordingReference {
    #[must_use]
    pub const fn session_id(&self) -> SessionId {
        match self {
            Self::Growing { session_id, .. } => *session_id,
            Self::Settled(recording) => recording.session_id(),
        }
    }
}

/// One on-disk deletion tombstone resolved by [`Store::recover_quarantined_media`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct QuarantineRecovery {
    pub session_id: SessionId,
    /// `true` when the file was renamed back because its owning row still resolved to it,
    /// `false` when the tombstone was unlinked because the row had already moved past it.
    pub restored: bool,
}

/// The current user-requested transcription projection for a retained recording.
///
/// Original timeline events remain untouched; saving a newer projection atomically replaces only
/// this derived row.
#[derive(Clone, Debug, PartialEq)]
pub struct DerivedTranscript {
    pub model: String,
    pub utterances: Vec<Utterance>,
}

/// One derived artifact and the exact external evidence replayed with it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GroundedDerivedView {
    pub artifact: String,
    pub usage: String,
    pub provider_model: String,
    pub grant_fingerprint: Option<String>,
    pub source_status: String,
    pub bundle: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GroundedDerivedArtifact {
    pub model: String,
    pub content_hash: String,
    pub view: GroundedDerivedView,
}

pub struct Store {
    writer: Mutex<Connection>,
    reader: Mutex<Connection>,
    embedding: Mutex<Option<TextEmbedding>>,
}

struct RetrievedChunkRow {
    text: String,
    title: String,
    kind: String,
    collection_id: Option<String>,
    source_session_id: Option<String>,
    source_path: Option<String>,
    provenance: String,
    document_id: String,
    ordinal: usize,
    metadata: String,
}

/// Non-blocking adapter from the core pipeline's persistence task to T008's SQLite store.
#[derive(Clone)]
pub struct TimelinePersistence {
    store: Arc<Store>,
}

impl TimelinePersistence {
    #[must_use]
    pub const fn new(store: Arc<Store>) -> Self {
        Self { store }
    }
}

impl PersistenceSink for TimelinePersistence {
    fn append<'a>(&'a self, events: Vec<TimelineEvent>) -> BoxFuture<'a, Result<(), RagError>> {
        let store = Arc::clone(&self.store);
        Box::pin(async move {
            tokio::task::spawn_blocking(move || store.append_events(&events))
                .await
                .map_err(|error| RagError::Storage(error.to_string()))?
        })
    }
}

impl Store {
    pub fn open(path: impl AsRef<Path>) -> Result<Self, RagError> {
        register_sqlite_vec();
        let mut writer = Connection::open(path.as_ref()).map_err(storage)?;
        migrate(&mut writer)?;
        let reader = Connection::open(path).map_err(storage)?;
        configure(&reader)?;
        Ok(Self {
            writer: Mutex::new(writer),
            reader: Mutex::new(reader),
            embedding: Mutex::new(None),
        })
    }

    pub fn open_in_memory() -> Result<Self, RagError> {
        register_sqlite_vec();
        static NEXT_DATABASE: AtomicU64 = AtomicU64::new(1);
        let uri = format!(
            "file:sotto-rag-{}?mode=memory&cache=shared",
            NEXT_DATABASE.fetch_add(1, Ordering::Relaxed)
        );
        let flags = OpenFlags::SQLITE_OPEN_READ_WRITE
            | OpenFlags::SQLITE_OPEN_CREATE
            | OpenFlags::SQLITE_OPEN_URI;
        let mut writer = Connection::open_with_flags(&uri, flags).map_err(storage)?;
        migrate(&mut writer)?;
        let reader = Connection::open_with_flags(&uri, flags).map_err(storage)?;
        configure(&reader)?;
        Ok(Self {
            writer: Mutex::new(writer),
            reader: Mutex::new(reader),
            embedding: Mutex::new(None),
        })
    }

    pub fn save_session(&self, session: &Session) -> Result<(), RagError> {
        let target = session.capture_target();
        if !target.has_valid_scope() {
            return Err(RagError::Storage(
                "session capture target has an invalid scope combination".to_owned(),
            ));
        }
        let mut connection = self.writer.lock().map_err(poisoned)?;
        let transaction = connection.transaction().map_err(storage)?;
        transaction.execute(
            "INSERT INTO sessions VALUES(?1,?2,?3,?4,?5,?6,?7,?8) ON CONFLICT(id) DO UPDATE SET ended_at_unix_ms=excluded.ended_at_unix_ms",
            params![session.id().get().to_string(), session.started_at_unix_ms(),
                session.ended_at_unix_ms(), target.bundle_id, target.display_name,
                target.window_title, target_kind(target.kind), target.audio_scoped],
        ).map_err(storage)?;
        ensure_session_entry(&transaction, session)?;
        transaction.commit().map_err(storage)
    }

    /// Saves a newly captured session directly into an existing prepared entry.
    ///
    /// This is the explicit counterpart to [`Self::save_session`], which creates an entry when no
    /// destination was chosen. The relation is inserted in the same transaction as the captured
    /// facts, so no durable session can briefly appear outside its selected entry.
    pub fn save_session_in_entry(
        &self,
        session: &Session,
        entry_id: EntryId,
    ) -> Result<(), RagError> {
        let target = session.capture_target();
        if !target.has_valid_scope() {
            return Err(RagError::Storage(
                "session capture target has an invalid scope combination".to_owned(),
            ));
        }
        let mut connection = self.writer.lock().map_err(poisoned)?;
        let transaction = connection.transaction().map_err(storage)?;
        transaction
            .execute(
                "INSERT INTO sessions VALUES(?1,?2,?3,?4,?5,?6,?7,?8) ON CONFLICT(id) DO UPDATE SET ended_at_unix_ms=excluded.ended_at_unix_ms",
                params![
                    session.id().get().to_string(),
                    session.started_at_unix_ms(),
                    session.ended_at_unix_ms(),
                    target.bundle_id,
                    target.display_name,
                    target.window_title,
                    target_kind(target.kind),
                    target.audio_scoped
                ],
            )
            .map_err(storage)?;
        attach_session_relation(&transaction, entry_id, session.id())?;
        transaction.commit().map_err(storage)
    }

    /// Creates a prepared entry. Recording sessions are attached separately and never detached.
    pub fn create_entry(&self, entry: &Entry) -> Result<(), RagError> {
        if !entry.session_ids().is_empty() {
            return Err(RagError::Storage(
                "create an entry before attaching recording sessions".to_owned(),
            ));
        }
        self.writer
            .lock()
            .map_err(poisoned)?
            .execute(
                "INSERT INTO entries(id,created_at_unix_ms,title) VALUES(?1,?2,?3)",
                params![
                    entry.id().get().to_string(),
                    entry.created_at_unix_ms(),
                    entry.title().map(RecordingTitle::as_str)
                ],
            )
            .map_err(storage)?;
        Ok(())
    }

    /// Lists entries newest first, including prepared entries with no recording sessions.
    pub fn list_entries(&self) -> Result<Vec<Entry>, RagError> {
        let connection = self.reader.lock().map_err(poisoned)?;
        let mut statement = connection
            .prepare(
                "SELECT e.id,e.created_at_unix_ms,e.title,es.session_id \
                 FROM entries e LEFT JOIN entry_sessions es ON es.entry_id=e.id \
                 LEFT JOIN sessions s ON s.id=es.session_id \
                 ORDER BY e.created_at_unix_ms DESC,e.id DESC,s.started_at_unix_ms,es.session_id",
            )
            .map_err(storage)?;
        let mut rows = statement.query([]).map_err(storage)?;
        let mut entries = Vec::<Entry>::new();
        while let Some(row) = rows.next().map_err(storage)? {
            let entry_id = parse_entry_id(&row.get::<_, String>(0).map_err(storage)?)?;
            if entries.last().map(Entry::id) != Some(entry_id) {
                entries.push(Entry::new(
                    entry_id,
                    row.get(1).map_err(storage)?,
                    row.get::<_, Option<String>>(2)
                        .map_err(storage)?
                        .as_deref()
                        .and_then(RecordingTitle::new),
                ));
            }
            if let Some(session_id) = row.get::<_, Option<String>>(3).map_err(storage)? {
                let session_id = parse_session_id(&session_id)?;
                if let Some(entry) = entries.last_mut() {
                    entry.attach_session(session_id);
                }
            }
        }
        Ok(entries)
    }

    /// Renames an entry without changing any captured session fact.
    pub fn set_entry_title(
        &self,
        entry_id: EntryId,
        title: Option<&RecordingTitle>,
    ) -> Result<(), RagError> {
        let changed = self
            .writer
            .lock()
            .map_err(poisoned)?
            .execute(
                "UPDATE entries SET title=?2 WHERE id=?1",
                params![
                    entry_id.get().to_string(),
                    title.map(RecordingTitle::as_str)
                ],
            )
            .map_err(storage)?;
        if changed == 0 {
            return Err(RagError::NotFound {
                id: entry_id.get().to_string(),
            });
        }
        Ok(())
    }

    /// Attaches one existing session. Its timeline, recording, and captured scope are untouched.
    pub fn attach_session(&self, entry_id: EntryId, session_id: SessionId) -> Result<(), RagError> {
        let mut connection = self.writer.lock().map_err(poisoned)?;
        let transaction = connection.transaction().map_err(storage)?;
        attach_session_relation(&transaction, entry_id, session_id)?;
        transaction.commit().map_err(storage)
    }

    /// Answers which entry owns a recording in one query.
    pub fn entry_for_session(&self, session_id: SessionId) -> Result<EntryId, RagError> {
        self.reader
            .lock()
            .map_err(poisoned)?
            .query_row(
                "SELECT entry_id FROM entry_sessions WHERE session_id=?1",
                [session_id.get().to_string()],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .map_err(storage)?
            .ok_or_else(|| RagError::NotFound {
                id: session_id.get().to_string(),
            })
            .and_then(|value| parse_entry_id(&value))
    }

    /// Deletes an entry, its managed recording files, and every session-owned persisted row.
    ///
    /// This crosses a filesystem/SQLite atomicity boundary that a single transaction cannot
    /// span: unlinking a file and committing a row are two different durability mechanisms, and
    /// no ordering of "unlink then commit" or "commit then unlink" is safe on its own — whichever
    /// happens first can survive a crash while the second never runs. The fix is not ordering,
    /// it is quarantine: every managed file is renamed to a `.{session_id}.deleting` tombstone
    /// *before* the row transaction opens, so the rename is reversible and the row transaction is
    /// the single atomic instant that decides whether the rename should stick. If that transaction
    /// fails, every tombstone is renamed back and nothing is lost. If it commits, every tombstone
    /// is unlinked for good. If the process dies between the two, [`Self::recover_quarantined_media`]
    /// (run from [`Self::enforce_recording_budget`]) resolves the leftover tombstone from the row's
    /// surviving state on the next pass over the recording directory — see its doc comment for
    /// which way that resolves and why.
    pub fn delete_entry(
        &self,
        entry_id: EntryId,
        recording_directory: &Path,
    ) -> Result<(), RagError> {
        // Membership is resolved exactly once, inside this same writer transaction, and the
        // writer lock stays held from that resolution through the row commit. That is what
        // guarantees the quarantined file set and the deleted row set are the same set: nothing
        // else can attach a session to this entry in between (every attach path — `save_session`,
        // `save_session_in_entry`, `attach_session` — also goes through `self.writer`, so it
        // simply blocks until this call releases the lock), and the DELETE statements below only
        // ever see the membership this same call already quarantined. Quarantine itself still has
        // to happen before the commit, per this method's doc comment, so the transaction stays
        // open (uncommitted) across the filesystem renames rather than being opened fresh
        // afterward.
        let mut connection = self.writer.lock().map_err(poisoned)?;
        let transaction = connection.transaction().map_err(storage)?;
        let id = entry_id.get().to_string();
        let session_ids = {
            let mut statement = transaction
                .prepare("SELECT session_id FROM entry_sessions WHERE entry_id=?1")
                .map_err(storage)?;
            statement
                .query_map([&id], |row| row.get::<_, String>(0))
                .map_err(storage)?
                .collect::<Result<Vec<_>, _>>()
                .map_err(storage)?
                .into_iter()
                .map(|session_id| parse_session_id(&session_id))
                .collect::<Result<Vec<_>, _>>()?
        };

        let mut quarantined: Vec<(PathBuf, PathBuf)> = Vec::with_capacity(session_ids.len());
        for session_id in session_ids {
            match self.quarantine_recording_media(session_id, recording_directory) {
                Ok(Some(pair)) => quarantined.push(pair),
                Ok(None) => {}
                Err(error) => {
                    restore_quarantined(&quarantined);
                    return Err(error);
                }
            }
        }

        let outcome = (|| {
            transaction
                .execute(
                    "DELETE FROM vec_chunks WHERE chunk_id IN (\
                       SELECT c.id FROM chunks c JOIN documents d ON d.id=c.doc_id \
                       JOIN entry_sessions es ON es.session_id=d.source_session_id \
                       WHERE es.entry_id=?1\
                     )",
                    [&id],
                )
                .map_err(storage)?;
            transaction
                .execute(
                    "DELETE FROM events WHERE session_id IN (\
                       SELECT session_id FROM entry_sessions WHERE entry_id=?1\
                     )",
                    [&id],
                )
                .map_err(storage)?;
            transaction
                .execute(
                    "DELETE FROM sessions WHERE id IN (\
                       SELECT session_id FROM entry_sessions WHERE entry_id=?1\
                     )",
                    [&id],
                )
                .map_err(storage)?;
            let changed = transaction
                .execute("DELETE FROM entries WHERE id=?1", [&id])
                .map_err(storage)?;
            if changed == 0 {
                return Err(RagError::NotFound { id });
            }
            transaction.commit().map_err(storage)
        })();

        match outcome {
            Ok(()) => {
                for (_, tombstone) in &quarantined {
                    if let Err(error) = std::fs::remove_file(tombstone) {
                        eprintln!(
                            "Entry deletion committed but tombstone {} could not be unlinked: {error}",
                            tombstone.display()
                        );
                    }
                }
                Ok(())
            }
            Err(error) => {
                restore_quarantined(&quarantined);
                Err(error)
            }
        }
    }

    /// Inserts only; duplicate identities fail rather than mutating the append-only log.
    ///
    /// This synchronous method blocks until its transaction commits. Callers must batch
    /// events and invoke it from a background task, never from the live capture pipeline.
    pub fn append_events(&self, events: &[TimelineEvent]) -> Result<(), RagError> {
        let mut connection = self.writer.lock().map_err(poisoned)?;
        let transaction = connection.transaction().map_err(storage)?;
        {
            let mut statement = transaction.prepare(
                "INSERT INTO events(id,session_id,ts,kind,supersedes,payload) VALUES(?1,?2,?3,?4,?5,?6)"
            ).map_err(storage)?;
            for event in events {
                let nanos = i64::try_from(event.ts().as_nanos()).map_err(message)?;
                let payload = serde_json::to_string(event.payload()).map_err(message)?;
                statement
                    .execute(params![
                        event.id().get(),
                        event.session_id().get().to_string(),
                        nanos,
                        event.kind().as_str(),
                        event.supersedes().map(sotto_core::EventId::get),
                        payload
                    ])
                    .map_err(storage)?;
            }
        }
        transaction.commit().map_err(storage)
    }

    /// Appends finalized recording-tail utterances after the live pipeline has fully stopped.
    /// Event ids are allocated under the same SQLite write transaction, so the original timeline
    /// remains append-only and no tail can collide with a concurrently persisted checkpoint.
    pub fn append_final_utterances(
        &self,
        session_id: SessionId,
        utterances: &[Utterance],
    ) -> Result<Vec<TimelineEvent>, RagError> {
        if utterances.is_empty() {
            return Ok(Vec::new());
        }
        let mut connection = self.writer.lock().map_err(poisoned)?;
        let transaction = connection.transaction().map_err(storage)?;
        let mut next_id = transaction
            .query_row(
                "SELECT COALESCE(MAX(id),0)+1 FROM events WHERE session_id=?1",
                [session_id.get().to_string()],
                |row| row.get::<_, u64>(0),
            )
            .map_err(storage)?;
        let mut events = Vec::with_capacity(utterances.len());
        for utterance in utterances {
            if utterance.end < utterance.start {
                return Err(RagError::Storage(
                    "final utterance ends before it starts".to_owned(),
                ));
            }
            let event_payload = EventPayload::UtteranceFinal(utterance.clone());
            let event: TimelineEvent = serde_json::from_value(json!({
                "id": next_id,
                "session_id": session_id.get(),
                "ts": {
                    "secs": utterance.end.as_secs(),
                    "nanos": utterance.end.subsec_nanos()
                },
                "supersedes": null,
                "payload": event_payload
            }))
            .map_err(message)?;
            let payload = serde_json::to_string(event.payload()).map_err(message)?;
            let nanos = i64::try_from(event.ts().as_nanos()).map_err(message)?;
            transaction
                .execute(
                    "INSERT INTO events(id,session_id,ts,kind,supersedes,payload) VALUES(?1,?2,?3,?4,NULL,?5)",
                    params![next_id, session_id.get().to_string(), nanos, event.kind().as_str(), payload],
                )
                .map_err(storage)?;
            events.push(event);
            next_id = next_id.saturating_add(1);
        }
        transaction.commit().map_err(storage)?;
        Ok(events)
    }

    /// Reloads the append-only log in **allocation order**, which is the only order it replays in.
    ///
    /// Ordering by `ts` looks like the same thing and is not. A timeline event's `ts` is the
    /// coordinate the event is *about*, and two producers write it from different clocks: live
    /// stage events carry the audio stream offset of whichever stream produced them, while the
    /// tail utterances written by stop-time finalization carry media time. Neither is monotonic
    /// against the other, so `ORDER BY ts` reorders ids — an observed session produced
    /// `EventId(40)` (ts 26.3 s) ahead of `EventId(36)` (ts 27.3 s) — and [`sotto_core::replay`]
    /// rejects the result as `NonMonotonicId`, taking search retention, notes, clustering and Ask
    /// down with it. Consumers that want time order sort by the payload coordinate they actually
    /// mean; see [`Store::render_prior_meeting`].
    pub fn load_session(&self, session_id: SessionId) -> Result<Vec<TimelineEvent>, RagError> {
        let connection = self.reader.lock().map_err(poisoned)?;
        let mut statement = connection
            .prepare("SELECT id,ts,supersedes,payload FROM events WHERE session_id=?1 ORDER BY id")
            .map_err(storage)?;
        let rows = statement
            .query_map([session_id.get().to_string()], |row| {
                Ok((
                    row.get::<_, u64>(0)?,
                    row.get::<_, u64>(1)?,
                    row.get::<_, Option<u64>>(2)?,
                    row.get::<_, String>(3)?,
                ))
            })
            .map_err(storage)?;
        let mut events = Vec::new();
        for row in rows {
            let (id, nanos, supersedes, payload) = row.map_err(storage)?;
            let payload: Value = serde_json::from_str(&payload).map_err(message)?;
            events.push(
                serde_json::from_value(json!({
                    "id": id, "session_id": session_id.get(),
                    "ts": {"secs": nanos / 1_000_000_000, "nanos": nanos % 1_000_000_000},
                    "supersedes": supersedes, "payload": payload
                }))
                .map_err(message)?,
            );
        }
        Ok(events)
    }

    pub fn load_session_record(&self, id: SessionId) -> Result<Session, RagError> {
        self.reader.lock().map_err(poisoned)?.query_row(
            "SELECT started_at_unix_ms,ended_at_unix_ms,capture_target_bundle_id,capture_target_display_name,capture_target_window_title,capture_target_kind,capture_target_audio_scoped FROM sessions WHERE id=?1",
            [id.get().to_string()], |row| {
                let kind: String = row.get(5)?;
                let target = CaptureTarget { bundle_id: row.get(2)?, display_name: row.get(3)?,
                    window_title: row.get(4)?, kind: parse_target_kind(&kind)?,
                    audio_scoped: row.get(6)? };
                let mut session = Session::new(id, target, row.get(0)?);
                if let Some(ended) = row.get(1)? { session.end(ended); }
                Ok(session)
            }).optional().map_err(storage)?.ok_or_else(|| RagError::NotFound { id: id.get().to_string() })
    }

    /// Lists persisted sessions newest first without loading their timeline payloads.
    ///
    /// The chosen name is joined in rather than substituted for the capture target, so one query
    /// answers both "what is this called" and "what was recorded".
    pub fn list_sessions(&self) -> Result<Vec<SessionSummary>, RagError> {
        let connection = self.reader.lock().map_err(poisoned)?;
        let mut statement = connection
            .prepare(
                "SELECT s.id,s.started_at_unix_ms,s.ended_at_unix_ms,s.capture_target_bundle_id,s.capture_target_display_name,s.capture_target_window_title,s.capture_target_kind,s.capture_target_audio_scoped,t.title FROM sessions s LEFT JOIN session_titles t ON t.session_id=s.id ORDER BY s.started_at_unix_ms DESC,s.id DESC",
            )
            .map_err(storage)?;
        let rows = statement
            .query_map([], |row| {
                let id: String = row.get(0)?;
                let parsed_id = id.parse::<u128>().map_err(|error| {
                    rusqlite::Error::FromSqlConversionFailure(0, Type::Text, Box::new(error))
                })?;
                let kind: String = row.get(6)?;
                Ok(SessionSummary {
                    id: SessionId::new(parsed_id),
                    capture_target: CaptureTarget {
                        bundle_id: row.get(3)?,
                        display_name: row.get(4)?,
                        window_title: row.get(5)?,
                        kind: parse_target_kind(&kind)?,
                        audio_scoped: row.get(7)?,
                    },
                    title: row
                        .get::<_, Option<String>>(8)?
                        .as_deref()
                        .and_then(RecordingTitle::new),
                    started_at_unix_ms: row.get(1)?,
                    ended_at_unix_ms: row.get(2)?,
                })
            })
            .map_err(storage)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(storage)
    }

    /// Stores, replaces, or clears the person's chosen name for one recording.
    ///
    /// `None` clears the name and the recording falls back to being named by what was captured.
    /// Nothing on the `sessions` row is written either way: a rename is a label, never an edit of
    /// the capture target, and the foreign key means a title can only exist for a real session.
    pub fn set_session_title(
        &self,
        session_id: SessionId,
        title: Option<&RecordingTitle>,
    ) -> Result<(), RagError> {
        let id = session_id.get().to_string();
        let connection = self.writer.lock().map_err(poisoned)?;
        match title {
            Some(title) => {
                let updated_at = i64::try_from(
                    SystemTime::now()
                        .duration_since(UNIX_EPOCH)
                        .map_err(message)?
                        .as_millis(),
                )
                .map_err(message)?;
                connection
                    .execute(
                        "INSERT INTO session_titles(session_id,title,updated_at_unix_ms) VALUES(?1,?2,?3) ON CONFLICT(session_id) DO UPDATE SET title=excluded.title,updated_at_unix_ms=excluded.updated_at_unix_ms",
                        params![id, title.as_str(), updated_at],
                    )
                    .map_err(storage)?;
            }
            None => {
                connection
                    .execute("DELETE FROM session_titles WHERE session_id=?1", [id])
                    .map_err(storage)?;
            }
        }
        Ok(())
    }

    /// The chosen name for one recording, if anyone has chosen one.
    pub fn load_session_title(
        &self,
        session_id: SessionId,
    ) -> Result<Option<RecordingTitle>, RagError> {
        self.reader
            .lock()
            .map_err(poisoned)?
            .query_row(
                "SELECT title FROM session_titles WHERE session_id=?1",
                [session_id.get().to_string()],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .map_err(storage)
            .map(|value| value.as_deref().and_then(RecordingTitle::new))
    }

    pub fn save_recording(&self, recording: &SessionRecording) -> Result<(), RagError> {
        let SessionRecording::Available {
            session_id,
            path,
            container,
            duration,
            byte_size,
            time_mapping,
        } = recording
        else {
            return Err(RagError::Storage(
                "only an available recording can be initially saved".to_owned(),
            ));
        };
        if path.is_empty() || !time_mapping.is_identity() {
            return Err(RagError::Storage(
                "recording path must be non-empty and its v1 time mapping must be identity"
                    .to_owned(),
            ));
        }
        let duration_ns = u64::try_from(duration.as_nanos()).map_err(message)?;
        let updated_at = wall_clock_unix_ms()?;
        self.writer.lock().map_err(poisoned)?.execute(
            "INSERT INTO session_recordings(session_id,state,path,container,duration_ns,byte_size,session_origin_ns,media_origin_ns,rate_numerator,rate_denominator,finalization_error,updated_at_unix_ms) VALUES(?1,'available',?2,?3,?4,?5,?6,?7,?8,?9,NULL,?10) ON CONFLICT(session_id) DO UPDATE SET state='available',path=excluded.path,container=excluded.container,duration_ns=excluded.duration_ns,byte_size=excluded.byte_size,session_origin_ns=excluded.session_origin_ns,media_origin_ns=excluded.media_origin_ns,rate_numerator=excluded.rate_numerator,rate_denominator=excluded.rate_denominator,finalization_error=NULL,updated_at_unix_ms=excluded.updated_at_unix_ms",
            params![
                session_id.get().to_string(),
                path,
                container.as_str(),
                duration_ns,
                byte_size,
                time_mapping.session_origin_ns,
                time_mapping.media_origin_ns,
                time_mapping.rate_numerator,
                time_mapping.rate_denominator,
                updated_at,
            ],
        ).map_err(storage)?;
        Ok(())
    }

    /// Writes the managed path before capture starts without inventing size or duration.
    pub fn save_growing_recording(
        &self,
        session_id: SessionId,
        path: &Path,
    ) -> Result<(), RagError> {
        let path = path.to_string_lossy();
        if path.is_empty() {
            return Err(RagError::Storage(
                "recording path must be non-empty".to_owned(),
            ));
        }
        self.writer.lock().map_err(poisoned)?.execute(
            "INSERT INTO session_recordings(session_id,state,path,container,duration_ns,byte_size,session_origin_ns,media_origin_ns,rate_numerator,rate_denominator,finalization_error,updated_at_unix_ms) VALUES(?1,'growing',?2,'mp4',NULL,NULL,NULL,NULL,NULL,NULL,NULL,?3) ON CONFLICT(session_id) DO UPDATE SET state='growing',path=excluded.path,container='mp4',duration_ns=NULL,byte_size=NULL,session_origin_ns=NULL,media_origin_ns=NULL,rate_numerator=NULL,rate_denominator=NULL,finalization_error=NULL,updated_at_unix_ms=excluded.updated_at_unix_ms",
            params![session_id.get().to_string(), path.as_ref(), wall_clock_unix_ms()?],
        ).map_err(storage)?;
        Ok(())
    }

    /// Keeps the early reference addressable and records why exact metadata could not settle.
    pub fn mark_recording_finalization_failed(
        &self,
        session_id: SessionId,
        error: &str,
    ) -> Result<(), RagError> {
        self.writer.lock().map_err(poisoned)?.execute(
            "UPDATE session_recordings SET finalization_error=?2,updated_at_unix_ms=?3 WHERE session_id=?1 AND state='growing'",
            params![session_id.get().to_string(), error, wall_clock_unix_ms()?],
        ).map_err(storage)?;
        Ok(())
    }

    pub fn load_recording_reference(
        &self,
        session_id: SessionId,
    ) -> Result<Option<RecordingReference>, RagError> {
        self.reader.lock().map_err(poisoned)?.query_row(
            "SELECT state,path,container,duration_ns,byte_size,session_origin_ns,media_origin_ns,rate_numerator,rate_denominator,finalization_error FROM session_recordings WHERE session_id=?1",
            [session_id.get().to_string()],
            |row| decode_recording_reference_row(session_id, row, 0),
        ).optional().map_err(storage)
    }

    pub fn load_recording(
        &self,
        session_id: SessionId,
    ) -> Result<Option<SessionRecording>, RagError> {
        self.reader
            .lock()
            .map_err(poisoned)?
            .query_row(
                "SELECT state,path,container,duration_ns,byte_size,session_origin_ns,media_origin_ns,rate_numerator,rate_denominator FROM session_recordings WHERE session_id=?1 AND state!='growing'",
                [session_id.get().to_string()],
                |row| decode_recording_row(session_id, row),
            )
            .optional()
            .map_err(storage)
    }

    pub fn list_recordings(&self) -> Result<Vec<SessionRecording>, RagError> {
        let connection = self.reader.lock().map_err(poisoned)?;
        let mut statement = connection
            .prepare(
                "SELECT r.session_id,r.state,r.path,r.container,r.duration_ns,r.byte_size,r.session_origin_ns,r.media_origin_ns,r.rate_numerator,r.rate_denominator FROM session_recordings r JOIN sessions s ON s.id=r.session_id WHERE r.state!='growing' ORDER BY s.started_at_unix_ms DESC,r.session_id DESC",
            )
            .map_err(storage)?;
        let rows = statement
            .query_map([], |row| {
                let raw_id: String = row.get(0)?;
                let parsed = raw_id.parse::<u128>().map_err(|error| {
                    rusqlite::Error::FromSqlConversionFailure(0, Type::Text, Box::new(error))
                })?;
                decode_recording_row_offset(SessionId::new(parsed), row, 1)
            })
            .map_err(storage)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(storage)
    }

    pub fn list_recording_references(&self) -> Result<Vec<RecordingReference>, RagError> {
        let connection = self.reader.lock().map_err(poisoned)?;
        let mut statement = connection.prepare(
            "SELECT r.session_id,r.state,r.path,r.container,r.duration_ns,r.byte_size,r.session_origin_ns,r.media_origin_ns,r.rate_numerator,r.rate_denominator,r.finalization_error FROM session_recordings r JOIN sessions s ON s.id=r.session_id ORDER BY s.started_at_unix_ms DESC,r.session_id DESC",
        ).map_err(storage)?;
        let rows = statement
            .query_map([], |row| {
                let raw_id: String = row.get(0)?;
                let parsed = raw_id.parse::<u128>().map_err(|error| {
                    rusqlite::Error::FromSqlConversionFailure(0, Type::Text, Box::new(error))
                })?;
                decode_recording_reference_row(SessionId::new(parsed), row, 1)
            })
            .map_err(storage)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(storage)
    }

    pub fn replace_derived_transcript(
        &self,
        session_id: SessionId,
        model: &str,
        utterances: &[Utterance],
    ) -> Result<(), RagError> {
        if model.trim().is_empty() {
            return Err(RagError::Storage(
                "derived transcript model identity must not be empty".to_owned(),
            ));
        }
        let encoded = serde_json::to_string(utterances).map_err(message)?;
        self.writer.lock().map_err(poisoned)?.execute(
            "INSERT INTO session_retranscriptions(session_id,model,utterances,updated_at_unix_ms) VALUES(?1,?2,?3,?4) ON CONFLICT(session_id) DO UPDATE SET model=excluded.model,utterances=excluded.utterances,updated_at_unix_ms=excluded.updated_at_unix_ms",
            params![session_id.get().to_string(), model, encoded, wall_clock_unix_ms()?],
        ).map_err(storage)?;
        Ok(())
    }

    pub fn load_derived_transcript(
        &self,
        session_id: SessionId,
    ) -> Result<Option<DerivedTranscript>, RagError> {
        let row = self
            .reader
            .lock()
            .map_err(poisoned)?
            .query_row(
                "SELECT model,utterances FROM session_retranscriptions WHERE session_id=?1",
                [session_id.get().to_string()],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
            )
            .optional()
            .map_err(storage)?;
        row.map(|(model, encoded)| {
            Ok(DerivedTranscript {
                model,
                utterances: serde_json::from_str(&encoded).map_err(message)?,
            })
        })
        .transpose()
    }

    pub fn recording_usage(&self) -> Result<RecordingUsage, RagError> {
        let connection = self.reader.lock().map_err(poisoned)?;
        let mut statement = connection.prepare(
            "SELECT state,path,byte_size FROM session_recordings WHERE state IN ('growing','available')",
        ).map_err(storage)?;
        let rows = statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, Option<u64>>(2)?,
                ))
            })
            .map_err(storage)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(storage)?;
        let used_bytes = rows
            .into_iter()
            .map(|(state, path, saved)| {
                if state == "growing" {
                    std::fs::metadata(path).map_or(0, |metadata| metadata.len())
                } else {
                    saved.unwrap_or(0)
                }
            })
            .sum();
        let budget_bytes = connection
            .query_row(
                "SELECT budget_bytes FROM recording_retention_settings WHERE singleton=1",
                [],
                |row| row.get::<_, u64>(0),
            )
            .map_err(storage)?;
        Ok(RecordingUsage {
            used_bytes,
            budget_bytes,
        })
    }

    pub fn set_recording_budget(&self, budget_bytes: u64) -> Result<(), RagError> {
        if budget_bytes == 0 {
            return Err(RagError::Storage(
                "recording budget must be greater than zero".to_owned(),
            ));
        }
        self.writer
            .lock()
            .map_err(poisoned)?
            .execute(
                "UPDATE recording_retention_settings SET budget_bytes=?1 WHERE singleton=1",
                [budget_bytes],
            )
            .map_err(storage)?;
        Ok(())
    }

    /// Removes only local media. Timeline rows and the session record remain intact.
    pub fn remove_recording_media(
        &self,
        session_id: SessionId,
        reason: RecordingMissingReason,
        recording_directory: &Path,
    ) -> Result<bool, RagError> {
        let Some(reference) = self.load_recording_reference(session_id)? else {
            return Ok(false);
        };
        let path = match reference {
            RecordingReference::Growing { path, .. } => PathBuf::from(path),
            RecordingReference::Settled(SessionRecording::Available { path, .. }) => {
                PathBuf::from(path)
            }
            RecordingReference::Settled(SessionRecording::Missing { .. }) => return Ok(false),
        };
        validate_recording_path(session_id, recording_directory, &path)?;
        let tombstone = recording_directory.join(format!(".{}.deleting", session_id.get()));
        if let Err(error) = std::fs::rename(&path, &tombstone) {
            if error.kind() == io::ErrorKind::NotFound {
                self.mark_recording_missing(session_id, reason)?;
                return Ok(true);
            }
            return Err(io_error(error));
        }
        let update = self.mark_recording_missing(session_id, reason);
        if let Err(error) = update {
            let _ = std::fs::rename(&tombstone, &path);
            return Err(error);
        }
        if let Err(error) = std::fs::remove_file(&tombstone) {
            eprintln!(
                "Recording metadata was removed but tombstone {} could not be unlinked: {error}",
                tombstone.display()
            );
        }
        Ok(true)
    }

    /// Renames one session's managed file to its `.{session_id}.deleting` tombstone without
    /// touching any row.
    ///
    /// This is the quarantine half of [`Self::delete_entry`]'s two-phase delete: unlike
    /// [`Self::remove_recording_media`], which owns both the rename and the row update for the
    /// single-session case, this method only ever moves the file. The caller decides the row
    /// outcome for every quarantined session in one transaction, then either unlinks every
    /// tombstone (commit) or renames every one back (rollback) — see [`Self::delete_entry`].
    ///
    /// Returns `Ok(None)` when the session has no recording, or its recording is already
    /// `Missing`; there is nothing to quarantine either way.
    fn quarantine_recording_media(
        &self,
        session_id: SessionId,
        recording_directory: &Path,
    ) -> Result<Option<(PathBuf, PathBuf)>, RagError> {
        let Some(reference) = self.load_recording_reference(session_id)? else {
            return Ok(None);
        };
        let path = match reference {
            RecordingReference::Growing { path, .. } => PathBuf::from(path),
            RecordingReference::Settled(SessionRecording::Available { path, .. }) => {
                PathBuf::from(path)
            }
            RecordingReference::Settled(SessionRecording::Missing { .. }) => return Ok(None),
        };
        validate_recording_path(session_id, recording_directory, &path)?;
        let tombstone = recording_directory.join(format!(".{}.deleting", session_id.get()));
        match std::fs::rename(&path, &tombstone) {
            Ok(()) => Ok(Some((path, tombstone))),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(io_error(error)),
        }
    }

    /// Resolves every on-disk `.{session_id}.deleting` tombstone left by an interrupted
    /// [`Self::delete_entry`] or [`Self::remove_recording_media`] that crashed between renaming
    /// the file away and committing (or rolling back) the row change that was meant to decide its
    /// fate.
    ///
    /// The row is the only durable record of which side of that line a crash landed on, so it is
    /// the sole input to the decision, checked against a session's `sessions` row and its
    /// `session_recordings.path` column rather than trusting anything about the tombstone itself:
    ///
    /// - The session row is gone, or its recording row survives with `path` already cleared (the
    ///   `deleted`/`pruned` terminal state) — the row-side commit reached disk before the crash.
    ///   Finish what it started: unlink the tombstone. Restoring here would resurrect a file whose
    ///   owning record has already moved past it, which is a worse outcome than losing the file.
    /// - The session row still exists and its recording row still carries a path — the commit
    ///   never reached disk, so from the row's perspective the file was never removed. Rename the
    ///   tombstone back, leaving the state exactly as if the deletion had not been attempted.
    ///
    /// Deliberately not run inside [`Self::open`]: opening a database has no `recording_directory`
    /// to scan. Instead this runs at the top of [`Self::enforce_recording_budget`], the one place
    /// already given a recording directory on every call site the app has today (after a recording
    /// settles, and whenever the retention budget changes) — so both existing callers resolve any
    /// leftover tombstone before they measure or prune anything, with no separate wiring required.
    pub fn recover_quarantined_media(
        &self,
        recording_directory: &Path,
    ) -> Result<Vec<QuarantineRecovery>, RagError> {
        let mut resolved = Vec::new();
        let entries = match std::fs::read_dir(recording_directory) {
            Ok(entries) => entries,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(resolved),
            Err(error) => return Err(io_error(error)),
        };
        for entry in entries {
            let entry = entry.map_err(io_error)?;
            let file_name = entry.file_name();
            let Some(session_id) = file_name.to_str().and_then(parse_tombstone_session_id) else {
                continue;
            };
            let tombstone = entry.path();
            let original = recording_directory.join(format!("{}.mp4", session_id.get()));
            let session_exists = self
                .reader
                .lock()
                .map_err(poisoned)?
                .query_row(
                    "SELECT 1 FROM sessions WHERE id=?1",
                    [session_id.get().to_string()],
                    |_| Ok(()),
                )
                .optional()
                .map_err(storage)?
                .is_some();
            let recording_path_present = session_exists
                && self
                    .reader
                    .lock()
                    .map_err(poisoned)?
                    .query_row(
                        "SELECT 1 FROM session_recordings WHERE session_id=?1 AND path IS NOT NULL",
                        [session_id.get().to_string()],
                        |_| Ok(()),
                    )
                    .optional()
                    .map_err(storage)?
                    .is_some();
            if session_exists && recording_path_present {
                std::fs::rename(&tombstone, &original).map_err(io_error)?;
                resolved.push(QuarantineRecovery {
                    session_id,
                    restored: true,
                });
            } else {
                std::fs::remove_file(&tombstone).map_err(io_error)?;
                resolved.push(QuarantineRecovery {
                    session_id,
                    restored: false,
                });
            }
        }
        Ok(resolved)
    }

    pub fn enforce_recording_budget(
        &self,
        recording_directory: &Path,
    ) -> Result<Vec<SessionId>, RagError> {
        self.recover_quarantined_media(recording_directory)?;
        let budget = self.recording_usage()?.budget_bytes;
        let connection = self.reader.lock().map_err(poisoned)?;
        let mut statement = connection
            .prepare(
                "SELECT r.session_id,r.state,r.path,r.byte_size FROM session_recordings r JOIN sessions s ON s.id=r.session_id WHERE r.state IN ('growing','available') ORDER BY s.started_at_unix_ms ASC,r.session_id ASC",
            )
            .map_err(storage)?;
        let rows = statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, Option<u64>>(3)?,
                ))
            })
            .map_err(storage)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(storage)?;
        drop(statement);
        drop(connection);
        let measured = rows
            .into_iter()
            .map(|(id, state, path, saved)| {
                let bytes = if state == "growing" {
                    std::fs::metadata(&path).map_or(0, |metadata| metadata.len())
                } else {
                    saved.unwrap_or(0)
                };
                (id, bytes)
            })
            .collect::<Vec<_>>();
        let mut used = measured.iter().map(|(_, bytes)| *bytes).sum::<u64>();
        let mut pruned = Vec::new();
        for (raw_id, bytes) in measured {
            if used <= budget {
                break;
            }
            let parsed = raw_id.parse::<u128>().map_err(message)?;
            let id = SessionId::new(parsed);
            if self.remove_recording_media(
                id,
                RecordingMissingReason::Pruned,
                recording_directory,
            )? {
                used = used.saturating_sub(bytes);
                pruned.push(id);
            }
        }
        Ok(pruned)
    }

    fn mark_recording_missing(
        &self,
        session_id: SessionId,
        reason: RecordingMissingReason,
    ) -> Result<(), RagError> {
        let state = match reason {
            RecordingMissingReason::Deleted => "deleted",
            RecordingMissingReason::Pruned => "pruned",
        };
        self.writer.lock().map_err(poisoned)?.execute(
            "UPDATE session_recordings SET state=?2,path=NULL,container=NULL,duration_ns=NULL,byte_size=NULL,session_origin_ns=NULL,media_origin_ns=NULL,rate_numerator=NULL,rate_denominator=NULL,finalization_error=NULL,updated_at_unix_ms=?3 WHERE session_id=?1",
            params![session_id.get().to_string(), state, wall_clock_unix_ms()?],
        ).map_err(storage)?;
        Ok(())
    }

    /// Loads a model-derived artifact only when it matches the exact timeline content.
    pub fn load_derived_view(
        &self,
        session_id: SessionId,
        kind: &str,
        model: &str,
        content_hash: &str,
    ) -> Result<Option<(String, String)>, RagError> {
        self.reader.lock().map_err(poisoned)?.query_row(
            "SELECT artifact,usage FROM derived_views WHERE session_id=?1 AND kind=?2 AND model=?3 AND content_hash=?4",
            params![session_id.get().to_string(), kind, model, content_hash],
            |row| Ok((row.get(0)?, row.get(1)?)),
        ).optional().map_err(storage)
    }

    /// Stores a derived view alongside, never inside, the append-only event log.
    pub fn save_derived_view(
        &self,
        session_id: SessionId,
        kind: &str,
        model: &str,
        content_hash: &str,
        artifact: &str,
        usage: &str,
    ) -> Result<(), RagError> {
        let created_at = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(message)?
            .as_secs();
        self.writer.lock().map_err(poisoned)?.execute(
            "INSERT INTO derived_views(session_id,kind,model,content_hash,artifact,usage,created_at) VALUES(?1,?2,?3,?4,?5,?6,?7) ON CONFLICT(session_id,kind,model,content_hash) DO NOTHING",
            params![session_id.get().to_string(), kind, model, content_hash, artifact, usage, created_at],
        ).map_err(storage)?;
        Ok(())
    }

    /// Atomically stores a cited artifact and its exact immutable external evidence bundle.
    #[expect(
        clippy::too_many_arguments,
        reason = "the composite SQLite identity and payload are deliberately explicit at this atomic boundary"
    )]
    pub fn save_grounded_derived_view(
        &self,
        session_id: SessionId,
        kind: &str,
        model: &str,
        content_hash: &str,
        artifact: &str,
        usage: &str,
        provider_model: &str,
        grant_fingerprint: Option<&str>,
        source_status: &str,
        bundle: &str,
    ) -> Result<(), RagError> {
        let created_at = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(message)?
            .as_secs();
        let mut connection = self.writer.lock().map_err(poisoned)?;
        let transaction = connection.transaction().map_err(storage)?;
        transaction.execute(
            "INSERT INTO grounded_derived_views(session_id,kind,model,content_hash,artifact,usage,provider_model,grant_fingerprint,source_status,bundle,created_at) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11) ON CONFLICT(session_id,kind,model,content_hash) DO NOTHING",
            params![session_id.get().to_string(), kind, model, content_hash, artifact, usage, provider_model, grant_fingerprint, source_status, bundle, created_at],
        ).map_err(storage)?;
        let stored = transaction.query_row(
            "SELECT artifact,usage,provider_model,grant_fingerprint,source_status,bundle FROM grounded_derived_views WHERE session_id=?1 AND kind=?2 AND model=?3 AND content_hash=?4",
            params![session_id.get().to_string(), kind, model, content_hash],
            |row| Ok(GroundedDerivedView { artifact: row.get(0)?, usage: row.get(1)?, provider_model: row.get(2)?, grant_fingerprint: row.get(3)?, source_status: row.get(4)?, bundle: row.get(5)? }),
        ).map_err(storage)?;
        if stored.artifact != artifact
            || stored.usage != usage
            || stored.provider_model != provider_model
            || stored.grant_fingerprint.as_deref() != grant_fingerprint
            || stored.source_status != source_status
            || stored.bundle != bundle
        {
            return Err(RagError::Storage(
                "grounded derived artifact identity already contains different evidence".to_owned(),
            ));
        }
        transaction.commit().map_err(storage)
    }

    pub fn load_grounded_derived_view(
        &self,
        session_id: SessionId,
        kind: &str,
        model: &str,
        content_hash: &str,
    ) -> Result<Option<GroundedDerivedView>, RagError> {
        self.reader.lock().map_err(poisoned)?.query_row(
            "SELECT artifact,usage,provider_model,grant_fingerprint,source_status,bundle FROM grounded_derived_views WHERE session_id=?1 AND kind=?2 AND model=?3 AND content_hash=?4",
            params![session_id.get().to_string(), kind, model, content_hash],
            |row| Ok(GroundedDerivedView { artifact: row.get(0)?, usage: row.get(1)?, provider_model: row.get(2)?, grant_fingerprint: row.get(3)?, source_status: row.get(4)?, bundle: row.get(5)? }),
        ).optional().map_err(storage)
    }

    pub fn load_latest_grounded_derived_view(
        &self,
        session_id: SessionId,
        kind: &str,
    ) -> Result<Option<GroundedDerivedArtifact>, RagError> {
        self.reader.lock().map_err(poisoned)?.query_row(
            "SELECT model,content_hash,artifact,usage,provider_model,grant_fingerprint,source_status,bundle FROM grounded_derived_views WHERE session_id=?1 AND kind=?2 ORDER BY created_at DESC,model,content_hash LIMIT 1",
            params![session_id.get().to_string(), kind],
            |row| Ok(GroundedDerivedArtifact { model: row.get(0)?, content_hash: row.get(1)?, view: GroundedDerivedView { artifact: row.get(2)?, usage: row.get(3)?, provider_model: row.get(4)?, grant_fingerprint: row.get(5)?, source_status: row.get(6)?, bundle: row.get(7)? } }),
        ).optional().map_err(storage)
    }

    /// Deletes one recording session and removes its entry only when that entry becomes empty.
    ///
    /// A multi-session entry survives loss of one recording. The implicit one-session entry that
    /// ordinary capture creates does not become a phantom prepared entry after its only session is
    /// deleted.
    pub fn delete_session(&self, session_id: SessionId) -> Result<(), RagError> {
        let mut connection = self.writer.lock().map_err(poisoned)?;
        let transaction = connection.transaction().map_err(storage)?;
        let id = session_id.get().to_string();
        let entry_id = transaction
            .query_row(
                "SELECT entry_id FROM entry_sessions WHERE session_id=?1",
                [&id],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .map_err(storage)?;
        transaction
            .execute(
                "DELETE FROM vec_chunks WHERE chunk_id IN (SELECT c.id FROM chunks c JOIN documents d ON d.id=c.doc_id WHERE d.source_session_id=?1)",
                [&id],
            )
            .map_err(storage)?;
        transaction
            .execute("DELETE FROM events WHERE session_id=?1", [&id])
            .map_err(storage)?;
        transaction
            .execute("DELETE FROM sessions WHERE id=?1", [&id])
            .map_err(storage)?;
        if let Some(entry_id) = entry_id {
            transaction
                .execute(
                    "DELETE FROM entries WHERE id=?1 AND NOT EXISTS (SELECT 1 FROM entry_sessions WHERE entry_id=?1)",
                    [&entry_id],
                )
                .map_err(storage)?;
        }
        transaction.commit().map_err(storage)
    }

    pub fn ingest_text(
        &self,
        text: &str,
        kind: DocumentKind,
        metadata: IngestMetadata,
    ) -> Result<bool, RagError> {
        self.validate_ingest(kind, &metadata)?;
        let hash = content_hash(text);
        if self.document_exists(kind, &hash, &metadata)? {
            return Ok(false);
        }
        let chunks = chunk_markdown(text);
        if chunks.is_empty() {
            return Err(RagError::Storage(
                "cannot ingest an empty local document".to_owned(),
            ));
        }
        let embeddings = self.embed(chunks.iter().map(String::as_str).collect())?;
        self.persist_document(kind, metadata, hash, chunks, embeddings)
    }

    /// Indexes a completed recording's final transcript as session-owned local evidence.
    ///
    /// Every completed recording is searchable — there is no per-recording opt-in and no policy
    /// table consulted at query time. The index is local, lives in the same file as the timeline it
    /// came from, and is deleted with the recording, so the choice a person actually has is whether
    /// to keep the recording at all.
    ///
    /// Idempotent by content hash: re-indexing an unchanged transcript does no embedding work and
    /// returns `false`. A recording that captured no speech has nothing to index and also returns
    /// `false` rather than failing — silence is a legitimate recording, not a broken one.
    pub fn index_prior_meeting(
        &self,
        session_id: SessionId,
        collection_id: Option<String>,
    ) -> Result<bool, RagError> {
        let Some((text, title)) = self.render_prior_meeting(session_id)? else {
            return Ok(false);
        };
        self.ingest_text(
            &text,
            DocumentKind::PriorMeeting,
            IngestMetadata {
                title,
                collection_id,
                source_session_id: Some(session_id),
                ..IngestMetadata::default()
            },
        )
    }

    /// Whether this recording's transcript is already in the cross-recording index.
    ///
    /// Presence of the document *is* the state. There is no separate policy row that could
    /// disagree with what is actually indexed.
    pub fn is_session_indexed(&self, session_id: SessionId) -> Result<bool, RagError> {
        self.reader
            .lock()
            .map_err(poisoned)?
            .query_row(
                "SELECT 1 FROM documents WHERE kind='prior_meeting' AND source_session_id=?1 LIMIT 1",
                [session_id.get().to_string()],
                |_| Ok(()),
            )
            .optional()
            .map(|value| value.is_some())
            .map_err(storage)
    }

    /// Indexes every completed recording that is not in the index yet, newest first.
    ///
    /// This is what makes "all recordings" true for a library that predates unconditional
    /// indexing, and what repairs a session whose indexing pass failed at stop time. It embeds, so
    /// callers run it off the UI thread; it is idempotent, so running it twice is free.
    ///
    /// Returns the recordings it added. A recording that cannot be rendered is reported in the
    /// error list and skipped: one unreadable timeline must not withhold the rest of the library
    /// from search.
    pub fn index_missing_prior_meetings(&self) -> Result<PriorMeetingIndexReport, RagError> {
        let pending = {
            let connection = self.reader.lock().map_err(poisoned)?;
            let mut statement = connection
                .prepare(
                    "SELECT s.id FROM sessions s WHERE s.ended_at_unix_ms IS NOT NULL AND NOT EXISTS (
                       SELECT 1 FROM documents d
                       WHERE d.kind='prior_meeting' AND d.source_session_id=s.id)
                     ORDER BY s.started_at_unix_ms DESC",
                )
                .map_err(storage)?;
            statement
                .query_map([], |row| row.get::<_, String>(0))
                .map_err(storage)?
                .map(|row| {
                    row.map_err(storage)
                        .and_then(|value| parse_session_id(&value))
                })
                .collect::<Result<Vec<_>, _>>()?
        };
        let mut report = PriorMeetingIndexReport::default();
        for session_id in pending {
            match self.index_prior_meeting(session_id, None) {
                Ok(true) => report.indexed.push(session_id),
                Ok(false) => report.skipped.push(session_id),
                Err(error) => report.failed.push((session_id, error.to_string())),
            }
        }
        Ok(report)
    }

    /// Indexes a reviewable meeting-note artifact while retaining its source meeting identity.
    pub fn ingest_meeting_note(
        &self,
        session_id: SessionId,
        title: String,
        text: &str,
        collection_id: Option<String>,
    ) -> Result<bool, RagError> {
        self.ensure_completed_session(session_id)?;
        self.ingest_text(
            text,
            DocumentKind::MeetingNote,
            IngestMetadata {
                title,
                collection_id,
                source_session_id: Some(session_id),
                ..IngestMetadata::default()
            },
        )
    }

    /// Resolves a local citation to the exact stored chunk and source metadata.
    pub fn load_local_evidence(&self, evidence_id: &str) -> Result<LocalEvidenceReceipt, RagError> {
        let connection = self.reader.lock().map_err(poisoned)?;
        let receipt = connection
            .query_row(
                "SELECT c.doc_id,c.ordinal,c.text,c.metadata,d.kind,d.title,d.source_path,d.collection_id,d.source_session_id,d.provenance_status FROM chunks c JOIN documents d ON d.id=c.doc_id WHERE c.id=?1",
                [evidence_id],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, usize>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, String>(4)?,
                        row.get::<_, String>(5)?,
                        row.get::<_, Option<String>>(6)?,
                        row.get::<_, Option<String>>(7)?,
                        row.get::<_, Option<String>>(8)?,
                        row.get::<_, String>(9)?,
                    ))
                },
            )
            .optional()
            .map_err(storage)?
            .ok_or_else(|| RagError::NotFound {
                id: evidence_id.to_owned(),
            })?;
        let source_session_id = receipt.8.as_deref().map(parse_session_id).transpose()?;
        Ok(LocalEvidenceReceipt {
            evidence_id: evidence_id.to_owned(),
            document_id: receipt.0,
            ordinal: receipt.1,
            text: receipt.2,
            metadata: serde_json::from_str(&receipt.3).map_err(message)?,
            kind: DocumentKind::parse(&receipt.4)?,
            title: receipt.5,
            source_path: receipt.6,
            collection_id: receipt.7,
            source_session_id,
            provenance: LocalProvenance::parse(&receipt.9)?,
        })
    }

    fn validate_ingest(
        &self,
        kind: DocumentKind,
        metadata: &IngestMetadata,
    ) -> Result<(), RagError> {
        match (kind.requires_source_session(), metadata.source_session_id) {
            (true, Some(session_id)) => self.ensure_completed_session(session_id),
            (true, None) => Err(RagError::Storage(format!(
                "{} requires a completed source session",
                kind.as_str()
            ))),
            (false, Some(_)) => Err(RagError::Storage(format!(
                "{} must remain independent of a meeting",
                kind.as_str()
            ))),
            (false, None) => Ok(()),
        }
    }

    fn ensure_completed_session(&self, session_id: SessionId) -> Result<(), RagError> {
        let session = self.load_session_record(session_id)?;
        if session.ended_at_unix_ms().is_none() {
            return Err(RagError::Storage(format!(
                "source session {} is not durably completed",
                session_id.get()
            )));
        }
        Ok(())
    }

    /// Renders one completed recording as indexable text, or `None` when it captured no speech.
    fn render_prior_meeting(
        &self,
        session_id: SessionId,
    ) -> Result<Option<(String, String)>, RagError> {
        let (mut text, title, events) = self.prior_meeting_preamble(session_id)?;
        let replayed = sotto_core::replay(&events).map_err(message)?;
        let mut finals: Vec<_> = replayed
            .active()
            .values()
            .filter_map(|event| {
                let EventPayload::UtteranceFinal(utterance) = event.payload() else {
                    return None;
                };
                Some((utterance.start, event.id().get(), utterance))
            })
            .collect();
        finals.sort_by_key(|(start, event_id, _)| (*start, *event_id));
        if finals.is_empty() {
            return Ok(None);
        }
        for (start, event_id, utterance) in finals {
            let milliseconds = start.as_millis();
            let minutes = milliseconds / 60_000;
            let seconds = (milliseconds % 60_000) / 1_000;
            let remainder = milliseconds % 1_000;
            let speaker = match utterance.source {
                Source::Mic => "You",
                Source::System => "Meeting audio",
            };
            text.push_str(&format!(
                "[{minutes:02}:{seconds:02}.{remainder:03}] [{speaker}] \"{}\" [event:{event_id}]\n",
                utterance.text,
            ));
        }
        Ok(Some((text, title)))
    }

    /// The heading every prior-meeting document opens with, its title, and the log behind it.
    ///
    /// Shared so a recording that captured no speech but carries typed notes can still be rendered
    /// and indexed: the annotated renderer appends to this same preamble rather than depending on
    /// a transcript existing first.
    pub(super) fn prior_meeting_preamble(
        &self,
        session_id: SessionId,
    ) -> Result<(String, String, Vec<TimelineEvent>), RagError> {
        self.ensure_completed_session(session_id)?;
        let session = self.load_session_record(session_id)?;
        let events = self.load_session(session_id)?;
        let display_name = &session.capture_target().display_name;
        let mut text =
            format!("# Prior meeting: {display_name}\n\nCapture target: {display_name}\n");
        if let Some(window_title) = &session.capture_target().window_title {
            text.push_str("Window: ");
            text.push_str(window_title);
            text.push('\n');
        }
        Ok((text, format!("Meeting — {display_name}"), events))
    }

    fn document_exists(
        &self,
        kind: DocumentKind,
        content_hash: &str,
        metadata: &IngestMetadata,
    ) -> Result<bool, RagError> {
        self.reader
            .lock()
            .map_err(poisoned)?
            .query_row(
                "SELECT 1 FROM documents WHERE kind=?1 AND content_hash=?2 AND collection_id IS ?3 AND source_session_id IS ?4 AND source_path IS ?5",
                params![
                    kind.as_str(),
                    content_hash,
                    metadata.collection_id.as_deref(),
                    metadata.source_session_id.map(|id| id.get().to_string()),
                    metadata.source_path.as_deref()
                ],
                |_| Ok(()),
            )
            .optional()
            .map(|row| row.is_some())
            .map_err(storage)
    }

    fn persist_document(
        &self,
        kind: DocumentKind,
        metadata: IngestMetadata,
        hash: String,
        chunks: Vec<String>,
        embeddings: Vec<Vec<f32>>,
    ) -> Result<bool, RagError> {
        if chunks.len() != embeddings.len() || embeddings.iter().any(|vector| vector.len() != 384) {
            return Err(RagError::Embedding(
                "local document embedding count or dimensions did not match its chunks".to_owned(),
            ));
        }
        let source_session_id = metadata.source_session_id.map(|id| id.get().to_string());
        let document_id = document_id(
            kind,
            &hash,
            metadata.collection_id.as_deref(),
            source_session_id.as_deref(),
            metadata.source_path.as_deref(),
        );
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(message)?
            .as_secs();
        let mut connection = self.writer.lock().map_err(poisoned)?;
        let transaction = connection.transaction().map_err(storage)?;
        transaction
            .execute(
                "INSERT INTO documents(id,kind,title,source_path,collection_id,source_session_id,provenance_status,ingested_at,content_hash) VALUES(?1,?2,?3,?4,?5,?6,'native',?7,?8)",
                params![
                    document_id,
                    kind.as_str(),
                    metadata.title,
                    metadata.source_path,
                    metadata.collection_id,
                    source_session_id,
                    now,
                    hash
                ],
            )
            .map_err(storage)?;
        let metadata_json = serde_json::to_string(&metadata.fields).map_err(message)?;
        for (ordinal, (text, vector)) in chunks.iter().zip(embeddings).enumerate() {
            let id = format!("{document_id}:{ordinal}");
            let blob = vector_blob(&vector);
            transaction
                .execute(
                    "INSERT INTO chunks VALUES(?1,?2,?3,?4,?5,?6,?7)",
                    params![
                        id,
                        document_id,
                        ordinal,
                        text,
                        word_count(text),
                        metadata_json,
                        blob
                    ],
                )
                .map_err(storage)?;
            transaction
                .execute(
                    "INSERT INTO vec_chunks(chunk_id,embedding) VALUES(?1,?2)",
                    params![id, blob],
                )
                .map_err(storage)?;
        }
        transaction.commit().map_err(storage)?;
        Ok(true)
    }

    pub fn unload_embedding_model(&self) -> Result<(), RagError> {
        *self.embedding.lock().map_err(poisoned)? = None;
        Ok(())
    }

    pub fn search_filtered(
        &self,
        query: &str,
        k: usize,
        filter: &SearchFilter,
    ) -> Result<Vec<Chunk>, RagError> {
        if k == 0 {
            return Ok(Vec::new());
        }
        let query_vector = self
            .embed(vec![query])?
            .pop()
            .ok_or_else(|| RagError::Embedding("empty embedding result".to_owned()))?;
        self.search_with_vector(query, k, filter, &query_vector, true)
    }

    fn search_with_vector(
        &self,
        query: &str,
        k: usize,
        filter: &SearchFilter,
        query_vector: &[f32],
        include_keyword: bool,
    ) -> Result<Vec<Chunk>, RagError> {
        let connection = self.reader.lock().map_err(poisoned)?;
        let mut scores = HashMap::<String, f64>::new();
        let filter_kind = filter.kind.map(DocumentKind::as_str);
        let filter_session = filter
            .source_session_id
            .map(|session_id| session_id.get().to_string());
        let mut vector = connection
            .prepare(
                // No retention predicate: an indexed prior meeting is searchable by construction.
                // The document's presence is the whole policy, and deleting the recording cascades
                // it away — see `Store::index_prior_meeting`.
                "SELECT v.chunk_id FROM vec_chunks v
             JOIN chunks c ON c.id=v.chunk_id JOIN documents d ON d.id=c.doc_id
             WHERE v.embedding MATCH ?1 AND k=?2
               AND (?3 IS NULL OR d.kind=?3)
               AND (?4 IS NULL OR d.collection_id=?4)
               AND (?5 IS NULL OR d.source_session_id=?5)
             ORDER BY v.distance",
            )
            .map_err(storage)?;
        let mut candidate_limit = k.saturating_mul(4).clamp(1, MAX_FILTERED_CANDIDATES);
        loop {
            scores.clear();
            for (rank, row) in vector
                .query_map(
                    params![
                        vector_blob(query_vector),
                        candidate_limit,
                        filter_kind,
                        filter.collection_id.as_deref(),
                        filter_session.as_deref()
                    ],
                    |row| row.get::<_, String>(0),
                )
                .map_err(storage)?
                .enumerate()
            {
                scores.insert(row.map_err(storage)?, rank_score(rank));
            }
            if scores.len() >= k || candidate_limit == MAX_FILTERED_CANDIDATES {
                break;
            }
            candidate_limit = candidate_limit
                .saturating_mul(2)
                .min(MAX_FILTERED_CANDIDATES);
        }
        if include_keyword {
            let mut keyword = connection
                .prepare(
                    "SELECT c.id FROM chunks_fts f JOIN chunks c ON c.rowid=f.rowid
                 JOIN documents d ON d.id=c.doc_id
                 WHERE chunks_fts MATCH ?1
                   AND (?3 IS NULL OR d.kind=?3)
                   AND (?4 IS NULL OR d.collection_id=?4)
                   AND (?5 IS NULL OR d.source_session_id=?5)
                 ORDER BY bm25(chunks_fts) LIMIT ?2",
                )
                .map_err(storage)?;
            for (rank, row) in keyword
                .query_map(
                    params![
                        query,
                        candidate_limit,
                        filter_kind,
                        filter.collection_id.as_deref(),
                        filter_session.as_deref()
                    ],
                    |row| row.get::<_, String>(0),
                )
                .map_err(storage)?
                .enumerate()
            {
                *scores.entry(row.map_err(storage)?).or_default() += rank_score(rank);
            }
        }
        let mut ranked: Vec<_> = scores.into_iter().collect();
        ranked.sort_by(|a, b| b.1.total_cmp(&a.1));
        let mut output = Vec::new();
        let mut fetch = connection.prepare("SELECT c.text,d.title,d.kind,d.collection_id,d.source_session_id,d.source_path,d.provenance_status,c.doc_id,c.ordinal,c.metadata FROM chunks c JOIN documents d ON d.id=c.doc_id WHERE c.id=?1").map_err(storage)?;
        for (id, _) in ranked {
            let row = fetch
                .query_row([&id], |row| {
                    Ok(RetrievedChunkRow {
                        text: row.get(0)?,
                        title: row.get(1)?,
                        kind: row.get(2)?,
                        collection_id: row.get(3)?,
                        source_session_id: row.get(4)?,
                        source_path: row.get(5)?,
                        provenance: row.get(6)?,
                        document_id: row.get(7)?,
                        ordinal: row.get(8)?,
                        metadata: row.get(9)?,
                    })
                })
                .map_err(storage)?;
            let parsed_source_session = row
                .source_session_id
                .as_deref()
                .map(parse_session_id)
                .transpose()?;
            if filter.kind.is_some_and(|value| value.as_str() != row.kind)
                || filter
                    .collection_id
                    .as_ref()
                    .is_some_and(|value| row.collection_id.as_ref() != Some(value))
                || filter
                    .source_session_id
                    .is_some_and(|value| parsed_source_session != Some(value))
            {
                continue;
            }
            let mut metadata =
                serde_json::from_str::<BTreeMap<String, String>>(&row.metadata).map_err(message)?;
            metadata.insert("evidence_scope".to_owned(), "local".to_owned());
            metadata.insert("document_id".to_owned(), row.document_id);
            metadata.insert("ordinal".to_owned(), row.ordinal.to_string());
            metadata.insert("kind".to_owned(), row.kind);
            metadata.insert("provenance_status".to_owned(), row.provenance);
            if let Some(value) = row.collection_id {
                metadata.insert("collection_id".to_owned(), value);
            }
            if let Some(value) = row.source_session_id {
                metadata.insert("source_session_id".to_owned(), value);
            }
            if let Some(value) = row.source_path {
                metadata.insert("source_path".to_owned(), value);
            }
            output.push(Chunk {
                id,
                text: row.text,
                source: row.title,
                metadata,
            });
            if output.len() == k {
                break;
            }
        }
        Ok(output)
    }

    fn embed(&self, texts: Vec<&str>) -> Result<Vec<Vec<f32>>, RagError> {
        let mut embedding = self.embedding.lock().map_err(poisoned)?;
        if embedding.is_none() {
            *embedding = Some(
                TextEmbedding::try_new(
                    InitOptions::new(EmbeddingModel::AllMiniLML6V2)
                        .with_show_download_progress(false),
                )
                .map_err(|error| RagError::Embedding(error.to_string()))?,
            );
        }
        embedding
            .as_mut()
            .ok_or_else(|| RagError::Embedding("model unavailable".to_owned()))?
            .embed(texts, None)
            .map_err(|error| RagError::Embedding(error.to_string()))
    }
}

impl Retriever for Store {
    fn search<'a>(
        &'a self,
        query: &'a str,
        k: usize,
    ) -> BoxFuture<'a, Result<Vec<Chunk>, RagError>> {
        Box::pin(async move { self.search_filtered(query, k, &SearchFilter::default()) })
    }
}

fn register_sqlite_vec() {
    // SAFETY: sqlite-vec exports SQLite's standard extension entry point but declares
    // it as a zero-argument symbol; this is the registration pattern documented and
    // tested by the sqlite-vec crate itself.
    unsafe {
        rusqlite::ffi::sqlite3_auto_extension(Some(std::mem::transmute::<
            *const (),
            unsafe extern "C" fn(
                *mut rusqlite::ffi::sqlite3,
                *mut *mut std::ffi::c_char,
                *const rusqlite::ffi::sqlite3_api_routines,
            ) -> std::ffi::c_int,
        >(
            sqlite_vec::sqlite3_vec_init as *const ()
        )))
    };
}
fn target_kind(kind: TargetKind) -> &'static str {
    match kind {
        TargetKind::Application => "application",
        TargetKind::Window => "window",
        TargetKind::Display => "display",
        TargetKind::Microphone => "microphone",
    }
}

fn parse_target_kind(kind: &str) -> rusqlite::Result<TargetKind> {
    match kind {
        "application" => Ok(TargetKind::Application),
        "window" => Ok(TargetKind::Window),
        "display" => Ok(TargetKind::Display),
        "microphone" => Ok(TargetKind::Microphone),
        unknown => Err(rusqlite::Error::FromSqlConversionFailure(
            5,
            Type::Text,
            Box::new(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("unknown capture target kind {unknown:?}"),
            )),
        )),
    }
}

fn decode_recording_row(
    session_id: SessionId,
    row: &rusqlite::Row<'_>,
) -> rusqlite::Result<SessionRecording> {
    decode_recording_row_offset(session_id, row, 0)
}

fn decode_recording_row_offset(
    session_id: SessionId,
    row: &rusqlite::Row<'_>,
    offset: usize,
) -> rusqlite::Result<SessionRecording> {
    let state: String = row.get(offset)?;
    match state.as_str() {
        "available" => {
            let container: String = row.get(offset + 2)?;
            if container != "mp4" {
                return Err(rusqlite::Error::FromSqlConversionFailure(
                    offset + 2,
                    Type::Text,
                    Box::new(io::Error::new(
                        io::ErrorKind::InvalidData,
                        format!("unknown recording container {container:?}"),
                    )),
                ));
            }
            Ok(SessionRecording::Available {
                session_id,
                path: row.get(offset + 1)?,
                container: RecordingContainer::Mp4,
                duration: std::time::Duration::from_nanos(row.get(offset + 3)?),
                byte_size: row.get(offset + 4)?,
                time_mapping: MediaTimeMapping {
                    session_origin_ns: row.get(offset + 5)?,
                    media_origin_ns: row.get(offset + 6)?,
                    rate_numerator: row.get(offset + 7)?,
                    rate_denominator: row.get(offset + 8)?,
                },
            })
        }
        "deleted" | "pruned" => Ok(SessionRecording::Missing {
            session_id,
            reason: if state == "deleted" {
                RecordingMissingReason::Deleted
            } else {
                RecordingMissingReason::Pruned
            },
        }),
        unknown => Err(rusqlite::Error::FromSqlConversionFailure(
            offset,
            Type::Text,
            Box::new(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("unknown recording state {unknown:?}"),
            )),
        )),
    }
}

fn decode_recording_reference_row(
    session_id: SessionId,
    row: &rusqlite::Row<'_>,
    offset: usize,
) -> rusqlite::Result<RecordingReference> {
    let state: String = row.get(offset)?;
    if state == "growing" {
        return Ok(RecordingReference::Growing {
            session_id,
            path: row.get(offset + 1)?,
            finalization_error: row.get(offset + 9)?,
        });
    }
    decode_recording_row_offset(session_id, row, offset).map(RecordingReference::Settled)
}

fn wall_clock_unix_ms() -> Result<u64, RagError> {
    let duration = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(message)?;
    u64::try_from(duration.as_millis()).map_err(message)
}

fn io_error(error: io::Error) -> RagError {
    RagError::Storage(error.to_string())
}

/// Renames every quarantined file back to where [`Store::quarantine_recording_media`] found it.
///
/// Used on every failure path out of [`Store::delete_entry`] after quarantine has begun — a
/// filesystem error partway through quarantining the entry's sessions, or the row transaction
/// itself failing. A rename-back failure here is not escalated: it leaves a tombstone that
/// [`Store::recover_quarantined_media`] resolves on the next pass, because the row it belongs to
/// was never touched and still carries the original path.
fn restore_quarantined(quarantined: &[(PathBuf, PathBuf)]) {
    for (original, tombstone) in quarantined {
        if let Err(error) = std::fs::rename(tombstone, original) {
            eprintln!(
                "Could not restore quarantined recording {} from {}: {error}",
                original.display(),
                tombstone.display()
            );
        }
    }
}

fn parse_tombstone_session_id(file_name: &str) -> Option<SessionId> {
    file_name
        .strip_prefix('.')
        .and_then(|rest| rest.strip_suffix(".deleting"))
        .and_then(|middle| middle.parse::<u128>().ok())
        .map(SessionId::new)
}

fn validate_recording_path(
    session_id: SessionId,
    recording_directory: &Path,
    path: &Path,
) -> Result<(), RagError> {
    let expected = recording_directory.join(format!("{}.mp4", session_id.get()));
    if path != expected {
        return Err(RagError::Storage(format!(
            "refusing to remove recording path outside the managed library: {}",
            path.display()
        )));
    }
    Ok(())
}

fn poisoned<T>(error: std::sync::PoisonError<T>) -> RagError {
    RagError::Storage(error.to_string())
}
fn message(error: impl std::fmt::Display) -> RagError {
    RagError::Storage(error.to_string())
}
fn rank_score(rank: usize) -> f64 {
    1.0 / (61.0 + rank as f64)
}
fn word_count(text: &str) -> usize {
    text.split_whitespace().count()
}
fn vector_blob(vector: &[f32]) -> Vec<u8> {
    vector
        .iter()
        .flat_map(|value| value.to_le_bytes())
        .collect()
}
fn content_hash(text: &str) -> String {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    text.hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

fn document_id(
    kind: DocumentKind,
    content_hash: &str,
    collection_id: Option<&str>,
    source_session_id: Option<&str>,
    source_path: Option<&str>,
) -> String {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    kind.as_str().hash(&mut hasher);
    content_hash.hash(&mut hasher);
    collection_id.hash(&mut hasher);
    source_session_id.hash(&mut hasher);
    source_path.hash(&mut hasher);
    format!("local-doc-v1-{:016x}", hasher.finish())
}

fn parse_session_id(value: &str) -> Result<SessionId, RagError> {
    value
        .parse::<u128>()
        .map(SessionId::new)
        .map_err(|error| RagError::Storage(format!("invalid source session id: {error}")))
}

fn parse_entry_id(value: &str) -> Result<EntryId, RagError> {
    value
        .parse::<u128>()
        .map(EntryId::new)
        .map_err(|error| RagError::Storage(format!("invalid entry id: {error}")))
}

fn ensure_session_entry(
    transaction: &rusqlite::Transaction<'_>,
    session: &Session,
) -> Result<(), RagError> {
    let session_id = session.id().get().to_string();
    let attached = transaction
        .query_row(
            "SELECT 1 FROM entry_sessions WHERE session_id=?1",
            [&session_id],
            |_| Ok(()),
        )
        .optional()
        .map_err(storage)?
        .is_some();
    if attached {
        return Ok(());
    }
    let mut candidate = session.id().get();
    loop {
        let candidate_text = candidate.to_string();
        let occupied = transaction
            .query_row(
                "SELECT 1 FROM entries WHERE id=?1",
                [&candidate_text],
                |_| Ok(()),
            )
            .optional()
            .map_err(storage)?
            .is_some();
        if !occupied {
            transaction
                .execute(
                    "INSERT INTO entries(id,created_at_unix_ms,title) VALUES(?1,?2,NULL)",
                    params![candidate_text, session.started_at_unix_ms()],
                )
                .map_err(storage)?;
            transaction
                .execute(
                    "INSERT INTO entry_sessions(session_id,entry_id) VALUES(?1,?2)",
                    params![session_id, candidate.to_string()],
                )
                .map_err(storage)?;
            return Ok(());
        }
        candidate = candidate.checked_add(1).ok_or_else(|| {
            RagError::Storage("could not allocate an implicit entry identity".to_owned())
        })?;
    }
}

fn attach_session_relation(
    transaction: &rusqlite::Transaction<'_>,
    entry_id: EntryId,
    session_id: SessionId,
) -> Result<(), RagError> {
    let session_id = session_id.get().to_string();
    let existing = transaction
        .query_row(
            "SELECT entry_id FROM entry_sessions WHERE session_id=?1",
            [&session_id],
            |row| row.get::<_, String>(0),
        )
        .optional()
        .map_err(storage)?;
    if let Some(existing) = existing {
        let existing = parse_entry_id(&existing)?;
        if existing == entry_id {
            return Ok(());
        }
        return Err(RagError::Storage(format!(
            "session {session_id} already belongs to entry {} and cannot be detached",
            existing.get()
        )));
    }
    transaction
        .execute(
            "INSERT INTO entry_sessions(session_id,entry_id) VALUES(?1,?2)",
            params![session_id, entry_id.get().to_string()],
        )
        .map_err(storage)?;
    Ok(())
}

fn chunk_markdown(text: &str) -> Vec<String> {
    const TARGET: usize = 400;
    const OVERLAP: usize = 50;
    let mut output = Vec::new();
    for section in text.split("\n#") {
        let words: Vec<_> = section.split_whitespace().collect();
        let mut start = 0;
        while start < words.len() {
            let end = (start + TARGET).min(words.len());
            output.push(words[start..end].join(" "));
            if end == words.len() {
                break;
            }
            start = end.saturating_sub(OVERLAP);
        }
    }
    output
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use rusqlite::params;

    use super::*;

    fn recording_session(id: u128, started_at: u64) -> Session {
        let mut session = Session::new(
            SessionId::new(id),
            CaptureTarget {
                bundle_id: Some("us.zoom.xos".to_owned()),
                display_name: "Zoom".to_owned(),
                window_title: Some(format!("Meeting {id}")),
                kind: TargetKind::Window,
                audio_scoped: true,
            },
            started_at,
        );
        session.end(started_at.saturating_add(1_000));
        session
    }

    fn available_recording(session_id: SessionId, path: &Path, bytes: u64) -> SessionRecording {
        SessionRecording::Available {
            session_id,
            path: path.to_string_lossy().into_owned(),
            container: RecordingContainer::Mp4,
            duration: Duration::from_secs(1),
            byte_size: bytes,
            time_mapping: MediaTimeMapping::IDENTITY,
        }
    }

    #[test]
    fn recording_reference_round_trips_with_explicit_identity_clock()
    -> Result<(), Box<dyn std::error::Error>> {
        let directory = tempfile::tempdir()?;
        let store = Store::open(directory.path().join("sotto.sqlite3"))?;
        let session = recording_session(1, 1_000);
        store.save_session(&session)?;
        let path = directory.path().join("recordings").join("1.mp4");
        std::fs::create_dir_all(path.parent().ok_or("recording parent missing")?)?;
        std::fs::write(&path, vec![7_u8; 32])?;
        let recording = available_recording(session.id(), &path, 32);
        store.save_recording(&recording)?;

        assert_eq!(store.load_recording(session.id())?, Some(recording));
        assert_eq!(
            store.recording_usage()?,
            RecordingUsage {
                used_bytes: 32,
                budget_bytes: DEFAULT_RECORDING_BUDGET_BYTES,
            }
        );
        Ok(())
    }

    #[test]
    fn growing_recording_is_measured_reported_failed_and_deletable()
    -> Result<(), Box<dyn std::error::Error>> {
        let directory = tempfile::tempdir()?;
        let database = directory.path().join("sotto.sqlite3");
        let recordings = directory.path().join("recordings");
        std::fs::create_dir_all(&recordings)?;
        let store = Store::open(&database)?;
        let session = recording_session(9, 9_000);
        store.save_session(&session)?;
        let path = recordings.join("9.mp4");
        std::fs::write(&path, vec![7_u8; 19])?;

        store.save_growing_recording(session.id(), &path)?;
        store.mark_recording_finalization_failed(session.id(), "injected probe failure")?;

        assert_eq!(
            store.load_recording_reference(session.id())?,
            Some(RecordingReference::Growing {
                session_id: session.id(),
                path: path.to_string_lossy().into_owned(),
                finalization_error: Some("injected probe failure".to_owned()),
            })
        );
        assert_eq!(store.recording_usage()?.used_bytes, 19);
        assert!(store.remove_recording_media(
            session.id(),
            RecordingMissingReason::Deleted,
            &recordings,
        )?);
        assert!(!path.exists());
        assert_eq!(store.recording_usage()?.used_bytes, 0);
        Ok(())
    }

    #[test]
    fn recording_delete_frees_media_but_keeps_session_record()
    -> Result<(), Box<dyn std::error::Error>> {
        let directory = tempfile::tempdir()?;
        let recording_directory = directory.path().join("recordings");
        std::fs::create_dir_all(&recording_directory)?;
        let store = Store::open(directory.path().join("sotto.sqlite3"))?;
        let session = recording_session(2, 2_000);
        store.save_session(&session)?;
        let path = recording_directory.join("2.mp4");
        std::fs::write(&path, vec![9_u8; 24])?;
        store.save_recording(&available_recording(session.id(), &path, 24))?;

        assert!(store.remove_recording_media(
            session.id(),
            RecordingMissingReason::Deleted,
            &recording_directory,
        )?);
        assert!(!path.exists());
        assert_eq!(
            store.load_session_record(session.id())?.id(),
            session.id(),
            "recording deletion must preserve the session record"
        );
        assert_eq!(
            store.load_recording(session.id())?,
            Some(SessionRecording::Missing {
                session_id: session.id(),
                reason: RecordingMissingReason::Deleted,
            })
        );
        assert_eq!(store.recording_usage()?.used_bytes, 0);
        Ok(())
    }

    #[test]
    fn budget_prunes_oldest_media_and_preserves_both_timelines()
    -> Result<(), Box<dyn std::error::Error>> {
        let directory = tempfile::tempdir()?;
        let recording_directory = directory.path().join("recordings");
        std::fs::create_dir_all(&recording_directory)?;
        let store = Store::open(directory.path().join("sotto.sqlite3"))?;
        for (id, started_at) in [(3_u128, 3_000_u64), (4, 4_000)] {
            let session = recording_session(id, started_at);
            store.save_session(&session)?;
            let path = recording_directory.join(format!("{id}.mp4"));
            std::fs::write(&path, vec![5_u8; 16])?;
            store.save_recording(&available_recording(session.id(), &path, 16))?;
        }
        store.set_recording_budget(16)?;

        assert_eq!(
            store.enforce_recording_budget(&recording_directory)?,
            vec![SessionId::new(3)]
        );
        assert_eq!(
            store.load_recording(SessionId::new(3))?,
            Some(SessionRecording::Missing {
                session_id: SessionId::new(3),
                reason: RecordingMissingReason::Pruned,
            })
        );
        assert!(recording_directory.join("4.mp4").exists());
        assert_eq!(
            store.load_session_record(SessionId::new(3))?.id(),
            SessionId::new(3),
            "pruning must preserve the oldest session"
        );
        assert_eq!(
            store.load_session_record(SessionId::new(4))?.id(),
            SessionId::new(4),
            "pruning must preserve the newest session"
        );
        Ok(())
    }

    #[test]
    fn unknown_capture_target_kind_is_rejected() {
        assert!(
            parse_target_kind("future_scope").is_err(),
            "unknown persisted scope must not be fabricated as a window"
        );
    }

    fn insert_chunk(store: &Store, id: &str, text: &str, vector: &[f32]) -> Result<(), RagError> {
        let connection = store.writer.lock().map_err(poisoned)?;
        connection
            .execute(
                "INSERT OR IGNORE INTO documents(id,kind,title,source_path,collection_id,source_session_id,provenance_status,ingested_at,content_hash) VALUES('doc','resource_document','Fixture',NULL,NULL,NULL,'native',0,'fixture')",
                [],
            )
            .map_err(storage)?;
        let blob = vector_blob(vector);
        connection
            .execute(
                "INSERT INTO chunks VALUES(?1,'doc',?2,?3,1,'{}',?4)",
                params![id, usize::from(id == "exact"), text, blob],
            )
            .map_err(storage)?;
        connection
            .execute(
                "INSERT INTO vec_chunks(chunk_id,embedding) VALUES(?1,?2)",
                params![id, blob],
            )
            .map_err(storage)?;
        Ok(())
    }

    #[test]
    fn hybrid_promotes_an_exact_project_name_over_dense_only() -> Result<(), RagError> {
        let store = Store::open_in_memory()?;
        let query = vec![0.0; 384];
        let mut nearest = query.clone();
        nearest[0] = 0.01;
        let mut exact_name = query.clone();
        exact_name[0] = 0.02;
        insert_chunk(&store, "dense", "General platform comparison", &nearest)?;
        insert_chunk(
            &store,
            "exact",
            "Project Quasar launch checklist",
            &exact_name,
        )
        .map_err(message)?;

        let dense =
            store.search_with_vector("Quasar", 2, &SearchFilter::default(), &query, false)?;
        let hybrid =
            store.search_with_vector("Quasar", 2, &SearchFilter::default(), &query, true)?;
        assert_eq!(dense[0].id, "dense");
        assert_eq!(hybrid[0].id, "exact");
        Ok(())
    }

    #[test]
    fn unchanged_content_skips_embedding_work() -> Result<(), RagError> {
        let store = Store::open_in_memory()?;
        let text = "unchanged fixture";
        store
            .writer
            .lock()
            .map_err(poisoned)?
            .execute(
                "INSERT INTO documents(id,kind,title,source_path,collection_id,source_session_id,provenance_status,ingested_at,content_hash) VALUES('existing','resource_document','Fixture',NULL,NULL,NULL,'native',0,?1)",
                [content_hash(text)],
            )
            .map_err(storage)?;

        assert!(!store.ingest_text(
            text,
            DocumentKind::ResourceDocument,
            IngestMetadata::default()
        )?);
        assert!(store.embedding.lock().map_err(poisoned)?.is_none());
        Ok(())
    }

    /// A session whose stop-time tail carries an earlier `ts` than the live events before it must
    /// still reload — and still replay — in allocation order.
    ///
    /// Observed in the field: `EventId(40)` was written by finalization with a media-time `ts` of
    /// 26.3 s while `EventId(36)` had already been written live with a stream-offset `ts` of
    /// 27.3 s. Reloading by `ts` handed `replay` id 40 before id 36, and the resulting
    /// `NonMonotonicId` surfaced as "Could not change this recording's search retention" on the
    /// Ask panel's Include control — and would equally have failed notes, clustering and Ask.
    #[test]
    fn a_tail_written_behind_the_live_clock_still_reloads_and_replays_in_id_order()
    -> Result<(), RagError> {
        let store = Store::open_in_memory()?;
        let session_id = SessionId::new(4_040);
        let mut session = Session::new(
            session_id,
            CaptureTarget {
                bundle_id: None,
                display_name: "Teams".to_owned(),
                window_title: Some("Chat".to_owned()),
                kind: TargetKind::Window,
                audio_scoped: true,
            },
            1,
        );
        session.end(2);
        store.save_session(&session)?;
        let mut timeline = sotto_core::TimelineBuilder::new(session);
        let utterance = |start: Duration, end: Duration, text: &str| sotto_core::Utterance {
            source: Source::System,
            start,
            end,
            text: text.to_owned(),
            avg_logprob: 0.0,
            annotations: Vec::new(),
        };
        // Live: the stream offset of the frame that produced the final.
        timeline.append(
            Duration::from_millis(27_283),
            EventPayload::UtteranceFinal(utterance(
                Duration::from_millis(18_549),
                Duration::from_millis(23_000),
                "live final, late stream offset",
            )),
        );
        // Stop-time finalization: media time, which sits behind the live clock.
        timeline.append(
            Duration::from_millis(26_300),
            EventPayload::UtteranceFinal(utterance(
                Duration::from_millis(23_300),
                Duration::from_millis(26_300),
                "tail final, earlier media time",
            )),
        );
        store.append_events(timeline.events())?;

        let reloaded = store.load_session(session_id)?;
        assert_eq!(
            reloaded.iter().map(TimelineEvent::id).collect::<Vec<_>>(),
            timeline
                .events()
                .iter()
                .map(TimelineEvent::id)
                .collect::<Vec<_>>(),
            "the log must reload in the order its ids were allocated, not by ts"
        );
        sotto_core::replay(&reloaded).map_err(message)?;

        assert!(
            store.index_prior_meeting(session_id, None)?,
            "indexing a recording with a behind-the-clock tail must succeed"
        );
        assert!(store.is_session_indexed(session_id)?);
        let (prior_text, _) = store
            .render_prior_meeting(session_id)?
            .ok_or_else(|| RagError::Storage("a transcribed recording must render".to_owned()))?;
        let live = prior_text
            .find("live final")
            .ok_or_else(|| RagError::Storage("live final missing from the document".to_owned()))?;
        let tail = prior_text
            .find("tail final")
            .ok_or_else(|| RagError::Storage("tail final missing from the document".to_owned()))?;
        assert!(
            live < tail,
            "the rendered document still reads in transcript-start order"
        );
        Ok(())
    }

    /// Backfill reaches every completed recording, tolerates a silent one, and repeats for free.
    ///
    /// A library recorded before indexing became unconditional has no `prior_meeting` documents at
    /// all, so "all recordings" would quietly mean "none of them" until this runs.
    #[test]
    fn backfill_indexes_every_completed_recording_and_is_idempotent() -> Result<(), RagError> {
        let store = Store::open_in_memory()?;
        let spoken = SessionId::new(701);
        let silent = SessionId::new(702);
        for (id, text) in [(spoken, Some("we agreed on Thursday")), (silent, None)] {
            let mut session = Session::new(
                id,
                CaptureTarget {
                    bundle_id: None,
                    display_name: "Backfill".to_owned(),
                    window_title: None,
                    kind: TargetKind::Window,
                    audio_scoped: true,
                },
                1,
            );
            session.end(2);
            store.save_session(&session)?;
            let mut timeline = sotto_core::TimelineBuilder::new(session);
            match text {
                Some(text) => {
                    timeline.append(
                        Duration::from_secs(1),
                        EventPayload::UtteranceFinal(sotto_core::Utterance {
                            source: Source::Mic,
                            start: Duration::ZERO,
                            end: Duration::from_secs(1),
                            text: text.to_owned(),
                            avg_logprob: 0.0,
                            annotations: Vec::new(),
                        }),
                    );
                }
                None => {
                    timeline.append(
                        Duration::from_secs(1),
                        EventPayload::Vad(sotto_core::VadSegment {
                            source: Source::Mic,
                            start: Duration::ZERO,
                            end: None,
                            kind: sotto_core::SpeechState::SpeechStart,
                        }),
                    );
                }
            }
            store.append_events(timeline.events())?;
        }

        let report = store.index_missing_prior_meetings()?;
        assert_eq!(
            report.indexed,
            vec![spoken],
            "a spoken recording is indexed"
        );
        assert_eq!(
            report.skipped,
            vec![silent],
            "a recording with no speech is skipped, not failed"
        );
        assert!(report.failed.is_empty(), "nothing here should fail");
        assert!(store.is_session_indexed(spoken)?);
        assert!(!store.is_session_indexed(silent)?);

        let repeat = store.index_missing_prior_meetings()?;
        assert!(
            repeat.indexed.is_empty(),
            "a second pass must not re-index what is already there"
        );
        Ok(())
    }

    #[test]
    fn completed_meeting_documents_use_neutral_finals_and_exact_session_provenance()
    -> Result<(), RagError> {
        let store = Store::open_in_memory()?;
        let session_id = SessionId::new(88);
        let mut session = Session::new(
            session_id,
            CaptureTarget {
                bundle_id: None,
                display_name: "Planning".to_owned(),
                window_title: Some("Roadmap".to_owned()),
                kind: TargetKind::Window,
                audio_scoped: true,
            },
            1,
        );
        session.end(2);
        store.save_session(&session)?;
        let mut timeline = sotto_core::TimelineBuilder::new(session);
        timeline.append(
            Duration::from_millis(500),
            EventPayload::UtterancePartial(sotto_core::Utterance {
                source: Source::Mic,
                start: Duration::ZERO,
                end: Duration::from_millis(500),
                text: "partial canary".to_owned(),
                avg_logprob: 0.0,
                annotations: Vec::new(),
            }),
        );
        let original = timeline.append(
            Duration::from_secs(1),
            EventPayload::UtteranceFinal(sotto_core::Utterance {
                source: Source::System,
                start: Duration::from_millis(500),
                end: Duration::from_secs(1),
                text: "Ship the old roadmap".to_owned(),
                avg_logprob: 0.0,
                annotations: Vec::new(),
            }),
        );
        timeline
            .supersede(
                Duration::from_millis(1_100),
                EventPayload::UtteranceFinal(sotto_core::Utterance {
                    source: Source::System,
                    start: Duration::from_millis(500),
                    end: Duration::from_secs(1),
                    text: "Ship the roadmap".to_owned(),
                    avg_logprob: 0.0,
                    annotations: Vec::new(),
                }),
                &original,
            )
            .map_err(message)?;
        store.append_events(timeline.events())?;

        let (prior_text, prior_title) = store
            .render_prior_meeting(session_id)?
            .ok_or_else(|| RagError::Storage("a transcribed recording must render".to_owned()))?;
        assert!(prior_text.contains("[00:00.500] [Meeting audio] \"Ship the roadmap\""));
        assert!(!prior_text.contains("Ship the old roadmap"));
        assert!(!prior_text.contains("partial canary"));
        assert!(!prior_text.contains("rep"));
        assert!(!prior_text.contains("customer"));
        assert_eq!(prior_title, "Meeting — Planning");

        let prior_metadata = IngestMetadata {
            title: prior_title,
            collection_id: Some("roadmap".to_owned()),
            source_session_id: Some(session_id),
            ..IngestMetadata::default()
        };
        store.validate_ingest(DocumentKind::PriorMeeting, &prior_metadata)?;
        let prior_hash = content_hash(&prior_text);
        let prior_chunks = chunk_markdown(&prior_text);
        let prior_vectors = vec![vec![0.0; 384]; prior_chunks.len()];
        assert!(store.persist_document(
            DocumentKind::PriorMeeting,
            prior_metadata,
            prior_hash,
            prior_chunks,
            prior_vectors
        )?);

        let note_text = "# Meeting notes\n\nDecision: ship the roadmap.";
        let note_metadata = IngestMetadata {
            title: "Roadmap notes".to_owned(),
            collection_id: Some("roadmap".to_owned()),
            source_session_id: Some(session_id),
            ..IngestMetadata::default()
        };
        store.validate_ingest(DocumentKind::MeetingNote, &note_metadata)?;
        let note_hash = content_hash(note_text);
        let note_id = document_id(
            DocumentKind::MeetingNote,
            &note_hash,
            Some("roadmap"),
            Some("88"),
            None,
        );
        let note_chunks = chunk_markdown(note_text);
        let note_vectors = vec![vec![0.0; 384]; note_chunks.len()];
        assert!(store.persist_document(
            DocumentKind::MeetingNote,
            note_metadata,
            note_hash,
            note_chunks,
            note_vectors
        )?);
        let note_receipt = store.load_local_evidence(&format!("{note_id}:0"))?;
        assert_eq!(note_receipt.kind, DocumentKind::MeetingNote);
        assert_eq!(note_receipt.source_session_id, Some(session_id));
        assert_eq!(note_receipt.collection_id.as_deref(), Some("roadmap"));

        assert!(
            store
                .validate_ingest(
                    DocumentKind::ResourceDocument,
                    &IngestMetadata {
                        source_session_id: Some(session_id),
                        ..IngestMetadata::default()
                    }
                )
                .is_err()
        );
        Ok(())
    }

    #[test]
    fn generic_kind_collection_and_session_filters_preserve_local_receipts() -> Result<(), RagError>
    {
        let store = Store::open_in_memory()?;
        let session_id = SessionId::new(99);
        let mut session = Session::new(
            session_id,
            CaptureTarget {
                bundle_id: None,
                display_name: "Review".to_owned(),
                window_title: None,
                kind: TargetKind::Window,
                audio_scoped: true,
            },
            1,
        );
        session.end(2);
        store.save_session(&session)?;
        for (ordinal, kind) in [
            DocumentKind::ResourceDocument,
            DocumentKind::ProjectNote,
            DocumentKind::MeetingNote,
            DocumentKind::PriorMeeting,
        ]
        .into_iter()
        .enumerate()
        {
            let text = format!("Local source {ordinal}");
            let metadata = IngestMetadata {
                title: format!("Source {ordinal}"),
                source_path: Some(format!("/source/{ordinal}.md")),
                collection_id: Some(if ordinal == 0 { "general" } else { "roadmap" }.to_owned()),
                source_session_id: kind.requires_source_session().then_some(session_id),
                fields: BTreeMap::from([("label".to_owned(), format!("value-{ordinal}"))]),
            };
            store.validate_ingest(kind, &metadata)?;
            store.persist_document(
                kind,
                metadata,
                content_hash(&text),
                vec![text],
                vec![vec![ordinal as f32 / 100.0; 384]],
            )?;
        }

        let project = store.search_with_vector(
            "",
            4,
            &SearchFilter {
                kind: Some(DocumentKind::ProjectNote),
                collection_id: Some("roadmap".to_owned()),
                source_session_id: None,
            },
            &[0.0; 384],
            false,
        )?;
        assert_eq!(project.len(), 1);
        assert_eq!(
            project[0].metadata.get("kind").map(String::as_str),
            Some("project_note")
        );
        assert_eq!(
            project[0]
                .metadata
                .get("evidence_scope")
                .map(String::as_str),
            Some("local")
        );
        assert_eq!(
            project[0].metadata.get("source_path").map(String::as_str),
            Some("/source/1.md")
        );
        let meeting_owned = store.search_with_vector(
            "",
            4,
            &SearchFilter {
                kind: None,
                collection_id: None,
                source_session_id: Some(session_id),
            },
            &[0.0; 384],
            false,
        )?;
        // Both session-owned kinds surface. The prior meeting used to be withheld here by the
        // per-recording search opt-in; every indexed recording is searchable now, so a scope of
        // "this recording" returns its transcript alongside its notes.
        assert_eq!(meeting_owned.len(), 2);
        let mut owned_kinds = meeting_owned
            .iter()
            .filter_map(|chunk| chunk.metadata.get("kind").map(String::as_str))
            .collect::<Vec<_>>();
        owned_kinds.sort_unstable();
        assert_eq!(owned_kinds, vec!["meeting_note", "prior_meeting"]);
        assert!(meeting_owned.iter().all(|chunk| {
            chunk
                .metadata
                .get("source_session_id")
                .is_some_and(|value| value == "99")
        }));
        let receipt = store.load_local_evidence(&project[0].id)?;
        assert_eq!(receipt.kind, DocumentKind::ProjectNote);
        assert_eq!(receipt.metadata.get("label"), Some(&"value-1".to_owned()));
        Ok(())
    }

    #[test]
    fn retained_session_policy_is_explicit_and_exclusion_is_unreachable() -> Result<(), RagError> {
        let store = Store::open_in_memory()?;
        let session_id = SessionId::new(404);
        let mut session = Session::new(
            session_id,
            CaptureTarget {
                bundle_id: None,
                display_name: "Retention".to_owned(),
                window_title: None,
                kind: TargetKind::Window,
                audio_scoped: true,
            },
            1,
        );
        session.end(2);
        store.save_session(&session)?;
        let metadata = IngestMetadata {
            title: "Retained meeting".to_owned(),
            source_session_id: Some(session_id),
            ..IngestMetadata::default()
        };
        store.persist_document(
            DocumentKind::PriorMeeting,
            metadata,
            "retained-hash".to_owned(),
            vec!["only retained answer [event:1]".to_owned()],
            vec![vec![0.0; 384]],
        )?;
        let filter = SearchFilter {
            kind: Some(DocumentKind::PriorMeeting),
            collection_id: None,
            source_session_id: None,
        };
        let started = std::time::Instant::now();
        assert_eq!(
            store
                .search_with_vector("", 5, &filter, &[0.0; 384], false)?
                .len(),
            1,
            "an indexed recording is searchable with no further opt-in"
        );
        eprintln!("bounded prior-meeting retrieval: {:?}", started.elapsed());
        assert!(store.is_session_indexed(session_id)?);

        // Deleting the recording is the only way out of the index, and it takes the vectors too.
        store
            .writer
            .lock()
            .map_err(poisoned)?
            .execute(
                "DELETE FROM sessions WHERE id=?1",
                [session_id.get().to_string()],
            )
            .map_err(storage)?;
        assert!(!store.is_session_indexed(session_id)?);
        Ok(())
    }

    #[test]
    fn scoped_search_exhausts_higher_ranked_nonmatches_before_returning_match()
    -> Result<(), RagError> {
        let store = Store::open_in_memory()?;
        {
            let connection = store.writer.lock().map_err(poisoned)?;
            for ordinal in 0..5 {
                let document_id = format!("nonmatch-doc-{ordinal}");
                let chunk_id = format!("nonmatch-{ordinal}");
                connection
                    .execute(
                        "INSERT INTO documents(id,kind,title,source_path,collection_id,source_session_id,provenance_status,ingested_at,content_hash) VALUES(?1,'resource_document','Other',NULL,'other',NULL,'native',0,?2)",
                        params![document_id, format!("nonmatch-hash-{ordinal}")],
                    )
                    .map_err(storage)?;
                let mut vector = vec![0.0; 384];
                vector[0] = ordinal as f32 / 100.0;
                let blob = vector_blob(&vector);
                connection
                    .execute(
                        "INSERT INTO chunks VALUES(?1,?2,0,'higher ranked',2,'{}',?3)",
                        params![chunk_id, document_id, blob],
                    )
                    .map_err(storage)?;
                connection
                    .execute(
                        "INSERT INTO vec_chunks(chunk_id,embedding) VALUES(?1,?2)",
                        params![chunk_id, blob],
                    )
                    .map_err(storage)?;
            }
            connection
                .execute(
                    "INSERT INTO documents(id,kind,title,source_path,collection_id,source_session_id,provenance_status,ingested_at,content_hash) VALUES('match-doc','project_note','Match',NULL,'wanted',NULL,'native',0,'match-hash')",
                    [],
                )
                .map_err(storage)?;
            let match_vector = vec![0.2; 384];
            let blob = vector_blob(&match_vector);
            connection
                .execute(
                    "INSERT INTO chunks VALUES('match','match-doc',0,'scoped result',2,'{}',?1)",
                    [&blob],
                )
                .map_err(storage)?;
            connection
                .execute(
                    "INSERT INTO vec_chunks(chunk_id,embedding) VALUES('match',?1)",
                    [&blob],
                )
                .map_err(storage)?;
        }

        let result = store.search_with_vector(
            "",
            1,
            &SearchFilter {
                kind: Some(DocumentKind::ProjectNote),
                collection_id: Some("wanted".to_owned()),
                source_session_id: None,
            },
            &[0.0; 384],
            false,
        )?;
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].id, "match");
        Ok(())
    }

    #[test]
    #[ignore = "performance check requires the fastembed model cache"]
    fn warm_query_embedding_and_hybrid_search_fit_latency_budget() -> Result<(), RagError> {
        let store = Store::open_in_memory()?;
        store.embed(vec!["warm up"])?;
        let started = Instant::now();
        let _results = store.search_filtered("project launch", 5, &SearchFilter::default())?;
        assert!(
            started.elapsed() < Duration::from_millis(50),
            "warm query took {:?}",
            started.elapsed()
        );
        Ok(())
    }
}
