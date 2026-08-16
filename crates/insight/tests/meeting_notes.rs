#![expect(
    clippy::tests_outside_test_module,
    reason = "integration test files compile only under cfg(test)"
)]

mod tests {
    use std::{
        collections::VecDeque,
        sync::{
            Arc, Mutex,
            atomic::{AtomicUsize, Ordering},
        },
        time::Duration,
    };

    use futures_util::stream;
    use insight::{
        MeetingNotesGenerator, NotesOverlayOperation, RecordingNotesSectionKind,
        append_notes_overlay_operation,
    };
    use providers::{AuthKind, AuthStatus, BackendCapabilities, BackendDescriptor, BackendId};
    use sotto_core::{
        BoxFuture, BoxStream, CancellationToken, CaptureTarget, CompletionProvider,
        CompletionRequest, Delta, EventPayload, ProviderError, Session, SessionId, Source,
        StopReason, TargetKind, TimelineBuilder, Usage, Utterance,
    };

    struct QueueProvider {
        outputs: Mutex<VecDeque<String>>,
        calls: AtomicUsize,
        inputs: Mutex<Vec<String>>,
    }

    impl QueueProvider {
        fn new(outputs: impl IntoIterator<Item = &'static str>) -> Self {
            Self {
                outputs: Mutex::new(outputs.into_iter().map(ToOwned::to_owned).collect()),
                calls: AtomicUsize::new(0),
                inputs: Mutex::new(Vec::new()),
            }
        }
    }

    impl CompletionProvider for QueueProvider {
        fn stream(
            &self,
            request: CompletionRequest,
            _cancellation: CancellationToken,
        ) -> BoxFuture<'_, Result<BoxStream<'static, Result<Delta, ProviderError>>, ProviderError>>
        {
            self.calls.fetch_add(1, Ordering::Relaxed);
            if let Some(message) = request.messages.first()
                && let Ok(mut inputs) = self.inputs.lock()
            {
                inputs.push(message.content.clone());
            }
            let output = self
                .outputs
                .lock()
                .map_err(|_| ProviderError::Network("notes output queue poisoned".to_owned()))
                .and_then(|mut outputs| {
                    outputs.pop_front().ok_or_else(|| {
                        ProviderError::Network("missing queued notes output".to_owned())
                    })
                });
            Box::pin(async move {
                let output = output?;
                Ok(Box::pin(stream::iter([Ok(Delta {
                    text: output,
                    is_final: true,
                    usage: Some(Usage {
                        input_tokens: 10,
                        output_tokens: 5,
                        ..Usage::default()
                    }),
                    stop_reason: Some(StopReason::EndTurn),
                })]))
                    as BoxStream<'static, Result<Delta, ProviderError>>)
            })
        }

        fn model_id(&self) -> &str {
            "mock-meeting-notes"
        }
    }

    fn fingerprint(id: &str) -> Result<providers::BackendFingerprint, Box<dyn std::error::Error>> {
        Ok(BackendDescriptor::new(
            BackendId::new(id)?,
            id,
            "mock-meeting-notes",
            1,
            BackendCapabilities::reasoning_baseline(),
            AuthKind::None,
            AuthStatus::Ready,
        )?
        .fingerprint()
        .clone())
    }

    async fn store_with_utterances(
        starts: &[u64],
    ) -> Result<(rag::Store, SessionId), Box<dyn std::error::Error>> {
        let store = rag::Store::open_in_memory().await?;
        let session_id = SessionId::new(37);
        let session = Session::new(
            session_id,
            CaptureTarget {
                bundle_id: Some("us.zoom.xos".to_owned()),
                display_name: "Zoom".to_owned(),
                window_title: Some("Weekly planning".to_owned()),
                kind: TargetKind::Window,
                audio_scoped: true,
            },
            1,
        );
        store.save_session(&session).await?;
        let mut timeline = TimelineBuilder::new(session);
        for (index, start) in starts.iter().copied().enumerate() {
            timeline.append(
                Duration::from_secs(start),
                EventPayload::UtteranceFinal(Utterance {
                    source: if index.is_multiple_of(2) {
                        Source::Mic
                    } else {
                        Source::System
                    },
                    start: Duration::from_secs(start),
                    end: Duration::from_secs(start.saturating_add(2)),
                    text: if index == 0 {
                        "Morgan will prepare the launch checklist".to_owned()
                    } else {
                        "The rollout risk remains open".to_owned()
                    },
                    avg_logprob: 0.0,
                    annotations: Vec::new(),
                }),
            );
        }
        store.append_events(timeline.events()).await?;
        Ok((store, session_id))
    }

    #[tokio::test]
    async fn adaptive_reduce_input_never_exposes_sotto_owned_block_ids()
    -> Result<(), Box<dyn std::error::Error>> {
        let (store, session_id) = store_with_utterances(&[1, 1_201]).await?;
        let first_window = r#"{"sections":[{"kind":"overview","blocks":[{"type":"claim","text":"Launch planning began","meeting_citations":[1]}]}]}"#;
        let second_window = r#"{"sections":[{"kind":"risks","blocks":[{"type":"claim","text":"Rollout risk was discussed","meeting_citations":[2]}]}]}"#;
        let reduced = r#"{"sections":[{"kind":"overview","blocks":[{"type":"claim","text":"The meeting covered launch planning and rollout risk","meeting_citations":[1,2]}]}]}"#;
        let provider = Arc::new(QueueProvider::new([first_window, second_window, reduced]));

        MeetingNotesGenerator::new(&store, provider.clone())
            .with_backend_fingerprint(fingerprint("test.notes.adaptive-reduce")?)
            .generate_grounded_with_cancellation(session_id, None, CancellationToken::new())
            .await?;

        let inputs = provider.inputs.lock().map_err(|_| "inputs poisoned")?;
        assert_eq!(inputs.len(), 3);
        assert!(inputs[2].contains("Launch planning began"));
        assert!(inputs[2].contains("Rollout risk was discussed"));
        assert!(
            !inputs[2].contains("\"id\""),
            "reduce receives provider draft blocks, not finalized Sotto identities"
        );
        Ok(())
    }

    #[tokio::test]
    async fn serialized_reasoning_request_never_contains_overlay_content()
    -> Result<(), Box<dyn std::error::Error>> {
        let (store, session_id) = store_with_utterances(&[1]).await?;
        let output = r#"{"sections":[{"kind":"overview","blocks":[{"type":"claim","text":"Launch planning began","meeting_citations":[1]}]}]}"#;
        let first_provider = Arc::new(QueueProvider::new([output]));
        let first = MeetingNotesGenerator::new(&store, first_provider)
            .with_backend_fingerprint(fingerprint("test.notes.before-overlay")?)
            .generate_grounded_with_cancellation(session_id, None, CancellationToken::new())
            .await?;
        let entry_id = store.entry_for_session(session_id).await?;
        let secret = "USER-OVERLAY-MUST-NOT-REACH-THE-MODEL";
        append_notes_overlay_operation(
            &store,
            entry_id,
            &first.artifact,
            &NotesOverlayOperation::Add {
                user_block_id: "private-user-block".to_owned(),
                section: RecordingNotesSectionKind::Overview,
                text: secret.to_owned(),
                action: false,
                owner: None,
                due_date: None,
            },
            10,
        )
        .await?;

        let second_provider = Arc::new(QueueProvider::new([output]));
        MeetingNotesGenerator::new(&store, second_provider.clone())
            .with_backend_fingerprint(fingerprint("test.notes.after-overlay")?)
            .generate_grounded_with_cancellation(session_id, None, CancellationToken::new())
            .await?;
        let inputs = second_provider
            .inputs
            .lock()
            .map_err(|_| "inputs poisoned")?;
        assert!(!inputs.iter().any(|input| input.contains(secret)));
        assert!(
            !inputs
                .iter()
                .any(|input| input.contains("private-user-block"))
        );
        Ok(())
    }
}
