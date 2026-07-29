use std::{
    collections::{BTreeMap, HashMap},
    hash::{Hash, Hasher},
    path::Path,
    sync::Mutex,
    time::{SystemTime, UNIX_EPOCH},
};

use fastembed::{EmbeddingModel, InitOptions, TextEmbedding};
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::{Value, json};
use sotto_core::{
    BoxFuture, CaptureTarget, Chunk, RagError, Retriever, Session, SessionId, TargetKind,
    TimelineEvent,
};

use crate::schema::{migrate, storage};

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
    connection: Mutex<Connection>,
    embedding: Mutex<Option<TextEmbedding>>,
}

impl Store {
    pub fn open(path: impl AsRef<Path>) -> Result<Self, RagError> {
        register_sqlite_vec();
        let mut connection = Connection::open(path).map_err(storage)?;
        migrate(&mut connection)?;
        Ok(Self {
            connection: Mutex::new(connection),
            embedding: Mutex::new(None),
        })
    }

    pub fn open_in_memory() -> Result<Self, RagError> {
        register_sqlite_vec();
        let mut connection = Connection::open_in_memory().map_err(storage)?;
        migrate(&mut connection)?;
        Ok(Self {
            connection: Mutex::new(connection),
            embedding: Mutex::new(None),
        })
    }

    pub fn save_session(&self, session: &Session) -> Result<(), RagError> {
        let target = session.capture_target();
        self.connection.lock().map_err(poisoned)?.execute(
            "INSERT INTO sessions VALUES(?1,?2,?3,?4,?5,?6,?7,?8) ON CONFLICT(id) DO UPDATE SET ended_at_unix_ms=excluded.ended_at_unix_ms",
            params![session.id().get().to_string(), session.started_at_unix_ms(),
                session.ended_at_unix_ms(), target.bundle_id, target.display_name,
                target.window_title, target_kind(target.kind), target.audio_scoped],
        ).map_err(storage)?;
        Ok(())
    }

    /// Inserts only; duplicate identities fail rather than mutating the append-only log.
    pub fn append_events(&self, events: &[TimelineEvent]) -> Result<(), RagError> {
        let mut connection = self.connection.lock().map_err(poisoned)?;
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
        let connection = self.connection.lock().map_err(poisoned)?;
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
        self.connection.lock().map_err(poisoned)?.query_row(
            "SELECT started_at_unix_ms,ended_at_unix_ms,capture_target_bundle_id,capture_target_display_name,capture_target_window_title,capture_target_kind,capture_target_audio_scoped FROM sessions WHERE id=?1",
            [id.get().to_string()], |row| {
                let kind: String = row.get(5)?;
                let target = CaptureTarget { bundle_id: row.get(2)?, display_name: row.get(3)?,
                    window_title: row.get(4)?, kind: if kind == "application" { TargetKind::Application } else { TargetKind::Window },
                    audio_scoped: row.get(6)? };
                let mut session = Session::new(id, target, row.get(0)?);
                if let Some(ended) = row.get(1)? { session.end(ended); }
                Ok(session)
            }).optional().map_err(storage)?.ok_or_else(|| RagError::NotFound { id: id.get().to_string() })
    }

    pub fn ingest_text(
        &self,
        text: &str,
        kind: DocumentKind,
        metadata: IngestMetadata,
    ) -> Result<bool, RagError> {
        let hash = content_hash(text);
        if self
            .connection
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
        let mut connection = self.connection.lock().map_err(poisoned)?;
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
        let connection = self.connection.lock().map_err(poisoned)?;
        let mut scores = HashMap::<String, f64>::new();
        let mut vector = connection.prepare("SELECT chunk_id FROM vec_chunks WHERE embedding MATCH ?1 AND k=?2 ORDER BY distance").map_err(storage)?;
        for (rank, row) in vector
            .query_map(
                params![vector_blob(&query_vector), k.saturating_mul(4)],
                |row| row.get::<_, String>(0),
            )
            .map_err(storage)?
            .enumerate()
        {
            scores.insert(row.map_err(storage)?, rank_score(rank));
        }
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
