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

    /// The schema version every migration path must converge on.
    ///
    /// Named rather than repeated as a literal in each migration test: `rag`'s own
    /// `SCHEMA_VERSION` is `pub(crate)` and so invisible from an integration test, and ten
    /// hand-written copies of the number meant every schema bump failed ten tests for no reason
    /// beyond the stale literal, burying any genuine convergence failure among them.
    const CURRENT_SCHEMA_VERSION: u32 = 15;

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
            CURRENT_SCHEMA_VERSION,
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

    /// The correctness point of T085/T086: a chosen name is a label over the entry that owns a
    /// recording, never an edit of what was recorded. Renaming repeatedly must leave the capture
    /// target byte-identical.
    #[test]
    fn renaming_a_recording_never_rewrites_what_was_captured()
    -> Result<(), Box<dyn std::error::Error>> {
        let store = Store::open_in_memory()?;
        let captured = session();
        store.save_session(&captured)?;
        let entry_id = store.entry_for_session(captured.id())?;

        for name in ["Standup", "BTU standup", "Monday standup"] {
            let title = RecordingTitle::new(name).ok_or("a name is a title")?;
            store.set_entry_title(entry_id, Some(&title))?;
            assert_eq!(
                store.entry_title(entry_id)?.as_ref(),
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

        store.set_entry_title(entry_id, None)?;
        assert_eq!(
            store.entry_title(entry_id)?,
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
    /// `RecordingTitle`, and a title can only exist for an entry that exists.
    #[test]
    fn the_store_refuses_a_blank_title_and_an_orphan_one() -> Result<(), Box<dyn std::error::Error>>
    {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("titles.sqlite3");
        let store = Store::open(&path)?;
        let captured = session();
        store.save_session(&captured)?;
        let entry_id = store.entry_for_session(captured.id())?;
        let title = RecordingTitle::new("Standup").ok_or("a name is a title")?;
        store.set_entry_title(entry_id, Some(&title))?;

        assert!(
            store
                .set_entry_title(EntryId::new(9_999), Some(&title))
                .is_err(),
            "a title for an entry that does not exist must be refused"
        );

        let connection = Connection::open(&path)?;
        connection.execute_batch("PRAGMA foreign_keys = ON;")?;
        assert!(
            connection
                .execute(
                    "UPDATE entries SET title='   ' WHERE id=?1",
                    [entry_id.get().to_string()],
                )
                .is_err(),
            "a whitespace-only title must fail the schema's own check"
        );

        store.delete_session(captured.id())?;
        assert_eq!(
            connection.query_row("SELECT COUNT(*) FROM entries", [], |row| row
                .get::<_, u32>(0))?,
            0,
            "deleting a recording's only session must take its owning entry, and the chosen \
             name on it, with it"
        );
        Ok(())
    }

    #[test]
    fn migrating_version_twelve_creates_one_entry_per_session_idempotently()
    -> Result<(), Box<dyn std::error::Error>> {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("rag-v12.sqlite3");
        {
            let store = Store::open(&path)?;
            store.save_session(&session())?;
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
            entries[0].title(),
            None,
            "a session that predates entries migrates in untitled: `session_titles` and every \
             path that wrote it are retired, so there is nothing left to carry a name forward \
             from"
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
            CURRENT_SCHEMA_VERSION
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
            CURRENT_SCHEMA_VERSION
        );
        Ok(())
    }

    /// A v14 library still carries the retired per-session title table. Leaving it would let a
    /// database claim to be current while holding a table nothing reads, so v15 drops it — and the
    /// titles inside it are deliberately not carried into `entries.title`, because a clean library
    /// was chosen over a compatibility path.
    #[test]
    fn migrating_version_fourteen_drops_the_retired_session_title_table()
    -> Result<(), Box<dyn std::error::Error>> {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("v14-titles.sqlite3");
        {
            let store = Store::open(&path)?;
            store.save_session(&session())?;
        }
        {
            let connection = Connection::open(&path)?;
            connection.execute_batch(
                "CREATE TABLE IF NOT EXISTS session_titles (\
                   session_id TEXT PRIMARY KEY REFERENCES sessions(id) ON DELETE CASCADE,\
                   title TEXT NOT NULL CHECK(length(trim(title))>0),\
                   updated_at_unix_ms INTEGER NOT NULL\
                 );\
                 INSERT INTO session_titles VALUES('7','Named before entries',1);\
                 PRAGMA user_version=14;",
            )?;
        }

        let migrated = Store::open(&path)?;
        assert_eq!(
            migrated.list_sessions()?.len(),
            1,
            "dropping the retired title table must not disturb the captured session"
        );
        drop(migrated);

        let connection = Connection::open(&path)?;
        assert_eq!(
            connection.query_row(
                "SELECT count(*) FROM sqlite_master WHERE type='table' AND name='session_titles'",
                [],
                |row| row.get::<_, u32>(0)
            )?,
            0,
            "the retired per-session title table must not survive the v15 migration"
        );
        assert_eq!(
            connection.query_row("PRAGMA user_version", [], |row| row.get::<_, u32>(0))?,
            CURRENT_SCHEMA_VERSION
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
            CURRENT_SCHEMA_VERSION
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
            CURRENT_SCHEMA_VERSION
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
            CURRENT_SCHEMA_VERSION
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
            CURRENT_SCHEMA_VERSION
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
            CURRENT_SCHEMA_VERSION
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
            CURRENT_SCHEMA_VERSION
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
    fn deleting_unknown_entry_returns_not_found_and_touches_no_file()
    -> Result<(), Box<dyn std::error::Error>> {
        let directory = tempfile::tempdir()?;
        let store = Store::open_in_memory()?;
        let entry = Entry::new(EntryId::new(900), 1_900_000_000_000, None);
        store.create_entry(&entry)?;
        let recorded = session();
        store.save_session_in_entry(&recorded, entry.id())?;
        let recording_path = directory
            .path()
            .join(format!("{}.mp4", recorded.id().get()));
        std::fs::write(&recording_path, b"retained meeting media")?;
        store.save_growing_recording(recorded.id(), &recording_path)?;

        let missing = EntryId::new(901);
        let outcome = store.delete_entry(missing, directory.path());
        assert!(
            matches!(outcome, Err(sotto_core::RagError::NotFound { .. })),
            "an unknown entry id must report NotFound, not a storage failure"
        );
        assert!(
            recording_path.exists(),
            "a delete that never resolves a real entry must not touch any file"
        );
        assert_eq!(
            store.list_entries()?.len(),
            1,
            "the real entry must survive a delete request for a different id"
        );
        Ok(())
    }

    #[test]
    fn delete_entry_restores_every_file_when_the_row_transaction_fails()
    -> Result<(), Box<dyn std::error::Error>> {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("entry-delete-rollback.sqlite3");
        let store = Store::open(&path)?;
        let entry = Entry::new(EntryId::new(120), 1_700_000_000_000, None);
        store.create_entry(&entry)?;
        let first = session();
        let second = Session::new(
            SessionId::new(8),
            CaptureTarget::microphone_only(),
            1_700_000_060_000,
        );
        let mut paths = Vec::new();
        for captured in [&first, &second] {
            store.save_session_in_entry(captured, entry.id())?;
            let recording_path = directory
                .path()
                .join(format!("{}.mp4", captured.id().get()));
            std::fs::write(&recording_path, b"retained meeting media")?;
            store.save_growing_recording(captured.id(), &recording_path)?;
            paths.push(recording_path);
        }

        // Force the row transaction to fail after quarantine has already renamed both files away,
        // simulating any mid-transaction failure (constraint, lock, disk) unrelated to the file
        // system half of the delete.
        let connection = Connection::open(&path)?;
        connection.execute_batch(&format!(
            "CREATE TRIGGER refuse_entry_delete BEFORE DELETE ON entries WHEN old.id='{}' \
             BEGIN SELECT RAISE(ABORT, 'simulated row-transaction failure'); END;",
            entry.id().get()
        ))?;

        let outcome = store.delete_entry(entry.id(), directory.path());
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

        assert_eq!(store.entry_for_session(first.id())?, entry.id());
        assert_eq!(store.entry_for_session(second.id())?, entry.id());
        assert_eq!(
            store.list_entries()?[0].session_ids(),
            &[first.id(), second.id()],
            "the entry must resolve fully intact, not half-deleted"
        );
        Ok(())
    }

    #[test]
    fn recover_quarantined_media_resolves_each_tombstone_from_the_row_that_survived_a_crash()
    -> Result<(), Box<dyn std::error::Error>> {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("recover.sqlite3");
        let store = Store::open(&path)?;

        // Case 1: the crash happened before the row-side commit reached disk. The session and its
        // recording's path both still resolve to this file, so recovery must restore it.
        let uncommitted = Session::new(
            SessionId::new(30),
            CaptureTarget::microphone_only(),
            1_700_000_000_000,
        );
        store.save_session(&uncommitted)?;
        let uncommitted_path = directory
            .path()
            .join(format!("{}.mp4", uncommitted.id().get()));
        std::fs::write(&uncommitted_path, b"still referenced media")?;
        store.save_growing_recording(uncommitted.id(), &uncommitted_path)?;
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
        store.save_session(&cleared_row)?;
        let cleared_row_path = directory
            .path()
            .join(format!("{}.mp4", cleared_row.id().get()));
        std::fs::write(&cleared_row_path, b"already-deleted-per-row media")?;
        store.save_growing_recording(cleared_row.id(), &cleared_row_path)?;
        let cleared_row_tombstone = directory
            .path()
            .join(format!(".{}.deleting", cleared_row.id().get()));
        std::fs::rename(&cleared_row_path, &cleared_row_tombstone)?;
        Connection::open(&path)?.execute(
            "UPDATE session_recordings SET state='deleted',path=NULL,container=NULL WHERE session_id=?1",
            [cleared_row.id().get().to_string()],
        )?;

        // Case 3: an entry-level delete's row transaction committed and removed the session row
        // entirely before the crash. There is nothing left to restore to, so recovery must finish
        // the unlink exactly as it does when only the recording row was cleared.
        let gone = Session::new(
            SessionId::new(32),
            CaptureTarget::microphone_only(),
            1_700_000_000_002,
        );
        store.save_session(&gone)?;
        let gone_path = directory.path().join(format!("{}.mp4", gone.id().get()));
        std::fs::write(&gone_path, b"orphaned media")?;
        store.save_growing_recording(gone.id(), &gone_path)?;
        let gone_tombstone = directory
            .path()
            .join(format!(".{}.deleting", gone.id().get()));
        std::fs::rename(&gone_path, &gone_tombstone)?;
        store.delete_session(gone.id())?;

        drop(store);
        let restarted = Store::open(&path)?;
        let resolved = restarted.recover_quarantined_media(directory.path())?;
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
                .load_recording_reference(uncommitted.id())?
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
    #[test]
    fn a_migration_step_that_fails_does_not_strand_the_earlier_commit_at_the_final_version()
    -> Result<(), Box<dyn std::error::Error>> {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("rag-v11-interrupted.sqlite3");
        {
            let store = Store::open(&path)?;
            store.save_session(&session())?;
        }
        {
            let connection = Connection::open(&path)?;
            connection.execute_batch(
                "DROP TABLE entry_sessions;
                 DROP TABLE entries;
                 CREATE TABLE entries(id TEXT PRIMARY KEY NOT NULL);
                 PRAGMA user_version=11;",
            )?;
        }

        assert!(
            Store::open(&path).is_err(),
            "a genuine schema conflict in the last migration step must surface as an error, not \
             a silently skipped step"
        );
        assert_eq!(
            Connection::open(&path)?
                .query_row("PRAGMA user_version", [], |row| row.get::<_, u32>(0))?,
            12,
            "the session_search_policy drop (v11 -> v12) must be durably parked at its own \
             checkpoint, not stranded at the final schema version by a step that never committed"
        );

        Connection::open(&path)?.execute_batch("DROP TABLE entries;")?;
        let recovered = Store::open(&path)?;
        assert_eq!(
            Connection::open(&path)?
                .query_row("PRAGMA user_version", [], |row| row.get::<_, u32>(0))?,
            CURRENT_SCHEMA_VERSION,
            "resuming from the correctly-parked intermediate version must converge to the \
             current schema"
        );
        let entries = recovered.list_entries()?;
        assert_eq!(
            entries.len(),
            1,
            "the entries backfill must run to completion on the next open"
        );

        let another = Session::new(
            SessionId::new(42),
            CaptureTarget::microphone_only(),
            1_800_000_000_000,
        );
        recovered.save_session(&another)?;
        assert_eq!(
            recovered.list_entries()?.len(),
            2,
            "a fresh recording must succeed once entries is genuinely complete — this is exactly \
             the 'no such table: entries' failure a stranded version would have produced"
        );
        Ok(())
    }

    /// Regression for the `delete_entry` quarantine race: the set of sessions whose media is
    /// quarantined and the set whose rows are deleted must be the same set even when another
    /// thread attaches a session to the entry at the same moment. Before the fix, membership was
    /// resolved once under the reader lock (snapshotting only `first`), released, and re-resolved
    /// live inside the row transaction — so a session attached in that gap had its row deleted
    /// without its media ever being quarantined, permanently orphaning the file.
    ///
    /// `second` is inserted directly (bypassing the usual implicit-entry creation) so it starts
    /// out fully formed — row, registered growing recording, file on disk — but attached to no
    /// entry, which is exactly what lets a single `attach_session` call race `delete_entry`
    /// without any secondary "row exists but recording not yet registered" gap confusing the
    /// result. Because `delete_entry` now holds the writer lock across membership resolution,
    /// quarantine, and the row commit, the two calls can never interleave: either the attach
    /// commits first (and `second` must vanish, row and file together) or `delete_entry` commits
    /// first, `second`'s entry has gone by the time the attach runs, and it is refused outright
    /// (and `second` must survive, row and file together). Both outcomes are asserted; only a
    /// split result — file gone but row alive, or row gone but file alive — is a failure.
    #[test]
    fn deleting_an_entry_cannot_diverge_the_quarantined_set_from_the_deleted_set()
    -> Result<(), Box<dyn std::error::Error>> {
        for attempt in 0_u128..8 {
            let directory = tempfile::tempdir()?;
            let path = directory.path().join("race.sqlite3");
            let store = std::sync::Arc::new(Store::open(&path)?);

            let entry = Entry::new(EntryId::new(500 + attempt), 1_700_000_000_000, None);
            store.create_entry(&entry)?;
            let first = Session::new(
                SessionId::new(1000 + attempt * 10),
                CaptureTarget::microphone_only(),
                1_700_000_000_000,
            );
            store.save_session_in_entry(&first, entry.id())?;
            let first_path = directory.path().join(format!("{}.mp4", first.id().get()));
            std::fs::write(&first_path, b"first retained media")?;
            store.save_growing_recording(first.id(), &first_path)?;

            let second_id = SessionId::new(1000 + attempt * 10 + 1);
            Connection::open(&path)?.execute(
                "INSERT INTO sessions VALUES(?1,?2,NULL,NULL,'Microphone only',NULL,'microphone',1)",
                params![second_id.get().to_string(), 1_700_000_000_001_i64],
            )?;
            let second_path = directory.path().join(format!("{}.mp4", second_id.get()));
            std::fs::write(&second_path, b"second retained media")?;
            store.save_growing_recording(second_id, &second_path)?;

            let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
            let attach_outcome = {
                let store = std::sync::Arc::clone(&store);
                let barrier = std::sync::Arc::clone(&barrier);
                let entry_id = entry.id();
                std::thread::spawn(move || {
                    barrier.wait();
                    store.attach_session(entry_id, second_id)
                })
            };
            let delete_outcome = {
                let store = std::sync::Arc::clone(&store);
                let barrier = std::sync::Arc::clone(&barrier);
                let entry_id = entry.id();
                let recording_directory = directory.path().to_path_buf();
                std::thread::spawn(move || {
                    barrier.wait();
                    store.delete_entry(entry_id, &recording_directory)
                })
            };
            let attach_outcome = attach_outcome
                .join()
                .map_err(|_| "attach thread panicked")?;
            let delete_outcome = delete_outcome
                .join()
                .map_err(|_| "delete thread panicked")?;

            assert!(
                delete_outcome.is_ok(),
                "attempt {attempt}: deleting the entry's own original member must always succeed"
            );
            assert!(
                !first_path.exists(),
                "attempt {attempt}: the entry's original member must always be cascaded away"
            );

            let second_row_survives = store.load_session_record(second_id).is_ok();
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
