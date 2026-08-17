use std::{
    collections::{BTreeMap, HashMap},
    hash::{Hash, Hasher},
    io,
    path::{Path, PathBuf},
    str::FromStr,
    sync::{
        Arc, Mutex, Once,
        atomic::{AtomicU64, Ordering},
    },
    time::{SystemTime, UNIX_EPOCH},
};

use fastembed::{EmbeddingModel, InitOptions, TextEmbedding};
use sea_orm::ColumnTrait;
use sea_orm::{
    ConnectionTrait, DatabaseBackend, DatabaseConnection, EntityTrait, QueryFilter, QueryOrder,
    QueryResult, Statement, TransactionTrait, Value as SeaValue,
};
use serde_json::{Value, json};
use sotto_core::types::{
    MediaTimeMapping, RecordingContainer, RecordingMissingReason, RecordingTitle, SessionRecording,
};
use sotto_core::{
    BoxFuture, CaptureTarget, Chunk, Entry, EntryId, EventId, EventPayload, MarkKind,
    PersistenceSink, RagError, Retriever, Session, SessionId, Source, TargetKind, TimelineEvent,
    Utterance, checked_user_annotation, replay_lenient,
};
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions};

use crate::{
    entities::{self, session_recordings::RecordingState},
    schema::{migrate, storage},
};

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

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct PriorMeetingIndexReport {
    pub indexed: Vec<SessionId>,
    pub skipped: Vec<SessionId>,
    pub failed: Vec<(SessionId, String)>,
}

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

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SessionSummary {
    pub id: SessionId,
    pub capture_target: CaptureTarget,
    pub title: Option<RecordingTitle>,
    pub started_at_unix_ms: u64,
    pub ended_at_unix_ms: Option<u64>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RecordingUsage {
    pub used_bytes: u64,
    pub budget_bytes: u64,
}

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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct QuarantineRecovery {
    pub session_id: SessionId,
    pub restored: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct DerivedTranscript {
    pub model: String,
    pub utterances: Vec<Utterance>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GroundedDerivedView {
    pub artifact: String,
    pub usage: String,
    pub provider_model: String,
    pub grant_fingerprint: Option<String>,
    pub source_status: String,
    pub bundle: String,
    pub normalizations: String,
    pub consultations: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GroundedDerivedArtifact {
    pub model: String,
    pub content_hash: String,
    pub view: GroundedDerivedView,
}

#[derive(Clone)]
pub struct Store {
    pub(crate) writer: DatabaseConnection,
    pub(crate) reader: DatabaseConnection,
    embedding: Arc<Mutex<Option<TextEmbedding>>>,
}

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
        Box::pin(async move { self.store.append_events(&events).await })
    }
}

impl Store {
    pub async fn open(path: impl AsRef<Path>) -> Result<Self, RagError> {
        register_sqlite_vec();
        let path = path.as_ref().to_path_buf();
        let writer = connect_file(&path, 1).await?;
        migrate(&writer).await?;
        let reader = connect_file(&path, 4).await?;
        Ok(Self {
            writer,
            reader,
            embedding: Arc::new(Mutex::new(None)),
        })
    }

    pub async fn open_in_memory() -> Result<Self, RagError> {
        register_sqlite_vec();
        static NEXT_DATABASE: AtomicU64 = AtomicU64::new(1);
        let name = format!(
            "sotto-rag-{}",
            NEXT_DATABASE.fetch_add(1, Ordering::Relaxed)
        );
        let writer = connect_memory(&name, 1).await?;
        migrate(&writer).await?;
        let reader = connect_memory(&name, 4).await?;
        Ok(Self {
            writer,
            reader,
            embedding: Arc::new(Mutex::new(None)),
        })
    }

    pub async fn save_session(&self, session: &Session) -> Result<(), RagError> {
        self.save_session_at_entry(session, None).await
    }

    pub async fn save_session_in_entry(
        &self,
        session: &Session,
        entry_id: EntryId,
    ) -> Result<(), RagError> {
        self.save_session_at_entry(session, Some(entry_id)).await
    }

    async fn save_session_at_entry(
        &self,
        session: &Session,
        entry_id: Option<EntryId>,
    ) -> Result<(), RagError> {
        let target = session.capture_target();
        if !target.has_valid_scope() {
            return Err(RagError::Storage(
                "session capture target has an invalid scope combination".to_owned(),
            ));
        }
        let transaction = self.writer.begin().await.map_err(storage)?;
        execute(
            &transaction,
            "INSERT INTO sessions VALUES(?,?,?,?,?,?,?,?) ON CONFLICT(id) DO UPDATE SET ended_at_unix_ms=excluded.ended_at_unix_ms",
            vec![
                session.id().get().to_string().into(),
                to_i64(session.started_at_unix_ms())?.into(),
                session.ended_at_unix_ms().map(to_i64).transpose()?.into(),
                target.bundle_id.clone().into(),
                target.display_name.clone().into(),
                target.window_title.clone().into(),
                target_kind(target.kind).into(),
                i32::from(target.audio_scoped).into(),
            ],
        )
        .await?;
        if let Some(entry_id) = entry_id {
            attach_session_relation(&transaction, entry_id, session.id()).await?;
        } else {
            ensure_session_entry(&transaction, session).await?;
        }
        transaction.commit().await.map_err(storage)
    }

    pub async fn create_entry(&self, entry: &Entry) -> Result<(), RagError> {
        if !entry.session_ids().is_empty() {
            return Err(RagError::Storage(
                "create an entry before attaching recording sessions".to_owned(),
            ));
        }
        execute(
            &self.writer,
            "INSERT INTO entries(id,created_at_unix_ms,title,series) VALUES(?,?,?,?)",
            vec![
                entry.id().get().to_string().into(),
                to_i64(entry.created_at_unix_ms())?.into(),
                entry
                    .title()
                    .map(RecordingTitle::as_str)
                    .map(str::to_owned)
                    .into(),
                entry.series().map(str::to_owned).into(),
            ],
        )
        .await?;
        Ok(())
    }

    pub async fn list_entries(&self) -> Result<Vec<Entry>, RagError> {
        let rows = query_all(
            &self.reader,
            "SELECT e.id,e.created_at_unix_ms,e.title,e.series,es.session_id FROM entries e LEFT JOIN entry_sessions es ON es.entry_id=e.id LEFT JOIN sessions s ON s.id=es.session_id ORDER BY e.created_at_unix_ms DESC,e.id DESC,s.started_at_unix_ms,es.session_id",
            Vec::new(),
        )
        .await?;
        let mut output = Vec::<Entry>::new();
        for row in rows {
            let entry_id = parse_entry_id(&get::<String>(&row, "id")?)?;
            if output.last().map(Entry::id) != Some(entry_id) {
                let mut entry = Entry::new(
                    entry_id,
                    from_i64(get(&row, "created_at_unix_ms")?)?,
                    get::<Option<String>>(&row, "title")?
                        .as_deref()
                        .and_then(RecordingTitle::new),
                );
                let series = get::<Option<String>>(&row, "series")?;
                entry.set_series(series.as_deref());
                output.push(entry);
            }
            if let Some(session_id) = get::<Option<String>>(&row, "session_id")?
                && let Some(entry) = output.last_mut()
            {
                entry.attach_session(parse_session_id(&session_id)?);
            }
        }
        Ok(output)
    }

    pub async fn set_entry_title(
        &self,
        entry_id: EntryId,
        title: Option<&RecordingTitle>,
    ) -> Result<(), RagError> {
        let changed = execute(
            &self.writer,
            "UPDATE entries SET title=? WHERE id=?",
            vec![
                title.map(RecordingTitle::as_str).map(str::to_owned).into(),
                entry_id.get().to_string().into(),
            ],
        )
        .await?;
        if changed == 0 {
            return Err(RagError::NotFound {
                id: entry_id.get().to_string(),
            });
        }
        Ok(())
    }

    pub async fn entry_title(&self, entry_id: EntryId) -> Result<Option<RecordingTitle>, RagError> {
        Ok(
            entities::entries::Entity::find_by_id(entry_id.get().to_string())
                .one(&self.reader)
                .await
                .map_err(storage)?
                .and_then(|row| row.title.as_deref().and_then(RecordingTitle::new)),
        )
    }

    pub async fn set_entry_series(
        &self,
        entry_id: EntryId,
        series: Option<&str>,
    ) -> Result<(), RagError> {
        let series = series.map(str::trim).filter(|series| !series.is_empty());
        let changed = execute(
            &self.writer,
            "UPDATE entries SET series=? WHERE id=?",
            vec![
                series.map(str::to_owned).into(),
                entry_id.get().to_string().into(),
            ],
        )
        .await?;
        if changed == 0 {
            return Err(RagError::NotFound {
                id: entry_id.get().to_string(),
            });
        }
        Ok(())
    }

    pub async fn attach_session(
        &self,
        entry_id: EntryId,
        session_id: SessionId,
    ) -> Result<(), RagError> {
        let transaction = self.writer.begin().await.map_err(storage)?;
        attach_session_relation(&transaction, entry_id, session_id).await?;
        transaction.commit().await.map_err(storage)
    }

    pub async fn entry_for_session(&self, session_id: SessionId) -> Result<EntryId, RagError> {
        entities::entry_sessions::Entity::find_by_id(session_id.get().to_string())
            .one(&self.reader)
            .await
            .map_err(storage)?
            .ok_or_else(|| RagError::NotFound {
                id: session_id.get().to_string(),
            })
            .and_then(|row| parse_entry_id(&row.entry_id))
    }

    pub async fn append_events(&self, events: &[TimelineEvent]) -> Result<(), RagError> {
        let transaction = self.writer.begin().await.map_err(storage)?;
        for event in events {
            execute(
                &transaction,
                "INSERT INTO events(id,session_id,ts,kind,supersedes,payload) VALUES(?,?,?,?,?,?)",
                vec![
                    to_i64(event.id().get())?.into(),
                    event.session_id().get().to_string().into(),
                    i64::try_from(event.ts().as_nanos())
                        .map_err(message)?
                        .into(),
                    event.kind().as_str().into(),
                    event
                        .supersedes()
                        .map(EventId::get)
                        .map(to_i64)
                        .transpose()?
                        .into(),
                    serde_json::to_string(event.payload())
                        .map_err(message)?
                        .into(),
                ],
            )
            .await?;
        }
        transaction.commit().await.map_err(storage)
    }

    pub async fn append_final_utterances(
        &self,
        session_id: SessionId,
        utterances: &[Utterance],
    ) -> Result<Vec<TimelineEvent>, RagError> {
        if utterances.is_empty() {
            return Ok(Vec::new());
        }
        let transaction = self.writer.begin().await.map_err(storage)?;
        let row = query_one(
            &transaction,
            "SELECT COALESCE(MAX(id),0)+1 AS next_id FROM events WHERE session_id=?",
            vec![session_id.get().to_string().into()],
        )
        .await?
        .ok_or_else(|| RagError::Storage("next event id query returned no row".to_owned()))?;
        let mut next_id = from_i64::<u64>(get(&row, "next_id")?)?;
        let mut events = Vec::with_capacity(utterances.len());
        for utterance in utterances {
            if utterance.end < utterance.start {
                return Err(RagError::Storage(
                    "final utterance ends before it starts".to_owned(),
                ));
            }
            let event: TimelineEvent = serde_json::from_value(json!({
                "id": next_id,
                "session_id": session_id.get(),
                "ts": {"secs": utterance.end.as_secs(), "nanos": utterance.end.subsec_nanos()},
                "supersedes": null,
                "payload": EventPayload::UtteranceFinal(utterance.clone())
            }))
            .map_err(message)?;
            self.insert_event(&transaction, &event).await?;
            events.push(event);
            next_id = next_id.saturating_add(1);
        }
        transaction.commit().await.map_err(storage)?;
        Ok(events)
    }

    async fn insert_event<C: ConnectionTrait>(
        &self,
        connection: &C,
        event: &TimelineEvent,
    ) -> Result<(), RagError> {
        execute(
            connection,
            "INSERT INTO events(id,session_id,ts,kind,supersedes,payload) VALUES(?,?,?,?,?,?)",
            vec![
                to_i64(event.id().get())?.into(),
                event.session_id().get().to_string().into(),
                i64::try_from(event.ts().as_nanos())
                    .map_err(message)?
                    .into(),
                event.kind().as_str().into(),
                event
                    .supersedes()
                    .map(EventId::get)
                    .map(to_i64)
                    .transpose()?
                    .into(),
                serde_json::to_string(event.payload())
                    .map_err(message)?
                    .into(),
            ],
        )
        .await?;
        Ok(())
    }

    pub async fn load_session(
        &self,
        session_id: SessionId,
    ) -> Result<Vec<TimelineEvent>, RagError> {
        let rows = entities::events::Entity::find()
            .filter(entities::events::Column::SessionId.eq(session_id.get().to_string()))
            .order_by_asc(entities::events::Column::Id)
            .all(&self.reader)
            .await
            .map_err(storage)?;
        rows.into_iter().map(decode_event).collect()
    }

    pub async fn load_session_record(&self, id: SessionId) -> Result<Session, RagError> {
        let row = entities::sessions::Entity::find_by_id(id.get().to_string())
            .one(&self.reader)
            .await
            .map_err(storage)?
            .ok_or_else(|| RagError::NotFound {
                id: id.get().to_string(),
            })?;
        decode_session(row)
    }

    pub async fn list_sessions(&self) -> Result<Vec<SessionSummary>, RagError> {
        let rows = query_all(
            &self.reader,
            "SELECT s.id,s.started_at_unix_ms,s.ended_at_unix_ms,s.capture_target_bundle_id,s.capture_target_display_name,s.capture_target_window_title,s.capture_target_kind,s.capture_target_audio_scoped,e.title FROM sessions s LEFT JOIN entry_sessions es ON es.session_id=s.id LEFT JOIN entries e ON e.id=es.entry_id ORDER BY s.started_at_unix_ms DESC,s.id DESC",
            Vec::new(),
        )
        .await?;
        rows.into_iter().map(decode_session_summary).collect()
    }
}

async fn connect_file(path: &Path, max_connections: u32) -> Result<DatabaseConnection, RagError> {
    let options = SqliteConnectOptions::new()
        .filename(path)
        .create_if_missing(true)
        .foreign_keys(true)
        .journal_mode(SqliteJournalMode::Wal)
        .busy_timeout(std::time::Duration::from_secs(5));
    connect(options, max_connections).await
}

async fn connect_memory(name: &str, max_connections: u32) -> Result<DatabaseConnection, RagError> {
    let uri = format!("sqlite:file:{name}?mode=memory&cache=shared");
    let options = SqliteConnectOptions::from_str(&uri)
        .map_err(storage)?
        .foreign_keys(true)
        .busy_timeout(std::time::Duration::from_secs(5));
    connect(options, max_connections).await
}

async fn connect(
    options: SqliteConnectOptions,
    max_connections: u32,
) -> Result<DatabaseConnection, RagError> {
    let pool = SqlitePoolOptions::new()
        .max_connections(max_connections)
        .connect_with(options)
        .await
        .map_err(storage)?;
    Ok(sea_orm::SqlxSqliteConnector::from_sqlx_sqlite_pool(pool))
}

pub(crate) async fn execute<C: ConnectionTrait>(
    connection: &C,
    sql: &str,
    values: Vec<SeaValue>,
) -> Result<u64, RagError> {
    connection
        .execute_raw(Statement::from_sql_and_values(
            DatabaseBackend::Sqlite,
            sql,
            values,
        ))
        .await
        .map(|result| result.rows_affected())
        .map_err(storage)
}

pub(crate) async fn remove_superseded_prior_meetings<C: ConnectionTrait>(
    connection: &C,
    session_id: SessionId,
    current_hash: &str,
) -> Result<(), RagError> {
    let values = vec![
        session_id.get().to_string().into(),
        current_hash.to_owned().into(),
    ];
    execute(
        connection,
        "DELETE FROM vec_chunks WHERE chunk_id IN (SELECT c.id FROM chunks c JOIN documents d ON d.id=c.doc_id WHERE d.kind='prior_meeting' AND d.source_session_id=? AND d.content_hash<>?)",
        values.clone(),
    )
    .await?;
    execute(
        connection,
        "DELETE FROM documents WHERE kind='prior_meeting' AND source_session_id=? AND content_hash<>?",
        values,
    )
    .await?;
    Ok(())
}

pub(crate) async fn query_one<C: ConnectionTrait>(
    connection: &C,
    sql: &str,
    values: Vec<SeaValue>,
) -> Result<Option<QueryResult>, RagError> {
    connection
        .query_one_raw(Statement::from_sql_and_values(
            DatabaseBackend::Sqlite,
            sql,
            values,
        ))
        .await
        .map_err(storage)
}

pub(crate) async fn query_all<C: ConnectionTrait>(
    connection: &C,
    sql: &str,
    values: Vec<SeaValue>,
) -> Result<Vec<QueryResult>, RagError> {
    connection
        .query_all_raw(Statement::from_sql_and_values(
            DatabaseBackend::Sqlite,
            sql,
            values,
        ))
        .await
        .map_err(storage)
}

pub(crate) fn get<T>(row: &QueryResult, column: &str) -> Result<T, RagError>
where
    T: sea_orm::TryGetable,
{
    row.try_get("", column).map_err(storage)
}

pub(crate) fn to_i64<T>(value: T) -> Result<i64, RagError>
where
    T: TryInto<i64>,
    T::Error: std::fmt::Display,
{
    value.try_into().map_err(message)
}

pub(crate) fn from_i64<T>(value: i64) -> Result<T, RagError>
where
    T: TryFrom<i64>,
    T::Error: std::fmt::Display,
{
    T::try_from(value).map_err(message)
}

fn decode_event(row: entities::events::Model) -> Result<TimelineEvent, RagError> {
    let nanos = from_i64::<u64>(row.ts)?;
    let payload: Value = serde_json::from_str(&row.payload).map_err(message)?;
    serde_json::from_value(json!({
        "id": from_i64::<u64>(row.id)?,
        "session_id": parse_session_id(&row.session_id)?.get(),
        "ts": {"secs": nanos / 1_000_000_000, "nanos": nanos % 1_000_000_000},
        "supersedes": row.supersedes.map(from_i64::<u64>).transpose()?,
        "payload": payload
    }))
    .map_err(message)
}

fn decode_session(row: entities::sessions::Model) -> Result<Session, RagError> {
    let id = parse_session_id(&row.id)?;
    let target = CaptureTarget {
        bundle_id: row.capture_target_bundle_id,
        display_name: row.capture_target_display_name,
        window_title: row.capture_target_window_title,
        kind: parse_target_kind(&row.capture_target_kind)?,
        audio_scoped: row.capture_target_audio_scoped != 0,
    };
    if !target.has_valid_scope() {
        return Err(RagError::Storage(
            "persisted session has an invalid capture target".to_owned(),
        ));
    }
    let mut session = Session::new(id, target, from_i64(row.started_at_unix_ms)?);
    if let Some(ended) = row.ended_at_unix_ms {
        session.end(from_i64(ended)?);
    }
    Ok(session)
}

fn decode_session_summary(row: QueryResult) -> Result<SessionSummary, RagError> {
    let target = CaptureTarget {
        bundle_id: get(&row, "capture_target_bundle_id")?,
        display_name: get(&row, "capture_target_display_name")?,
        window_title: get(&row, "capture_target_window_title")?,
        kind: parse_target_kind(&get::<String>(&row, "capture_target_kind")?)?,
        audio_scoped: get::<i32>(&row, "capture_target_audio_scoped")? != 0,
    };
    if !target.has_valid_scope() {
        return Err(RagError::Storage(
            "persisted session summary has an invalid capture target".to_owned(),
        ));
    }
    Ok(SessionSummary {
        id: parse_session_id(&get::<String>(&row, "id")?)?,
        capture_target: target,
        title: get::<Option<String>>(&row, "title")?
            .as_deref()
            .and_then(RecordingTitle::new),
        started_at_unix_ms: from_i64(get(&row, "started_at_unix_ms")?)?,
        ended_at_unix_ms: get::<Option<i64>>(&row, "ended_at_unix_ms")?
            .map(from_i64)
            .transpose()?,
    })
}

fn decode_recording(
    row: entities::session_recordings::Model,
) -> Result<SessionRecording, RagError> {
    let session_id = parse_session_id(&row.session_id)?;
    match row.state {
        RecordingState::Available => {
            if row.container.as_deref() != Some("mp4") {
                return Err(RagError::Storage(
                    "available recording has an unknown container".to_owned(),
                ));
            }
            Ok(SessionRecording::Available {
                session_id,
                path: required(row.path, "available recording path")?,
                container: RecordingContainer::Mp4,
                duration: std::time::Duration::from_nanos(from_i64(required(
                    row.duration_ns,
                    "available recording duration",
                )?)?),
                byte_size: from_i64(required(row.byte_size, "available recording size")?)?,
                time_mapping: MediaTimeMapping {
                    session_origin_ns: from_i64(required(
                        row.session_origin_ns,
                        "recording session origin",
                    )?)?,
                    media_origin_ns: from_i64(required(
                        row.media_origin_ns,
                        "recording media origin",
                    )?)?,
                    rate_numerator: from_i64(i64::from(required(
                        row.rate_numerator,
                        "recording rate numerator",
                    )?))?,
                    rate_denominator: from_i64(i64::from(required(
                        row.rate_denominator,
                        "recording rate denominator",
                    )?))?,
                },
            })
        }
        RecordingState::Deleted | RecordingState::Pruned => Ok(SessionRecording::Missing {
            session_id,
            reason: if row.state == RecordingState::Deleted {
                RecordingMissingReason::Deleted
            } else {
                RecordingMissingReason::Pruned
            },
        }),
        RecordingState::Growing => Err(RagError::Storage(
            "growing recording cannot be decoded as settled".to_owned(),
        )),
    }
}

fn decode_recording_reference(
    row: entities::session_recordings::Model,
) -> Result<RecordingReference, RagError> {
    if row.state == RecordingState::Growing {
        return Ok(RecordingReference::Growing {
            session_id: parse_session_id(&row.session_id)?,
            path: required(row.path, "growing recording path")?,
            finalization_error: row.finalization_error,
        });
    }
    decode_recording(row).map(RecordingReference::Settled)
}

fn decode_recording_query(row: QueryResult) -> Result<SessionRecording, RagError> {
    decode_recording(recording_model(row)?)
}

fn decode_recording_reference_query(row: QueryResult) -> Result<RecordingReference, RagError> {
    decode_recording_reference(recording_model(row)?)
}

fn recording_model(row: QueryResult) -> Result<entities::session_recordings::Model, RagError> {
    let state = match get::<String>(&row, "state")?.as_str() {
        "growing" => RecordingState::Growing,
        "available" => RecordingState::Available,
        "deleted" => RecordingState::Deleted,
        "pruned" => RecordingState::Pruned,
        other => {
            return Err(RagError::Storage(format!(
                "unknown recording state {other:?}"
            )));
        }
    };
    Ok(entities::session_recordings::Model {
        session_id: get(&row, "session_id")?,
        state,
        path: get(&row, "path")?,
        container: get(&row, "container")?,
        duration_ns: get(&row, "duration_ns")?,
        byte_size: get(&row, "byte_size")?,
        session_origin_ns: get(&row, "session_origin_ns")?,
        media_origin_ns: get(&row, "media_origin_ns")?,
        rate_numerator: get(&row, "rate_numerator")?,
        rate_denominator: get(&row, "rate_denominator")?,
        finalization_error: get(&row, "finalization_error")?,
        updated_at_unix_ms: get(&row, "updated_at_unix_ms")?,
    })
}

fn required<T>(value: Option<T>, field: &str) -> Result<T, RagError> {
    value.ok_or_else(|| RagError::Storage(format!("persisted row is missing {field}")))
}

fn decode_grounded_view(row: entities::grounded_derived_views::Model) -> GroundedDerivedView {
    GroundedDerivedView {
        artifact: row.artifact,
        usage: row.usage,
        provider_model: row.provider_model,
        grant_fingerprint: row.grant_fingerprint,
        source_status: row.source_status,
        bundle: row.bundle,
        normalizations: row.normalizations,
        consultations: row.consultations,
    }
}

async fn load_events_from<C: ConnectionTrait>(
    connection: &C,
    session_id: SessionId,
) -> Result<Vec<TimelineEvent>, RagError> {
    let rows = query_all(
        connection,
        "SELECT id,session_id,ts,kind,supersedes,payload FROM events WHERE session_id=? ORDER BY id",
        vec![session_id.get().to_string().into()],
    )
    .await?;
    rows.into_iter()
        .map(|row| {
            decode_event(entities::events::Model {
                id: get(&row, "id")?,
                session_id: get(&row, "session_id")?,
                ts: get(&row, "ts")?,
                kind: get(&row, "kind")?,
                supersedes: get(&row, "supersedes")?,
                payload: get(&row, "payload")?,
            })
        })
        .collect()
}

async fn ensure_completed<C: ConnectionTrait>(
    connection: &C,
    session_id: SessionId,
) -> Result<(), RagError> {
    let row = query_one(
        connection,
        "SELECT ended_at_unix_ms FROM sessions WHERE id=?",
        vec![session_id.get().to_string().into()],
    )
    .await?
    .ok_or_else(|| RagError::NotFound {
        id: session_id.get().to_string(),
    })?;
    if get::<Option<i64>>(&row, "ended_at_unix_ms")?.is_none() {
        return Err(RagError::Storage(
            "post-meeting annotations require a completed session".to_owned(),
        ));
    }
    Ok(())
}

async fn ensure_session_entry<C: ConnectionTrait>(
    connection: &C,
    session: &Session,
) -> Result<(), RagError> {
    let session_id = session.id().get().to_string();
    if query_one(
        connection,
        "SELECT 1 AS present FROM entry_sessions WHERE session_id=?",
        vec![session_id.clone().into()],
    )
    .await?
    .is_some()
    {
        return Ok(());
    }
    let mut candidate = session.id().get();
    loop {
        let candidate_text = candidate.to_string();
        let occupied = query_one(
            connection,
            "SELECT 1 AS present FROM entries WHERE id=?",
            vec![candidate_text.clone().into()],
        )
        .await?
        .is_some();
        if !occupied {
            execute(
                connection,
                "INSERT INTO entries(id,created_at_unix_ms,title) VALUES(?,?,NULL)",
                vec![
                    candidate_text.clone().into(),
                    to_i64(session.started_at_unix_ms())?.into(),
                ],
            )
            .await?;
            execute(
                connection,
                "INSERT INTO entry_sessions(session_id,entry_id) VALUES(?,?)",
                vec![session_id.into(), candidate_text.into()],
            )
            .await?;
            return Ok(());
        }
        candidate = candidate.checked_add(1).ok_or_else(|| {
            RagError::Storage("could not allocate an implicit entry identity".to_owned())
        })?;
    }
}

async fn attach_session_relation<C: ConnectionTrait>(
    connection: &C,
    entry_id: EntryId,
    session_id: SessionId,
) -> Result<(), RagError> {
    let session_text = session_id.get().to_string();
    if let Some(row) = query_one(
        connection,
        "SELECT entry_id FROM entry_sessions WHERE session_id=?",
        vec![session_text.clone().into()],
    )
    .await?
    {
        let existing = parse_entry_id(&get::<String>(&row, "entry_id")?)?;
        if existing == entry_id {
            return Ok(());
        }
        return Err(RagError::Storage(format!(
            "session {session_text} already belongs to entry {} and cannot be detached",
            existing.get()
        )));
    }
    execute(
        connection,
        "INSERT INTO entry_sessions(session_id,entry_id) VALUES(?,?)",
        vec![session_text.into(), entry_id.get().to_string().into()],
    )
    .await?;
    Ok(())
}

fn register_sqlite_vec() {
    static REGISTER: Once = Once::new();
    REGISTER.call_once(|| unsafe {
        // SAFETY: sqlite-vec exports SQLite's extension entry point with SQLite's documented ABI.
        libsqlite3_sys::sqlite3_auto_extension(Some(std::mem::transmute::<
            *const (),
            unsafe extern "C" fn(
                *mut libsqlite3_sys::sqlite3,
                *mut *mut std::ffi::c_char,
                *const libsqlite3_sys::sqlite3_api_routines,
            ) -> std::ffi::c_int,
        >(
            sqlite_vec::sqlite3_vec_init as *const ()
        )));
    });
}

fn target_kind(kind: TargetKind) -> &'static str {
    match kind {
        TargetKind::Application => "application",
        TargetKind::Window => "window",
        TargetKind::Display => "display",
        TargetKind::Microphone => "microphone",
        TargetKind::Imported => "imported",
    }
}

fn parse_target_kind(kind: &str) -> Result<TargetKind, RagError> {
    match kind {
        "application" => Ok(TargetKind::Application),
        "window" => Ok(TargetKind::Window),
        "display" => Ok(TargetKind::Display),
        "microphone" => Ok(TargetKind::Microphone),
        "imported" => Ok(TargetKind::Imported),
        unknown => Err(RagError::Storage(format!(
            "unknown capture target kind {unknown:?}"
        ))),
    }
}

fn recording_path(reference: RecordingReference) -> Result<Option<PathBuf>, RagError> {
    match reference {
        RecordingReference::Growing { path, .. }
        | RecordingReference::Settled(SessionRecording::Available { path, .. }) => {
            Ok(Some(PathBuf::from(path)))
        }
        RecordingReference::Settled(SessionRecording::Missing { .. }) => Ok(None),
    }
}

fn scan_tombstones(directory: &Path) -> Result<Vec<(SessionId, PathBuf, PathBuf)>, RagError> {
    let entries = match std::fs::read_dir(directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(io_error(error)),
    };
    let mut output = Vec::new();
    for entry in entries {
        let entry = entry.map_err(io_error)?;
        let file_name = entry.file_name();
        let Some(session_id) = file_name.to_str().and_then(parse_tombstone_session_id) else {
            continue;
        };
        output.push((
            session_id,
            entry.path(),
            directory.join(format!("{}.mp4", session_id.get())),
        ));
    }
    Ok(output)
}

async fn restore_quarantined(quarantined: Vec<(PathBuf, PathBuf)>) {
    let _ = tokio::task::spawn_blocking(move || {
        for (original, tombstone) in quarantined {
            if let Err(error) = std::fs::rename(&tombstone, &original) {
                eprintln!(
                    "Could not restore quarantined recording {} from {}: {error}",
                    original.display(),
                    tombstone.display()
                );
            }
        }
    })
    .await;
}

async fn unlink_tombstones(quarantined: Vec<(PathBuf, PathBuf)>) {
    let _ = tokio::task::spawn_blocking(move || {
        for (_, tombstone) in quarantined {
            if let Err(error) = std::fs::remove_file(&tombstone) {
                eprintln!(
                    "Could not unlink recording tombstone {}: {error}",
                    tombstone.display()
                );
            }
        }
    })
    .await;
}

fn parse_tombstone_session_id(file_name: &str) -> Option<SessionId> {
    file_name
        .strip_prefix('.')?
        .strip_suffix(".deleting")?
        .parse::<u128>()
        .ok()
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

fn wall_clock_unix_ms() -> Result<u64, RagError> {
    u64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(message)?
            .as_millis(),
    )
    .map_err(message)
}

fn io_error(error: io::Error) -> RagError {
    RagError::Storage(error.to_string())
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
fn chunk_markdown(text: &str) -> Vec<String> {
    const TARGET: usize = 400;
    const OVERLAP: usize = 50;
    let mut output = Vec::new();
    for section in text.split("\n#") {
        let words = section.split_whitespace().collect::<Vec<_>>();
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

impl Store {
    pub async fn ingest_text(
        &self,
        text: &str,
        kind: DocumentKind,
        metadata: IngestMetadata,
    ) -> Result<bool, RagError> {
        self.validate_ingest(kind, &metadata).await?;
        let hash = content_hash(text);
        if self.document_exists(kind, &hash, &metadata).await? {
            return Ok(false);
        }
        let chunks = chunk_markdown(text);
        if chunks.is_empty() {
            return Err(RagError::Storage(
                "cannot ingest an empty local document".to_owned(),
            ));
        }
        let embeddings = self.embed(chunks.clone()).await?;
        self.persist_document(kind, metadata, hash, chunks, embeddings)
            .await
    }

    pub async fn index_prior_meeting(
        &self,
        session_id: SessionId,
        collection_id: Option<String>,
    ) -> Result<bool, RagError> {
        let Some((text, title)) = self.render_prior_meeting(session_id).await? else {
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
        .await
    }

    pub async fn is_session_indexed(&self, session_id: SessionId) -> Result<bool, RagError> {
        Ok(entities::documents::Entity::find()
            .filter(entities::documents::Column::Kind.eq(DocumentKind::PriorMeeting.as_str()))
            .filter(entities::documents::Column::SourceSessionId.eq(session_id.get().to_string()))
            .one(&self.reader)
            .await
            .map_err(storage)?
            .is_some())
    }

    pub async fn index_missing_prior_meetings(&self) -> Result<PriorMeetingIndexReport, RagError> {
        let rows = query_all(
            &self.reader,
            "SELECT s.id FROM sessions s WHERE s.ended_at_unix_ms IS NOT NULL AND NOT EXISTS (SELECT 1 FROM documents d WHERE d.kind='prior_meeting' AND d.source_session_id=s.id) ORDER BY s.started_at_unix_ms DESC",
            Vec::new(),
        )
        .await?;
        let pending = rows
            .into_iter()
            .map(|row| get::<String>(&row, "id").and_then(|id| parse_session_id(&id)))
            .collect::<Result<Vec<_>, _>>()?;
        let mut report = PriorMeetingIndexReport::default();
        for session_id in pending {
            match self.index_prior_meeting(session_id, None).await {
                Ok(true) => report.indexed.push(session_id),
                Ok(false) => report.skipped.push(session_id),
                Err(error) => report.failed.push((session_id, error.to_string())),
            }
        }
        Ok(report)
    }

    pub async fn ingest_meeting_note(
        &self,
        session_id: SessionId,
        title: String,
        text: &str,
        collection_id: Option<String>,
    ) -> Result<bool, RagError> {
        self.ensure_completed_session(session_id).await?;
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
        .await
    }

    pub async fn load_local_evidence(
        &self,
        evidence_id: &str,
    ) -> Result<LocalEvidenceReceipt, RagError> {
        let chunk = entities::chunks::Entity::find_by_id(evidence_id)
            .one(&self.reader)
            .await
            .map_err(storage)?
            .ok_or_else(|| RagError::NotFound {
                id: evidence_id.to_owned(),
            })?;
        let document = entities::documents::Entity::find_by_id(&chunk.doc_id)
            .one(&self.reader)
            .await
            .map_err(storage)?
            .ok_or_else(|| RagError::NotFound {
                id: chunk.doc_id.clone(),
            })?;
        Ok(LocalEvidenceReceipt {
            evidence_id: evidence_id.to_owned(),
            document_id: chunk.doc_id,
            ordinal: from_i64(chunk.ordinal)?,
            text: chunk.text,
            metadata: serde_json::from_str(&chunk.metadata).map_err(message)?,
            kind: DocumentKind::parse(&document.kind)?,
            title: document.title,
            source_path: document.source_path,
            collection_id: document.collection_id,
            source_session_id: document
                .source_session_id
                .as_deref()
                .map(parse_session_id)
                .transpose()?,
            provenance: LocalProvenance::parse(&document.provenance_status)?,
        })
    }

    async fn validate_ingest(
        &self,
        kind: DocumentKind,
        metadata: &IngestMetadata,
    ) -> Result<(), RagError> {
        match (kind.requires_source_session(), metadata.source_session_id) {
            (true, Some(session_id)) => self.ensure_completed_session(session_id).await,
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

    async fn ensure_completed_session(&self, session_id: SessionId) -> Result<(), RagError> {
        let session = self.load_session_record(session_id).await?;
        if session.ended_at_unix_ms().is_none() {
            return Err(RagError::Storage(format!(
                "source session {} is not durably completed",
                session_id.get()
            )));
        }
        Ok(())
    }

    async fn render_prior_meeting(
        &self,
        session_id: SessionId,
    ) -> Result<Option<(String, String)>, RagError> {
        let (mut text, title, events) = self.prior_meeting_preamble(session_id).await?;
        let replayed = sotto_core::replay(&events).map_err(message)?;
        let mut finals = replayed
            .active()
            .values()
            .filter_map(|event| {
                let EventPayload::UtteranceFinal(utterance) = event.payload() else {
                    return None;
                };
                Some((utterance.start, event.id().get(), utterance))
            })
            .collect::<Vec<_>>();
        finals.sort_by_key(|(start, event_id, _)| (*start, *event_id));
        if finals.is_empty() {
            return Ok(None);
        }
        for (start, event_id, utterance) in finals {
            let milliseconds = start.as_millis();
            let speaker = match utterance.source {
                Source::Mic => "You",
                Source::System => "Meeting audio",
            };
            text.push_str(&format!(
                "[{:02}:{:02}.{:03}] [{speaker}] \"{}\" [event:{event_id}]\n",
                milliseconds / 60_000,
                (milliseconds % 60_000) / 1_000,
                milliseconds % 1_000,
                utterance.text,
            ));
        }
        Ok(Some((text, title)))
    }

    async fn prior_meeting_preamble(
        &self,
        session_id: SessionId,
    ) -> Result<(String, String, Vec<TimelineEvent>), RagError> {
        self.ensure_completed_session(session_id).await?;
        let session = self.load_session_record(session_id).await?;
        let events = self.load_session(session_id).await?;
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

    async fn document_exists(
        &self,
        kind: DocumentKind,
        hash: &str,
        metadata: &IngestMetadata,
    ) -> Result<bool, RagError> {
        let row = query_one(
            &self.reader,
            "SELECT 1 AS present FROM documents WHERE kind=? AND content_hash=? AND collection_id IS ? AND source_session_id IS ? AND source_path IS ?",
            vec![
                kind.as_str().into(), hash.to_owned().into(), metadata.collection_id.clone().into(),
                metadata.source_session_id.map(|id| id.get().to_string()).into(),
                metadata.source_path.clone().into(),
            ],
        ).await?;
        Ok(row.is_some())
    }

    async fn persist_document(
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
        let transaction = self.writer.begin().await.map_err(storage)?;
        execute(
            &transaction,
            "INSERT INTO documents(id,kind,title,source_path,collection_id,source_session_id,provenance_status,ingested_at,content_hash) VALUES(?,?,?,?,?,?,'native',?,?)",
            vec![
                document_id.clone().into(), kind.as_str().into(), metadata.title.into(),
                metadata.source_path.into(), metadata.collection_id.into(), source_session_id.into(),
                to_i64(wall_clock_unix_ms()?)?.into(), hash.into(),
            ],
        ).await?;
        let metadata_json = serde_json::to_string(&metadata.fields).map_err(message)?;
        for (ordinal, (text, vector)) in chunks.into_iter().zip(embeddings).enumerate() {
            let id = format!("{document_id}:{ordinal}");
            let blob = vector_blob(&vector);
            execute(
                &transaction,
                "INSERT INTO chunks(id,doc_id,ordinal,text,token_count,metadata,embedding) VALUES(?,?,?,?,?,?,?)",
                vec![
                    id.clone().into(), document_id.clone().into(), to_i64(ordinal)?.into(),
                    text.clone().into(), to_i64(word_count(&text))?.into(), metadata_json.clone().into(),
                    SeaValue::Bytes(Some(blob.clone())),
                ],
            ).await?;
            execute(
                &transaction,
                "INSERT INTO vec_chunks(chunk_id,embedding) VALUES(?,?)",
                vec![id.into(), SeaValue::Bytes(Some(blob))],
            )
            .await?;
        }
        transaction.commit().await.map_err(storage)?;
        Ok(true)
    }

    pub async fn unload_embedding_model(&self) -> Result<(), RagError> {
        let embedding = Arc::clone(&self.embedding);
        tokio::task::spawn_blocking(move || {
            *embedding.lock().map_err(poisoned)? = None;
            Ok(())
        })
        .await
        .map_err(message)?
    }

    pub async fn search_filtered(
        &self,
        query: &str,
        k: usize,
        filter: &SearchFilter,
    ) -> Result<Vec<Chunk>, RagError> {
        if k == 0 {
            return Ok(Vec::new());
        }
        let query_vector = self
            .embed(vec![query.to_owned()])
            .await?
            .pop()
            .ok_or_else(|| RagError::Embedding("empty embedding result".to_owned()))?;
        self.search_with_vector(query, k, filter, &query_vector, true)
            .await
    }

    async fn search_with_vector(
        &self,
        query: &str,
        k: usize,
        filter: &SearchFilter,
        query_vector: &[f32],
        include_keyword: bool,
    ) -> Result<Vec<Chunk>, RagError> {
        let filter_kind = filter.kind.map(DocumentKind::as_str).map(str::to_owned);
        let filter_session = filter.source_session_id.map(|id| id.get().to_string());
        let mut candidate_limit = k.saturating_mul(4).clamp(1, MAX_FILTERED_CANDIDATES);
        let mut scores = HashMap::<String, f64>::new();
        loop {
            scores.clear();
            let rows = query_all(
                &self.reader,
                "SELECT v.chunk_id FROM vec_chunks v JOIN chunks c ON c.id=v.chunk_id JOIN documents d ON d.id=c.doc_id WHERE v.embedding MATCH ? AND k=? AND (? IS NULL OR d.kind=?) AND (? IS NULL OR d.collection_id=?) AND (? IS NULL OR d.source_session_id=?) ORDER BY v.distance",
                vec![
                    SeaValue::Bytes(Some(vector_blob(query_vector))), to_i64(candidate_limit)?.into(),
                    filter_kind.clone().into(), filter_kind.clone().into(),
                    filter.collection_id.clone().into(), filter.collection_id.clone().into(),
                    filter_session.clone().into(), filter_session.clone().into(),
                ],
            ).await?;
            for (rank, row) in rows.into_iter().enumerate() {
                scores.insert(get(&row, "chunk_id")?, rank_score(rank));
            }
            if scores.len() >= k || candidate_limit == MAX_FILTERED_CANDIDATES {
                break;
            }
            candidate_limit = candidate_limit
                .saturating_mul(2)
                .min(MAX_FILTERED_CANDIDATES);
        }
        if include_keyword {
            let rows = query_all(
                &self.reader,
                "SELECT c.id FROM chunks_fts f JOIN chunks c ON c.rowid=f.rowid JOIN documents d ON d.id=c.doc_id WHERE chunks_fts MATCH ? AND (? IS NULL OR d.kind=?) AND (? IS NULL OR d.collection_id=?) AND (? IS NULL OR d.source_session_id=?) ORDER BY bm25(chunks_fts) LIMIT ?",
                vec![
                    query.to_owned().into(), filter_kind.clone().into(), filter_kind.into(),
                    filter.collection_id.clone().into(), filter.collection_id.clone().into(),
                    filter_session.clone().into(), filter_session.into(), to_i64(candidate_limit)?.into(),
                ],
            ).await?;
            for (rank, row) in rows.into_iter().enumerate() {
                *scores.entry(get(&row, "id")?).or_default() += rank_score(rank);
            }
        }
        let mut ranked = scores.into_iter().collect::<Vec<_>>();
        ranked.sort_by(|a, b| b.1.total_cmp(&a.1));
        let mut output = Vec::new();
        for (id, _) in ranked {
            let row = query_one(
                &self.reader,
                "SELECT c.text,d.title,d.kind,d.collection_id,d.source_session_id,d.source_path,d.provenance_status,c.doc_id,c.ordinal,c.metadata FROM chunks c JOIN documents d ON d.id=c.doc_id WHERE c.id=?",
                vec![id.clone().into()],
            ).await?.ok_or_else(|| RagError::NotFound { id: id.clone() })?;
            let mut metadata =
                serde_json::from_str::<BTreeMap<String, String>>(&get::<String>(&row, "metadata")?)
                    .map_err(message)?;
            metadata.insert("evidence_scope".to_owned(), "local".to_owned());
            metadata.insert("document_id".to_owned(), get(&row, "doc_id")?);
            metadata.insert(
                "ordinal".to_owned(),
                get::<i64>(&row, "ordinal")?.to_string(),
            );
            metadata.insert("kind".to_owned(), get(&row, "kind")?);
            metadata.insert(
                "provenance_status".to_owned(),
                get(&row, "provenance_status")?,
            );
            for (column, key) in [
                ("collection_id", "collection_id"),
                ("source_session_id", "source_session_id"),
                ("source_path", "source_path"),
            ] {
                if let Some(value) = get::<Option<String>>(&row, column)? {
                    metadata.insert(key.to_owned(), value);
                }
            }
            output.push(Chunk {
                id,
                text: get(&row, "text")?,
                source: get(&row, "title")?,
                metadata,
            });
            if output.len() == k {
                break;
            }
        }
        Ok(output)
    }

    async fn embed(&self, texts: Vec<String>) -> Result<Vec<Vec<f32>>, RagError> {
        let embedding = Arc::clone(&self.embedding);
        tokio::task::spawn_blocking(move || {
            let mut model = embedding.lock().map_err(poisoned)?;
            if model.is_none() {
                *model = Some(
                    TextEmbedding::try_new(
                        InitOptions::new(EmbeddingModel::AllMiniLML6V2)
                            .with_show_download_progress(false),
                    )
                    .map_err(|error| RagError::Embedding(error.to_string()))?,
                );
            }
            model
                .as_mut()
                .ok_or_else(|| RagError::Embedding("model unavailable".to_owned()))?
                .embed(texts, None)
                .map_err(|error| RagError::Embedding(error.to_string()))
        })
        .await
        .map_err(message)?
    }
}

impl Retriever for Store {
    fn search<'a>(
        &'a self,
        query: &'a str,
        k: usize,
    ) -> BoxFuture<'a, Result<Vec<Chunk>, RagError>> {
        Box::pin(async move {
            self.search_filtered(query, k, &SearchFilter::default())
                .await
        })
    }
}

impl Store {
    pub async fn append_completed_annotation(
        &self,
        session_id: SessionId,
        anchor: EventId,
        text: &str,
        mark: MarkKind,
        supersedes: Option<EventId>,
    ) -> Result<TimelineEvent, RagError> {
        let transaction = self.writer.begin().await.map_err(storage)?;
        ensure_completed(&transaction, session_id).await?;
        let events = load_events_from(&transaction, session_id).await?;
        let replayed = replay_lenient(&events);
        let Some(anchor_event) = events.iter().find(|event| event.id() == anchor) else {
            return Err(RagError::Storage(format!(
                "annotation anchor {} is not present in session {}",
                anchor.get(),
                session_id.get()
            )));
        };
        if !matches!(
            anchor_event.payload(),
            EventPayload::UtteranceFinal(_) | EventPayload::UtterancePartial(_)
        ) {
            return Err(RagError::Storage(
                "post-meeting annotations must anchor to a transcript row".to_owned(),
            ));
        }
        let replacement_anchor = if let Some(target) = supersedes {
            let Some(EventPayload::UserAnnotation(previous)) = replayed
                .state()
                .active()
                .get(&target)
                .map(TimelineEvent::payload)
            else {
                return Err(RagError::Storage(
                    "annotation edit must supersede an active user annotation".to_owned(),
                ));
            };
            if previous.anchor != anchor {
                return Err(RagError::Storage(
                    "annotation edits must retain their original transcript anchor".to_owned(),
                ));
            }
            previous.anchor
        } else {
            anchor
        };
        let annotation =
            checked_user_annotation(replacement_anchor, text, mark).map_err(message)?;
        let next_id = events
            .iter()
            .map(TimelineEvent::id)
            .max()
            .map_or(1, |id| id.get().saturating_add(1));
        let timestamp = events
            .iter()
            .map(TimelineEvent::ts)
            .max()
            .unwrap_or_default()
            .saturating_add(std::time::Duration::from_nanos(1));
        let event: TimelineEvent = serde_json::from_value(json!({
            "id": next_id,
            "session_id": session_id.get(),
            "ts": {"secs": timestamp.as_secs(), "nanos": timestamp.subsec_nanos()},
            "supersedes": supersedes.map(EventId::get),
            "payload": {"type": "user_annotation", "value": annotation}
        }))
        .map_err(message)?;
        self.insert_event(&transaction, &event).await?;
        transaction.commit().await.map_err(storage)?;
        Ok(event)
    }

    pub async fn refresh_searchable_prior_meeting(
        &self,
        session_id: SessionId,
    ) -> Result<bool, RagError> {
        let Some((text, title)) = self.render_annotated_prior_meeting(session_id).await? else {
            return Ok(false);
        };
        let hash = content_hash(&text);
        self.ingest_text(
            &text,
            DocumentKind::PriorMeeting,
            IngestMetadata {
                title,
                source_session_id: Some(session_id),
                ..IngestMetadata::default()
            },
        )
        .await?;
        let transaction = self.writer.begin().await.map_err(storage)?;
        remove_superseded_prior_meetings(&transaction, session_id, &hash).await?;
        transaction.commit().await.map_err(storage)?;
        Ok(true)
    }

    pub(crate) async fn render_annotated_prior_meeting(
        &self,
        session_id: SessionId,
    ) -> Result<Option<(String, String)>, RagError> {
        let (mut text, title) = match self.render_prior_meeting(session_id).await? {
            Some(rendered) => rendered,
            None => {
                let (text, title, _) = self.prior_meeting_preamble(session_id).await?;
                (text, title)
            }
        };
        let transcribed = text.contains("[event:");
        let events = self.load_session(session_id).await?;
        let replayed = sotto_core::replay(&events).map_err(message)?;
        let mut annotations = replayed
            .active()
            .values()
            .filter_map(|event| {
                let EventPayload::UserAnnotation(annotation) = event.payload() else {
                    return None;
                };
                Some((event.id(), annotation))
            })
            .collect::<Vec<_>>();
        annotations.sort_by_key(|(id, _)| *id);
        if annotations.is_empty() && !transcribed {
            return Ok(None);
        }
        for (event_id, annotation) in annotations {
            text.push_str(&format!(
                "[Your note] \"{}\" [anchor:{}] [event:{}]\n",
                annotation.text,
                annotation.anchor.get(),
                event_id.get(),
            ));
        }
        Ok(Some((text, title)))
    }
}

impl Store {
    pub async fn delete_entry(
        &self,
        entry_id: EntryId,
        recording_directory: &Path,
    ) -> Result<(), RagError> {
        let transaction = self.writer.begin().await.map_err(storage)?;
        let id = entry_id.get().to_string();
        let rows = query_all(
            &transaction,
            "SELECT session_id FROM entry_sessions WHERE entry_id=?",
            vec![id.clone().into()],
        )
        .await?;
        let session_ids = rows
            .into_iter()
            .map(|row| get::<String>(&row, "session_id").and_then(|id| parse_session_id(&id)))
            .collect::<Result<Vec<_>, _>>()?;
        let mut quarantined = Vec::with_capacity(session_ids.len());
        for session_id in session_ids {
            match self
                .quarantine_recording_media(session_id, recording_directory)
                .await
            {
                Ok(Some(pair)) => quarantined.push(pair),
                Ok(None) => {}
                Err(error) => {
                    restore_quarantined(quarantined.clone()).await;
                    return Err(error);
                }
            }
        }

        let outcome = async {
            execute(
                &transaction,
                "DELETE FROM vec_chunks WHERE chunk_id IN (SELECT c.id FROM chunks c JOIN documents d ON d.id=c.doc_id JOIN entry_sessions es ON es.session_id=d.source_session_id WHERE es.entry_id=?)",
                vec![id.clone().into()],
            )
            .await?;
            execute(
                &transaction,
                "DELETE FROM events WHERE session_id IN (SELECT session_id FROM entry_sessions WHERE entry_id=?)",
                vec![id.clone().into()],
            )
            .await?;
            execute(
                &transaction,
                "DELETE FROM sessions WHERE id IN (SELECT session_id FROM entry_sessions WHERE entry_id=?)",
                vec![id.clone().into()],
            )
            .await?;
            let changed = execute(
                &transaction,
                "DELETE FROM entries WHERE id=?",
                vec![id.clone().into()],
            )
            .await?;
            if changed == 0 {
                return Err(RagError::NotFound { id });
            }
            transaction.commit().await.map_err(storage)
        }
        .await;

        match outcome {
            Ok(()) => {
                unlink_tombstones(quarantined).await;
                Ok(())
            }
            Err(error) => {
                restore_quarantined(quarantined).await;
                Err(error)
            }
        }
    }

    pub async fn remove_recording_media(
        &self,
        session_id: SessionId,
        reason: RecordingMissingReason,
        recording_directory: &Path,
    ) -> Result<bool, RagError> {
        let Some(reference) = self.load_recording_reference(session_id).await? else {
            return Ok(false);
        };
        let path = recording_path(reference)?;
        let Some(path) = path else {
            return Ok(false);
        };
        validate_recording_path(session_id, recording_directory, &path)?;
        let tombstone = recording_directory.join(format!(".{}.deleting", session_id.get()));
        let source = path.clone();
        let destination = tombstone.clone();
        let renamed = tokio::task::spawn_blocking(move || std::fs::rename(source, destination))
            .await
            .map_err(message)?;
        if let Err(error) = renamed {
            if error.kind() == io::ErrorKind::NotFound {
                self.mark_recording_missing(session_id, reason).await?;
                return Ok(true);
            }
            return Err(io_error(error));
        }
        if let Err(error) = self.mark_recording_missing(session_id, reason).await {
            let restore_from = tombstone.clone();
            let restore_to = path.clone();
            let _ = tokio::task::spawn_blocking(move || std::fs::rename(restore_from, restore_to))
                .await;
            return Err(error);
        }
        let _ = tokio::task::spawn_blocking(move || std::fs::remove_file(tombstone)).await;
        Ok(true)
    }

    async fn quarantine_recording_media(
        &self,
        session_id: SessionId,
        recording_directory: &Path,
    ) -> Result<Option<(PathBuf, PathBuf)>, RagError> {
        let Some(reference) = self.load_recording_reference(session_id).await? else {
            return Ok(None);
        };
        let Some(path) = recording_path(reference)? else {
            return Ok(None);
        };
        validate_recording_path(session_id, recording_directory, &path)?;
        let tombstone = recording_directory.join(format!(".{}.deleting", session_id.get()));
        let source = path.clone();
        let destination = tombstone.clone();
        match tokio::task::spawn_blocking(move || std::fs::rename(source, destination))
            .await
            .map_err(message)?
        {
            Ok(()) => Ok(Some((path, tombstone))),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(io_error(error)),
        }
    }

    pub async fn recover_quarantined_media(
        &self,
        recording_directory: &Path,
    ) -> Result<Vec<QuarantineRecovery>, RagError> {
        let directory = recording_directory.to_path_buf();
        let tombstones = tokio::task::spawn_blocking(move || scan_tombstones(&directory))
            .await
            .map_err(message)??;
        let mut resolved = Vec::with_capacity(tombstones.len());
        for (session_id, tombstone, original) in tombstones {
            let session_exists =
                entities::sessions::Entity::find_by_id(session_id.get().to_string())
                    .one(&self.reader)
                    .await
                    .map_err(storage)?
                    .is_some();
            let recording_path_present = if session_exists {
                entities::session_recordings::Entity::find_by_id(session_id.get().to_string())
                    .one(&self.reader)
                    .await
                    .map_err(storage)?
                    .is_some_and(|row| row.path.is_some())
            } else {
                false
            };
            if session_exists && recording_path_present {
                tokio::task::spawn_blocking(move || std::fs::rename(tombstone, original))
                    .await
                    .map_err(message)?
                    .map_err(io_error)?;
                resolved.push(QuarantineRecovery {
                    session_id,
                    restored: true,
                });
            } else {
                tokio::task::spawn_blocking(move || std::fs::remove_file(tombstone))
                    .await
                    .map_err(message)?
                    .map_err(io_error)?;
                resolved.push(QuarantineRecovery {
                    session_id,
                    restored: false,
                });
            }
        }
        Ok(resolved)
    }

    pub async fn enforce_recording_budget(
        &self,
        recording_directory: &Path,
    ) -> Result<Vec<SessionId>, RagError> {
        self.recover_quarantined_media(recording_directory).await?;
        let budget = self.recording_usage().await?.budget_bytes;
        let rows = query_all(
            &self.reader,
            "SELECT r.session_id,r.state,r.path,r.byte_size FROM session_recordings r JOIN sessions s ON s.id=r.session_id WHERE r.state IN ('growing','available') ORDER BY s.started_at_unix_ms ASC,r.session_id ASC",
            Vec::new(),
        )
        .await?;
        let raw = rows
            .into_iter()
            .map(|row| {
                Ok((
                    get::<String>(&row, "session_id")?,
                    get::<String>(&row, "state")?,
                    get::<String>(&row, "path")?,
                    get::<Option<i64>>(&row, "byte_size")?,
                ))
            })
            .collect::<Result<Vec<_>, RagError>>()?;
        let measured = tokio::task::spawn_blocking(move || {
            raw.into_iter()
                .map(|(id, state, path, saved)| {
                    let bytes = if state == "growing" {
                        std::fs::metadata(path).map_or(0, |metadata| metadata.len())
                    } else {
                        saved
                            .and_then(|value| u64::try_from(value).ok())
                            .unwrap_or(0)
                    };
                    (id, bytes)
                })
                .collect::<Vec<_>>()
        })
        .await
        .map_err(message)?;
        let mut used = measured.iter().map(|(_, bytes)| *bytes).sum::<u64>();
        let mut pruned = Vec::new();
        for (raw_id, bytes) in measured {
            if used <= budget {
                break;
            }
            let id = parse_session_id(&raw_id)?;
            if self
                .remove_recording_media(id, RecordingMissingReason::Pruned, recording_directory)
                .await?
            {
                used = used.saturating_sub(bytes);
                pruned.push(id);
            }
        }
        Ok(pruned)
    }

    pub async fn delete_session(&self, session_id: SessionId) -> Result<(), RagError> {
        let transaction = self.writer.begin().await.map_err(storage)?;
        let id = session_id.get().to_string();
        let entry_id = query_one(
            &transaction,
            "SELECT entry_id FROM entry_sessions WHERE session_id=?",
            vec![id.clone().into()],
        )
        .await?
        .map(|row| get::<String>(&row, "entry_id"))
        .transpose()?;
        execute(
            &transaction,
            "DELETE FROM vec_chunks WHERE chunk_id IN (SELECT c.id FROM chunks c JOIN documents d ON d.id=c.doc_id WHERE d.source_session_id=?)",
            vec![id.clone().into()],
        )
        .await?;
        execute(
            &transaction,
            "DELETE FROM events WHERE session_id=?",
            vec![id.clone().into()],
        )
        .await?;
        execute(
            &transaction,
            "DELETE FROM sessions WHERE id=?",
            vec![id.into()],
        )
        .await?;
        if let Some(entry_id) = entry_id {
            execute(
                &transaction,
                "DELETE FROM entries WHERE id=? AND NOT EXISTS (SELECT 1 FROM entry_sessions WHERE entry_id=?)",
                vec![entry_id.clone().into(), entry_id.into()],
            ).await?;
        }
        transaction.commit().await.map_err(storage)
    }
}

impl Store {
    pub async fn save_recording(&self, recording: &SessionRecording) -> Result<(), RagError> {
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
                "only available recording metadata can be saved directly".to_owned(),
            ));
        };
        if path.is_empty() || *container != RecordingContainer::Mp4 || !time_mapping.is_identity() {
            return Err(RagError::Storage(
                "recording metadata has an unsupported container or time mapping".to_owned(),
            ));
        }
        execute(
            &self.writer,
            "INSERT INTO session_recordings(session_id,state,path,container,duration_ns,byte_size,session_origin_ns,media_origin_ns,rate_numerator,rate_denominator,finalization_error,updated_at_unix_ms) VALUES(?,'available',?,'mp4',?,?,?,?,?,?,NULL,?) ON CONFLICT(session_id) DO UPDATE SET state='available',path=excluded.path,container='mp4',duration_ns=excluded.duration_ns,byte_size=excluded.byte_size,session_origin_ns=excluded.session_origin_ns,media_origin_ns=excluded.media_origin_ns,rate_numerator=excluded.rate_numerator,rate_denominator=excluded.rate_denominator,finalization_error=NULL,updated_at_unix_ms=excluded.updated_at_unix_ms",
            vec![
                session_id.get().to_string().into(),
                path.clone().into(),
                i64::try_from(duration.as_nanos()).map_err(message)?.into(),
                to_i64(*byte_size)?.into(),
                to_i64(time_mapping.session_origin_ns)?.into(),
                to_i64(time_mapping.media_origin_ns)?.into(),
                i64::from(time_mapping.rate_numerator).into(),
                i64::from(time_mapping.rate_denominator).into(),
                to_i64(wall_clock_unix_ms()?)?.into(),
            ],
        )
        .await?;
        Ok(())
    }

    pub async fn save_growing_recording(
        &self,
        session_id: SessionId,
        path: &Path,
    ) -> Result<(), RagError> {
        let path = path.to_string_lossy().into_owned();
        if path.is_empty() {
            return Err(RagError::Storage(
                "recording path must be non-empty".to_owned(),
            ));
        }
        execute(
            &self.writer,
            "INSERT INTO session_recordings(session_id,state,path,container,duration_ns,byte_size,session_origin_ns,media_origin_ns,rate_numerator,rate_denominator,finalization_error,updated_at_unix_ms) VALUES(?,'growing',?,'mp4',NULL,NULL,NULL,NULL,NULL,NULL,NULL,?) ON CONFLICT(session_id) DO UPDATE SET state='growing',path=excluded.path,container='mp4',duration_ns=NULL,byte_size=NULL,session_origin_ns=NULL,media_origin_ns=NULL,rate_numerator=NULL,rate_denominator=NULL,finalization_error=NULL,updated_at_unix_ms=excluded.updated_at_unix_ms",
            vec![
                session_id.get().to_string().into(),
                path.into(),
                to_i64(wall_clock_unix_ms()?)?.into(),
            ],
        )
        .await?;
        Ok(())
    }

    pub async fn mark_recording_finalization_failed(
        &self,
        session_id: SessionId,
        error: &str,
    ) -> Result<(), RagError> {
        execute(
            &self.writer,
            "UPDATE session_recordings SET finalization_error=?,updated_at_unix_ms=? WHERE session_id=? AND state='growing'",
            vec![
                error.to_owned().into(),
                to_i64(wall_clock_unix_ms()?)?.into(),
                session_id.get().to_string().into(),
            ],
        )
        .await?;
        Ok(())
    }

    pub async fn load_recording_reference(
        &self,
        session_id: SessionId,
    ) -> Result<Option<RecordingReference>, RagError> {
        entities::session_recordings::Entity::find_by_id(session_id.get().to_string())
            .one(&self.reader)
            .await
            .map_err(storage)?
            .map(decode_recording_reference)
            .transpose()
    }

    pub async fn load_recording(
        &self,
        session_id: SessionId,
    ) -> Result<Option<SessionRecording>, RagError> {
        let row = entities::session_recordings::Entity::find_by_id(session_id.get().to_string())
            .one(&self.reader)
            .await
            .map_err(storage)?;
        match row {
            Some(row) if row.state != RecordingState::Growing => decode_recording(row).map(Some),
            _ => Ok(None),
        }
    }

    pub async fn list_recordings(&self) -> Result<Vec<SessionRecording>, RagError> {
        let rows = query_all(
            &self.reader,
            "SELECT r.* FROM session_recordings r JOIN sessions s ON s.id=r.session_id WHERE r.state!='growing' ORDER BY s.started_at_unix_ms DESC,r.session_id DESC",
            Vec::new(),
        )
        .await?;
        rows.into_iter().map(decode_recording_query).collect()
    }

    pub async fn list_recording_references(&self) -> Result<Vec<RecordingReference>, RagError> {
        let rows = query_all(
            &self.reader,
            "SELECT r.* FROM session_recordings r JOIN sessions s ON s.id=r.session_id ORDER BY s.started_at_unix_ms DESC,r.session_id DESC",
            Vec::new(),
        )
        .await?;
        rows.into_iter()
            .map(decode_recording_reference_query)
            .collect()
    }

    pub async fn replace_derived_transcript(
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
        execute(
            &self.writer,
            "INSERT INTO session_retranscriptions(session_id,model,utterances,updated_at_unix_ms) VALUES(?,?,?,?) ON CONFLICT(session_id) DO UPDATE SET model=excluded.model,utterances=excluded.utterances,updated_at_unix_ms=excluded.updated_at_unix_ms",
            vec![
                session_id.get().to_string().into(),
                model.to_owned().into(),
                serde_json::to_string(utterances).map_err(message)?.into(),
                to_i64(wall_clock_unix_ms()?)?.into(),
            ],
        )
        .await?;
        Ok(())
    }

    pub async fn load_derived_transcript(
        &self,
        session_id: SessionId,
    ) -> Result<Option<DerivedTranscript>, RagError> {
        entities::session_retranscriptions::Entity::find_by_id(session_id.get().to_string())
            .one(&self.reader)
            .await
            .map_err(storage)?
            .map(|row| {
                Ok(DerivedTranscript {
                    model: row.model,
                    utterances: serde_json::from_str(&row.utterances).map_err(message)?,
                })
            })
            .transpose()
    }

    pub async fn recording_usage(&self) -> Result<RecordingUsage, RagError> {
        let rows = entities::session_recordings::Entity::find()
            .filter(
                entities::session_recordings::Column::State
                    .is_in([RecordingState::Growing, RecordingState::Available]),
            )
            .all(&self.reader)
            .await
            .map_err(storage)?;
        let measurements = rows
            .into_iter()
            .map(|row| (row.state, row.path, row.byte_size))
            .collect::<Vec<_>>();
        let used_bytes = tokio::task::spawn_blocking(move || {
            measurements
                .into_iter()
                .map(|(state, path, saved)| {
                    if state == RecordingState::Growing {
                        path.and_then(|value| std::fs::metadata(value).ok())
                            .map_or(0, |metadata| metadata.len())
                    } else {
                        saved
                            .and_then(|value| u64::try_from(value).ok())
                            .unwrap_or(0)
                    }
                })
                .sum::<u64>()
        })
        .await
        .map_err(message)?;
        let settings = entities::recording_retention_settings::Entity::find_by_id(1)
            .one(&self.reader)
            .await
            .map_err(storage)?
            .ok_or_else(|| {
                RagError::Storage("recording retention settings are missing".to_owned())
            })?;
        Ok(RecordingUsage {
            used_bytes,
            budget_bytes: from_i64(settings.budget_bytes)?,
        })
    }

    pub async fn set_recording_budget(&self, budget_bytes: u64) -> Result<(), RagError> {
        if budget_bytes == 0 {
            return Err(RagError::Storage(
                "recording budget must be greater than zero".to_owned(),
            ));
        }
        execute(
            &self.writer,
            "UPDATE recording_retention_settings SET budget_bytes=? WHERE singleton=1",
            vec![to_i64(budget_bytes)?.into()],
        )
        .await?;
        Ok(())
    }

    async fn mark_recording_missing(
        &self,
        session_id: SessionId,
        reason: RecordingMissingReason,
    ) -> Result<(), RagError> {
        let state = match reason {
            RecordingMissingReason::Deleted => "deleted",
            RecordingMissingReason::Pruned => "pruned",
        };
        execute(
            &self.writer,
            "UPDATE session_recordings SET state=?,path=NULL,container=NULL,duration_ns=NULL,byte_size=NULL,session_origin_ns=NULL,media_origin_ns=NULL,rate_numerator=NULL,rate_denominator=NULL,finalization_error=NULL,updated_at_unix_ms=? WHERE session_id=?",
            vec![
                state.into(),
                to_i64(wall_clock_unix_ms()?)?.into(),
                session_id.get().to_string().into(),
            ],
        )
        .await?;
        Ok(())
    }

    pub async fn load_derived_view(
        &self,
        session_id: SessionId,
        kind: &str,
        model: &str,
        content_hash: &str,
    ) -> Result<Option<(String, String)>, RagError> {
        let key = (
            session_id.get().to_string(),
            kind.to_owned(),
            model.to_owned(),
            content_hash.to_owned(),
        );
        Ok(entities::derived_views::Entity::find_by_id(key)
            .one(&self.reader)
            .await
            .map_err(storage)?
            .map(|row| (row.artifact, row.usage)))
    }

    pub async fn save_derived_view(
        &self,
        session_id: SessionId,
        kind: &str,
        model: &str,
        content_hash: &str,
        artifact: &str,
        usage: &str,
    ) -> Result<(), RagError> {
        execute(
            &self.writer,
            "INSERT INTO derived_views(session_id,kind,model,content_hash,artifact,usage,created_at) VALUES(?,?,?,?,?,?,?) ON CONFLICT(session_id,kind,model,content_hash) DO NOTHING",
            vec![
                session_id.get().to_string().into(), kind.to_owned().into(), model.to_owned().into(),
                content_hash.to_owned().into(), artifact.to_owned().into(), usage.to_owned().into(),
                to_i64(wall_clock_unix_ms()?)?.into(),
            ],
        ).await?;
        Ok(())
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "the public persistence contract preserves the grounded artifact identity fields explicitly"
    )]
    pub async fn save_grounded_derived_view(
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
        normalizations: &str,
        consultations: &str,
    ) -> Result<(), RagError> {
        let transaction = self.writer.begin().await.map_err(storage)?;
        execute(
            &transaction,
            "INSERT INTO grounded_derived_views(session_id,kind,model,content_hash,artifact,usage,provider_model,grant_fingerprint,source_status,bundle,normalizations,consultations,created_at) VALUES(?,?,?,?,?,?,?,?,?,?,?,?,?) ON CONFLICT(session_id,kind,model,content_hash) DO NOTHING",
            vec![
                session_id.get().to_string().into(), kind.to_owned().into(), model.to_owned().into(),
                content_hash.to_owned().into(), artifact.to_owned().into(), usage.to_owned().into(),
                provider_model.to_owned().into(), grant_fingerprint.map(str::to_owned).into(),
                source_status.to_owned().into(), bundle.to_owned().into(),
                normalizations.to_owned().into(),
                consultations.to_owned().into(),
                to_i64(wall_clock_unix_ms()?)?.into(),
            ],
        ).await?;
        let stored = entities::grounded_derived_views::Entity::find_by_id((
            session_id.get().to_string(),
            kind.to_owned(),
            model.to_owned(),
            content_hash.to_owned(),
        ))
        .one(&transaction)
        .await
        .map_err(storage)?
        .ok_or_else(|| RagError::Storage("grounded artifact insert vanished".to_owned()))?;
        if stored.artifact != artifact
            || stored.usage != usage
            || stored.provider_model != provider_model
            || stored.grant_fingerprint.as_deref() != grant_fingerprint
            || stored.source_status != source_status
            || stored.bundle != bundle
            || stored.normalizations != normalizations
            || stored.consultations != consultations
        {
            return Err(RagError::Storage(
                "grounded derived artifact identity already contains different evidence".to_owned(),
            ));
        }
        transaction.commit().await.map_err(storage)
    }

    pub async fn load_grounded_derived_view(
        &self,
        session_id: SessionId,
        kind: &str,
        model: &str,
        content_hash: &str,
    ) -> Result<Option<GroundedDerivedView>, RagError> {
        let key = (
            session_id.get().to_string(),
            kind.to_owned(),
            model.to_owned(),
            content_hash.to_owned(),
        );
        Ok(entities::grounded_derived_views::Entity::find_by_id(key)
            .one(&self.reader)
            .await
            .map_err(storage)?
            .map(decode_grounded_view))
    }

    pub async fn load_latest_grounded_derived_view(
        &self,
        session_id: SessionId,
        kind: &str,
    ) -> Result<Option<GroundedDerivedArtifact>, RagError> {
        Ok(entities::grounded_derived_views::Entity::find()
            .filter(
                entities::grounded_derived_views::Column::SessionId
                    .eq(session_id.get().to_string()),
            )
            .filter(entities::grounded_derived_views::Column::Kind.eq(kind))
            .order_by_desc(entities::grounded_derived_views::Column::CreatedAt)
            .one(&self.reader)
            .await
            .map_err(storage)?
            .map(|row| GroundedDerivedArtifact {
                model: row.model.clone(),
                content_hash: row.content_hash.clone(),
                view: decode_grounded_view(row),
            }))
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use super::*;

    #[tokio::test]
    async fn entry_series_round_trips_and_blank_clears_it() -> Result<(), RagError> {
        let store = Store::open_in_memory().await?;
        let entry_id = EntryId::new(18);
        store.create_entry(&Entry::new(entry_id, 1, None)).await?;
        store
            .set_entry_series(entry_id, Some(" Weekly standup "))
            .await?;
        assert_eq!(
            store.list_entries().await?[0].series(),
            Some("Weekly standup")
        );
        store.set_entry_series(entry_id, Some("  ")).await?;
        assert_eq!(store.list_entries().await?[0].series(), None);
        Ok(())
    }

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

    #[tokio::test]
    async fn recording_reference_round_trips_with_explicit_identity_clock()
    -> Result<(), Box<dyn std::error::Error>> {
        let directory = tempfile::tempdir()?;
        let store = Store::open(directory.path().join("sotto.sqlite3")).await?;
        let session = recording_session(1, 1_000);
        store.save_session(&session).await?;
        let path = directory.path().join("recordings").join("1.mp4");
        std::fs::create_dir_all(path.parent().ok_or("recording parent missing")?)?;
        std::fs::write(&path, vec![7_u8; 32])?;
        let recording = available_recording(session.id(), &path, 32);
        store.save_recording(&recording).await?;

        assert_eq!(store.load_recording(session.id()).await?, Some(recording));
        assert_eq!(
            store.recording_usage().await?,
            RecordingUsage {
                used_bytes: 32,
                budget_bytes: DEFAULT_RECORDING_BUDGET_BYTES,
            }
        );
        Ok(())
    }

    #[tokio::test]
    async fn growing_recording_is_measured_reported_failed_and_deletable()
    -> Result<(), Box<dyn std::error::Error>> {
        let directory = tempfile::tempdir()?;
        let database = directory.path().join("sotto.sqlite3");
        let recordings = directory.path().join("recordings");
        std::fs::create_dir_all(&recordings)?;
        let store = Store::open(&database).await?;
        let session = recording_session(9, 9_000);
        store.save_session(&session).await?;
        let path = recordings.join("9.mp4");
        std::fs::write(&path, vec![7_u8; 19])?;

        store.save_growing_recording(session.id(), &path).await?;
        store
            .mark_recording_finalization_failed(session.id(), "injected probe failure")
            .await?;

        assert_eq!(
            store.load_recording_reference(session.id()).await?,
            Some(RecordingReference::Growing {
                session_id: session.id(),
                path: path.to_string_lossy().into_owned(),
                finalization_error: Some("injected probe failure".to_owned()),
            })
        );
        assert_eq!(store.recording_usage().await?.used_bytes, 19);
        assert!(
            store
                .remove_recording_media(session.id(), RecordingMissingReason::Deleted, &recordings,)
                .await?
        );
        assert!(!path.exists());
        assert_eq!(store.recording_usage().await?.used_bytes, 0);
        Ok(())
    }

    #[tokio::test]
    async fn recording_delete_frees_media_but_keeps_session_record()
    -> Result<(), Box<dyn std::error::Error>> {
        let directory = tempfile::tempdir()?;
        let recording_directory = directory.path().join("recordings");
        std::fs::create_dir_all(&recording_directory)?;
        let store = Store::open(directory.path().join("sotto.sqlite3")).await?;
        let session = recording_session(2, 2_000);
        store.save_session(&session).await?;
        let path = recording_directory.join("2.mp4");
        std::fs::write(&path, vec![9_u8; 24])?;
        store
            .save_recording(&available_recording(session.id(), &path, 24))
            .await?;

        assert!(
            store
                .remove_recording_media(
                    session.id(),
                    RecordingMissingReason::Deleted,
                    &recording_directory,
                )
                .await?
        );
        assert!(!path.exists());
        assert_eq!(
            store.load_session_record(session.id()).await?.id(),
            session.id(),
            "recording deletion must preserve the session record"
        );
        assert_eq!(
            store.load_recording(session.id()).await?,
            Some(SessionRecording::Missing {
                session_id: session.id(),
                reason: RecordingMissingReason::Deleted,
            })
        );
        assert_eq!(store.recording_usage().await?.used_bytes, 0);
        Ok(())
    }

    #[tokio::test]
    async fn budget_prunes_oldest_media_and_preserves_both_timelines()
    -> Result<(), Box<dyn std::error::Error>> {
        let directory = tempfile::tempdir()?;
        let recording_directory = directory.path().join("recordings");
        std::fs::create_dir_all(&recording_directory)?;
        let store = Store::open(directory.path().join("sotto.sqlite3")).await?;
        for (id, started_at) in [(3_u128, 3_000_u64), (4, 4_000)] {
            let session = recording_session(id, started_at);
            store.save_session(&session).await?;
            let path = recording_directory.join(format!("{id}.mp4"));
            std::fs::write(&path, vec![5_u8; 16])?;
            store
                .save_recording(&available_recording(session.id(), &path, 16))
                .await?;
        }
        store.set_recording_budget(16).await?;

        assert_eq!(
            store.enforce_recording_budget(&recording_directory).await?,
            vec![SessionId::new(3)]
        );
        assert_eq!(
            store.load_recording(SessionId::new(3)).await?,
            Some(SessionRecording::Missing {
                session_id: SessionId::new(3),
                reason: RecordingMissingReason::Pruned,
            })
        );
        assert!(recording_directory.join("4.mp4").exists());
        assert_eq!(
            store.load_session_record(SessionId::new(3)).await?.id(),
            SessionId::new(3),
            "pruning must preserve the oldest session"
        );
        assert_eq!(
            store.load_session_record(SessionId::new(4)).await?.id(),
            SessionId::new(4),
            "pruning must preserve the newest session"
        );
        Ok(())
    }

    #[tokio::test]
    async fn unknown_capture_target_kind_is_rejected() {
        assert!(
            parse_target_kind("future_scope").is_err(),
            "unknown persisted scope must not be fabricated as a window"
        );
    }

    async fn insert_chunk(
        store: &Store,
        id: &str,
        text: &str,
        vector: &[f32],
    ) -> Result<(), RagError> {
        execute(
            &store.writer,
            "INSERT OR IGNORE INTO documents(id,kind,title,source_path,collection_id,source_session_id,provenance_status,ingested_at,content_hash) VALUES('doc','resource_document','Fixture',NULL,NULL,NULL,'native',0,'fixture')",
            Vec::new(),
        ).await?;
        let blob = vector_blob(vector);
        execute(
            &store.writer,
            "INSERT INTO chunks VALUES(?,'doc',?,?,1,'{}',?)",
            vec![
                id.to_owned().into(),
                i64::from(id == "exact").into(),
                text.to_owned().into(),
                blob.clone().into(),
            ],
        )
        .await?;
        execute(
            &store.writer,
            "INSERT INTO vec_chunks(chunk_id,embedding) VALUES(?,?)",
            vec![id.to_owned().into(), blob.into()],
        )
        .await?;
        Ok(())
    }

    #[tokio::test]
    async fn hybrid_promotes_an_exact_project_name_over_dense_only() -> Result<(), RagError> {
        let store = Store::open_in_memory().await?;
        let query = vec![0.0; 384];
        let mut nearest = query.clone();
        nearest[0] = 0.01;
        let mut exact_name = query.clone();
        exact_name[0] = 0.02;
        insert_chunk(&store, "dense", "General platform comparison", &nearest).await?;
        insert_chunk(
            &store,
            "exact",
            "Project Quasar launch checklist",
            &exact_name,
        )
        .await
        .map_err(message)?;

        let dense = store
            .search_with_vector("Quasar", 2, &SearchFilter::default(), &query, false)
            .await?;
        let hybrid = store
            .search_with_vector("Quasar", 2, &SearchFilter::default(), &query, true)
            .await?;
        assert_eq!(dense[0].id, "dense");
        assert_eq!(hybrid[0].id, "exact");
        Ok(())
    }

    #[tokio::test]
    async fn unchanged_content_skips_embedding_work() -> Result<(), RagError> {
        let store = Store::open_in_memory().await?;
        let text = "unchanged fixture";
        execute(
            &store.writer,
            "INSERT INTO documents(id,kind,title,source_path,collection_id,source_session_id,provenance_status,ingested_at,content_hash) VALUES('existing','resource_document','Fixture',NULL,NULL,NULL,'native',0,?)",
            vec![content_hash(text).into()],
        ).await?;

        assert!(
            !store
                .ingest_text(
                    text,
                    DocumentKind::ResourceDocument,
                    IngestMetadata::default()
                )
                .await?
        );
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
    #[tokio::test]
    async fn a_tail_written_behind_the_live_clock_still_reloads_and_replays_in_id_order()
    -> Result<(), RagError> {
        let store = Store::open_in_memory().await?;
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
        store.save_session(&session).await?;
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
        store.append_events(timeline.events()).await?;

        let reloaded = store.load_session(session_id).await?;
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
            store.index_prior_meeting(session_id, None).await?,
            "indexing a recording with a behind-the-clock tail must succeed"
        );
        assert!(store.is_session_indexed(session_id).await?);
        let (prior_text, _) = store
            .render_prior_meeting(session_id)
            .await?
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
    #[tokio::test]
    async fn backfill_indexes_every_completed_recording_and_is_idempotent() -> Result<(), RagError>
    {
        let store = Store::open_in_memory().await?;
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
            store.save_session(&session).await?;
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
            store.append_events(timeline.events()).await?;
        }

        let report = store.index_missing_prior_meetings().await?;
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
        assert!(store.is_session_indexed(spoken).await?);
        assert!(!store.is_session_indexed(silent).await?);

        let repeat = store.index_missing_prior_meetings().await?;
        assert!(
            repeat.indexed.is_empty(),
            "a second pass must not re-index what is already there"
        );
        Ok(())
    }

    #[tokio::test]
    async fn completed_meeting_documents_use_neutral_finals_and_exact_session_provenance()
    -> Result<(), RagError> {
        let store = Store::open_in_memory().await?;
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
        store.save_session(&session).await?;
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
        store.append_events(timeline.events()).await?;

        let (prior_text, prior_title) = store
            .render_prior_meeting(session_id)
            .await?
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
        store
            .validate_ingest(DocumentKind::PriorMeeting, &prior_metadata)
            .await?;
        let prior_hash = content_hash(&prior_text);
        let prior_chunks = chunk_markdown(&prior_text);
        let prior_vectors = vec![vec![0.0; 384]; prior_chunks.len()];
        assert!(
            store
                .persist_document(
                    DocumentKind::PriorMeeting,
                    prior_metadata,
                    prior_hash,
                    prior_chunks,
                    prior_vectors
                )
                .await?
        );

        let note_text = "# Meeting notes\n\nDecision: ship the roadmap.";
        let note_metadata = IngestMetadata {
            title: "Roadmap notes".to_owned(),
            collection_id: Some("roadmap".to_owned()),
            source_session_id: Some(session_id),
            ..IngestMetadata::default()
        };
        store
            .validate_ingest(DocumentKind::MeetingNote, &note_metadata)
            .await?;
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
        assert!(
            store
                .persist_document(
                    DocumentKind::MeetingNote,
                    note_metadata,
                    note_hash,
                    note_chunks,
                    note_vectors
                )
                .await?
        );
        let note_receipt = store.load_local_evidence(&format!("{note_id}:0")).await?;
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
                .await
                .is_err()
        );
        Ok(())
    }

    #[tokio::test]
    async fn generic_kind_collection_and_session_filters_preserve_local_receipts()
    -> Result<(), RagError> {
        let store = Store::open_in_memory().await?;
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
        store.save_session(&session).await?;
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
            store.validate_ingest(kind, &metadata).await?;
            store
                .persist_document(
                    kind,
                    metadata,
                    content_hash(&text),
                    vec![text],
                    vec![vec![ordinal as f32 / 100.0; 384]],
                )
                .await?;
        }

        let project = store
            .search_with_vector(
                "",
                4,
                &SearchFilter {
                    kind: Some(DocumentKind::ProjectNote),
                    collection_id: Some("roadmap".to_owned()),
                    source_session_id: None,
                },
                &[0.0; 384],
                false,
            )
            .await?;
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
        let meeting_owned = store
            .search_with_vector(
                "",
                4,
                &SearchFilter {
                    kind: None,
                    collection_id: None,
                    source_session_id: Some(session_id),
                },
                &[0.0; 384],
                false,
            )
            .await?;
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
        let receipt = store.load_local_evidence(&project[0].id).await?;
        assert_eq!(receipt.kind, DocumentKind::ProjectNote);
        assert_eq!(receipt.metadata.get("label"), Some(&"value-1".to_owned()));
        Ok(())
    }

    #[tokio::test]
    async fn retained_session_policy_is_explicit_and_exclusion_is_unreachable()
    -> Result<(), RagError> {
        let store = Store::open_in_memory().await?;
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
        store.save_session(&session).await?;
        let metadata = IngestMetadata {
            title: "Retained meeting".to_owned(),
            source_session_id: Some(session_id),
            ..IngestMetadata::default()
        };
        store
            .persist_document(
                DocumentKind::PriorMeeting,
                metadata,
                "retained-hash".to_owned(),
                vec!["only retained answer [event:1]".to_owned()],
                vec![vec![0.0; 384]],
            )
            .await?;
        let filter = SearchFilter {
            kind: Some(DocumentKind::PriorMeeting),
            collection_id: None,
            source_session_id: None,
        };
        let started = std::time::Instant::now();
        assert_eq!(
            store
                .search_with_vector("", 5, &filter, &[0.0; 384], false)
                .await?
                .len(),
            1,
            "an indexed recording is searchable with no further opt-in"
        );
        eprintln!("bounded prior-meeting retrieval: {:?}", started.elapsed());
        assert!(store.is_session_indexed(session_id).await?);

        // Deleting the recording is the only way out of the index, and it takes the vectors too.
        execute(
            &store.writer,
            "DELETE FROM sessions WHERE id=?",
            vec![session_id.get().to_string().into()],
        )
        .await?;
        assert!(!store.is_session_indexed(session_id).await?);
        Ok(())
    }

    #[tokio::test]
    async fn scoped_search_exhausts_higher_ranked_nonmatches_before_returning_match()
    -> Result<(), RagError> {
        let store = Store::open_in_memory().await?;
        for ordinal in 0..5 {
            let document_id = format!("nonmatch-doc-{ordinal}");
            let chunk_id = format!("nonmatch-{ordinal}");
            execute(
                    &store.writer,
                    "INSERT INTO documents(id,kind,title,source_path,collection_id,source_session_id,provenance_status,ingested_at,content_hash) VALUES(?,'resource_document','Other',NULL,'other',NULL,'native',0,?)",
                    vec![document_id.clone().into(), format!("nonmatch-hash-{ordinal}").into()],
                ).await?;
            let mut vector = vec![0.0; 384];
            vector[0] = ordinal as f32 / 100.0;
            let blob = vector_blob(&vector);
            execute(
                &store.writer,
                "INSERT INTO chunks VALUES(?,?,0,'higher ranked',2,'{}',?)",
                vec![
                    chunk_id.clone().into(),
                    document_id.into(),
                    blob.clone().into(),
                ],
            )
            .await?;
            execute(
                &store.writer,
                "INSERT INTO vec_chunks(chunk_id,embedding) VALUES(?,?)",
                vec![chunk_id.into(), blob.into()],
            )
            .await?;
        }
        execute(
            &store.writer,
            "INSERT INTO documents(id,kind,title,source_path,collection_id,source_session_id,provenance_status,ingested_at,content_hash) VALUES('match-doc','project_note','Match',NULL,'wanted',NULL,'native',0,'match-hash')",
            Vec::new(),
        ).await?;
        let blob = vector_blob(&vec![0.2; 384]);
        execute(
            &store.writer,
            "INSERT INTO chunks VALUES('match','match-doc',0,'scoped result',2,'{}',?)",
            vec![blob.clone().into()],
        )
        .await?;
        execute(
            &store.writer,
            "INSERT INTO vec_chunks(chunk_id,embedding) VALUES('match',?)",
            vec![blob.into()],
        )
        .await?;

        let result = store
            .search_with_vector(
                "",
                1,
                &SearchFilter {
                    kind: Some(DocumentKind::ProjectNote),
                    collection_id: Some("wanted".to_owned()),
                    source_session_id: None,
                },
                &[0.0; 384],
                false,
            )
            .await?;
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].id, "match");
        Ok(())
    }

    #[tokio::test]
    #[ignore = "performance check requires the fastembed model cache"]
    async fn warm_query_embedding_and_hybrid_search_fit_latency_budget() -> Result<(), RagError> {
        let store = Store::open_in_memory().await?;
        store.embed(vec!["warm up".to_owned()]).await?;
        let started = Instant::now();
        let _results = store
            .search_filtered("project launch", 5, &SearchFilter::default())
            .await?;
        assert!(
            started.elapsed() < Duration::from_millis(50),
            "warm query took {:?}",
            started.elapsed()
        );
        Ok(())
    }
}
