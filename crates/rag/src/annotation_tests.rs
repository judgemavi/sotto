use std::time::Duration;

use sotto_core::{
    CaptureTarget, EventPayload, MarkKind, Session, SessionId, Source, TargetKind, TimelineBuilder,
    Utterance, replay_lenient,
};

use sea_orm::TransactionTrait;

use crate::store_async::{Store, execute, query_one, remove_superseded_prior_meetings};

#[tokio::test]
async fn completed_annotations_append_and_supersede_across_reopen()
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
    let store = Store::open(&database).await?;
    store.save_session(timeline.session()).await?;
    store.append_events(timeline.events()).await?;
    let original = store
        .append_completed_annotation(
            session_id,
            anchor.id(),
            "Confirm by Friday",
            MarkKind::Note,
            None,
        )
        .await?;
    let edit = store
        .append_completed_annotation(
            session_id,
            anchor.id(),
            "Confirm by Thursday",
            MarkKind::Important,
            Some(original.id()),
        )
        .await?;
    drop(store);

    let reopened = Store::open(&database).await?;
    let events = reopened.load_session(session_id).await?;
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
        .render_annotated_prior_meeting(session_id)
        .await?
        .ok_or("an annotated recording must render")?;
    assert!(search_text.contains("[Your note] \"Confirm by Thursday\""));
    assert!(!search_text.contains("Confirm by Friday"));

    for (id, hash) in [("old-prior", "old-hash"), ("current-prior", "current-hash")] {
        execute(
                &reopened.writer,
                "INSERT INTO documents(id,kind,title,source_path,collection_id,source_session_id,provenance_status,ingested_at,content_hash) VALUES(?,'prior_meeting','Meeting',NULL,NULL,?,'native',0,?)",
                vec![
                    id.to_owned().into(),
                    session_id.get().to_string().into(),
                    hash.to_owned().into(),
                ],
            ).await?;
    }
    let transaction = reopened.writer.begin().await?;
    remove_superseded_prior_meetings(&transaction, session_id, "current-hash").await?;
    remove_superseded_prior_meetings(&transaction, session_id, "current-hash").await?;
    transaction.commit().await?;
    let row = query_one(
            &reopened.writer,
            "SELECT COUNT(*) AS count FROM documents WHERE kind='prior_meeting' AND source_session_id=?",
            vec![session_id.get().to_string().into()],
        ).await?.ok_or("count row missing")?;
    assert_eq!(
        row.try_get_by::<i64, _>("count")?,
        1,
        "refresh cleanup must be idempotent and retain only current evidence",
    );
    Ok(())
}
