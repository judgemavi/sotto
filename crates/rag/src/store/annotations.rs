//! Append-only writes made after a session's capture actor has stopped.

use std::time::Duration;

use rusqlite::{OptionalExtension, Transaction, params};
use serde_json::{Value, json};
use sotto_core::{
    EventId, EventPayload, MarkKind, RagError, SessionId, TimelineEvent, checked_user_annotation,
    replay_lenient,
};

use super::{DocumentKind, IngestMetadata, Store, content_hash, message, poisoned, storage};

impl Store {
    /// Appends or supersedes a user annotation after durable session completion.
    ///
    /// Event identity is allocated inside the same SQLite transaction as the insert, so two
    /// post-meeting writers cannot claim the same next id. Existing events are never updated.
    pub fn append_completed_annotation(
        &self,
        session_id: SessionId,
        anchor: EventId,
        text: &str,
        mark: MarkKind,
        supersedes: Option<EventId>,
    ) -> Result<TimelineEvent, RagError> {
        let mut connection = self.writer.lock().map_err(poisoned)?;
        let transaction = connection.transaction().map_err(storage)?;
        ensure_completed(&transaction, session_id)?;
        let events = load_events(&transaction, session_id)?;
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
            .unwrap_or(Duration::ZERO)
            .saturating_add(Duration::from_nanos(1));
        let event: TimelineEvent = serde_json::from_value(json!({
            "id": next_id,
            "session_id": session_id.get(),
            "ts": {"secs": timestamp.as_secs(), "nanos": timestamp.subsec_nanos()},
            "supersedes": supersedes.map(EventId::get),
            "payload": {
                "type": "user_annotation",
                "value": annotation
            }
        }))
        .map_err(message)?;
        let nanos = i64::try_from(timestamp.as_nanos()).map_err(message)?;
        let payload = serde_json::to_string(event.payload()).map_err(message)?;
        transaction
            .execute(
                "INSERT INTO events(id,session_id,ts,kind,supersedes,payload) VALUES(?1,?2,?3,?4,?5,?6)",
                params![
                    event.id().get(),
                    session_id.get().to_string(),
                    nanos,
                    event.kind().as_str(),
                    supersedes.map(EventId::get),
                    payload,
                ],
            )
            .map_err(storage)?;
        transaction.commit().map_err(storage)?;
        Ok(event)
    }

    /// Refreshes the one prior-meeting document after a late annotation.
    ///
    /// A newly embedded version is committed before older versions are removed. If embedding
    /// fails, the previous searchable evidence remains available rather than disappearing.
    ///
    /// Unconditional since every completed recording is indexed: there is no policy row left to
    /// consult, and a recording that has not been indexed yet gets its first document here.
    pub fn refresh_searchable_prior_meeting(
        &self,
        session_id: SessionId,
    ) -> Result<bool, RagError> {
        let Some((text, title)) = self.render_annotated_prior_meeting(session_id)? else {
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
        )?;

        let mut connection = self.writer.lock().map_err(poisoned)?;
        let transaction = connection.transaction().map_err(storage)?;
        remove_superseded_prior_meetings(&transaction, session_id, &hash)?;
        transaction.commit().map_err(storage)?;
        Ok(true)
    }

    /// Renders the transcript plus the person's own notes, or `None` when there is neither.
    ///
    /// A recording that captured no speech but carries typed notes is still worth finding — the
    /// notes are the content — so this falls back to the shared preamble rather than requiring a
    /// transcript to exist first.
    fn render_annotated_prior_meeting(
        &self,
        session_id: SessionId,
    ) -> Result<Option<(String, String)>, RagError> {
        let (mut text, title) = match self.render_prior_meeting(session_id)? {
            Some(rendered) => rendered,
            None => {
                let (text, title, _) = self.prior_meeting_preamble(session_id)?;
                (text, title)
            }
        };
        let transcribed = text.contains("[event:");
        let events = self.load_session(session_id)?;
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
        annotations.sort_by_key(|(event_id, _)| *event_id);
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

fn remove_superseded_prior_meetings(
    transaction: &Transaction<'_>,
    session_id: SessionId,
    current_hash: &str,
) -> Result<(), RagError> {
    transaction
        .execute(
            "DELETE FROM vec_chunks WHERE chunk_id IN (SELECT c.id FROM chunks c JOIN documents d ON d.id=c.doc_id WHERE d.kind='prior_meeting' AND d.source_session_id=?1 AND d.content_hash<>?2)",
            params![session_id.get().to_string(), current_hash],
        )
        .map_err(storage)?;
    transaction
        .execute(
            "DELETE FROM documents WHERE kind='prior_meeting' AND source_session_id=?1 AND content_hash<>?2",
            params![session_id.get().to_string(), current_hash],
        )
        .map_err(storage)?;
    Ok(())
}

fn ensure_completed(transaction: &Transaction<'_>, session_id: SessionId) -> Result<(), RagError> {
    let ended = transaction
        .query_row(
            "SELECT ended_at_unix_ms FROM sessions WHERE id=?1",
            [session_id.get().to_string()],
            |row| row.get::<_, Option<u64>>(0),
        )
        .optional()
        .map_err(storage)?
        .ok_or_else(|| RagError::NotFound {
            id: session_id.get().to_string(),
        })?;
    if ended.is_none() {
        return Err(RagError::Storage(
            "post-meeting annotations require a durably completed session".to_owned(),
        ));
    }
    Ok(())
}

fn load_events(
    transaction: &Transaction<'_>,
    session_id: SessionId,
) -> Result<Vec<TimelineEvent>, RagError> {
    let mut statement = transaction
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
                "id": id,
                "session_id": session_id.get(),
                "ts": {"secs": nanos / 1_000_000_000, "nanos": nanos % 1_000_000_000},
                "supersedes": supersedes,
                "payload": payload,
            }))
            .map_err(message)?,
        );
    }
    Ok(events)
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use sotto_core::{
        CaptureTarget, EventPayload, MarkKind, Session, SessionId, Source, TargetKind,
        TimelineBuilder, Utterance, replay_lenient,
    };

    use super::{Store, remove_superseded_prior_meetings};

    #[test]
    fn completed_annotations_append_and_supersede_across_reopen()
    -> Result<(), Box<dyn std::error::Error>> {
        let directory = tempfile::tempdir()?;
        let database = directory.path().join("sotto.sqlite3");
        let session_id = SessionId::new(56);
        let mut session = Session::new(
            session_id,
            CaptureTarget {
                bundle_id: None,
                display_name: "Planning".to_owned(),
                window_title: None,
                kind: TargetKind::Window,
                audio_scoped: true,
            },
            1_000,
        );
        session.end(2_000);
        let mut timeline = TimelineBuilder::new(session);
        let anchor = timeline.append(
            Duration::from_secs(1),
            EventPayload::UtteranceFinal(Utterance {
                source: Source::Mic,
                start: Duration::from_secs(1),
                end: Duration::from_secs(2),
                text: "Mina owns the follow-up".to_owned(),
                avg_logprob: 0.0,
                annotations: Vec::new(),
            }),
        );
        let store = Store::open(&database)?;
        store.save_session(timeline.session())?;
        store.append_events(timeline.events())?;
        let original = store.append_completed_annotation(
            session_id,
            anchor.id(),
            "Confirm by Friday",
            MarkKind::Note,
            None,
        )?;
        let edit = store.append_completed_annotation(
            session_id,
            anchor.id(),
            "Confirm by Thursday",
            MarkKind::Important,
            Some(original.id()),
        )?;
        drop(store);

        let reopened = Store::open(&database)?;
        let events = reopened.load_session(session_id)?;
        assert_eq!(
            events
                .iter()
                .map(|event| event.id().get())
                .collect::<Vec<_>>(),
            [1, 2, 3]
        );
        assert_eq!(
            events[0], anchor,
            "captured transcript fact must not change"
        );
        assert_eq!(edit.supersedes(), Some(original.id()));
        let replayed = replay_lenient(&events);
        let active = replayed.state().active();
        assert!(!active.contains_key(&original.id()));
        let EventPayload::UserAnnotation(annotation) = active[&edit.id()].payload() else {
            return Err("edited annotation did not replay".into());
        };
        assert_eq!(annotation.anchor, anchor.id());
        assert_eq!(annotation.text, "Confirm by Thursday");
        assert_eq!(annotation.mark, MarkKind::Important);
        let (search_text, _) = reopened
            .render_annotated_prior_meeting(session_id)?
            .ok_or("an annotated recording must render")?;
        assert!(search_text.contains("[Your note] \"Confirm by Thursday\""));
        assert!(!search_text.contains("Confirm by Friday"));

        let mut connection = reopened.writer.lock().map_err(|_| "writer poisoned")?;
        for (id, hash) in [("old-prior", "old-hash"), ("current-prior", "current-hash")] {
            connection.execute(
                "INSERT INTO documents(id,kind,title,source_path,collection_id,source_session_id,provenance_status,ingested_at,content_hash) VALUES(?1,'prior_meeting','Meeting',NULL,NULL,?2,'native',0,?3)",
                rusqlite::params![id, session_id.get().to_string(), hash],
            )?;
        }
        let transaction = connection.transaction()?;
        remove_superseded_prior_meetings(&transaction, session_id, "current-hash")?;
        remove_superseded_prior_meetings(&transaction, session_id, "current-hash")?;
        transaction.commit()?;
        assert_eq!(
            connection.query_row(
                "SELECT COUNT(*) FROM documents WHERE kind='prior_meeting' AND source_session_id=?1",
                [session_id.get().to_string()],
                |row| row.get::<_, usize>(0),
            )?,
            1,
            "refresh cleanup must be idempotent and retain only current evidence",
        );
        Ok(())
    }
}
