#![expect(
    clippy::tests_outside_test_module,
    reason = "integration test files compile only under cfg(test)"
)]

mod tests {
    use std::time::Duration;

    use rag::{DocumentKind, LocalProvenance, Store};
    use sea_orm::{
        ConnectionTrait, Database, DatabaseBackend, DatabaseConnection, Statement, Value,
    };
    use sotto_core::{
        CaptureTarget, Entry, EntryId, EventPayload, MarkKind, Session, SessionId, Source,
        SpeechState, TargetKind, TimelineBuilder, Utterance, VadSegment, replay_lenient,
        types::RecordingTitle,
    };

    fn session() -> Session {
        Session::new(
            SessionId::new(7),
            CaptureTarget {
                bundle_id: Some("us.zoom.xos".to_owned()),
                display_name: "Zoom".to_owned(),
                window_title: Some("Account call".to_owned()),
                kind: TargetKind::Window,
                audio_scoped: true,
            },
            1_700_000_000_000,
        )
    }

    async fn database(path: &std::path::Path) -> Result<DatabaseConnection, sea_orm::DbErr> {
        Database::connect(format!("sqlite://{}?mode=rwc", path.display())).await
    }

    async fn execute(
        connection: &DatabaseConnection,
        sql: &str,
        values: Vec<Value>,
    ) -> Result<u64, sea_orm::DbErr> {
        connection
            .execute_raw(Statement::from_sql_and_values(
                DatabaseBackend::Sqlite,
                sql,
                values,
            ))
            .await
            .map(|result| result.rows_affected())
    }

    async fn scalar_i64(
        connection: &DatabaseConnection,
        sql: &str,
        values: Vec<Value>,
    ) -> Result<i64, sea_orm::DbErr> {
        connection
            .query_one_raw(Statement::from_sql_and_values(
                DatabaseBackend::Sqlite,
                sql,
                values,
            ))
            .await?
            .ok_or_else(|| sea_orm::DbErr::Custom("scalar row missing".to_owned()))?
            .try_get_by("value")
    }

    #[tokio::test]
    async fn database_from_before_the_seaorm_baseline_is_refused_with_delete_guidance()
    -> Result<(), Box<dyn std::error::Error>> {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("legacy.sqlite3");
        let connection = database(&path).await?;
        connection
            .execute_unprepared("PRAGMA user_version=14")
            .await?;
        connection.close().await?;
        let error = match Store::open(&path).await {
            Ok(_) => return Err("legacy database unexpectedly opened".into()),
            Err(error) => error,
        };
        let message = error.to_string();
        assert!(message.contains("predates the SeaORM baseline"));
        assert!(message.contains("delete the database"));
        Ok(())
    }

    #[tokio::test]
    async fn timeline_round_trips_in_order_with_supersession()
    -> Result<(), Box<dyn std::error::Error>> {
        let store = Store::open_in_memory().await?;
        let session = session();
        store.save_session(&session).await?;
        let mut timeline = TimelineBuilder::new(session);
        let first = timeline.append(
            Duration::from_millis(10),
            EventPayload::Vad(VadSegment {
                source: Source::System,
                start: Duration::ZERO,
                end: None,
                kind: SpeechState::SpeechStart,
            }),
        );
        timeline.supersede(
            Duration::from_millis(20),
            EventPayload::Vad(VadSegment {
                source: Source::System,
                start: Duration::ZERO,
                end: Some(Duration::from_millis(20)),
                kind: SpeechState::SpeechEnd,
            }),
            &first,
        )?;
        store.append_events(timeline.events()).await?;
        assert_eq!(
            store.load_session(SessionId::new(7)).await?,
            timeline.events()
        );
        Ok(())
    }

    #[tokio::test]
    async fn finalized_tail_appends_without_rewriting_original_events()
    -> Result<(), Box<dyn std::error::Error>> {
        let store = Store::open_in_memory().await?;
        let session = session();
        store.save_session(&session).await?;
        let mut timeline = TimelineBuilder::new(session);
        timeline.append(
            Duration::from_millis(10),
            EventPayload::Vad(VadSegment {
                source: Source::Mic,
                start: Duration::ZERO,
                end: None,
                kind: SpeechState::SpeechStart,
            }),
        );
        store.append_events(timeline.events()).await?;

        let tail = Utterance {
            source: Source::Mic,
            start: Duration::from_secs(8),
            end: Duration::from_secs(9),
            text: "the final words".to_owned(),
            avg_logprob: -0.2,
            annotations: vec![],
        };
        let appended = store
            .append_final_utterances(SessionId::new(7), std::slice::from_ref(&tail))
            .await?;
        assert_eq!(appended[0].id().get(), 2);
        assert_eq!(appended[0].ts(), tail.end);
        let loaded = store.load_session(SessionId::new(7)).await?;
        assert_eq!(loaded[0], timeline.events()[0]);
        assert!(matches!(
            loaded[1].payload(),
            EventPayload::UtteranceFinal(value) if value == &tail
        ));
        Ok(())
    }

    #[tokio::test]
    async fn retranscription_replaces_only_the_derived_projection()
    -> Result<(), Box<dyn std::error::Error>> {
        let store = Store::open_in_memory().await?;
        let session = session();
        store.save_session(&session).await?;
        let original = Utterance {
            source: Source::System,
            start: Duration::ZERO,
            end: Duration::from_secs(1),
            text: "original capture".to_owned(),
            avg_logprob: -0.3,
            annotations: vec![],
        };
        let mut timeline = TimelineBuilder::new(session);
        timeline.append(original.end, EventPayload::UtteranceFinal(original.clone()));
        store.append_events(timeline.events()).await?;

        let replacement = Utterance {
            text: "corrected from retained media".to_owned(),
            ..original
        };
        store
            .replace_derived_transcript(
                SessionId::new(7),
                "small.en",
                std::slice::from_ref(&replacement),
            )
            .await?;
        store
            .replace_derived_transcript(
                SessionId::new(7),
                "medium.en",
                std::slice::from_ref(&replacement),
            )
            .await?;

        let derived = store
            .load_derived_transcript(SessionId::new(7))
            .await?
            .ok_or("missing derived transcript")?;
        assert_eq!(derived.model, "medium.en");
        assert_eq!(derived.utterances, [replacement]);
        assert_eq!(
            store.load_session(SessionId::new(7)).await?,
            timeline.events()
        );
        Ok(())
    }

    #[tokio::test]
    async fn user_annotations_round_trip_in_order_with_append_only_edits()
    -> Result<(), Box<dyn std::error::Error>> {
        let store = Store::open_in_memory().await?;
        let session = session();
        store.save_session(&session).await?;
        let mut timeline = TimelineBuilder::new(session);
        let anchor = timeline.append(
            Duration::from_millis(10),
            EventPayload::UtteranceFinal(Utterance {
                source: Source::Mic,
                start: Duration::ZERO,
                end: Duration::from_millis(10),
                text: "Assign the owner".to_owned(),
                avg_logprob: -0.1,
                annotations: vec![],
            }),
        );
        let original = timeline.append_user_annotation(
            Duration::from_millis(20),
            anchor.id(),
            "Confirm who owns this",
            MarkKind::FollowUp,
        )?;
        let edit = timeline.supersede_user_annotation(
            Duration::from_millis(30),
            "Mina owns the follow-up",
            MarkKind::Important,
            original.id(),
        )?;

        store.append_events(timeline.events()).await?;
        let loaded = store.load_session(SessionId::new(7)).await?;
        assert_eq!(loaded, timeline.events());
        let replayed = replay_lenient(&loaded);
        assert!(replayed.issues().is_empty());
        assert!(!replayed.state().active().contains_key(&original.id()));
        let EventPayload::UserAnnotation(annotation) = replayed
            .state()
            .active()
            .get(&edit.id())
            .ok_or("edited annotation missing after replay")?
            .payload()
        else {
            return Err("edited event did not replay as a user annotation".into());
        };
        assert_eq!(annotation.anchor, anchor.id());
        assert_eq!(annotation.text, "Mina owns the follow-up");
        assert_eq!(annotation.mark, MarkKind::Important);
        Ok(())
    }

    #[tokio::test]
    async fn every_connection_enforces_foreign_keys() -> Result<(), Box<dyn std::error::Error>> {
        let store = Store::open_in_memory().await?;
        let mut timeline = TimelineBuilder::new(session());
        timeline.append(
            Duration::ZERO,
            EventPayload::Vad(VadSegment {
                source: Source::Mic,
                start: Duration::ZERO,
                end: None,
                kind: SpeechState::SpeechStart,
            }),
        );
        assert!(
            store.append_events(timeline.events()).await.is_err(),
            "missing session must violate its foreign key"
        );
        Ok(())
    }

    #[tokio::test]
    async fn session_catalogue_is_unique_newest_first_and_preserves_completion()
    -> Result<(), Box<dyn std::error::Error>> {
        let store = Store::open_in_memory().await?;
        let older = session();
        let mut newer = Session::new(
            SessionId::new(8),
            CaptureTarget {
                bundle_id: Some("com.microsoft.teams2".to_owned()),
                display_name: "Teams".to_owned(),
                window_title: Some("Planning".to_owned()),
                kind: TargetKind::Application,
                audio_scoped: true,
            },
            1_800_000_000_000,
        );
        newer.end(1_800_000_060_000);
        store.save_session(&older).await?;
        store.save_session(&newer).await?;
        store.save_session(&newer).await?;

        let catalogue = store.list_sessions().await?;
        assert_eq!(catalogue.len(), 2, "upsert must not duplicate a session");
        assert_eq!(catalogue[0].id, SessionId::new(8));
        assert_eq!(catalogue[0].ended_at_unix_ms, Some(1_800_000_060_000));
        assert_eq!(catalogue[0].capture_target.display_name, "Teams");
        assert_eq!(catalogue[1].id, SessionId::new(7));
        assert_eq!(catalogue[1].ended_at_unix_ms, None);
        Ok(())
    }

    #[tokio::test]
    async fn prepared_entry_is_creatable_listable_renameable_and_deletable()
    -> Result<(), Box<dyn std::error::Error>> {
        let directory = tempfile::tempdir()?;
        let store = Store::open_in_memory().await?;
        let entry = Entry::new(EntryId::new(40), 1_900_000_000_000, None);
        store.create_entry(&entry).await?;

        let listed = store.list_entries().await?;
        assert_eq!(listed, vec![entry.clone()]);
        assert!(listed[0].session_ids().is_empty());

        let title = RecordingTitle::new("Prepared planning").ok_or("title")?;
        store.set_entry_title(entry.id(), Some(&title)).await?;
        assert_eq!(store.list_entries().await?[0].title(), Some(&title));

        store.delete_entry(entry.id(), directory.path()).await?;
        assert!(store.list_entries().await?.is_empty());
        Ok(())
    }

    #[tokio::test]
    async fn several_sessions_share_one_entry_without_changing_captured_facts()
    -> Result<(), Box<dyn std::error::Error>> {
        let store = Store::open_in_memory().await?;
        let entry = Entry::new(EntryId::new(41), 1_900_000_000_000, None);
        store.create_entry(&entry).await?;
        let first = session();
        let second = Session::new(
            SessionId::new(8),
            CaptureTarget::microphone_only(),
            1_700_000_060_000,
        );
        store.save_session_in_entry(&first, entry.id()).await?;
        store.save_session_in_entry(&second, entry.id()).await?;
        store.save_session_in_entry(&second, entry.id()).await?;

        assert_eq!(store.entry_for_session(first.id()).await?, entry.id());
        assert_eq!(store.entry_for_session(second.id()).await?, entry.id());
        assert_eq!(
            store.list_entries().await?[0].session_ids(),
            &[first.id(), second.id()]
        );
        assert_eq!(
            store
                .load_session_record(first.id())
                .await?
                .capture_target(),
            first.capture_target()
        );
        assert_eq!(
            store
                .load_session_record(second.id())
                .await?
                .capture_target(),
            second.capture_target()
        );

        let other = Entry::new(EntryId::new(42), 1_900_000_000_001, None);
        store.create_entry(&other).await?;
        assert!(
            store.attach_session(other.id(), second.id()).await.is_err(),
            "an attached session cannot be detached into another entry"
        );
        assert_eq!(store.entry_for_session(second.id()).await?, entry.id());

        store.delete_session(first.id()).await?;
        let remaining = store.list_entries().await?;
        let original = remaining
            .iter()
            .find(|listed| listed.id() == entry.id())
            .ok_or("session deletion must keep its entry")?;
        assert_eq!(original.session_ids(), &[second.id()]);

        store.delete_session(second.id()).await?;
        let remaining = store.list_entries().await?;
        assert!(
            remaining.iter().all(|listed| listed.id() != entry.id()),
            "deleting an entry's final session must not leave an orphan library row"
        );
        assert!(
            remaining.iter().any(|listed| listed.id() == other.id()),
            "deleting a session must not remove an unrelated prepared entry"
        );
        Ok(())
    }

    #[tokio::test]
    async fn microphone_only_scope_round_trips_through_session_persistence()
    -> Result<(), Box<dyn std::error::Error>> {
        let store = Store::open_in_memory().await?;
        let mut session = Session::new(
            SessionId::new(50),
            CaptureTarget::microphone_only(),
            1_800_000_000_000,
        );
        session.end(1_800_000_030_000);
        store.save_session(&session).await?;

        let loaded = store.load_session_record(session.id()).await?;
        assert_eq!(loaded.capture_target(), &CaptureTarget::microphone_only());
        assert!(loaded.capture_target().is_microphone_only());
        assert_eq!(
            store.list_sessions().await?[0].capture_target,
            CaptureTarget::microphone_only()
        );
        Ok(())
    }

    /// The correctness point of T085/T086: a chosen name is a label over the entry that owns a
    /// recording, never an edit of what was recorded. Renaming repeatedly must leave the capture
    /// target byte-identical.
    #[tokio::test]
    async fn renaming_a_recording_never_rewrites_what_was_captured()
    -> Result<(), Box<dyn std::error::Error>> {
        let store = Store::open_in_memory().await?;
        let captured = session();
        store.save_session(&captured).await?;
        let entry_id = store.entry_for_session(captured.id()).await?;

        for name in ["Standup", "BTU standup", "Monday standup"] {
            let title = RecordingTitle::new(name).ok_or("a name is a title")?;
            store.set_entry_title(entry_id, Some(&title)).await?;
            assert_eq!(
                store.entry_title(entry_id).await?.as_ref(),
                Some(&title),
                "the latest chosen name must be the one the store returns"
            );
            assert_eq!(
                store
                    .load_session_record(captured.id())
                    .await?
                    .capture_target(),
                captured.capture_target(),
                "renaming must not touch the recorded capture scope"
            );
            let summary = store
                .list_sessions()
                .await?
                .into_iter()
                .find(|summary| summary.id == captured.id())
                .ok_or("the renamed session must stay in the catalogue")?;
            assert_eq!(summary.title.as_ref(), Some(&title));
            assert_eq!(
                summary.capture_target,
                *captured.capture_target(),
                "the catalogue must keep reporting the captured scope beside the chosen name"
            );
        }

        store.set_entry_title(entry_id, None).await?;
        assert_eq!(
            store.entry_title(entry_id).await?,
            None,
            "clearing a title removes it rather than storing an empty one"
        );
        assert_eq!(
            store
                .load_session_record(captured.id())
                .await?
                .capture_target(),
            captured.capture_target(),
            "clearing a title is still not an edit of what was captured"
        );
        Ok(())
    }

    /// The store is the last line: a blank title must be impossible even for a writer that skipped
    /// `RecordingTitle`, and a title can only exist for an entry that exists.
    #[tokio::test]
    async fn the_store_refuses_a_blank_title_and_an_orphan_one()
    -> Result<(), Box<dyn std::error::Error>> {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("titles.sqlite3");
        let store = Store::open(&path).await?;
        let captured = session();
        store.save_session(&captured).await?;
        let entry_id = store.entry_for_session(captured.id()).await?;
        let title = RecordingTitle::new("Standup").ok_or("a name is a title")?;
        store.set_entry_title(entry_id, Some(&title)).await?;

        assert!(
            store
                .set_entry_title(EntryId::new(9_999), Some(&title))
                .await
                .is_err(),
            "a title for an entry that does not exist must be refused"
        );

        let connection = database(&path).await?;
        assert!(
            execute(
                &connection,
                "UPDATE entries SET title='   ' WHERE id=?",
                vec![entry_id.get().to_string().into()],
            )
            .await
            .is_err(),
            "a whitespace-only title must fail the schema's own check"
        );

        store.delete_session(captured.id()).await?;
        assert_eq!(
            scalar_i64(
                &connection,
                "SELECT COUNT(*) AS value FROM entries",
                Vec::new()
            )
            .await?,
            0,
            "deleting a recording's only session must take its owning entry, and the chosen \
             name on it, with it"
        );
        Ok(())
    }

    #[tokio::test]
    async fn grounded_artifact_bundle_commit_replay_conflict_and_delete_atomically()
    -> Result<(), Box<dyn std::error::Error>> {
        let store = Store::open_in_memory().await?;
        let stored_session = session();
        store.save_session(&stored_session).await?;
        let mut timeline = TimelineBuilder::new(stored_session);
        timeline.append(
            Duration::ZERO,
            EventPayload::Vad(VadSegment {
                source: Source::Mic,
                start: Duration::ZERO,
                end: None,
                kind: SpeechState::SpeechStart,
            }),
        );
        store.append_events(timeline.events()).await?;
        store
            .save_derived_view(
                SessionId::new(7),
                "meeting_notes.v1",
                "backend-fingerprint",
                "timeline-hash",
                r#"{"overview":[]}"#,
                r#"{"input_tokens":1}"#,
            )
            .await?;
        store
            .save_grounded_derived_view(
                SessionId::new(7),
                "meeting_notes.v2",
                "backend-fingerprint",
                "timeline-bundle-hash",
                r#"{"overview":[]}"#,
                r#"{"input_tokens":1}"#,
                "mock-model",
                Some("mcp-grant-v1-test"),
                "available",
                r#"{"excerpts":[],"digest":"test","estimated_tokens":0}"#,
            )
            .await?;
        let replay = store
            .load_grounded_derived_view(
                SessionId::new(7),
                "meeting_notes.v2",
                "backend-fingerprint",
                "timeline-bundle-hash",
            )
            .await?
            .ok_or("grounded artifact missing")?;
        assert_eq!(
            replay.grant_fingerprint.as_deref(),
            Some("mcp-grant-v1-test")
        );
        assert_eq!(replay.source_status, "available");
        assert!(
            store
                .save_grounded_derived_view(
                    SessionId::new(7),
                    "meeting_notes.v2",
                    "backend-fingerprint",
                    "timeline-bundle-hash",
                    r#"{"overview":["different"]}"#,
                    r#"{"input_tokens":1}"#,
                    "mock-model",
                    Some("mcp-grant-v1-test"),
                    "available",
                    r#"{"excerpts":[],"digest":"test","estimated_tokens":0}"#,
                )
                .await
                .is_err()
        );
        assert_eq!(
            store
                .load_grounded_derived_view(
                    SessionId::new(7),
                    "meeting_notes.v2",
                    "backend-fingerprint",
                    "timeline-bundle-hash",
                )
                .await?
                .ok_or("grounded artifact vanished")?
                .artifact,
            r#"{"overview":[]}"#
        );

        store.delete_session(SessionId::new(7)).await?;
        assert!(store.load_session(SessionId::new(7)).await?.is_empty());
        assert!(
            store
                .load_grounded_derived_view(
                    SessionId::new(7),
                    "meeting_notes.v2",
                    "backend-fingerprint",
                    "timeline-bundle-hash",
                )
                .await?
                .is_none()
        );
        assert!(
            store
                .load_derived_view(
                    SessionId::new(7),
                    "meeting_notes.v1",
                    "backend-fingerprint",
                    "timeline-hash",
                )
                .await?
                .is_none()
        );
        Ok(())
    }

    #[tokio::test]
    async fn deleting_entry_cascades_every_owned_recording_artifact_and_index_document()
    -> Result<(), Box<dyn std::error::Error>> {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("entry-delete.sqlite3");
        let store = Store::open(&path).await?;
        let entry = Entry::new(EntryId::new(99), 1_700_000_000_000, None);
        store.create_entry(&entry).await?;
        let first = session();
        let second = Session::new(
            SessionId::new(8),
            CaptureTarget::microphone_only(),
            1_700_000_060_000,
        );
        for captured in [&first, &second] {
            store.save_session_in_entry(captured, entry.id()).await?;
            let recording_path = directory
                .path()
                .join(format!("{}.mp4", captured.id().get()));
            std::fs::write(&recording_path, b"retained meeting media")?;
            store
                .save_growing_recording(captured.id(), &recording_path)
                .await?;
            store
                .save_derived_view(
                    captured.id(),
                    "meeting_notes.v1",
                    "backend",
                    "content",
                    "{}",
                    "{}",
                )
                .await?;
        }
        for recorded in [
            session(),
            Session::new(
                SessionId::new(8),
                CaptureTarget::microphone_only(),
                1_700_000_060_000,
            ),
        ] {
            let mut timeline = TimelineBuilder::new(recorded);
            timeline.append(
                Duration::from_secs(1),
                EventPayload::Vad(VadSegment {
                    source: Source::Mic,
                    start: Duration::ZERO,
                    end: None,
                    kind: SpeechState::SpeechStart,
                }),
            );
            store.append_events(timeline.events()).await?;
        }

        let connection = database(&path).await?;
        for captured in [&first, &second] {
            let id = captured.id().get();
            execute(&connection,
                "INSERT INTO documents(id,kind,title,source_session_id,provenance_status,ingested_at,content_hash) VALUES(?,'prior_meeting','Meeting',?,'native',0,?)",
                vec![format!("document-{id}").into(), id.to_string().into(), format!("hash-{id}").into()],
            ).await?;
            execute(&connection,
                "INSERT INTO chunks(id,doc_id,ordinal,text,token_count,metadata,embedding) VALUES(?,?,0,'meeting text',2,'{}',zeroblob(1536))",
                vec![format!("chunk-{id}").into(), format!("document-{id}").into()],
            ).await?;
            execute(
                &connection,
                "INSERT INTO vec_chunks(chunk_id,embedding) VALUES(?,zeroblob(1536))",
                vec![format!("chunk-{id}").into()],
            )
            .await?;
        }

        store.delete_entry(entry.id(), directory.path()).await?;
        for captured in [&first, &second] {
            assert!(
                !directory
                    .path()
                    .join(format!("{}.mp4", captured.id().get()))
                    .exists(),
                "entry deletion must unlink each managed recording"
            );
            assert!(matches!(
                store.load_session_record(captured.id()).await,
                Err(sotto_core::RagError::NotFound { .. })
            ));
            assert!(store.load_session(captured.id()).await?.is_empty());
            assert!(
                store
                    .load_recording_reference(captured.id())
                    .await?
                    .is_none()
            );
            assert!(
                store
                    .load_derived_view(captured.id(), "meeting_notes.v1", "backend", "content")
                    .await?
                    .is_none()
            );
        }
        for table in [
            "entries",
            "entry_sessions",
            "documents",
            "chunks",
            "vec_chunks",
        ] {
            let count = scalar_i64(
                &connection,
                &format!("SELECT COUNT(*) AS value FROM {table}"),
                Vec::new(),
            )
            .await?;
            assert_eq!(count, 0, "{table} must not retain entry-owned rows");
        }
        Ok(())
    }

    #[tokio::test]
    async fn deleting_unknown_entry_returns_not_found_and_touches_no_file()
    -> Result<(), Box<dyn std::error::Error>> {
        let directory = tempfile::tempdir()?;
        let store = Store::open_in_memory().await?;
        let entry = Entry::new(EntryId::new(900), 1_900_000_000_000, None);
        store.create_entry(&entry).await?;
        let recorded = session();
        store.save_session_in_entry(&recorded, entry.id()).await?;
        let recording_path = directory
            .path()
            .join(format!("{}.mp4", recorded.id().get()));
        std::fs::write(&recording_path, b"retained meeting media")?;
        store
            .save_growing_recording(recorded.id(), &recording_path)
            .await?;

        let missing = EntryId::new(901);
        let outcome = store.delete_entry(missing, directory.path()).await;
        assert!(
            matches!(outcome, Err(sotto_core::RagError::NotFound { .. })),
            "an unknown entry id must report NotFound, not a storage failure"
        );
        assert!(
            recording_path.exists(),
            "a delete that never resolves a real entry must not touch any file"
        );
        assert_eq!(
            store.list_entries().await?.len(),
            1,
            "the real entry must survive a delete request for a different id"
        );
        Ok(())
    }

    #[tokio::test]
    async fn delete_entry_restores_every_file_when_the_row_transaction_fails()
    -> Result<(), Box<dyn std::error::Error>> {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("entry-delete-rollback.sqlite3");
        let store = Store::open(&path).await?;
        let entry = Entry::new(EntryId::new(120), 1_700_000_000_000, None);
        store.create_entry(&entry).await?;
        let first = session();
        let second = Session::new(
            SessionId::new(8),
            CaptureTarget::microphone_only(),
            1_700_000_060_000,
        );
        let mut paths = Vec::new();
        for captured in [&first, &second] {
            store.save_session_in_entry(captured, entry.id()).await?;
            let recording_path = directory
                .path()
                .join(format!("{}.mp4", captured.id().get()));
            std::fs::write(&recording_path, b"retained meeting media")?;
            store
                .save_growing_recording(captured.id(), &recording_path)
                .await?;
            paths.push(recording_path);
        }

        // Force the row transaction to fail after quarantine has already renamed both files away,
        // simulating any mid-transaction failure (constraint, lock, disk) unrelated to the file
        // system half of the delete.
        let connection = database(&path).await?;
        connection
            .execute_unprepared(&format!(
                "CREATE TRIGGER refuse_entry_delete BEFORE DELETE ON entries WHEN old.id='{}' \
             BEGIN SELECT RAISE(ABORT, 'simulated row-transaction failure'); END;",
                entry.id().get()
            ))
            .await?;

        let outcome = store.delete_entry(entry.id(), directory.path()).await;
        assert!(
            matches!(outcome, Err(sotto_core::RagError::Storage(_))),
            "a rejected row transaction must fail the whole delete"
        );

        for recording_path in &paths {
            assert!(
                recording_path.exists(),
                "quarantined media must be restored when the row transaction fails"
            );
        }
        let tombstones = std::fs::read_dir(directory.path())?
            .filter_map(Result::ok)
            .filter(|item| item.file_name().to_string_lossy().ends_with(".deleting"))
            .count();
        assert_eq!(
            tombstones, 0,
            "no tombstone may survive a delete that restored every file"
        );

        assert_eq!(store.entry_for_session(first.id()).await?, entry.id());
        assert_eq!(store.entry_for_session(second.id()).await?, entry.id());
        assert_eq!(
            store.list_entries().await?[0].session_ids(),
            &[first.id(), second.id()],
            "the entry must resolve fully intact, not half-deleted"
        );
        Ok(())
    }

    #[tokio::test]
    async fn recover_quarantined_media_resolves_each_tombstone_from_the_row_that_survived_a_crash()
    -> Result<(), Box<dyn std::error::Error>> {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("recover.sqlite3");
        let store = Store::open(&path).await?;

        // Case 1: the crash happened before the row-side commit reached disk. The session and its
        // recording's path both still resolve to this file, so recovery must restore it.
        let uncommitted = Session::new(
            SessionId::new(30),
            CaptureTarget::microphone_only(),
            1_700_000_000_000,
        );
        store.save_session(&uncommitted).await?;
        let uncommitted_path = directory
            .path()
            .join(format!("{}.mp4", uncommitted.id().get()));
        std::fs::write(&uncommitted_path, b"still referenced media")?;
        store
            .save_growing_recording(uncommitted.id(), &uncommitted_path)
            .await?;
        let uncommitted_tombstone = directory
            .path()
            .join(format!(".{}.deleting", uncommitted.id().get()));
        std::fs::rename(&uncommitted_path, &uncommitted_tombstone)?;

        // Case 2: the crash happened after the row-side commit reached disk but before the final
        // unlink. The recording row already shows no path, so recovery must finish the unlink.
        let cleared_row = Session::new(
            SessionId::new(31),
            CaptureTarget::microphone_only(),
            1_700_000_000_001,
        );
        store.save_session(&cleared_row).await?;
        let cleared_row_path = directory
            .path()
            .join(format!("{}.mp4", cleared_row.id().get()));
        std::fs::write(&cleared_row_path, b"already-deleted-per-row media")?;
        store
            .save_growing_recording(cleared_row.id(), &cleared_row_path)
            .await?;
        let cleared_row_tombstone = directory
            .path()
            .join(format!(".{}.deleting", cleared_row.id().get()));
        std::fs::rename(&cleared_row_path, &cleared_row_tombstone)?;
        execute(
            &database(&path).await?,
            "UPDATE session_recordings SET state='deleted',path=NULL,container=NULL WHERE session_id=?",
            vec![cleared_row.id().get().to_string().into()],
        ).await?;

        // Case 3: an entry-level delete's row transaction committed and removed the session row
        // entirely before the crash. There is nothing left to restore to, so recovery must finish
        // the unlink exactly as it does when only the recording row was cleared.
        let gone = Session::new(
            SessionId::new(32),
            CaptureTarget::microphone_only(),
            1_700_000_000_002,
        );
        store.save_session(&gone).await?;
        let gone_path = directory.path().join(format!("{}.mp4", gone.id().get()));
        std::fs::write(&gone_path, b"orphaned media")?;
        store.save_growing_recording(gone.id(), &gone_path).await?;
        let gone_tombstone = directory
            .path()
            .join(format!(".{}.deleting", gone.id().get()));
        std::fs::rename(&gone_path, &gone_tombstone)?;
        store.delete_session(gone.id()).await?;

        drop(store);
        let restarted = Store::open(&path).await?;
        let resolved = restarted
            .recover_quarantined_media(directory.path())
            .await?;
        assert_eq!(
            resolved.len(),
            3,
            "one sweep must resolve every tombstone left behind"
        );

        assert!(
            uncommitted_path.exists(),
            "an uncommitted delete must restore its file"
        );
        assert!(!uncommitted_tombstone.exists());
        assert_eq!(
            restarted
                .load_recording_reference(uncommitted.id())
                .await?
                .map(|reference| reference.session_id()),
            Some(uncommitted.id())
        );

        assert!(
            !cleared_row_path.exists(),
            "a recording row already marked deleted must not resurrect its file"
        );
        assert!(!cleared_row_tombstone.exists());

        assert!(
            !gone_path.exists(),
            "a fully deleted session must not resurrect its file"
        );
        assert!(!gone_tombstone.exists());

        let restored_ids: Vec<_> = resolved
            .iter()
            .filter(|resolution| resolution.restored)
            .map(|resolution| resolution.session_id)
            .collect();
        assert_eq!(
            restored_ids,
            vec![uncommitted.id()],
            "only the uncommitted case may resolve as a restore"
        );
        Ok(())
    }

    #[tokio::test]
    async fn deleting_meeting_removes_only_session_owned_local_knowledge_and_vectors()
    -> Result<(), Box<dyn std::error::Error>> {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("rag.sqlite3");
        let store = Store::open(&path).await?;
        let mut completed = session();
        completed.end(1_700_000_060_000);
        store.save_session(&completed).await?;
        let mut timeline = TimelineBuilder::new(completed);
        timeline.append(
            Duration::from_secs(1),
            EventPayload::UtteranceFinal(Utterance {
                source: Source::Mic,
                start: Duration::from_secs(1),
                end: Duration::from_secs(2),
                text: "Record the decision".to_owned(),
                avg_logprob: 0.0,
                annotations: Vec::new(),
            }),
        );
        store.append_events(timeline.events()).await?;
        store
            .save_derived_view(
                SessionId::new(7),
                "meeting_notes.v1",
                "backend",
                "content",
                "{}",
                "{}",
            )
            .await?;

        let connection = database(&path).await?;
        for (ordinal, kind, source_session_id) in [
            (0, "resource_document", None),
            (1, "project_note", None),
            (2, "prior_meeting", Some("7")),
            (3, "meeting_note", Some("7")),
        ] {
            let document_id = format!("document-{ordinal}");
            let chunk_id = format!("local-evidence-{ordinal}");
            execute(&connection,
                "INSERT INTO documents(id,kind,title,source_path,collection_id,source_session_id,provenance_status,ingested_at,content_hash) VALUES(?,?,?,?,'collection-a',?,'native',0,?)",
                vec![
                    document_id.clone().into(), kind.into(), format!("Title {ordinal}").into(),
                    format!("/local/{ordinal}.md").into(), source_session_id.map(str::to_owned).into(),
                    format!("hash-{ordinal}").into(),
                ],
            ).await?;
            execute(&connection,
                "INSERT INTO chunks(id,doc_id,ordinal,text,token_count,metadata,embedding) VALUES(?,?,0,?,2,'{\"label\":\"exact\"}',zeroblob(1536))",
                vec![chunk_id.clone().into(), document_id.into(), format!("Evidence {ordinal}").into()],
            ).await?;
            execute(
                &connection,
                "INSERT INTO vec_chunks(chunk_id,embedding) VALUES(?,zeroblob(1536))",
                vec![chunk_id.into()],
            )
            .await?;
        }
        for (ordinal, expected_kind, expected_session) in [
            (0, DocumentKind::ResourceDocument, None),
            (1, DocumentKind::ProjectNote, None),
            (2, DocumentKind::PriorMeeting, Some(SessionId::new(7))),
            (3, DocumentKind::MeetingNote, Some(SessionId::new(7))),
        ] {
            let receipt = store
                .load_local_evidence(&format!("local-evidence-{ordinal}"))
                .await?;
            assert_eq!(receipt.kind, expected_kind);
            assert_eq!(receipt.source_session_id, expected_session);
            assert_eq!(receipt.provenance, LocalProvenance::Native);
            assert_eq!(receipt.title, format!("Title {ordinal}"));
            assert_eq!(receipt.text, format!("Evidence {ordinal}"));
            assert_eq!(receipt.collection_id.as_deref(), Some("collection-a"));
            assert_eq!(receipt.source_path, Some(format!("/local/{ordinal}.md")));
        }

        store.delete_session(SessionId::new(7)).await?;
        assert!(matches!(
            store.load_session_record(SessionId::new(7)).await,
            Err(sotto_core::RagError::NotFound { .. })
        ));
        assert_eq!(
            scalar_i64(
                &connection,
                "SELECT COUNT(*) AS value FROM documents",
                Vec::new()
            )
            .await?,
            2
        );
        assert_eq!(
            scalar_i64(
                &connection,
                "SELECT COUNT(*) AS value FROM documents WHERE source_session_id IS NOT NULL",
                Vec::new()
            )
            .await?,
            0
        );
        assert_eq!(
            scalar_i64(
                &connection,
                "SELECT COUNT(*) AS value FROM chunks",
                Vec::new()
            )
            .await?,
            2
        );
        assert_eq!(
            scalar_i64(
                &connection,
                "SELECT COUNT(*) AS value FROM vec_chunks",
                Vec::new()
            )
            .await?,
            2
        );
        assert_eq!(
            scalar_i64(
                &connection,
                "SELECT COUNT(*) AS value FROM chunks_fts",
                Vec::new()
            )
            .await?,
            2
        );
        assert!(
            store
                .load_derived_view(SessionId::new(7), "meeting_notes.v1", "backend", "content")
                .await?
                .is_none()
        );
        Ok(())
    }

    /// Regression for the migration-versioning hazard: a step that has not reached its result
    /// must never leave `user_version` stamped at the final schema version, or a crash right
    /// after it commits strands the database forever (the next open sees `version == SCHEMA_
    /// VERSION`, so no migration block's condition ever matches again).
    ///
    /// This forces the v12 -> v13 step (create/backfill `entries`) to fail outright by seeding a
    /// stub `entries` table the backfill's column list does not match, which simulates a crash
    /// landing between the v11 -> v12 commit (drop `session_search_policy`) and the v12 -> v13
    /// commit: the earlier step succeeds and commits, the later one never does. The earlier
    /// commit must be found parked at its own v12 checkpoint, not stranded past it — and clearing
    /// the conflict must let the very next open finish the walk to the current schema (v14 as of
    /// T071's `imported` capture-target-kind widening), including a working `entries` table (the
    /// concrete failure this hazard produces is `Store::save_session`'s `ensure_session_entry`
    /// failing with "no such table: entries" on the very next recording).
    #[tokio::test]
    async fn deleting_an_entry_cannot_diverge_the_quarantined_set_from_the_deleted_set()
    -> Result<(), Box<dyn std::error::Error>> {
        for attempt in 0_u128..8 {
            let directory = tempfile::tempdir()?;
            let path = directory.path().join("race.sqlite3");
            let store = std::sync::Arc::new(Store::open(&path).await?);

            let entry = Entry::new(EntryId::new(500 + attempt), 1_700_000_000_000, None);
            store.create_entry(&entry).await?;
            let first = Session::new(
                SessionId::new(1000 + attempt * 10),
                CaptureTarget::microphone_only(),
                1_700_000_000_000,
            );
            store.save_session_in_entry(&first, entry.id()).await?;
            let first_path = directory.path().join(format!("{}.mp4", first.id().get()));
            std::fs::write(&first_path, b"first retained media")?;
            store
                .save_growing_recording(first.id(), &first_path)
                .await?;

            let second_id = SessionId::new(1000 + attempt * 10 + 1);
            execute(
                &database(&path).await?,
                "INSERT INTO sessions VALUES(?,?,NULL,NULL,'Microphone only',NULL,'microphone',1)",
                vec![
                    second_id.get().to_string().into(),
                    1_700_000_000_001_i64.into(),
                ],
            )
            .await?;
            let second_path = directory.path().join(format!("{}.mp4", second_id.get()));
            std::fs::write(&second_path, b"second retained media")?;
            store
                .save_growing_recording(second_id, &second_path)
                .await?;

            let barrier = std::sync::Arc::new(tokio::sync::Barrier::new(2));
            let attach_outcome = {
                let store = std::sync::Arc::clone(&store);
                let barrier = std::sync::Arc::clone(&barrier);
                let entry_id = entry.id();
                tokio::spawn(async move {
                    barrier.wait().await;
                    store.attach_session(entry_id, second_id).await
                })
            };
            let delete_outcome = {
                let store = std::sync::Arc::clone(&store);
                let barrier = std::sync::Arc::clone(&barrier);
                let entry_id = entry.id();
                let recording_directory = directory.path().to_path_buf();
                tokio::spawn(async move {
                    barrier.wait().await;
                    store.delete_entry(entry_id, &recording_directory).await
                })
            };
            let attach_outcome = attach_outcome.await.map_err(|_| "attach task panicked")?;
            let delete_outcome = delete_outcome.await.map_err(|_| "delete task panicked")?;

            assert!(
                delete_outcome.is_ok(),
                "attempt {attempt}: deleting the entry's own original member must always succeed"
            );
            assert!(
                !first_path.exists(),
                "attempt {attempt}: the entry's original member must always be cascaded away"
            );

            let second_row_survives = store.load_session_record(second_id).await.is_ok();
            let second_file_survives = second_path.exists();
            match attach_outcome {
                Ok(()) => {
                    assert!(
                        !second_row_survives,
                        "attempt {attempt}: the attach committed first, so `second` was a real \
                         member and must be cascaded away with the entry"
                    );
                    assert!(
                        !second_file_survives,
                        "attempt {attempt}: `second`'s media must be quarantined and unlinked \
                         exactly when its row is deleted, not left orphaned on disk"
                    );
                }
                Err(_) => {
                    assert!(
                        second_row_survives,
                        "attempt {attempt}: the attach was refused because the entry was \
                         already gone, so `second`'s own row must be untouched"
                    );
                    assert!(
                        second_file_survives,
                        "attempt {attempt}: a refused attach must leave `second`'s media exactly \
                         where it was, never quarantined for an entry it never joined"
                    );
                }
            }
            assert_eq!(
                second_row_survives, second_file_survives,
                "attempt {attempt}: `second`'s row and media must agree on whether it was ever \
                 part of the deleted entry — a split result is an orphaned or lost file"
            );
        }
        Ok(())
    }
}
