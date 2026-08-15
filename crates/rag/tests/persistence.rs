#![expect(
    clippy::tests_outside_test_module,
    reason = "integration test files compile only under cfg(test)"
)]

mod tests {
    use std::time::Duration;

    use rag::{DEFAULT_RECORDING_BUDGET_BYTES, DocumentKind, LocalProvenance, Store};
    use rusqlite::{Connection, params};
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

    #[test]
    fn migrating_version_six_adds_recording_retention_atomically()
    -> Result<(), Box<dyn std::error::Error>> {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("rag-v6.sqlite3");
        drop(Store::open(&path)?);
        {
            let connection = Connection::open(&path)?;
            connection.execute_batch(
                "DROP INDEX session_recordings_state_age_idx;
                 DROP TABLE session_recordings;
                 DROP TABLE recording_retention_settings;
                 PRAGMA user_version=6;",
            )?;
        }

        let migrated = Store::open(&path)?;
        assert_eq!(
            migrated.recording_usage()?.budget_bytes,
            DEFAULT_RECORDING_BUDGET_BYTES,
            "v6 migration must install the explicit default recording budget"
        );
        assert_eq!(
            Connection::open(path)?
                .query_row("PRAGMA user_version", [], |row| row.get::<_, u32>(0))?,
            13,
            "recording retention and retranscription migration must reach the current schema"
        );
        Ok(())
    }

    fn install_legacy_document_schema(
        connection: &Connection,
        with_indexes: bool,
    ) -> Result<(), Box<dyn std::error::Error>> {
        connection.execute_batch(
            "PRAGMA foreign_keys = OFF;
             DROP INDEX documents_identity_idx;
             DROP INDEX documents_kind_collection_idx;
             DROP TABLE documents;
             CREATE TABLE documents (
               id TEXT PRIMARY KEY, kind TEXT NOT NULL, title TEXT NOT NULL, source_path TEXT,
               account_id TEXT, ingested_at INTEGER NOT NULL, content_hash TEXT NOT NULL UNIQUE
             );
             CREATE TABLE accounts (
               id TEXT PRIMARY KEY, name TEXT NOT NULL, metadata TEXT NOT NULL DEFAULT '{}'
             );
             PRAGMA foreign_keys = ON;",
        )?;
        if with_indexes {
            connection.execute_batch(
                "CREATE INDEX documents_kind_account_idx ON documents(kind, account_id);",
            )?;
        }
        Ok(())
    }

    #[test]
    fn timeline_round_trips_in_order_with_supersession() -> Result<(), Box<dyn std::error::Error>> {
        let store = Store::open_in_memory()?;
        let session = session();
        store.save_session(&session)?;
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
        store.append_events(timeline.events())?;
        assert_eq!(store.load_session(SessionId::new(7))?, timeline.events());
        Ok(())
    }

    #[test]
    fn finalized_tail_appends_without_rewriting_original_events()
    -> Result<(), Box<dyn std::error::Error>> {
        let store = Store::open_in_memory()?;
        let session = session();
        store.save_session(&session)?;
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
        store.append_events(timeline.events())?;

        let tail = Utterance {
            source: Source::Mic,
            start: Duration::from_secs(8),
            end: Duration::from_secs(9),
            text: "the final words".to_owned(),
            avg_logprob: -0.2,
            annotations: vec![],
        };
        let appended =
            store.append_final_utterances(SessionId::new(7), std::slice::from_ref(&tail))?;
        assert_eq!(appended[0].id().get(), 2);
        assert_eq!(appended[0].ts(), tail.end);
        let loaded = store.load_session(SessionId::new(7))?;
        assert_eq!(loaded[0], timeline.events()[0]);
        assert!(matches!(
            loaded[1].payload(),
            EventPayload::UtteranceFinal(value) if value == &tail
        ));
        Ok(())
    }

    #[test]
    fn retranscription_replaces_only_the_derived_projection()
    -> Result<(), Box<dyn std::error::Error>> {
        let store = Store::open_in_memory()?;
        let session = session();
        store.save_session(&session)?;
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
        store.append_events(timeline.events())?;

        let replacement = Utterance {
            text: "corrected from retained media".to_owned(),
            ..original
        };
        store.replace_derived_transcript(
            SessionId::new(7),
            "small.en",
            std::slice::from_ref(&replacement),
        )?;
        store.replace_derived_transcript(
            SessionId::new(7),
            "medium.en",
            std::slice::from_ref(&replacement),
        )?;

        let derived = store
            .load_derived_transcript(SessionId::new(7))?
            .ok_or("missing derived transcript")?;
        assert_eq!(derived.model, "medium.en");
        assert_eq!(derived.utterances, [replacement]);
        assert_eq!(store.load_session(SessionId::new(7))?, timeline.events());
        Ok(())
    }

    #[test]
    fn user_annotations_round_trip_in_order_with_append_only_edits()
    -> Result<(), Box<dyn std::error::Error>> {
        let store = Store::open_in_memory()?;
        let session = session();
        store.save_session(&session)?;
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

        store.append_events(timeline.events())?;
        let loaded = store.load_session(SessionId::new(7))?;
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

    #[test]
    fn every_connection_enforces_foreign_keys() -> Result<(), Box<dyn std::error::Error>> {
        let store = Store::open_in_memory()?;
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
            store.append_events(timeline.events()).is_err(),
            "missing session must violate its foreign key"
        );
        Ok(())
    }

    #[test]
    fn session_catalogue_is_unique_newest_first_and_preserves_completion()
    -> Result<(), Box<dyn std::error::Error>> {
        let store = Store::open_in_memory()?;
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
        store.save_session(&older)?;
        store.save_session(&newer)?;
        store.save_session(&newer)?;

        let catalogue = store.list_sessions()?;
        assert_eq!(catalogue.len(), 2, "upsert must not duplicate a session");
        assert_eq!(catalogue[0].id, SessionId::new(8));
        assert_eq!(catalogue[0].ended_at_unix_ms, Some(1_800_000_060_000));
        assert_eq!(catalogue[0].capture_target.display_name, "Teams");
        assert_eq!(catalogue[1].id, SessionId::new(7));
        assert_eq!(catalogue[1].ended_at_unix_ms, None);
        Ok(())
    }

    #[test]
    fn prepared_entry_is_creatable_listable_renameable_and_deletable()
    -> Result<(), Box<dyn std::error::Error>> {
        let directory = tempfile::tempdir()?;
        let store = Store::open_in_memory()?;
        let entry = Entry::new(EntryId::new(40), 1_900_000_000_000, None);
        store.create_entry(&entry)?;

        let listed = store.list_entries()?;
        assert_eq!(listed, vec![entry.clone()]);
        assert!(listed[0].session_ids().is_empty());

        let title = RecordingTitle::new("Prepared planning").ok_or("title")?;
        store.set_entry_title(entry.id(), Some(&title))?;
        assert_eq!(store.list_entries()?[0].title(), Some(&title));

        store.delete_entry(entry.id(), directory.path())?;
        assert!(store.list_entries()?.is_empty());
        Ok(())
    }

    #[test]
    fn several_sessions_share_one_entry_without_changing_captured_facts()
    -> Result<(), Box<dyn std::error::Error>> {
        let store = Store::open_in_memory()?;
        let entry = Entry::new(EntryId::new(41), 1_900_000_000_000, None);
        store.create_entry(&entry)?;
        let first = session();
        let second = Session::new(
            SessionId::new(8),
            CaptureTarget::microphone_only(),
            1_700_000_060_000,
        );
        store.save_session_in_entry(&first, entry.id())?;
        store.save_session_in_entry(&second, entry.id())?;
        store.save_session_in_entry(&second, entry.id())?;

        assert_eq!(store.entry_for_session(first.id())?, entry.id());
        assert_eq!(store.entry_for_session(second.id())?, entry.id());
        assert_eq!(
            store.list_entries()?[0].session_ids(),
            &[first.id(), second.id()]
        );
        assert_eq!(
            store.load_session_record(first.id())?.capture_target(),
            first.capture_target()
        );
        assert_eq!(
            store.load_session_record(second.id())?.capture_target(),
            second.capture_target()
        );

        let other = Entry::new(EntryId::new(42), 1_900_000_000_001, None);
        store.create_entry(&other)?;
        assert!(
            store.attach_session(other.id(), second.id()).is_err(),
            "an attached session cannot be detached into another entry"
        );
        assert_eq!(store.entry_for_session(second.id())?, entry.id());

        store.delete_session(first.id())?;
        let remaining = store.list_entries()?;
        let original = remaining
            .iter()
            .find(|listed| listed.id() == entry.id())
            .ok_or("session deletion must keep its entry")?;
        assert_eq!(original.session_ids(), &[second.id()]);

        store.delete_session(second.id())?;
        let remaining = store.list_entries()?;
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

    #[test]
    fn microphone_only_scope_round_trips_through_session_persistence()
    -> Result<(), Box<dyn std::error::Error>> {
        let store = Store::open_in_memory()?;
        let mut session = Session::new(
            SessionId::new(50),
            CaptureTarget::microphone_only(),
            1_800_000_000_000,
        );
        session.end(1_800_000_030_000);
        store.save_session(&session)?;

        let loaded = store.load_session_record(session.id())?;
        assert_eq!(loaded.capture_target(), &CaptureTarget::microphone_only());
        assert!(loaded.capture_target().is_microphone_only());
        assert_eq!(
            store.list_sessions()?[0].capture_target,
            CaptureTarget::microphone_only()
        );
        Ok(())
    }

    /// The correctness point of T085: a chosen name is a label over a recording, never an edit of
    /// what was recorded. Renaming repeatedly must leave the capture target byte-identical.
    #[test]
    fn renaming_a_recording_never_rewrites_what_was_captured()
    -> Result<(), Box<dyn std::error::Error>> {
        let store = Store::open_in_memory()?;
        let captured = session();
        store.save_session(&captured)?;

        for name in ["Standup", "BTU standup", "Monday standup"] {
            let title = RecordingTitle::new(name).ok_or("a name is a title")?;
            store.set_session_title(captured.id(), Some(&title))?;
            assert_eq!(
                store.load_session_title(captured.id())?.as_ref(),
                Some(&title),
                "the latest chosen name must be the one the store returns"
            );
            assert_eq!(
                store.load_session_record(captured.id())?.capture_target(),
                captured.capture_target(),
                "renaming must not touch the recorded capture scope"
            );
            let summary = store
                .list_sessions()?
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

        store.set_session_title(captured.id(), None)?;
        assert_eq!(
            store.load_session_title(captured.id())?,
            None,
            "clearing a title removes it rather than storing an empty one"
        );
        assert_eq!(
            store.load_session_record(captured.id())?.capture_target(),
            captured.capture_target(),
            "clearing a title is still not an edit of what was captured"
        );
        Ok(())
    }

    /// The store is the last line: a blank title must be impossible even for a writer that skipped
    /// `RecordingTitle`, and a title can only exist for a session that exists.
    #[test]
    fn the_store_refuses_a_blank_title_and_an_orphan_one() -> Result<(), Box<dyn std::error::Error>>
    {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("titles.sqlite3");
        let store = Store::open(&path)?;
        let captured = session();
        store.save_session(&captured)?;
        let title = RecordingTitle::new("Standup").ok_or("a name is a title")?;
        store.set_session_title(captured.id(), Some(&title))?;

        assert!(
            store
                .set_session_title(SessionId::new(9_999), Some(&title))
                .is_err(),
            "a title for a session that does not exist must be refused"
        );

        let connection = Connection::open(&path)?;
        connection.execute_batch("PRAGMA foreign_keys = ON;")?;
        assert!(
            connection
                .execute(
                    "INSERT INTO session_titles(session_id,title,updated_at_unix_ms) VALUES('7','   ',1) ON CONFLICT(session_id) DO UPDATE SET title=excluded.title",
                    [],
                )
                .is_err(),
            "a whitespace-only title must fail the schema's own check"
        );

        store.delete_session(captured.id())?;
        assert_eq!(
            connection.query_row("SELECT COUNT(*) FROM session_titles", [], |row| row
                .get::<_, u32>(0))?,
            0,
            "deleting a recording must take its chosen name with it"
        );
        Ok(())
    }

    #[test]
    fn migrating_version_ten_adds_titles_and_keeps_every_captured_fact()
    -> Result<(), Box<dyn std::error::Error>> {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("rag-v10.sqlite3");
        {
            let store = Store::open(&path)?;
            store.save_session(&session())?;
        }
        {
            let connection = Connection::open(&path)?;
            connection.execute_batch("DROP TABLE session_titles;")?;
            connection.pragma_update(None, "user_version", 10)?;
        }

        let migrated = Store::open(&path)?;
        assert_eq!(
            migrated
                .load_session_record(SessionId::new(7))?
                .capture_target(),
            session().capture_target(),
            "a database that predates titles must keep every captured fact"
        );
        assert_eq!(
            migrated.load_session_title(SessionId::new(7))?,
            None,
            "an existing recording is untitled after the migration, not retitled"
        );
        let title = RecordingTitle::new("Standup").ok_or("a name is a title")?;
        migrated.set_session_title(SessionId::new(7), Some(&title))?;
        assert_eq!(migrated.load_session_title(SessionId::new(7))?, Some(title));
        assert_eq!(
            Connection::open(path)?
                .query_row("PRAGMA user_version", [], |row| row.get::<_, u32>(0))?,
            13,
            "the title migration must land on the current schema version"
        );
        Ok(())
    }

    #[test]
    fn migrating_version_twelve_creates_one_entry_per_session_idempotently()
    -> Result<(), Box<dyn std::error::Error>> {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("rag-v12.sqlite3");
        let title = RecordingTitle::new("Named before entries").ok_or("title")?;
        {
            let store = Store::open(&path)?;
            store.save_session(&session())?;
            store.set_session_title(SessionId::new(7), Some(&title))?;
        }
        {
            let connection = Connection::open(&path)?;
            connection.execute_batch(
                "DROP TABLE entry_sessions;
                 DROP TABLE entries;
                 PRAGMA user_version=12;",
            )?;
        }

        let migrated = Store::open(&path)?;
        let entries = migrated.list_entries()?;
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].id(), EntryId::new(7));
        assert_eq!(entries[0].session_ids(), &[SessionId::new(7)]);
        assert_eq!(
            migrated.load_session_title(SessionId::new(7))?,
            Some(title),
            "the in-review title path stays intact until its ownership closes"
        );
        drop(migrated);

        let reopened = Store::open(&path)?;
        assert_eq!(
            reopened.list_entries()?.len(),
            1,
            "re-running migration must be a no-op"
        );
        assert_eq!(
            Connection::open(path)?
                .query_row("PRAGMA user_version", [], |row| row.get::<_, u32>(0))?,
            13
        );
        Ok(())
    }

    /// `meeting_notes.v2` rows are unreadable dead data now that only `recording_notes.v1` is ever
    /// read back. Leaving them in place would make `load_latest_grounded_notes_status` silently
    /// return `Ok(None)` for a session whose only artifact is one of these, which is worse than no
    /// cached summary at all — so the v13 pass deletes them rather than migrating them forward.
    #[test]
    fn migrating_version_twelve_deletes_retired_v2_grounded_notes()
    -> Result<(), Box<dyn std::error::Error>> {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("rag-v12-v2-cleanup.sqlite3");
        {
            let store = Store::open(&path)?;
            store.save_session(&session())?;
            store.save_grounded_derived_view(
                SessionId::new(7),
                "meeting_notes.v2",
                "backend",
                "content",
                "{}",
                "{}",
                "model",
                None,
                "not_selected",
                r#"{"excerpts":[]}"#,
            )?;
            store.save_grounded_derived_view(
                SessionId::new(7),
                "recording_notes.v1",
                "backend",
                "content",
                "{}",
                "{}",
                "model",
                None,
                "not_selected",
                r#"{"excerpts":[]}"#,
            )?;
        }
        Connection::open(&path)?.pragma_update(None, "user_version", 12)?;

        let migrated = Store::open(&path)?;
        assert!(
            migrated
                .load_grounded_derived_view(
                    SessionId::new(7),
                    "meeting_notes.v2",
                    "backend",
                    "content"
                )?
                .is_none(),
            "the retired v2 kind must not survive the v13 migration"
        );
        assert!(
            migrated
                .load_grounded_derived_view(
                    SessionId::new(7),
                    "recording_notes.v1",
                    "backend",
                    "content"
                )?
                .is_some(),
            "the cleanup must be scoped to meeting_notes.v2 and leave the current kind alone"
        );
        assert_eq!(
            Connection::open(path)?
                .query_row("PRAGMA user_version", [], |row| row.get::<_, u32>(0))?,
            13
        );
        Ok(())
    }

    #[test]
    fn migrating_version_nine_preserves_sessions_and_accepts_microphone_scope()
    -> Result<(), Box<dyn std::error::Error>> {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("rag-v9.sqlite3");
        {
            let store = Store::open(&path)?;
            store.save_session(&session())?;
        }
        Connection::open(&path)?.pragma_update(None, "user_version", 9)?;

        let migrated = Store::open(&path)?;
        assert_eq!(
            migrated
                .load_session_record(SessionId::new(7))?
                .capture_target(),
            session().capture_target()
        );
        let microphone = Session::new(
            SessionId::new(50),
            CaptureTarget::microphone_only(),
            1_800_000_000_000,
        );
        migrated.save_session(&microphone)?;
        assert!(
            migrated
                .load_session_record(microphone.id())?
                .capture_target()
                .is_microphone_only()
        );
        assert_eq!(
            Connection::open(path)?
                .query_row("PRAGMA user_version", [], |row| row.get::<_, u32>(0))?,
            13
        );
        Ok(())
    }

    #[test]
    fn migrating_a_version_one_database_preserves_existing_data()
    -> Result<(), Box<dyn std::error::Error>> {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("rag.sqlite3");
        {
            let store = Store::open(&path)?;
            store.save_session(&session())?;
        }
        {
            let connection = Connection::open(&path)?;
            install_legacy_document_schema(&connection, true)?;
            connection.execute_batch(
                "DROP INDEX documents_kind_account_idx;\
                 DROP INDEX chunks_doc_id_idx;\
                 PRAGMA user_version = 1;",
            )?;
        }

        let migrated = Store::open(&path)?;
        let loaded = migrated.load_session_record(SessionId::new(7))?;
        let expected = session();
        assert_eq!(loaded.id(), expected.id());
        assert_eq!(loaded.capture_target(), expected.capture_target());
        assert_eq!(loaded.started_at_unix_ms(), expected.started_at_unix_ms());
        assert_eq!(loaded.ended_at_unix_ms(), expected.ended_at_unix_ms());
        let connection = Connection::open(path)?;
        assert_eq!(
            connection.query_row("PRAGMA user_version", [], |row| row.get::<_, u32>(0))?,
            13
        );
        Ok(())
    }

    #[test]
    fn migrating_a_version_two_database_preserves_data_and_adds_derived_views()
    -> Result<(), Box<dyn std::error::Error>> {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("rag.sqlite3");
        {
            let store = Store::open(&path)?;
            store.save_session(&session())?;
        }
        {
            let connection = Connection::open(&path)?;
            install_legacy_document_schema(&connection, true)?;
            connection.execute_batch(
                "DROP INDEX derived_views_session_kind_idx;\
                 DROP TABLE derived_views;\
                 PRAGMA user_version = 2;",
            )?;
        }

        let migrated = Store::open(&path)?;
        assert_eq!(
            migrated
                .load_session_record(SessionId::new(7))?
                .capture_target(),
            session().capture_target()
        );
        migrated.save_derived_view(
            SessionId::new(7),
            "topical_clusters.v1",
            "test-model",
            "timeline-and-prompt-hash",
            r#"{"regions":[]}"#,
            r#"{"input_tokens":1}"#,
        )?;
        assert_eq!(
            migrated.load_derived_view(
                SessionId::new(7),
                "topical_clusters.v1",
                "test-model",
                "timeline-and-prompt-hash"
            )?,
            Some((
                r#"{"regions":[]}"#.to_owned(),
                r#"{"input_tokens":1}"#.to_owned()
            ))
        );
        let connection = Connection::open(path)?;
        assert_eq!(
            connection.query_row("PRAGMA user_version", [], |row| row.get::<_, u32>(0))?,
            13
        );
        Ok(())
    }

    #[test]
    fn grounded_artifact_bundle_commit_replay_conflict_and_delete_atomically()
    -> Result<(), Box<dyn std::error::Error>> {
        let store = Store::open_in_memory()?;
        let stored_session = session();
        store.save_session(&stored_session)?;
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
        store.append_events(timeline.events())?;
        store.save_derived_view(
            SessionId::new(7),
            "meeting_notes.v1",
            "backend-fingerprint",
            "timeline-hash",
            r#"{"overview":[]}"#,
            r#"{"input_tokens":1}"#,
        )?;
        store.save_grounded_derived_view(
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
        )?;
        let replay = store
            .load_grounded_derived_view(
                SessionId::new(7),
                "meeting_notes.v2",
                "backend-fingerprint",
                "timeline-bundle-hash",
            )?
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
                .is_err()
        );
        assert_eq!(
            store
                .load_grounded_derived_view(
                    SessionId::new(7),
                    "meeting_notes.v2",
                    "backend-fingerprint",
                    "timeline-bundle-hash",
                )?
                .ok_or("grounded artifact vanished")?
                .artifact,
            r#"{"overview":[]}"#
        );

        store.delete_session(SessionId::new(7))?;
        assert!(store.load_session(SessionId::new(7))?.is_empty());
        assert!(
            store
                .load_grounded_derived_view(
                    SessionId::new(7),
                    "meeting_notes.v2",
                    "backend-fingerprint",
                    "timeline-bundle-hash",
                )?
                .is_none()
        );
        assert!(
            store
                .load_derived_view(
                    SessionId::new(7),
                    "meeting_notes.v1",
                    "backend-fingerprint",
                    "timeline-hash",
                )?
                .is_none()
        );
        Ok(())
    }

    #[test]
    fn migrating_version_three_adds_atomic_grounded_views() -> Result<(), Box<dyn std::error::Error>>
    {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("rag.sqlite3");
        {
            let store = Store::open(&path)?;
            store.save_session(&session())?;
        }
        {
            let connection = Connection::open(&path)?;
            install_legacy_document_schema(&connection, true)?;
            connection.execute_batch(
                "DROP INDEX grounded_derived_views_session_kind_idx;\
                 DROP TABLE grounded_derived_views;\
                 PRAGMA user_version = 3;",
            )?;
        }
        let migrated = Store::open(&path)?;
        migrated.save_grounded_derived_view(
            SessionId::new(7),
            "meeting_notes.v2",
            "backend",
            "content",
            "{}",
            "{}",
            "mock-model",
            Some("grant"),
            "not_selected",
            r#"{"excerpts":[],"digest":"test","estimated_tokens":0}"#,
        )?;
        assert!(
            migrated
                .load_grounded_derived_view(
                    SessionId::new(7),
                    "meeting_notes.v2",
                    "backend",
                    "content",
                )?
                .is_some()
        );
        assert_eq!(
            Connection::open(path)?
                .query_row("PRAGMA user_version", [], |row| row.get::<_, u32>(0))?,
            13
        );
        Ok(())
    }

    #[test]
    fn migrating_version_four_uses_one_conservative_generic_mapping()
    -> Result<(), Box<dyn std::error::Error>> {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("rag.sqlite3");
        {
            let store = Store::open(&path)?;
            let mut source_session = session();
            source_session.end(1_700_000_060_000);
            store.save_session(&source_session)?;
            store.save_grounded_derived_view(
                SessionId::new(7),
                "recording_notes.v1",
                "backend",
                "content",
                "{}",
                "{}",
                "model",
                None,
                "not_selected",
                r#"{"excerpts":[]}"#,
            )?;
        }
        {
            let connection = Connection::open(&path)?;
            install_legacy_document_schema(&connection, true)?;
            for (ordinal, kind) in [
                "battlecard",
                "product_document",
                "account_note",
                "past_call",
                "recap",
            ]
            .into_iter()
            .enumerate()
            {
                let document_id = format!("legacy-{ordinal}");
                let chunk_id = format!("legacy-chunk-{ordinal}");
                connection.execute(
                    "INSERT INTO documents(id,kind,title,source_path,account_id,ingested_at,content_hash) VALUES(?1,?2,?3,?4,?5,0,?6)",
                    params![
                        document_id,
                        kind,
                        format!("Source {ordinal}"),
                        format!("/source/{ordinal}.md"),
                        "collection-a",
                        format!("hash-{ordinal}")
                    ],
                )?;
                connection.execute(
                    "INSERT INTO chunks(id,doc_id,ordinal,text,token_count,metadata,embedding) VALUES(?1,?2,0,?3,2,'{\"owner\":\"local\"}',zeroblob(1536))",
                    params![chunk_id, document_id, format!("exact text {ordinal}")],
                )?;
            }
            connection.pragma_update(None, "user_version", 4)?;
        }

        let migrated = Store::open(&path)?;
        let connection = Connection::open(&path)?;
        assert_eq!(
            connection.query_row("PRAGMA user_version", [], |row| row.get::<_, u32>(0))?,
            13
        );
        assert_eq!(
            connection.query_row(
                "SELECT COUNT(*) FROM documents WHERE kind='resource_document'",
                [],
                |row| row.get::<_, usize>(0)
            )?,
            5
        );
        assert_eq!(
            connection.query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='accounts'",
                [],
                |row| row.get::<_, usize>(0)
            )?,
            0
        );
        let columns = connection
            .prepare("PRAGMA table_info(documents)")?
            .query_map([], |row| row.get::<_, String>(1))?
            .collect::<Result<Vec<_>, _>>()?;
        assert!(columns.contains(&"collection_id".to_owned()));
        assert!(columns.contains(&"source_session_id".to_owned()));
        assert!(!columns.iter().any(|column| column.contains("account")));
        for ordinal in 0..5 {
            let receipt = migrated.load_local_evidence(&format!("legacy-chunk-{ordinal}"))?;
            assert_eq!(receipt.kind, DocumentKind::ResourceDocument);
            assert_eq!(receipt.document_id, format!("legacy-{ordinal}"));
            assert_eq!(receipt.text, format!("exact text {ordinal}"));
            assert_eq!(receipt.source_path, Some(format!("/source/{ordinal}.md")));
            assert_eq!(receipt.collection_id.as_deref(), Some("collection-a"));
            assert_eq!(receipt.source_session_id, None);
            assert_eq!(
                receipt.provenance,
                if ordinal >= 2 {
                    LocalProvenance::LegacyUnlinked
                } else {
                    LocalProvenance::Native
                }
            );
            assert_eq!(
                receipt.metadata.get("owner").map(String::as_str),
                Some("local")
            );
            assert_eq!(
                receipt
                    .metadata
                    .get("provenance_status")
                    .map(String::as_str),
                (ordinal >= 2).then_some("legacy_unlinked")
            );
        }
        // `meeting_notes.v2` is deliberately not used as the fixture kind here: the same v13 pass
        // this test exercises also purges that retired kind (see the v13 migration block), so a
        // still-current kind is what actually proves a grounded artifact survives this document
        // migration untouched.
        assert_eq!(
            migrated
                .load_grounded_derived_view(
                    SessionId::new(7),
                    "recording_notes.v1",
                    "backend",
                    "content"
                )?
                .ok_or("grounded artifact lost during v5 migration")?
                .provider_model,
            "model"
        );
        Ok(())
    }

    #[test]
    fn unknown_version_four_document_kind_aborts_without_mutation()
    -> Result<(), Box<dyn std::error::Error>> {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("rag.sqlite3");
        drop(Store::open(&path)?);
        {
            let connection = Connection::open(&path)?;
            install_legacy_document_schema(&connection, true)?;
            connection.execute(
                "INSERT INTO documents VALUES('unknown','future_kind','Unknown',NULL,NULL,0,'hash')",
                [],
            )?;
            connection.pragma_update(None, "user_version", 4)?;
        }

        assert!(Store::open(&path).is_err());
        let connection = Connection::open(path)?;
        assert_eq!(
            connection.query_row("PRAGMA user_version", [], |row| row.get::<_, u32>(0))?,
            4
        );
        assert_eq!(
            connection.query_row("SELECT kind FROM documents WHERE id='unknown'", [], |row| {
                row.get::<_, String>(0)
            })?,
            "future_kind"
        );
        Ok(())
    }

    #[test]
    fn unknown_kind_preflight_leaves_versions_one_through_three_reopenable_and_unchanged()
    -> Result<(), Box<dyn std::error::Error>> {
        for version in 1_u32..=3 {
            let directory = tempfile::tempdir()?;
            let path = directory.path().join(format!("rag-v{version}.sqlite3"));
            drop(Store::open(&path)?);
            {
                let connection = Connection::open(&path)?;
                install_legacy_document_schema(&connection, true)?;
                connection.execute(
                    "INSERT INTO documents VALUES('unknown','future_kind','Unknown',NULL,NULL,0,'hash')",
                    [],
                )?;
                connection.pragma_update(None, "user_version", version)?;
            }

            for _ in 0..2 {
                assert!(Store::open(&path).is_err());
                let connection = Connection::open(&path)?;
                assert_eq!(
                    connection.query_row("PRAGMA user_version", [], |row| row.get::<_, u32>(0))?,
                    version
                );
                assert_eq!(
                    connection.query_row(
                        "SELECT kind FROM documents WHERE id='unknown'",
                        [],
                        |row| row.get::<_, String>(0)
                    )?,
                    "future_kind"
                );
                assert_eq!(
                    connection.query_row(
                        "SELECT COUNT(*) FROM sqlite_master WHERE name='documents_v5'",
                        [],
                        |row| row.get::<_, usize>(0)
                    )?,
                    0
                );
            }
        }
        Ok(())
    }

    #[test]
    fn fresh_version_five_with_current_kind_reopens_without_legacy_preflight()
    -> Result<(), Box<dyn std::error::Error>> {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("rag-v5.sqlite3");
        drop(Store::open(&path)?);
        {
            let connection = Connection::open(&path)?;
            connection.execute(
                "INSERT INTO documents(id,kind,title,source_path,collection_id,source_session_id,provenance_status,ingested_at,content_hash) VALUES('current','resource_document','Current',NULL,NULL,NULL,'native',0,'hash')",
                [],
            )?;
        }
        drop(Store::open(&path)?);
        let connection = Connection::open(path)?;
        assert_eq!(
            connection.query_row("PRAGMA user_version", [], |row| row.get::<_, u32>(0))?,
            13
        );
        assert_eq!(
            connection.query_row("SELECT kind FROM documents WHERE id='current'", [], |row| {
                row.get::<_, String>(0)
            })?,
            "resource_document"
        );
        Ok(())
    }

    #[test]
    fn deleting_entry_cascades_every_owned_recording_artifact_and_index_document()
    -> Result<(), Box<dyn std::error::Error>> {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("entry-delete.sqlite3");
        let store = Store::open(&path)?;
        let entry = Entry::new(EntryId::new(99), 1_700_000_000_000, None);
        store.create_entry(&entry)?;
        let first = session();
        let second = Session::new(
            SessionId::new(8),
            CaptureTarget::microphone_only(),
            1_700_000_060_000,
        );
        for captured in [&first, &second] {
            store.save_session_in_entry(captured, entry.id())?;
            let recording_path = directory
                .path()
                .join(format!("{}.mp4", captured.id().get()));
            std::fs::write(&recording_path, b"retained meeting media")?;
            store.save_growing_recording(captured.id(), &recording_path)?;
            store.save_derived_view(
                captured.id(),
                "meeting_notes.v1",
                "backend",
                "content",
                "{}",
                "{}",
            )?;
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
            store.append_events(timeline.events())?;
        }

        let connection = Connection::open(&path)?;
        connection.execute_batch("PRAGMA foreign_keys = ON;")?;
        for captured in [&first, &second] {
            let id = captured.id().get();
            connection.execute(
                "INSERT INTO documents(id,kind,title,source_session_id,provenance_status,ingested_at,content_hash) VALUES(?1,'prior_meeting','Meeting',?2,'native',0,?3)",
                params![format!("document-{id}"), id.to_string(), format!("hash-{id}")],
            )?;
            connection.execute(
                "INSERT INTO chunks(id,doc_id,ordinal,text,token_count,metadata,embedding) VALUES(?1,?2,0,'meeting text',2,'{}',zeroblob(1536))",
                params![format!("chunk-{id}"), format!("document-{id}")],
            )?;
            connection.execute(
                "INSERT INTO vec_chunks(chunk_id,embedding) VALUES(?1,zeroblob(1536))",
                [format!("chunk-{id}")],
            )?;
        }

        store.delete_entry(entry.id(), directory.path())?;
        for captured in [&first, &second] {
            assert!(
                !directory
                    .path()
                    .join(format!("{}.mp4", captured.id().get()))
                    .exists(),
                "entry deletion must unlink each managed recording"
            );
            assert!(matches!(
                store.load_session_record(captured.id()),
                Err(sotto_core::RagError::NotFound { .. })
            ));
            assert!(store.load_session(captured.id())?.is_empty());
            assert!(store.load_recording_reference(captured.id())?.is_none());
            assert!(
                store
                    .load_derived_view(captured.id(), "meeting_notes.v1", "backend", "content")?
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
            let count =
                connection.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                    row.get::<_, usize>(0)
                })?;
            assert_eq!(count, 0, "{table} must not retain entry-owned rows");
        }
        Ok(())
    }

    #[test]
    fn deleting_meeting_removes_only_session_owned_local_knowledge_and_vectors()
    -> Result<(), Box<dyn std::error::Error>> {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("rag.sqlite3");
        let store = Store::open(&path)?;
        let mut completed = session();
        completed.end(1_700_000_060_000);
        store.save_session(&completed)?;
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
        store.append_events(timeline.events())?;
        store.save_derived_view(
            SessionId::new(7),
            "meeting_notes.v1",
            "backend",
            "content",
            "{}",
            "{}",
        )?;

        let connection = Connection::open(&path)?;
        connection.execute_batch("PRAGMA foreign_keys = ON;")?;
        for (ordinal, kind, source_session_id) in [
            (0, "resource_document", None),
            (1, "project_note", None),
            (2, "prior_meeting", Some("7")),
            (3, "meeting_note", Some("7")),
        ] {
            let document_id = format!("document-{ordinal}");
            let chunk_id = format!("local-evidence-{ordinal}");
            connection.execute(
                "INSERT INTO documents(id,kind,title,source_path,collection_id,source_session_id,provenance_status,ingested_at,content_hash) VALUES(?1,?2,?3,?4,'collection-a',?5,'native',0,?6)",
                params![
                    document_id,
                    kind,
                    format!("Title {ordinal}"),
                    format!("/local/{ordinal}.md"),
                    source_session_id,
                    format!("hash-{ordinal}")
                ],
            )?;
            connection.execute(
                "INSERT INTO chunks(id,doc_id,ordinal,text,token_count,metadata,embedding) VALUES(?1,?2,0,?3,2,'{\"label\":\"exact\"}',zeroblob(1536))",
                params![chunk_id, document_id, format!("Evidence {ordinal}")],
            )?;
            connection.execute(
                "INSERT INTO vec_chunks(chunk_id,embedding) VALUES(?1,zeroblob(1536))",
                [chunk_id],
            )?;
        }
        for (ordinal, expected_kind, expected_session) in [
            (0, DocumentKind::ResourceDocument, None),
            (1, DocumentKind::ProjectNote, None),
            (2, DocumentKind::PriorMeeting, Some(SessionId::new(7))),
            (3, DocumentKind::MeetingNote, Some(SessionId::new(7))),
        ] {
            let receipt = store.load_local_evidence(&format!("local-evidence-{ordinal}"))?;
            assert_eq!(receipt.kind, expected_kind);
            assert_eq!(receipt.source_session_id, expected_session);
            assert_eq!(receipt.provenance, LocalProvenance::Native);
            assert_eq!(receipt.title, format!("Title {ordinal}"));
            assert_eq!(receipt.text, format!("Evidence {ordinal}"));
            assert_eq!(receipt.collection_id.as_deref(), Some("collection-a"));
            assert_eq!(receipt.source_path, Some(format!("/local/{ordinal}.md")));
        }

        store.delete_session(SessionId::new(7))?;
        assert!(matches!(
            store.load_session_record(SessionId::new(7)),
            Err(sotto_core::RagError::NotFound { .. })
        ));
        assert_eq!(
            connection.query_row("SELECT COUNT(*) FROM documents", [], |row| {
                row.get::<_, usize>(0)
            })?,
            2
        );
        assert_eq!(
            connection.query_row(
                "SELECT COUNT(*) FROM documents WHERE source_session_id IS NOT NULL",
                [],
                |row| row.get::<_, usize>(0)
            )?,
            0
        );
        assert_eq!(
            connection.query_row("SELECT COUNT(*) FROM chunks", [], |row| {
                row.get::<_, usize>(0)
            })?,
            2
        );
        assert_eq!(
            connection.query_row("SELECT COUNT(*) FROM vec_chunks", [], |row| {
                row.get::<_, usize>(0)
            })?,
            2
        );
        assert_eq!(
            connection.query_row("SELECT COUNT(*) FROM chunks_fts", [], |row| {
                row.get::<_, usize>(0)
            })?,
            2
        );
        assert!(
            store
                .load_derived_view(SessionId::new(7), "meeting_notes.v1", "backend", "content")?
                .is_none()
        );
        Ok(())
    }
}
