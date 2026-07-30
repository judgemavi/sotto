use std::{
    collections::{BTreeMap, HashMap},
    hash::{Hash, Hasher},
    io,
    path::Path,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::{SystemTime, UNIX_EPOCH},
};

use fastembed::{EmbeddingModel, InitOptions, TextEmbedding};
use rusqlite::{Connection, OpenFlags, OptionalExtension, params, types::Type};
use serde_json::{Value, json};
use sotto_core::{
    BoxFuture, CaptureTarget, Chunk, PersistenceSink, RagError, Retriever, Session, SessionId,
    TargetKind, TimelineEvent,
};

use crate::schema::{configure, migrate, storage};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DocumentKind {
    Battlecard,
    ProductDocument,
    AccountNote,
    PastCall,
    Recap,
}

impl DocumentKind {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Battlecard => "battlecard",
            Self::ProductDocument => "product_document",
            Self::AccountNote => "account_note",
            Self::PastCall => "past_call",
            Self::Recap => "recap",
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct IngestMetadata {
    pub title: String,
    pub source_path: Option<String>,
    pub account_id: Option<String>,
    pub fields: BTreeMap<String, String>,
}

#[derive(Clone, Debug, Default)]
pub struct SearchFilter {
    pub kind: Option<DocumentKind>,
    pub account_id: Option<String>,
}

pub struct Store {
    writer: Mutex<Connection>,
    reader: Mutex<Connection>,
    embedding: Mutex<Option<TextEmbedding>>,
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
        self.writer.lock().map_err(poisoned)?.execute(
            "INSERT INTO sessions VALUES(?1,?2,?3,?4,?5,?6,?7,?8) ON CONFLICT(id) DO UPDATE SET ended_at_unix_ms=excluded.ended_at_unix_ms",
            params![session.id().get().to_string(), session.started_at_unix_ms(),
                session.ended_at_unix_ms(), target.bundle_id, target.display_name,
                target.window_title, target_kind(target.kind), target.audio_scoped],
        ).map_err(storage)?;
        Ok(())
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

    pub fn load_session(&self, session_id: SessionId) -> Result<Vec<TimelineEvent>, RagError> {
        let connection = self.reader.lock().map_err(poisoned)?;
        let mut statement = connection
            .prepare(
                "SELECT id,ts,supersedes,payload FROM events WHERE session_id=?1 ORDER BY ts,id",
            )
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

    pub fn ingest_text(
        &self,
        text: &str,
        kind: DocumentKind,
        metadata: IngestMetadata,
    ) -> Result<bool, RagError> {
        let hash = content_hash(text);
        if self
            .reader
            .lock()
            .map_err(poisoned)?
            .query_row(
                "SELECT 1 FROM documents WHERE content_hash=?1",
                [&hash],
                |_| Ok(()),
            )
            .optional()
            .map_err(storage)?
            .is_some()
        {
            return Ok(false);
        }
        let chunks = chunk_markdown(text);
        let embeddings = self.embed(chunks.iter().map(String::as_str).collect())?;
        let document_id = format!("doc-{hash}");
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(message)?
            .as_secs();
        let mut connection = self.writer.lock().map_err(poisoned)?;
        let transaction = connection.transaction().map_err(storage)?;
        transaction
            .execute(
                "INSERT INTO documents VALUES(?1,?2,?3,?4,?5,?6,?7)",
                params![
                    document_id,
                    kind.as_str(),
                    metadata.title,
                    metadata.source_path,
                    metadata.account_id,
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
        let mut vector = connection.prepare("SELECT chunk_id FROM vec_chunks WHERE embedding MATCH ?1 AND k=?2 ORDER BY distance").map_err(storage)?;
        for (rank, row) in vector
            .query_map(
                params![vector_blob(query_vector), k.saturating_mul(4)],
                |row| row.get::<_, String>(0),
            )
            .map_err(storage)?
            .enumerate()
        {
            scores.insert(row.map_err(storage)?, rank_score(rank));
        }
        if include_keyword {
            let mut keyword = connection.prepare("SELECT c.id FROM chunks_fts f JOIN chunks c ON c.rowid=f.rowid WHERE chunks_fts MATCH ?1 ORDER BY bm25(chunks_fts) LIMIT ?2").map_err(storage)?;
            for (rank, row) in keyword
                .query_map(params![query, k.saturating_mul(4)], |row| {
                    row.get::<_, String>(0)
                })
                .map_err(storage)?
                .enumerate()
            {
                *scores.entry(row.map_err(storage)?).or_default() += rank_score(rank);
            }
        }
        let mut ranked: Vec<_> = scores.into_iter().collect();
        ranked.sort_by(|a, b| b.1.total_cmp(&a.1));
        let mut output = Vec::new();
        let mut fetch = connection.prepare("SELECT c.text,d.title,d.kind,d.account_id,c.metadata FROM chunks c JOIN documents d ON d.id=c.doc_id WHERE c.id=?1").map_err(storage)?;
        for (id, _) in ranked {
            let (text, source, kind, account, fields): (
                String,
                String,
                String,
                Option<String>,
                String,
            ) = fetch
                .query_row([&id], |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                    ))
                })
                .map_err(storage)?;
            if filter.kind.is_some_and(|value| value.as_str() != kind)
                || filter
                    .account_id
                    .as_ref()
                    .is_some_and(|value| account.as_ref() != Some(value))
            {
                continue;
            }
            let mut metadata =
                serde_json::from_str::<BTreeMap<String, String>>(&fields).map_err(message)?;
            metadata.insert("kind".to_owned(), kind);
            if let Some(value) = account {
                metadata.insert("account_id".to_owned(), value);
            }
            output.push(Chunk {
                id,
                text,
                source,
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
    }
}

fn parse_target_kind(kind: &str) -> rusqlite::Result<TargetKind> {
    match kind {
        "application" => Ok(TargetKind::Application),
        "window" => Ok(TargetKind::Window),
        "display" => Ok(TargetKind::Display),
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
                "INSERT OR IGNORE INTO documents VALUES('doc','battlecard','Fixture',NULL,NULL,0,'fixture')",
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
    fn hybrid_promotes_an_exact_competitor_name_over_dense_only() -> Result<(), RagError> {
        let store = Store::open_in_memory()?;
        let query = vec![0.0; 384];
        let mut nearest = query.clone();
        nearest[0] = 0.01;
        let mut competitor = query.clone();
        competitor[0] = 0.02;
        insert_chunk(&store, "dense", "General platform comparison", &nearest)?;
        insert_chunk(
            &store,
            "exact",
            "How to position against QuasarCRM",
            &competitor,
        )?;

        let dense =
            store.search_with_vector("QuasarCRM", 2, &SearchFilter::default(), &query, false)?;
        let hybrid =
            store.search_with_vector("QuasarCRM", 2, &SearchFilter::default(), &query, true)?;
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
                "INSERT INTO documents VALUES('existing','product_document','Fixture',NULL,NULL,0,?1)",
                [content_hash(text)],
            )
            .map_err(storage)?;

        assert!(!store.ingest_text(
            text,
            DocumentKind::ProductDocument,
            IngestMetadata::default()
        )?);
        assert!(store.embedding.lock().map_err(poisoned)?.is_none());
        Ok(())
    }

    #[test]
    #[ignore = "performance check requires the fastembed model cache"]
    fn warm_query_embedding_and_hybrid_search_fit_latency_budget() -> Result<(), RagError> {
        let store = Store::open_in_memory()?;
        store.embed(vec!["warm up"])?;
        let started = Instant::now();
        let _results = store.search_filtered("competitor pricing", 5, &SearchFilter::default())?;
        assert!(
            started.elapsed() < Duration::from_millis(50),
            "warm query took {:?}",
            started.elapsed()
        );
        Ok(())
    }
}
