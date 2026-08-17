use sea_orm::{ConnectionTrait, DatabaseBackend, DatabaseConnection, Statement};
use sea_orm_migration::{MigrationName, MigrationTrait, MigratorTrait, SchemaManager};
use sotto_core::{RagError, SQLITE_SCHEMA};

pub(crate) const SCHEMA_VERSION: u32 = 19;

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

const DERIVED_VIEWS_SCHEMA: &str = r#"
CREATE TABLE derived_views (
 session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
 kind TEXT NOT NULL, model TEXT NOT NULL, content_hash TEXT NOT NULL,
 artifact TEXT NOT NULL, usage TEXT NOT NULL, created_at INTEGER NOT NULL,
 PRIMARY KEY(session_id, kind, model, content_hash)
);
CREATE INDEX derived_views_session_kind_idx ON derived_views(session_id, kind);
"#;

const GROUNDED_EVIDENCE_SCHEMA: &str = r#"
CREATE TABLE grounded_derived_views (
 session_id TEXT NOT NULL, kind TEXT NOT NULL, model TEXT NOT NULL, content_hash TEXT NOT NULL,
 artifact TEXT NOT NULL, usage TEXT NOT NULL, provider_model TEXT NOT NULL,
 grant_fingerprint TEXT,
 source_status TEXT NOT NULL CHECK(source_status IN ('not_selected','available','unavailable')),
 bundle TEXT NOT NULL, created_at INTEGER NOT NULL,
 PRIMARY KEY(session_id, kind, model, content_hash),
 FOREIGN KEY(session_id) REFERENCES sessions(id) ON DELETE CASCADE
);
CREATE INDEX grounded_derived_views_session_kind_idx
 ON grounded_derived_views(session_id,kind);
"#;

const RECORDING_RETENTION_SCHEMA: &str = r#"
CREATE TABLE session_recordings (
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
CREATE INDEX session_recordings_state_age_idx
 ON session_recordings(state,updated_at_unix_ms,session_id);
CREATE TABLE recording_retention_settings (
 singleton INTEGER PRIMARY KEY CHECK(singleton=1),
 budget_bytes INTEGER NOT NULL CHECK(budget_bytes>0)
);
INSERT INTO recording_retention_settings(singleton,budget_bytes) VALUES(1,20000000000);
"#;

const RETRANSCRIPTION_SCHEMA: &str = r#"
CREATE TABLE session_retranscriptions (
 session_id TEXT PRIMARY KEY REFERENCES sessions(id) ON DELETE CASCADE,
 model TEXT NOT NULL,
 utterances TEXT NOT NULL,
 updated_at_unix_ms INTEGER NOT NULL
);
"#;

const ENTRY_SCHEMA: &str = r#"
CREATE TABLE entries (
 id TEXT PRIMARY KEY NOT NULL,
 created_at_unix_ms INTEGER NOT NULL,
 title TEXT CHECK(title IS NULL OR length(trim(title))>0)
);
CREATE TABLE entry_sessions (
 session_id TEXT PRIMARY KEY REFERENCES sessions(id) ON DELETE CASCADE,
 entry_id TEXT NOT NULL REFERENCES entries(id) ON DELETE CASCADE
);
CREATE INDEX entry_sessions_entry_idx ON entry_sessions(entry_id,session_id);
"#;

const NOTES_OVERLAY_SCHEMA: &str = r#"
CREATE TABLE entry_note_overlay_ops (
 sequence INTEGER PRIMARY KEY AUTOINCREMENT,
 entry_id TEXT NOT NULL REFERENCES entries(id) ON DELETE CASCADE,
 artifact_version TEXT NOT NULL,
 operation TEXT NOT NULL,
 created_at_unix_ms INTEGER NOT NULL
);
CREATE INDEX entry_note_overlay_entry_idx
 ON entry_note_overlay_ops(entry_id,sequence);
"#;

struct BaselineMigration;

struct GroundedNormalizationsMigration;

struct NotesOverlayMigration;

struct EntrySeriesMigration;

struct GroundedConsultationsMigration;

impl MigrationName for BaselineMigration {
    fn name(&self) -> &str {
        "m0001_current_schema_baseline"
    }
}

#[async_trait::async_trait]
impl MigrationTrait for BaselineMigration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), sea_orm::DbErr> {
        let database = manager.get_connection();
        for schema in [
            SQLITE_SCHEMA,
            RAG_SCHEMA,
            DERIVED_VIEWS_SCHEMA,
            GROUNDED_EVIDENCE_SCHEMA,
            RECORDING_RETENTION_SCHEMA,
            RETRANSCRIPTION_SCHEMA,
            ENTRY_SCHEMA,
        ] {
            database.execute_unprepared(schema).await?;
        }
        database
            .execute_unprepared(&format!("PRAGMA user_version={SCHEMA_VERSION};"))
            .await?;
        Ok(())
    }

    async fn down(&self, _manager: &SchemaManager) -> Result<(), sea_orm::DbErr> {
        Err(sea_orm::DbErr::Migration(
            "the local baseline is intentionally irreversible".to_owned(),
        ))
    }
}

impl MigrationName for GroundedNormalizationsMigration {
    fn name(&self) -> &str {
        "m0002_persist_grounded_normalizations"
    }
}

impl MigrationName for NotesOverlayMigration {
    fn name(&self) -> &str {
        "m0003_entry_note_overlay"
    }
}

impl MigrationName for EntrySeriesMigration {
    fn name(&self) -> &str {
        "m0004_entry_series"
    }
}

impl MigrationName for GroundedConsultationsMigration {
    fn name(&self) -> &str {
        "m0005_persist_grounded_screen_consultations"
    }
}

#[async_trait::async_trait]
impl MigrationTrait for GroundedConsultationsMigration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), sea_orm::DbErr> {
        manager
            .get_connection()
            .execute_unprepared(
                "ALTER TABLE grounded_derived_views \
                 ADD COLUMN consultations TEXT NOT NULL DEFAULT '[]';",
            )
            .await?;
        manager
            .get_connection()
            .execute_unprepared(&format!("PRAGMA user_version={SCHEMA_VERSION};"))
            .await?;
        Ok(())
    }

    async fn down(&self, _manager: &SchemaManager) -> Result<(), sea_orm::DbErr> {
        Err(sea_orm::DbErr::Migration(
            "screen consultation receipts are intentionally durable".to_owned(),
        ))
    }
}

#[async_trait::async_trait]
impl MigrationTrait for EntrySeriesMigration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), sea_orm::DbErr> {
        manager
            .get_connection()
            .execute_unprepared(
                "ALTER TABLE entries ADD COLUMN series TEXT \
                 CHECK(series IS NULL OR length(trim(series))>0);",
            )
            .await?;
        manager
            .get_connection()
            .execute_unprepared(&format!("PRAGMA user_version={SCHEMA_VERSION};"))
            .await?;
        Ok(())
    }

    async fn down(&self, _manager: &SchemaManager) -> Result<(), sea_orm::DbErr> {
        Err(sea_orm::DbErr::Migration(
            "entry series links are durable library structure".to_owned(),
        ))
    }
}

#[async_trait::async_trait]
impl MigrationTrait for NotesOverlayMigration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), sea_orm::DbErr> {
        manager
            .get_connection()
            .execute_unprepared(NOTES_OVERLAY_SCHEMA)
            .await?;
        manager
            .get_connection()
            .execute_unprepared(&format!("PRAGMA user_version={SCHEMA_VERSION};"))
            .await?;
        Ok(())
    }

    async fn down(&self, _manager: &SchemaManager) -> Result<(), sea_orm::DbErr> {
        Err(sea_orm::DbErr::Migration(
            "the user-authored notes overlay is intentionally append-only".to_owned(),
        ))
    }
}

#[async_trait::async_trait]
impl MigrationTrait for GroundedNormalizationsMigration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), sea_orm::DbErr> {
        manager
            .get_connection()
            .execute_unprepared(
                "ALTER TABLE grounded_derived_views \
                 ADD COLUMN normalizations TEXT NOT NULL DEFAULT '[]';",
            )
            .await?;
        manager
            .get_connection()
            .execute_unprepared(&format!("PRAGMA user_version={SCHEMA_VERSION};"))
            .await?;
        Ok(())
    }

    async fn down(&self, _manager: &SchemaManager) -> Result<(), sea_orm::DbErr> {
        Err(sea_orm::DbErr::Migration(
            "grounded downgrade history is intentionally durable".to_owned(),
        ))
    }
}

struct Migrator;

impl MigratorTrait for Migrator {
    fn migrations() -> Vec<Box<dyn MigrationTrait>> {
        vec![
            Box::new(BaselineMigration),
            Box::new(GroundedNormalizationsMigration),
            Box::new(NotesOverlayMigration),
            Box::new(EntrySeriesMigration),
            Box::new(GroundedConsultationsMigration),
        ]
    }
}

pub(crate) async fn migrate(database: &DatabaseConnection) -> Result<(), RagError> {
    let version = database
        .query_one_raw(Statement::from_string(
            DatabaseBackend::Sqlite,
            "PRAGMA user_version".to_owned(),
        ))
        .await
        .map_err(storage)?
        .ok_or_else(|| RagError::Storage("SQLite did not return user_version".to_owned()))?
        .try_get::<i64>("", "user_version")
        .map_err(storage)?;
    let has_migration_table = database
        .query_one_raw(Statement::from_string(
            DatabaseBackend::Sqlite,
            "SELECT 1 AS present FROM sqlite_master WHERE type='table' AND name='seaql_migrations'"
                .to_owned(),
        ))
        .await
        .map_err(storage)?
        .is_some();

    if version > i64::from(SCHEMA_VERSION) {
        return Err(RagError::Migration {
            from: u32::try_from(version).unwrap_or(u32::MAX),
            to: SCHEMA_VERSION,
        });
    }
    if version != 0 && !has_migration_table {
        return Err(RagError::Storage(format!(
            "database schema version {version} predates the SeaORM baseline; delete the database and reopen Sotto"
        )));
    }
    Migrator::up(database, None).await.map_err(storage)
}

pub(crate) fn storage(error: impl std::fmt::Display) -> RagError {
    RagError::Storage(error.to_string())
}
