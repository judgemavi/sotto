#[cfg(test)]
mod tests {
    use std::{
        collections::VecDeque,
        sync::{
            Arc, Mutex,
            atomic::{AtomicUsize, Ordering},
        },
        time::Duration,
    };

    use insight::{ContextMode, Pricing, Summarizer, SummaryError};
    use screen::{
        InspectScreenRequest, ScreenInspection, ScreenInspectionSource, ScreenPrecision,
        ScreenProvenance,
    };
    use sotto_core::{
        Annotation, BoxFuture, BoxStream, CancellationToken, CaptureTarget, CompletionProvider,
        CompletionRequest, Delta, EventPayload, ProviderError, Session, SessionId, Source,
        StopReason, TargetKind, TimelineBuilder, Usage, Utterance,
    };

    const RECAP: &str = r#"{"attendees":[],"talk_time":{"rep_percent":45,"customer_percent":55},"topics":[{"text":"pricing","citations":[1]}],"customer_questions":[],"objections":[{"text":"price","resolved":true,"resolution":"support included","citations":[1,2]}],"commitments":[],"next_steps":[],"competitor_mentions":[]}"#;

    struct MockProvider;

    impl CompletionProvider for MockProvider {
        fn stream(
            &self,
            _request: CompletionRequest,
            _cancellation: CancellationToken,
        ) -> BoxFuture<'_, Result<BoxStream<'static, Result<Delta, ProviderError>>, ProviderError>>
        {
            Box::pin(async {
                Ok(Box::pin(futures_util::stream::iter([Ok(Delta {
                    text: RECAP.to_owned(),
                    is_final: true,
                    usage: Some(Usage {
                        input_tokens: 100,
                        output_tokens: 50,
                        cache_read_tokens: 0,
                        cache_write_tokens: 0,
                    }),
                    stop_reason: Some(StopReason::EndTurn),
                })]))
                    as BoxStream<'static, Result<Delta, ProviderError>>)
            })
        }

        fn model_id(&self) -> &str {
            "mock-summary"
        }
    }

    struct QueueProvider {
        outputs: Mutex<VecDeque<String>>,
        inputs: Arc<Mutex<Vec<String>>>,
    }

    impl CompletionProvider for QueueProvider {
        fn stream(
            &self,
            request: CompletionRequest,
            _cancellation: CancellationToken,
        ) -> BoxFuture<'_, Result<BoxStream<'static, Result<Delta, ProviderError>>, ProviderError>>
        {
            let output = self
                .outputs
                .lock()
                .map_err(|_| ProviderError::Network("output queue poisoned".to_owned()))
                .and_then(|mut outputs| {
                    outputs
                        .pop_front()
                        .ok_or_else(|| ProviderError::Network("missing queued output".to_owned()))
                });
            if let Some(message) = request.messages.first()
                && let Ok(mut inputs) = self.inputs.lock()
            {
                inputs.push(message.content.clone());
            }
            Box::pin(async move {
                let output = output?;
                Ok(Box::pin(futures_util::stream::iter([Ok(Delta {
                    text: output,
                    is_final: true,
                    usage: Some(Usage::default()),
                    stop_reason: Some(StopReason::EndTurn),
                })]))
                    as BoxStream<'static, Result<Delta, ProviderError>>)
            })
        }

        fn model_id(&self) -> &str {
            "mock-summary"
        }
    }

    struct FakeInspector {
        calls: AtomicUsize,
    }

    impl ScreenInspectionSource for FakeInspector {
        fn inspect(
            &self,
            _events: &[sotto_core::TimelineEvent],
            request: &InspectScreenRequest,
        ) -> ScreenInspection {
            self.calls.fetch_add(1, Ordering::Relaxed);
            ScreenInspection::Available {
                provenance: ScreenProvenance {
                    requested: request.selector.clone(),
                    captured_at: Duration::from_secs(10),
                    visible_from: Duration::from_secs(10),
                    visible_to: Some(Duration::from_secs(20)),
                    snapshot_event_id: sotto_core::EventId::new(3),
                    frame_ref: sotto_core::FrameRef::new("/private/cache/frame.png"),
                    precision: ScreenPrecision::SampledChangeFrame,
                },
                ocr_text: Some("Enterprise pricing slide".to_owned()),
                authorized_image: None,
            }
        }
    }

    fn persisted_store() -> Result<rag::Store, Box<dyn std::error::Error>> {
        let store = rag::Store::open_in_memory()?;
        let session = Session::new(
            SessionId::new(7),
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
        timeline.append(
            Duration::from_secs(1),
            EventPayload::UtteranceFinal(Utterance {
                source: Source::System,
                start: Duration::from_secs(1),
                end: Duration::from_secs(3),
                text: "The price is too high.".to_owned(),
                avg_logprob: 0.0,
                annotations: vec![Annotation::Hesitant],
            }),
        );
        timeline.append(
            Duration::from_secs(4),
            EventPayload::UtteranceFinal(Utterance {
                source: Source::Mic,
                start: Duration::from_secs(4),
                end: Duration::from_secs(6),
                text: "Migration and support are included.".to_owned(),
                avg_logprob: 0.0,
                annotations: Vec::new(),
            }),
        );
        store.append_events(timeline.events())?;
        Ok(store)
    }

    #[tokio::test]
    async fn summarizes_persisted_timeline_with_valid_citations_usage_and_cost()
    -> Result<(), Box<dyn std::error::Error>> {
        let store = persisted_store()?;
        let report = Summarizer::new(&store, Arc::new(MockProvider))
            .with_pricing(Pricing {
                input_per_million_usd: 10.0,
                output_per_million_usd: 20.0,
                cache_read_per_million_usd: 0.0,
                cache_write_per_million_usd: 0.0,
            })
            .summarize(SessionId::new(7))
            .await?;
        assert_eq!(
            report.recap.objections.len(),
            1,
            "objection must survive structured parsing"
        );
        assert!(
            report.recap.objections[0].resolved,
            "resolution must remain explicit"
        );
        assert_eq!(report.usage.input_tokens, 100, "usage must be visible");
        assert!(
            (report
                .cost
                .ok_or("configured pricing must produce cost")?
                .usd
                - 0.002)
                .abs()
                < f64::EPSILON,
            "configured token prices must produce a visible cost"
        );
        Ok(())
    }

    #[tokio::test]
    async fn image_mode_fails_explicitly_instead_of_sending_frame_paths_as_text()
    -> Result<(), Box<dyn std::error::Error>> {
        let store = persisted_store()?;
        let result = Summarizer::new(&store, Arc::new(MockProvider))
            .with_context(ContextMode::MetadataAndImages)
            .summarize(SessionId::new(7))
            .await;
        assert!(
            matches!(result, Err(SummaryError::ImageContextUnsupported)),
            "text-only provider contract must make the unrun image arm explicit"
        );
        Ok(())
    }

    #[tokio::test]
    async fn screen_evidence_is_absent_initially_and_added_only_after_typed_request()
    -> Result<(), Box<dyn std::error::Error>> {
        let store = persisted_store()?;
        let inputs = Arc::new(Mutex::new(Vec::new()));
        let provider = Arc::new(QueueProvider {
            outputs: Mutex::new(VecDeque::from([
                r#"{"action":"inspect_screen","timestamp_seconds":15.0,"evidence":"local_ocr","reason":"verify the pricing slide"}"#.to_owned(),
                RECAP.to_owned(),
            ])),
            inputs: Arc::clone(&inputs),
        });
        let inspector = Arc::new(FakeInspector {
            calls: AtomicUsize::new(0),
        });
        let report = Summarizer::new(&store, provider)
            .with_screen_inspector(inspector.clone())
            .summarize(SessionId::new(7))
            .await?;

        assert_eq!(
            report.calls, 2,
            "inspection requires exactly one second pass"
        );
        assert_eq!(
            inspector.calls.load(Ordering::Relaxed),
            1,
            "OCR inspection must run only after the typed request"
        );
        let inputs = inputs.lock().map_err(|_| "input capture poisoned")?;
        let initial = inputs.first().ok_or("missing initial request")?;
        assert!(
            initial.contains("from:1.000s to:3.000s"),
            "initial transcript must carry start and end timestamps"
        );
        assert!(
            !initial.contains("screen at:"),
            "initial context must omit screen snapshot rows"
        );
        assert!(
            !initial.contains("OCR"),
            "initial context must omit eager OCR text"
        );
        assert!(
            !initial.contains("frame.png"),
            "initial context must omit local frame references"
        );
        let second = inputs.get(1).ok_or("missing second request")?;
        assert!(
            second.contains("requested=timestamp:15.000s"),
            "second pass must retain the requested timestamp"
        );
        assert!(
            second.contains("captured_at=10.000s"),
            "second pass must distinguish sampled capture time"
        );
        assert!(
            second.contains("visible_interval=[10.000s,20.000s)"),
            "second pass must expose the resolved visibility interval"
        );
        assert!(
            second.contains("snapshot_event=3"),
            "second pass must cite the snapshot event"
        );
        assert!(
            second.contains("precision=sampled_change_frame"),
            "second pass must not claim exact-video precision"
        );
        assert!(
            second.contains("Local OCR: Enterprise pricing slide"),
            "second pass must include explicitly requested local OCR"
        );
        assert!(
            !second.contains("/private/cache/frame.png"),
            "local frame paths must not masquerade as provider image content"
        );
        Ok(())
    }

    #[tokio::test]
    async fn no_inspection_action_does_not_invoke_screen_work()
    -> Result<(), Box<dyn std::error::Error>> {
        let store = persisted_store()?;
        let inputs = Arc::new(Mutex::new(Vec::new()));
        let provider = Arc::new(QueueProvider {
            outputs: Mutex::new(VecDeque::from([RECAP.to_owned()])),
            inputs: Arc::clone(&inputs),
        });
        let inspector = Arc::new(FakeInspector {
            calls: AtomicUsize::new(0),
        });
        let report = Summarizer::new(&store, provider)
            .with_screen_inspector(inspector.clone())
            .summarize(SessionId::new(7))
            .await?;

        assert_eq!(
            report.calls, 1,
            "a completed first pass must not create an inspection pass"
        );
        assert_eq!(
            inspector.calls.load(Ordering::Relaxed),
            0,
            "no inspect_screen action means no OCR or image inspection work"
        );
        assert_eq!(
            inputs.lock().map_err(|_| "input capture poisoned")?.len(),
            1,
            "reasoning must stop after the completed transcript-first request"
        );
        Ok(())
    }

    #[tokio::test]
    async fn two_hour_timeline_is_mapped_in_bounded_windows_then_reduced()
    -> Result<(), Box<dyn std::error::Error>> {
        let store = rag::Store::open_in_memory()?;
        let session = Session::new(
            SessionId::new(8),
            CaptureTarget {
                bundle_id: None,
                display_name: "Meet".to_owned(),
                window_title: Some("Long call".to_owned()),
                kind: TargetKind::Window,
                audio_scoped: true,
            },
            1,
        );
        store.save_session(&session)?;
        let mut timeline = TimelineBuilder::new(session);
        for index in 0_u64..=6 {
            let start = Duration::from_secs(index * 20 * 60);
            timeline.append(
                start,
                EventPayload::UtteranceFinal(Utterance {
                    source: if index % 2 == 0 {
                        Source::Mic
                    } else {
                        Source::System
                    },
                    start,
                    end: start + Duration::from_secs(2),
                    text: format!("window {index}"),
                    avg_logprob: 0.0,
                    annotations: Vec::new(),
                }),
            );
        }
        store.append_events(timeline.events())?;
        let report = Summarizer::new(&store, Arc::new(MockProvider))
            .summarize(SessionId::new(8))
            .await?;
        assert_eq!(
            report.calls, 8,
            "seven bounded map calls plus one reduce call are required"
        );
        assert_eq!(
            report.usage.input_tokens, 800,
            "usage must aggregate across map-reduce calls"
        );
        Ok(())
    }
}
