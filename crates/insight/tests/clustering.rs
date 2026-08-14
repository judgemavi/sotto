#![expect(
    clippy::tests_outside_test_module,
    reason = "integration test files compile only under cfg(test)"
)]

mod tests {
    use std::{
        sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        },
        time::Duration,
    };

    use futures_util::stream;
    use insight::{ClusterError, Clusterer, DerivedView, OpenThreadKind};
    use providers::{
        AuthKind, AuthStatus, BackendCapabilities, BackendDescriptor, BackendFingerprint, BackendId,
    };
    use sotto_core::{
        BoxFuture, BoxStream, CancellationToken, CaptureTarget, CompletionProvider,
        CompletionRequest, Delta, EventPayload, ProviderError, Session, SessionId, Source,
        SpeechState, StopReason, TargetKind, TimelineBuilder, Usage, Utterance, VadSegment,
    };

    struct MockProvider {
        calls: Arc<AtomicUsize>,
        output: &'static str,
    }

    fn fingerprint(id: &str) -> Result<BackendFingerprint, Box<dyn std::error::Error>> {
        Ok(BackendDescriptor::new(
            BackendId::new(id)?,
            id,
            "mock-cluster",
            1,
            BackendCapabilities::reasoning_baseline(),
            AuthKind::None,
            AuthStatus::Ready,
        )?
        .fingerprint()
        .clone())
    }

    impl CompletionProvider for MockProvider {
        fn stream(
            &self,
            _request: CompletionRequest,
            _cancellation: CancellationToken,
        ) -> BoxFuture<'_, Result<BoxStream<'static, Result<Delta, ProviderError>>, ProviderError>>
        {
            self.calls.fetch_add(1, Ordering::Relaxed);
            let output = self.output;
            Box::pin(async move {
                Ok(Box::pin(stream::iter(vec![Ok(Delta {
                    text: output.to_owned(),
                    is_final: true,
                    usage: Some(Usage {
                        input_tokens: 80,
                        output_tokens: 20,
                        ..Usage::default()
                    }),
                    stop_reason: Some(StopReason::EndTurn),
                })]))
                    as BoxStream<'static, Result<Delta, ProviderError>>)
            })
        }

        fn model_id(&self) -> &str {
            "mock-cluster"
        }
    }

    fn store() -> Result<(rag::Store, SessionId), Box<dyn std::error::Error>> {
        let store = rag::Store::open_in_memory()?;
        let id = SessionId::new(44);
        let session = Session::new(
            id,
            CaptureTarget {
                bundle_id: Some("com.google.Chrome".to_owned()),
                display_name: "Google Meet".to_owned(),
                window_title: Some("Weekly planning".to_owned()),
                kind: TargetKind::Window,
                audio_scoped: true,
            },
            1,
        );
        store.save_session(&session)?;
        let mut timeline = TimelineBuilder::new(session);
        for (index, text) in [
            "Friday is the launch date",
            "The launch brief still needs an owner",
        ]
        .into_iter()
        .enumerate()
        {
            timeline.append(
                Duration::from_secs(index as u64),
                EventPayload::UtteranceFinal(Utterance {
                    source: if index == 0 {
                        Source::System
                    } else {
                        Source::Mic
                    },
                    start: Duration::from_secs(index as u64),
                    end: Duration::from_secs(index as u64 + 1),
                    text: text.to_owned(),
                    avg_logprob: 0.0,
                    annotations: Vec::new(),
                }),
            );
        }
        timeline.append(
            Duration::from_secs(2),
            EventPayload::Vad(VadSegment {
                source: Source::System,
                start: Duration::from_secs(2),
                end: Some(Duration::from_secs(3)),
                kind: SpeechState::SpeechEnd,
            }),
        );
        store.append_events(timeline.events())?;
        Ok((store, id))
    }

    #[tokio::test]
    async fn unchanged_timeline_is_cached_without_mutating_events()
    -> Result<(), Box<dyn std::error::Error>> {
        let (store, id) = store()?;
        let before = serde_json::to_vec(&store.load_session(id)?)?;
        let calls = Arc::new(AtomicUsize::new(0));
        let clusterer = Clusterer::new(
            &store,
            Arc::new(MockProvider {
                calls: Arc::clone(&calls),
                output: r#"{"regions":[{"label":"Launch planning","event_ids":[1,2]}],"links":[{"from":1,"to":2,"reason":"The later commitment follows the date decision"}],"open_threads":[{"kind":"action_item","event_id":2,"summary":"The launch brief still needs an owner"}]}"#,
            }),
        )
        .with_backend_fingerprint(fingerprint("test.first")?);
        let first = clusterer.cluster(id).await?;
        let second = clusterer.cluster(id).await?;
        let after = serde_json::to_vec(&store.load_session(id)?)?;

        assert!(!first.cached, "first run must invoke the provider");
        assert!(
            second.cached,
            "unchanged content must use the derived-view cache"
        );
        assert_eq!(
            calls.load(Ordering::Relaxed),
            1,
            "cache hit must not spend tokens"
        );
        assert_eq!(
            before, after,
            "clustering must leave the timeline byte-identical"
        );
        assert_eq!(
            first.view, second.view,
            "cached artifact must be idempotent"
        );

        let other_backend = Clusterer::new(
            &store,
            Arc::new(MockProvider {
                calls: Arc::clone(&calls),
                output: r#"{"regions":[{"label":"Launch planning","event_ids":[1,2]}],"links":[{"from":1,"to":2,"reason":"The later commitment follows the date decision"}],"open_threads":[{"kind":"action_item","event_id":2,"summary":"The launch brief still needs an owner"}]}"#,
            }),
        )
        .with_backend_fingerprint(fingerprint("test.second")?)
        .cluster(id)
        .await?;
        assert!(
            !other_backend.cached,
            "the same model through a different backend must not share a cache entry"
        );
        assert_eq!(
            calls.load(Ordering::Relaxed),
            2,
            "distinct backend fingerprints must invoke reasoning independently"
        );
        Ok(())
    }

    #[test]
    fn meeting_general_open_thread_kinds_round_trip() -> Result<(), Box<dyn std::error::Error>> {
        let json = r#"{"regions":[],"links":[],"open_threads":[{"kind":"question","event_id":1,"summary":"Which room is available?"},{"kind":"decision","event_id":2,"summary":"The launch date is not final"},{"kind":"action_item","event_id":3,"summary":"The brief has no owner"},{"kind":"risk","event_id":4,"summary":"The dependency may slip"}]}"#;

        let view: DerivedView = serde_json::from_str(json)?;
        let kinds: Vec<_> = view.open_threads.iter().map(|thread| thread.kind).collect();

        assert_eq!(
            kinds,
            vec![
                OpenThreadKind::Question,
                OpenThreadKind::Decision,
                OpenThreadKind::ActionItem,
                OpenThreadKind::Risk,
            ],
            "all meeting-general open-thread kinds must deserialize"
        );
        assert_eq!(
            serde_json::to_string(&view)?,
            json,
            "meeting-general open-thread kinds must round-trip"
        );
        Ok(())
    }

    #[test]
    fn legacy_objection_kind_is_rejected() {
        let json = r#"{"regions":[],"links":[],"open_threads":[{"kind":"objection","event_id":1,"summary":"Legacy sales vocabulary"}]}"#;
        let result = serde_json::from_str::<DerivedView>(json);

        assert!(
            result.is_err(),
            "the one-way meeting taxonomy must not retain an objection alias"
        );
    }

    #[test]
    fn legacy_root_objections_field_is_rejected() {
        let json = r#"{"regions":[],"links":[],"open_threads":[],"objections":[]}"#;
        let result = serde_json::from_str::<DerivedView>(json);

        assert!(
            result.is_err(),
            "the one-way schema must reject a legacy root objections field"
        );
    }

    #[test]
    fn unknown_nested_fields_are_rejected() {
        let json = r#"{"regions":[{"label":"Planning","event_ids":[1],"confidence":0.9}],"links":[],"open_threads":[]}"#;
        let result = serde_json::from_str::<DerivedView>(json);

        assert!(
            result.is_err(),
            "nested clustering values must reject fields outside the versioned schema"
        );
    }

    #[tokio::test]
    async fn known_non_transcript_event_id_is_not_citable() -> Result<(), Box<dyn std::error::Error>>
    {
        let (store, id) = store()?;
        let calls = Arc::new(AtomicUsize::new(0));
        let result = Clusterer::new(
            &store,
            Arc::new(MockProvider {
                calls,
                output: r#"{"regions":[{"label":"Unsupported evidence","event_ids":[3]}],"links":[],"open_threads":[]}"#,
            }),
        )
        .with_backend_fingerprint(fingerprint("test.unseen-event")?)
        .cluster(id)
        .await;

        assert!(
            matches!(result, Err(ClusterError::UnknownEvent(event_id)) if event_id.get() == 3),
            "a session-known VAD id omitted from the transcript prompt must fail closed"
        );
        Ok(())
    }
}
