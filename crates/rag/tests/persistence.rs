#[cfg(test)]
mod tests {
    use std::time::Duration;

    use rag::Store;
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
}
