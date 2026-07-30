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
    use insight::Clusterer;
    use sotto_core::{
        BoxFuture, BoxStream, CancellationToken, CaptureTarget, CompletionProvider,
        CompletionRequest, Delta, EventPayload, ProviderError, Session, SessionId, Source,
        StopReason, TargetKind, TimelineBuilder, Usage, Utterance,
    };

    struct MockProvider {
        calls: Arc<AtomicUsize>,
    }

    impl CompletionProvider for MockProvider {
        fn stream(
            &self,
            _request: CompletionRequest,
            _cancellation: CancellationToken,
        ) -> BoxFuture<'_, Result<BoxStream<'static, Result<Delta, ProviderError>>, ProviderError>>
        {
            self.calls.fetch_add(1, Ordering::Relaxed);
            Box::pin(async {
                let output = r#"{"regions":[{"label":"Pricing","event_ids":[1,2]}],"links":[{"from":1,"to":2,"reason":"The response answers the pricing objection"}],"open_threads":[]}"#;
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
                bundle_id: Some("us.zoom.xos".to_owned()),
                display_name: "Zoom".to_owned(),
                window_title: Some("Acme".to_owned()),
                kind: TargetKind::Window,
                audio_scoped: true,
            },
            1,
        );
        store.save_session(&session)?;
        let mut timeline = TimelineBuilder::new(session);
        for (index, text) in ["The price is too high", "Migration is included"]
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
            }),
        );
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
        Ok(())
    }
}
