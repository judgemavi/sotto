#![expect(
    clippy::tests_outside_test_module,
    reason = "integration test files compile only under cfg(test)"
)]

mod tests {
    use std::time::Duration;

    use rag::Store;
    use rusqlite::Connection;
    use sotto_core::{
        CaptureTarget, EventPayload, Session, SessionId, Source, SpeechState, TargetKind,
        TimelineBuilder, VadSegment,
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
            3
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
            3
        );
        Ok(())
    }
}
