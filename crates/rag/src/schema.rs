use rusqlite::{Connection, OptionalExtension};
use sotto_core::{RagError, SQLITE_SCHEMA};

pub(crate) const SCHEMA_VERSION: u32 = 13;

/// Entries are the document-bearing library objects above captured sessions.
///
/// `entry_sessions.session_id` is unique because a recording belongs to exactly one entry for its
/// whole life. The relation is separate from `sessions` so the captured-fact row is not rebuilt or
/// broadened. Entry deletion deliberately lifts the store's existing session cascade in code;
/// deleting a single session merely cascades this relation and leaves the entry intact.
const ENTRY_SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS entries (
 id TEXT PRIMARY KEY NOT NULL,
 created_at_unix_ms INTEGER NOT NULL,
 title TEXT CHECK(title IS NULL OR length(trim(title))>0)
);
CREATE TABLE IF NOT EXISTS entry_sessions (
 session_id TEXT PRIMARY KEY REFERENCES sessions(id) ON DELETE CASCADE,
 entry_id TEXT NOT NULL REFERENCES entries(id) ON DELETE CASCADE
);
CREATE INDEX IF NOT EXISTS entry_sessions_entry_idx ON entry_sessions(entry_id,session_id);
"#;

/// A person's chosen name for a recording, held in its own table rather than as a column on
/// `sessions`.
///
/// The separation is the point. Every column on `sessions` beginning `capture_target_` is a
/// captured fact about what the OS content filter was built from, and a rename must never touch
/// one of them. A row here is a label over the recording: absent until someone chooses a name,
/// removed again when they clear it, and cascading away with the session it names.
///
/// The `CHECK` is the storage-level half of `RecordingTitle`'s guarantee — a blank title cannot be
/// persisted even by a caller that skipped the constructor.
const SESSION_TITLE_SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS session_titles (
 session_id TEXT PRIMARY KEY REFERENCES sessions(id) ON DELETE CASCADE,
 title TEXT NOT NULL CHECK(length(trim(title))>0),
 updated_at_unix_ms INTEGER NOT NULL
);
"#;

const RETRANSCRIPTION_SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS session_retranscriptions (
 session_id TEXT PRIMARY KEY REFERENCES sessions(id) ON DELETE CASCADE,
 model TEXT NOT NULL,
 utterances TEXT NOT NULL,
 updated_at_unix_ms INTEGER NOT NULL
);
"#;

const RECORDING_RETENTION_SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS session_recordings (
 session_id TEXT PRIMARY KEY REFERENCES sessions(id) ON DELETE CASCADE,
 state TEXT NOT NULL CHECK(state IN ('growing','available','deleted','pruned')),
 path TEXT,
 container TEXT,
 duration_ns INTEGER,
 byte_size INTEGER,
 session_origin_ns INTEGER,
 media_origin_ns INTEGER,
 rate_numerator INTEGER,
 rate_denominator INTEGER,
 finalization_error TEXT,
 updated_at_unix_ms INTEGER NOT NULL,
 CHECK(
   (state='growing' AND path IS NOT NULL AND container='mp4' AND duration_ns IS NULL
    AND byte_size IS NULL AND session_origin_ns IS NULL AND media_origin_ns IS NULL
    AND rate_numerator IS NULL AND rate_denominator IS NULL)
   OR
   (state='available' AND path IS NOT NULL AND container='mp4' AND duration_ns IS NOT NULL
    AND byte_size IS NOT NULL AND session_origin_ns IS NOT NULL AND media_origin_ns IS NOT NULL
    AND rate_numerator=1 AND rate_denominator=1)
   OR
   (state IN ('deleted','pruned') AND path IS NULL AND container IS NULL
    AND duration_ns IS NULL AND byte_size IS NULL AND session_origin_ns IS NULL
    AND media_origin_ns IS NULL AND rate_numerator IS NULL AND rate_denominator IS NULL)
 )
);
CREATE INDEX IF NOT EXISTS session_recordings_state_age_idx
 ON session_recordings(state,updated_at_unix_ms,session_id);
CREATE TABLE IF NOT EXISTS recording_retention_settings (
 singleton INTEGER PRIMARY KEY CHECK(singleton=1),
 budget_bytes INTEGER NOT NULL CHECK(budget_bytes>0)
);
INSERT OR IGNORE INTO recording_retention_settings(singleton,budget_bytes)
 VALUES(1,20000000000);
"#;

const RAG_SCHEMA: &str = r#"
CREATE TABLE documents (
 id TEXT PRIMARY KEY, kind TEXT NOT NULL, title TEXT NOT NULL, source_path TEXT,
 collection_id TEXT, source_session_id TEXT REFERENCES sessions(id) ON DELETE CASCADE,
 provenance_status TEXT NOT NULL DEFAULT 'native', ingested_at INTEGER NOT NULL,
 content_hash TEXT NOT NULL,
 CHECK(kind IN ('resource_document','project_note','meeting_note','prior_meeting')),
 CHECK(provenance_status IN ('native','legacy_unlinked')),
 CHECK((kind IN ('resource_document','project_note') AND source_session_id IS NULL) OR
       (kind IN ('meeting_note','prior_meeting') AND source_session_id IS NOT NULL))
);
CREATE TABLE chunks (
 id TEXT PRIMARY KEY, doc_id TEXT NOT NULL REFERENCES documents(id) ON DELETE CASCADE,
 ordinal INTEGER NOT NULL, text TEXT NOT NULL, token_count INTEGER NOT NULL,
 metadata TEXT NOT NULL, embedding BLOB NOT NULL, UNIQUE(doc_id, ordinal)
);
CREATE VIRTUAL TABLE chunks_fts USING fts5(text, content='chunks', content_rowid='rowid');
CREATE TRIGGER chunks_ai AFTER INSERT ON chunks BEGIN
 INSERT INTO chunks_fts(rowid,text) VALUES(new.rowid,new.text);
END;
CREATE TRIGGER chunks_ad AFTER DELETE ON chunks BEGIN
 INSERT INTO chunks_fts(chunks_fts,rowid,text) VALUES('delete',old.rowid,old.text);
END;
CREATE VIRTUAL TABLE vec_chunks USING vec0(chunk_id TEXT PRIMARY KEY, embedding float[384]);
CREATE INDEX documents_kind_collection_idx ON documents(kind, collection_id);
CREATE UNIQUE INDEX documents_identity_idx ON documents(
 kind, content_hash, IFNULL(collection_id,''), IFNULL(source_session_id,''), IFNULL(source_path,'')
);
CREATE INDEX chunks_doc_id_idx ON chunks(doc_id);
"#;

const LOCAL_KNOWLEDGE_V5_SCHEMA: &str = r#"
CREATE TABLE documents_v5 (
 id TEXT PRIMARY KEY, kind TEXT NOT NULL, title TEXT NOT NULL, source_path TEXT,
 collection_id TEXT, source_session_id TEXT REFERENCES sessions(id) ON DELETE CASCADE,
 provenance_status TEXT NOT NULL DEFAULT 'native', ingested_at INTEGER NOT NULL,
 content_hash TEXT NOT NULL,
 CHECK(kind IN ('resource_document','project_note','meeting_note','prior_meeting')),
 CHECK(provenance_status IN ('native','legacy_unlinked')),
 CHECK((kind IN ('resource_document','project_note') AND source_session_id IS NULL) OR
       (kind IN ('meeting_note','prior_meeting') AND source_session_id IS NOT NULL))
);
INSERT INTO documents_v5(
 id,kind,title,source_path,collection_id,source_session_id,provenance_status,ingested_at,content_hash
)
SELECT id,'resource_document',title,source_path,account_id,NULL,
 CASE WHEN kind IN ('account_note','past_call','recap')
      THEN 'legacy_unlinked' ELSE 'native' END,
 ingested_at,content_hash
FROM documents;
UPDATE chunks
SET metadata=json_set(
 CASE WHEN json_valid(metadata) AND json_type(metadata)='object' THEN metadata ELSE '{}' END,
 '$.provenance_status','legacy_unlinked'
)
WHERE doc_id IN (
 SELECT id FROM documents WHERE kind IN ('account_note','past_call','recap')
);
DROP INDEX documents_kind_account_idx;
DROP TABLE accounts;
DROP TABLE documents;
ALTER TABLE documents_v5 RENAME TO documents;
CREATE INDEX documents_kind_collection_idx ON documents(kind, collection_id);
CREATE UNIQUE INDEX documents_identity_idx ON documents(
 kind, content_hash, IFNULL(collection_id,''), IFNULL(source_session_id,''), IFNULL(source_path,'')
);
"#;

// Used for both fresh databases and v2 -> v3 migrations. Keeping one definition prevents
// those two installation paths from drifting into subtly different schemas.
const DERIVED_VIEWS_SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS derived_views (
 session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
 kind TEXT NOT NULL, model TEXT NOT NULL, content_hash TEXT NOT NULL,
 artifact TEXT NOT NULL, usage TEXT NOT NULL, created_at INTEGER NOT NULL,
 PRIMARY KEY(session_id, kind, model, content_hash)
);
CREATE INDEX IF NOT EXISTS derived_views_session_kind_idx ON derived_views(session_id, kind);
"#;

const GROUNDED_EVIDENCE_SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS grounded_derived_views (
 session_id TEXT NOT NULL, kind TEXT NOT NULL, model TEXT NOT NULL, content_hash TEXT NOT NULL,
 artifact TEXT NOT NULL, usage TEXT NOT NULL, provider_model TEXT NOT NULL,
 grant_fingerprint TEXT,
 source_status TEXT NOT NULL CHECK(source_status IN ('not_selected','available','unavailable')),
 bundle TEXT NOT NULL, created_at INTEGER NOT NULL,
 PRIMARY KEY(session_id, kind, model, content_hash),
 FOREIGN KEY(session_id) REFERENCES sessions(id) ON DELETE CASCADE
);
CREATE INDEX IF NOT EXISTS grounded_derived_views_session_kind_idx
 ON grounded_derived_views(session_id,kind);
"#;

pub(crate) fn migrate(connection: &mut Connection) -> Result<(), RagError> {
    configure(connection)?;
    let version = connection
        .query_row("PRAGMA user_version", [], |row| row.get::<_, u32>(0))
        .map_err(storage)?;
    if version > SCHEMA_VERSION {
        return Err(RagError::Migration {
            from: version,
            to: SCHEMA_VERSION,
        });
    }
    if (1..=4).contains(&version) {
        let unknown_kind = connection
            .query_row(
                "SELECT kind FROM documents WHERE kind NOT IN ('battlecard','product_document','account_note','past_call','recap') ORDER BY kind LIMIT 1",
                [],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .map_err(storage)?;
        if let Some(kind) = unknown_kind {
            return Err(RagError::Storage(format!(
                "cannot migrate unknown local document kind {kind:?}"
            )));
        }
    }
    if version == 0 {
        let transaction = connection.transaction().map_err(storage)?;
        transaction.execute_batch(SQLITE_SCHEMA).map_err(storage)?;
        transaction.execute_batch(RAG_SCHEMA).map_err(storage)?;
        transaction
            .execute_batch(DERIVED_VIEWS_SCHEMA)
            .map_err(storage)?;
        transaction
            .execute_batch(GROUNDED_EVIDENCE_SCHEMA)
            .map_err(storage)?;
        transaction
            .execute_batch(RECORDING_RETENTION_SCHEMA)
            .map_err(storage)?;
        transaction
            .execute_batch(RETRANSCRIPTION_SCHEMA)
            .map_err(storage)?;
        transaction
            .execute_batch(SESSION_TITLE_SCHEMA)
            .map_err(storage)?;
        transaction.execute_batch(ENTRY_SCHEMA).map_err(storage)?;
        transaction
            .pragma_update(None, "user_version", SCHEMA_VERSION)
            .map_err(storage)?;
        transaction.commit().map_err(storage)?;
    } else if version == 1 {
        let transaction = connection.transaction().map_err(storage)?;
        transaction
            .execute_batch(
                "CREATE INDEX IF NOT EXISTS documents_kind_account_idx ON documents(kind, account_id);\
                 CREATE INDEX IF NOT EXISTS chunks_doc_id_idx ON chunks(doc_id);",
            )
            .map_err(storage)?;
        transaction
            .pragma_update(None, "user_version", 2)
            .map_err(storage)?;
        transaction.commit().map_err(storage)?;
    }
    if version <= 2 && version != 0 {
        let transaction = connection.transaction().map_err(storage)?;
        transaction
            .execute_batch(DERIVED_VIEWS_SCHEMA)
            .map_err(storage)?;
        transaction
            .pragma_update(None, "user_version", 3)
            .map_err(storage)?;
        transaction.commit().map_err(storage)?;
    }
    if version <= 3 && version != 0 {
        let transaction = connection.transaction().map_err(storage)?;
        transaction
            .execute_batch(GROUNDED_EVIDENCE_SCHEMA)
            .map_err(storage)?;
        transaction
            .pragma_update(None, "user_version", 4)
            .map_err(storage)?;
        transaction.commit().map_err(storage)?;
    }
    if version <= 4 && version != 0 {
        connection
            .execute_batch("PRAGMA foreign_keys = OFF;")
            .map_err(storage)?;
        let migration = (|| {
            let transaction = connection.transaction().map_err(storage)?;
            transaction
                .execute_batch(LOCAL_KNOWLEDGE_V5_SCHEMA)
                .map_err(storage)?;
            transaction
                .execute_batch(RECORDING_RETENTION_SCHEMA)
                .map_err(storage)?;
            let foreign_key_violation = transaction
                .query_row("PRAGMA foreign_key_check", [], |_| Ok(()))
                .optional()
                .map_err(storage)?;
            if foreign_key_violation.is_some() {
                return Err(RagError::Storage(
                    "local knowledge migration violated a foreign key".to_owned(),
                ));
            }
            transaction
                .pragma_update(None, "user_version", SCHEMA_VERSION)
                .map_err(storage)?;
            transaction.commit().map_err(storage)
        })();
        connection
            .execute_batch("PRAGMA foreign_keys = ON;")
            .map_err(storage)?;
        migration?;
    }
    if version == 5 {
        let transaction = connection.transaction().map_err(storage)?;
        transaction
            .execute_batch(RECORDING_RETENTION_SCHEMA)
            .map_err(storage)?;
        transaction
            .pragma_update(None, "user_version", SCHEMA_VERSION)
            .map_err(storage)?;
        transaction.commit().map_err(storage)?;
    }
    if (1..=7).contains(&version) {
        let transaction = connection.transaction().map_err(storage)?;
        transaction
            .execute_batch(RETRANSCRIPTION_SCHEMA)
            .map_err(storage)?;
        transaction
            .pragma_update(None, "user_version", SCHEMA_VERSION)
            .map_err(storage)?;
        transaction.commit().map_err(storage)?;
    }
    if version == 6 {
        let transaction = connection.transaction().map_err(storage)?;
        transaction
            .execute_batch(RECORDING_RETENTION_SCHEMA)
            .map_err(storage)?;
        transaction
            .pragma_update(None, "user_version", SCHEMA_VERSION)
            .map_err(storage)?;
        transaction.commit().map_err(storage)?;
    }
    if version == 8 {
        let transaction = connection.transaction().map_err(storage)?;
        transaction
            .execute_batch(
                "ALTER TABLE session_recordings RENAME TO session_recordings_v8;\
                 CREATE TABLE session_recordings (\
                  session_id TEXT PRIMARY KEY REFERENCES sessions(id) ON DELETE CASCADE,\
                  state TEXT NOT NULL CHECK(state IN ('growing','available','deleted','pruned')),\
                  path TEXT,container TEXT,duration_ns INTEGER,byte_size INTEGER,\
                  session_origin_ns INTEGER,media_origin_ns INTEGER,rate_numerator INTEGER,\
                  rate_denominator INTEGER,finalization_error TEXT,updated_at_unix_ms INTEGER NOT NULL,\
                  CHECK((state='growing' AND path IS NOT NULL AND container='mp4'\
                    AND duration_ns IS NULL AND byte_size IS NULL AND session_origin_ns IS NULL\
                    AND media_origin_ns IS NULL AND rate_numerator IS NULL AND rate_denominator IS NULL)\
                   OR (state='available' AND path IS NOT NULL AND container='mp4'\
                    AND duration_ns IS NOT NULL AND byte_size IS NOT NULL\
                    AND session_origin_ns IS NOT NULL AND media_origin_ns IS NOT NULL\
                    AND rate_numerator=1 AND rate_denominator=1)\
                   OR (state IN ('deleted','pruned') AND path IS NULL AND container IS NULL\
                    AND duration_ns IS NULL AND byte_size IS NULL AND session_origin_ns IS NULL\
                    AND media_origin_ns IS NULL AND rate_numerator IS NULL AND rate_denominator IS NULL)));\
                 INSERT INTO session_recordings(session_id,state,path,container,duration_ns,byte_size,\
                  session_origin_ns,media_origin_ns,rate_numerator,rate_denominator,updated_at_unix_ms)\
                 SELECT session_id,state,path,container,duration_ns,byte_size,session_origin_ns,\
                  media_origin_ns,rate_numerator,rate_denominator,updated_at_unix_ms FROM session_recordings_v8;\
                 DROP TABLE session_recordings_v8;\
                 CREATE INDEX session_recordings_state_age_idx\
                  ON session_recordings(state,updated_at_unix_ms,session_id);",
            )
            .map_err(storage)?;
        transaction
            .pragma_update(None, "user_version", SCHEMA_VERSION)
            .map_err(storage)?;
        transaction.commit().map_err(storage)?;
    }
    if (1..=9).contains(&version) {
        connection
            .execute_batch("PRAGMA foreign_keys = OFF;")
            .map_err(storage)?;
        let migration = (|| {
            let transaction = connection.transaction().map_err(storage)?;
            transaction
                .execute_batch(
                    "CREATE TABLE sessions_v10 (\
                      id TEXT PRIMARY KEY NOT NULL,\
                      started_at_unix_ms INTEGER NOT NULL,\
                      ended_at_unix_ms INTEGER,\
                      capture_target_bundle_id TEXT,\
                      capture_target_display_name TEXT NOT NULL,\
                      capture_target_window_title TEXT,\
                      capture_target_kind TEXT NOT NULL CHECK (\
                        capture_target_kind IN ('application','window','display','microphone')\
                      ),\
                      capture_target_audio_scoped INTEGER NOT NULL CHECK (\
                        capture_target_audio_scoped IN (0,1)\
                      )\
                    );\
                    INSERT INTO sessions_v10 SELECT * FROM sessions;\
                    DROP TABLE sessions;\
                    ALTER TABLE sessions_v10 RENAME TO sessions;",
                )
                .map_err(storage)?;
            let foreign_key_violation = transaction
                .query_row("PRAGMA foreign_key_check", [], |_| Ok(()))
                .optional()
                .map_err(storage)?;
            if foreign_key_violation.is_some() {
                return Err(RagError::Storage(
                    "microphone capture-scope migration violated a foreign key".to_owned(),
                ));
            }
            transaction
                .pragma_update(None, "user_version", SCHEMA_VERSION)
                .map_err(storage)?;
            transaction.commit().map_err(storage)
        })();
        connection
            .execute_batch("PRAGMA foreign_keys = ON;")
            .map_err(storage)?;
        migration?;
    }
    // Adding a place for chosen names touches nothing that was captured: existing sessions keep
    // every `capture_target_` column exactly as recorded and simply have no title row yet, which
    // is the same state a freshly captured recording is in.
    if (1..=10).contains(&version) {
        let transaction = connection.transaction().map_err(storage)?;
        transaction
            .execute_batch(SESSION_TITLE_SCHEMA)
            .map_err(storage)?;
        transaction
            .pragma_update(None, "user_version", SCHEMA_VERSION)
            .map_err(storage)?;
        transaction.commit().map_err(storage)?;
    }
    // v12 retires the per-recording search opt-in. Every completed recording is indexed, so the
    // presence of its `prior_meeting` document is the only state there is, and a policy row that
    // could disagree with the index is worse than no row at all. Dropping the table is safe in
    // both directions: nothing reads it any more, and the documents it used to gate are untouched
    // — a library that was fully opted out simply has recordings to backfill, which
    // `Store::index_missing_prior_meetings` does.
    if (1..=11).contains(&version) {
        let transaction = connection.transaction().map_err(storage)?;
        transaction
            .execute_batch("DROP TABLE IF EXISTS session_search_policy;")
            .map_err(storage)?;
        transaction
            .pragma_update(None, "user_version", SCHEMA_VERSION)
            .map_err(storage)?;
        transaction.commit().map_err(storage)?;
    }
    // Every existing recording gains one mechanical entry. The ids intentionally match only for
    // this migration; they remain distinct domain types and future prepared entries have their
    // own identities. Title adoption waits for T085's title-persistence ownership to close.
    //
    // The same pass also drops any `meeting_notes.v2` rows. Retiring that read path made them
    // unreadable dead data: `load_latest_grounded_notes_status` would silently return `Ok(None)`
    // for a session whose only artifact was one of these, which is worse than having no cached
    // summary at all. Deleting them is honest about the state and simply asks for a regenerate.
    if (1..=12).contains(&version) {
        let transaction = connection.transaction().map_err(storage)?;
        transaction.execute_batch(ENTRY_SCHEMA).map_err(storage)?;
        transaction
            .execute_batch(
                "INSERT OR IGNORE INTO entries(id,created_at_unix_ms,title)\
                 SELECT id,started_at_unix_ms,NULL FROM sessions;\
                 INSERT OR IGNORE INTO entry_sessions(session_id,entry_id)\
                 SELECT id,id FROM sessions;\
                 DELETE FROM grounded_derived_views WHERE kind='meeting_notes.v2';",
            )
            .map_err(storage)?;
        transaction
            .pragma_update(None, "user_version", SCHEMA_VERSION)
            .map_err(storage)?;
        transaction.commit().map_err(storage)?;
    }
    Ok(())
}

pub(crate) fn configure(connection: &Connection) -> Result<(), RagError> {
    connection
        .execute_batch("PRAGMA foreign_keys = ON; PRAGMA journal_mode = WAL;")
        .map_err(storage)
}

pub(crate) fn storage(error: rusqlite::Error) -> RagError {
    RagError::Storage(error.to_string())
}
