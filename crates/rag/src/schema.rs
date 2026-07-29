use rusqlite::Connection;
use sotto_core::{RagError, SQLITE_SCHEMA};

pub(crate) const SCHEMA_VERSION: u32 = 2;

const RAG_SCHEMA: &str = r#"
CREATE TABLE documents (
 id TEXT PRIMARY KEY, kind TEXT NOT NULL, title TEXT NOT NULL, source_path TEXT,
 account_id TEXT, ingested_at INTEGER NOT NULL, content_hash TEXT NOT NULL UNIQUE
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
CREATE TABLE accounts (id TEXT PRIMARY KEY, name TEXT NOT NULL, metadata TEXT NOT NULL DEFAULT '{}');
CREATE VIRTUAL TABLE vec_chunks USING vec0(chunk_id TEXT PRIMARY KEY, embedding float[384]);
CREATE INDEX documents_kind_account_idx ON documents(kind, account_id);
CREATE INDEX chunks_doc_id_idx ON chunks(doc_id);
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
    if version == 0 {
        let transaction = connection.transaction().map_err(storage)?;
        transaction.execute_batch(SQLITE_SCHEMA).map_err(storage)?;
        transaction.execute_batch(RAG_SCHEMA).map_err(storage)?;
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
