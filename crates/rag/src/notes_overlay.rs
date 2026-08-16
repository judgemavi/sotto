use sea_orm::{ConnectionTrait as _, Value as SeaValue};
use sotto_core::{EntryId, RagError};

use crate::{
    schema::storage,
    store_async::{Store, from_i64, get, query_all, to_i64},
};

/// One durable row in an entry's append-only user-authored notes overlay.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NotesOverlayRow {
    pub sequence: u64,
    pub artifact_version: String,
    pub operation: String,
    pub created_at_unix_ms: u64,
}

impl Store {
    /// Appends one operation. Existing overlay rows are never updated or replaced.
    pub async fn append_notes_overlay_operation(
        &self,
        entry_id: EntryId,
        artifact_version: &str,
        operation: &str,
        created_at_unix_ms: u64,
    ) -> Result<u64, RagError> {
        let result = self
            .writer
            .execute_raw(sea_orm::Statement::from_sql_and_values(
                sea_orm::DatabaseBackend::Sqlite,
                "INSERT INTO entry_note_overlay_ops(entry_id,artifact_version,operation,created_at_unix_ms) VALUES(?,?,?,?)",
                vec![
                    SeaValue::from(entry_id.get().to_string()),
                    SeaValue::from(artifact_version.to_owned()),
                    SeaValue::from(operation.to_owned()),
                    SeaValue::from(to_i64(created_at_unix_ms)?),
                ],
            ))
            .await
            .map_err(storage)?;
        Ok(result.last_insert_id())
    }

    pub async fn load_notes_overlay(
        &self,
        entry_id: EntryId,
    ) -> Result<Vec<NotesOverlayRow>, RagError> {
        query_all(
            &self.reader,
            "SELECT sequence,artifact_version,operation,created_at_unix_ms FROM entry_note_overlay_ops WHERE entry_id=? ORDER BY sequence",
            vec![entry_id.get().to_string().into()],
        )
        .await?
        .into_iter()
        .map(|row| {
            Ok(NotesOverlayRow {
                sequence: from_i64(get(&row, "sequence")?)?,
                artifact_version: get(&row, "artifact_version")?,
                operation: get(&row, "operation")?,
                created_at_unix_ms: from_i64(get(&row, "created_at_unix_ms")?)?,
            })
        })
        .collect()
    }
}

#[cfg(test)]
mod tests {
    use sotto_core::{Entry, EntryId};

    use super::*;

    #[tokio::test]
    async fn overlay_is_append_only_and_replays_in_sequence_after_reopen() -> Result<(), RagError> {
        let directory =
            tempfile::tempdir().map_err(|error| RagError::Storage(error.to_string()))?;
        let path = directory.path().join("overlay.sqlite");
        let entry_id = EntryId::new(7);
        {
            let store = Store::open(&path).await?;
            store.create_entry(&Entry::new(entry_id, 1, None)).await?;
            assert_eq!(
                store
                    .append_notes_overlay_operation(entry_id, "v1", "first", 2)
                    .await?,
                1
            );
            assert_eq!(
                store
                    .append_notes_overlay_operation(entry_id, "v2", "second", 3)
                    .await?,
                2
            );
        }
        let reopened = Store::open(&path).await?;
        assert_eq!(
            reopened.load_notes_overlay(entry_id).await?,
            vec![
                NotesOverlayRow {
                    sequence: 1,
                    artifact_version: "v1".to_owned(),
                    operation: "first".to_owned(),
                    created_at_unix_ms: 2
                },
                NotesOverlayRow {
                    sequence: 2,
                    artifact_version: "v2".to_owned(),
                    operation: "second".to_owned(),
                    created_at_unix_ms: 3
                },
            ]
        );
        Ok(())
    }
}
