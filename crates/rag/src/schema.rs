use rusqlite::{Connection, OptionalExtension};
use sotto_core::{RagError, SQLITE_SCHEMA};

pub(crate) const SCHEMA_VERSION: u32 = 15;

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
    // Every block below still fires against the single `version` read above, exactly as before —
    // that is what lets one open() carry an old database through every applicable block in one
    // call, since a block's own commit never changes what a *later* block in this same call sees.
    // What changed is what each block stamps: its own literal target checkpoint, never
    // `SCHEMA_VERSION`, so a crash between any two commits leaves `user_version` at a value that
    // still satisfies every block that has not yet run. Two properties keep that true:
    //
    // - Every target is `own_checkpoint + 1` (or, for a block guarded by an inclusive range, that
    //   range's upper bound + 1) — never higher — so a target can never leap past a later block's
    //   own upper bound and take it out of range on the next open.
    // - Every exact-match block (`version == 1/5/6/8`, each handling a real historical database
    //   caught at that literal number) is positioned so that no earlier-running block *also*
    //   applicable to that same origin ever stamps past its trigger value first. `version == 6` in
    //   particular runs before the `1..=7` retranscription block, not after: originally it came
    //   second, so a crash right after `1..=7` committed (which can stamp as high as 7) and before
    //   `version == 6` ran would have made `version == 6`'s own exact match un-satisfiable forever,
    //   silently losing its recording-retention repair for a real database parked at 6.
    //
    // Re-running an already-applied block on resume (e.g. `version == 6` firing again because a
    // crash landed `user_version` back on 6) is always safe: every statement here is
    // `CREATE ... IF NOT EXISTS`, `INSERT ... OR IGNORE`, or `DROP ... IF EXISTS`.
    //
    // Two blocks (v4 and v9) rebuild a table other rows reference by foreign key and must toggle
    // `PRAGMA foreign_keys` around that rebuild; the pragma is a documented no-op inside an open
    // transaction, which is why those two blocks commit on their own rather than folding into one
    // connection-wide transaction with the rest.
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
                .pragma_update(None, "user_version", 6)
                .map_err(storage)?;
            transaction.commit().map_err(storage)
        })();
        connection
            .execute_batch("PRAGMA foreign_keys = ON;")
            .map_err(storage)?;
        migration?;
    }
    // The exact-match repair for a real database parked at literal version 6 runs *before* the
    // `1..=7` retranscription block immediately below, even though historically it was added
    // after it in this file. Ordering it first is what keeps it reachable: both blocks fire for
    // the same origin (6), but only the range block can stamp past 6 (up to 7), and if it ran
    // first and a crash landed right after its commit, this block's `version == 6` condition would
    // never match again on the next open — silently dropping its recording-retention repair for
    // good. Running it first means the worst a crash can do is make it (harmlessly) re-fire.
    if version == 6 {
        let transaction = connection.transaction().map_err(storage)?;
        transaction
            .execute_batch(RECORDING_RETENTION_SCHEMA)
            .map_err(storage)?;
        transaction
            .pragma_update(None, "user_version", 7)
            .map_err(storage)?;
        transaction.commit().map_err(storage)?;
    }
    if version == 5 {
        let transaction = connection.transaction().map_err(storage)?;
        transaction
            .execute_batch(RECORDING_RETENTION_SCHEMA)
            .map_err(storage)?;
        transaction
            .pragma_update(None, "user_version", 6)
            .map_err(storage)?;
        transaction.commit().map_err(storage)?;
    }
    if (1..=7).contains(&version) {
        let transaction = connection.transaction().map_err(storage)?;
        transaction
            .execute_batch(RETRANSCRIPTION_SCHEMA)
            .map_err(storage)?;
        transaction
            .pragma_update(None, "user_version", 8)
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
            .pragma_update(None, "user_version", 9)
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
                .pragma_update(None, "user_version", 10)
                .map_err(storage)?;
            transaction.commit().map_err(storage)
        })();
        connection
            .execute_batch("PRAGMA foreign_keys = ON;")
            .map_err(storage)?;
        migration?;
    }
    // A per-session title table briefly lived here (v10 -> v11, `session_titles`). The entry is
    // now the only titled object (T086/ADR-0021: "the title becomes the entry's; a session keeps
    // only its captured facts"), so that table, its schema, and its APIs are retired outright
    // rather than bridged — there is nothing left for a v10 database to gain at this checkpoint,
    // and removing the step is safe: every later block below still guards on the same
    // originally-read `version` snapshot, not on this step having run, so no later range or
    // exact-match condition depended on ever passing through the old checkpoint 11 to fire.
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
            .pragma_update(None, "user_version", 12)
            .map_err(storage)?;
        transaction.commit().map_err(storage)?;
    }
    // Every existing recording gains one mechanical entry. The ids intentionally match only for
    // this migration; they remain distinct domain types and future prepared entries have their
    // own identities. Titles are not carried forward from the retired `session_titles` table —
    // there is nothing to carry, since that table and every path that wrote it are gone by this
    // schema; a migrated entry starts untitled and the person renames it same as any other entry.
    //
    // The same pass also drops any `meeting_notes.v2` rows. Retiring that read path made them
    // unreadable dead data: `load_latest_grounded_notes_status` would silently return `Ok(None)`
    // for a session whose only artifact was one of these, which is worse than having no cached
    // summary at all. Deleting them is honest about the state and simply asks for a regenerate.
    //
    // Its own target is written as a literal fact about this step (mirroring every step above),
    // not as "whatever the crate constant currently says", so a step added after this one does
    // not silently reopen the same hazard this discipline exists to close.
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
            .pragma_update(None, "user_version", 13)
            .map_err(storage)?;
        transaction.commit().map_err(storage)?;
    }
    // v14 widens the `capture_target_kind` CHECK to accept `'imported'` (T071/ADR-0019): an
    // imported session has no OS-scoped capture target, and the CHECK is embedded in the table
    // definition itself, so accepting a fifth value needs the same rebuild-and-swap `sessions_v10`
    // used above to add `'microphone'`. No column semantics change and no row's existing data is
    // touched; this only widens what the `capture_target_kind` column is allowed to say.
    //
    // This is the last step, so its own target genuinely is `SCHEMA_VERSION` — see the same note
    // on the v13 step above for why it is still written as the literal `14`.
    if (1..=13).contains(&version) {
        connection
            .execute_batch("PRAGMA foreign_keys = OFF;")
            .map_err(storage)?;
        let migration = (|| {
            let transaction = connection.transaction().map_err(storage)?;
            transaction
                .execute_batch(
                    "CREATE TABLE sessions_v14 (\
                      id TEXT PRIMARY KEY NOT NULL,\
                      started_at_unix_ms INTEGER NOT NULL,\
                      ended_at_unix_ms INTEGER,\
                      capture_target_bundle_id TEXT,\
                      capture_target_display_name TEXT NOT NULL,\
                      capture_target_window_title TEXT,\
                      capture_target_kind TEXT NOT NULL CHECK (\
                        capture_target_kind IN\
                          ('application','window','display','microphone','imported')\
                      ),\
                      capture_target_audio_scoped INTEGER NOT NULL CHECK (\
                        capture_target_audio_scoped IN (0,1)\
                      )\
                    );\
                    INSERT INTO sessions_v14 SELECT * FROM sessions;\
                    DROP TABLE sessions;\
                    ALTER TABLE sessions_v14 RENAME TO sessions;",
                )
                .map_err(storage)?;
            let foreign_key_violation = transaction
                .query_row("PRAGMA foreign_key_check", [], |_| Ok(()))
                .optional()
                .map_err(storage)?;
            if foreign_key_violation.is_some() {
                return Err(RagError::Storage(
                    "imported capture-scope migration violated a foreign key".to_owned(),
                ));
            }
            transaction
                .pragma_update(None, "user_version", 14)
                .map_err(storage)?;
            transaction.commit().map_err(storage)
        })();
        connection
            .execute_batch("PRAGMA foreign_keys = ON;")
            .map_err(storage)?;
        migration?;
    }
    // `session_titles` is retired: the entry is the only titled object (T086, ADR-0021). A database
    // written before that change still carries the table, and a schema that claims to be current
    // while holding a table nothing reads is the same dishonesty this chain's per-step versioning
    // exists to prevent — so dropping it earns its own version rather than riding along silently.
    //
    // Titles are deliberately *not* carried across into `entries.title`. That is a decision, not an
    // oversight: a clean library was judged preferable to a compatibility path, so a recording that
    // predates entries migrates in untitled and is renamed again if its name still matters.
    if (1..=14).contains(&version) {
        let transaction = connection.transaction().map_err(storage)?;
        transaction
            .execute_batch("DROP TABLE IF EXISTS session_titles;")
            .map_err(storage)?;
        transaction
            .pragma_update(None, "user_version", 15)
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
