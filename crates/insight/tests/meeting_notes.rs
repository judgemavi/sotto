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
    use insight::{MeetingNotesError, MeetingNotesGenerator};
    use providers::{
        AuthKind, AuthStatus, BackendCapabilities, BackendDescriptor, BackendFingerprint,
        BackendId, Registry, Role,
        codex::{CodexDescriptorMode, CodexProbe, CodexProvider},
    };
    use sotto_core::{
        BoxFuture, BoxStream, CancellationToken, CaptureTarget, CompletionProvider,
        CompletionRequest, Delta, EventPayload, ProviderError, Session, SessionId, Source,
        StopReason, TargetKind, TimelineBuilder, Usage, Utterance,
    };

    const VALID_NOTES: &str = r#"{"overview":[{"text":"The team planned the launch","citations":[1]}],"topics":[{"text":"Launch readiness","citations":[1]}],"decisions":[],"action_items":[{"text":"Morgan will prepare the checklist","citations":[1],"owner":"Morgan","owner_citations":[1],"due_date":null,"due_date_citations":[]}],"open_questions":[],"risks":[],"follow_ups":[]}"#;

    struct QueueProvider {
        outputs: Mutex<VecDeque<String>>,
        calls: AtomicUsize,
        inputs: Mutex<Vec<String>>,
    }

    struct CancellationProvider {
        observed: AtomicUsize,
    }

    impl CompletionProvider for CancellationProvider {
        fn stream(
            &self,
            _request: CompletionRequest,
            cancellation: CancellationToken,
        ) -> BoxFuture<'_, Result<BoxStream<'static, Result<Delta, ProviderError>>, ProviderError>>
        {
            self.observed
                .store(usize::from(cancellation.is_cancelled()), Ordering::Release);
            Box::pin(async { Err(ProviderError::Cancelled) })
        }

        fn model_id(&self) -> &str {
            "mock-cancellation"
        }
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

    fn fingerprint(id: &str) -> Result<BackendFingerprint, Box<dyn std::error::Error>> {
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

    fn store_with_utterances(
        starts: &[u64],
    ) -> Result<(rag::Store, SessionId), Box<dyn std::error::Error>> {
        let store = rag::Store::open_in_memory()?;
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
        store.save_session(&session)?;
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
        store.append_events(timeline.events())?;
        Ok((store, session_id))
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn experimental_codex_subscription_generates_cited_notes_without_an_api_key()
    -> Result<(), Box<dyn std::error::Error>> {
        use std::os::unix::fs::PermissionsExt;

        let (store, session_id) = store_with_utterances(&[1])?;
        let directory = std::env::temp_dir().join(format!(
            "sotto-t047-codex-notes-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)?
                .as_nanos()
        ));
        std::fs::create_dir(&directory)?;
        let executable = directory.join("codex");
        let script = format!(
            r#"#!/bin/sh
set -eu
case "${{1:-}}" in
  --version) printf '%s\n' 'codex-cli test'; exit 0 ;;
  login) printf '%s\n' 'Logged in using ChatGPT'; exit 0 ;;
esac
args=" $* "
case "$args" in *' --sandbox read-only '*) ;; *) exit 21 ;; esac
case "$args" in *' --ephemeral '*) ;; *) exit 22 ;; esac
case "$args" in *' --output-schema '*) exit 23 ;; esac
case "$args" in *' -c web_search=disabled '*) ;; *) exit 27 ;; esac
case "$args" in *' --disable shell_tool '*) ;; *) exit 24 ;; esac
case "$args" in *' --disable unified_exec '*) ;; *) exit 25 ;; esac
prompt=$(cat)
case "$prompt" in *'event:1'*) ;; *) exit 26 ;; esac
printf '%s\n' '{{"type":"item.completed","item":{{"type":"agent_message","text":{notes:?}}}}}'
printf '%s\n' '{{"type":"turn.completed","usage":{{"input_tokens":10,"cached_input_tokens":0,"output_tokens":5}}}}'
"#,
            notes = VALID_NOTES
        );
        std::fs::write(&executable, script)?;
        let mut permissions = std::fs::metadata(&executable)?.permissions();
        permissions.set_mode(0o700);
        std::fs::set_permissions(&executable, permissions)?;

        let provider =
            CodexProvider::new(&executable, "fake-codex-meeting").with_experimental_user_opt_in();
        let probe = provider.probe().await;
        assert_eq!(
            probe.login_status,
            AuthStatus::Ready,
            "the fake supported login check must establish ChatGPT readiness"
        );
        let descriptor = provider.descriptor_with_mode(
            &CodexProbe {
                version: probe.version,
                login_status: probe.login_status,
            },
            CodexDescriptorMode::ExperimentalUserOptIn,
        )?;
        let backend_id = descriptor.id().clone();
        let mut registry = Registry::default();
        registry.register_reasoning(descriptor.clone(), Arc::new(provider))?;
        registry.select(Role::Summarizer, Some(&backend_id))?;
        let resolved = registry
            .resolve(Role::Summarizer)?
            .ok_or("Codex backend must resolve")?;
        let report = MeetingNotesGenerator::new(&store, resolved.provider())
            .with_reasoning_provider(resolved.provider())
            .with_backend_fingerprint(resolved.cache_fingerprint().clone())
            .generate(session_id)
            .await?;

        assert_eq!(report.calls, 1, "one-window notes require one Codex call");
        assert_eq!(
            report.normalizations.len(),
            3,
            "the fresh notes result must carry every tolerated downgrade: max_tokens, temperature, \
             and the JSON-object guarantee Codex cannot provide"
        );
        assert!(
            report
                .normalizations
                .iter()
                .any(|observed| observed.normalization.control.to_string() == "json_object output"),
            "losing the JSON-object guarantee must be reported, not assumed"
        );
        assert_eq!(
            report.notes.overview[0].citations,
            [sotto_core::EventId::new(1)],
            "the Codex result must retain a validated meeting citation"
        );
        assert_eq!(
            report.notes.action_items[0].owner.as_deref(),
            Some("Morgan"),
            "the cited action owner must survive product parsing"
        );
        assert!(
            descriptor.auth_kind() == AuthKind::CodexLogin,
            "the successful path must use the CLI login rather than an API key"
        );
        assert_eq!(
            resolved.normalization_observations().len(),
            3,
            "the independent resolution diagnostic must retain the same downgrade evidence"
        );
        std::fs::remove_dir_all(directory)?;
        Ok(())
    }

    #[tokio::test]
    async fn general_notes_cache_without_mutating_timeline_and_backend_changes_miss()
    -> Result<(), Box<dyn std::error::Error>> {
        let (store, session_id) = store_with_utterances(&[1])?;
        let before = serde_json::to_vec(&store.load_session(session_id)?)?;
        let provider = Arc::new(QueueProvider::new([VALID_NOTES, VALID_NOTES]));
        let first_fingerprint = fingerprint("test.notes.first")?;

        let first = MeetingNotesGenerator::new(&store, provider.clone())
            .with_backend_fingerprint(first_fingerprint.clone())
            .generate(session_id)
            .await?;
        let cached = MeetingNotesGenerator::new(&store, provider.clone())
            .with_backend_fingerprint(first_fingerprint)
            .generate(session_id)
            .await?;
        let other_backend = MeetingNotesGenerator::new(&store, provider.clone())
            .with_backend_fingerprint(fingerprint("test.notes.second")?)
            .generate(session_id)
            .await?;
        let after = serde_json::to_vec(&store.load_session(session_id)?)?;

        assert!(!first.cached);
        assert_eq!(first.calls, 1);
        assert!(first.normalizations.is_empty());
        assert!(cached.cached);
        assert_eq!(
            cached.calls, 0,
            "cache hits must perform zero provider calls"
        );
        assert!(
            cached.normalizations.is_empty(),
            "cache hits must report an explicit empty downgrade list"
        );
        assert!(
            !other_backend.cached,
            "backend identity must partition notes"
        );
        assert!(other_backend.normalizations.is_empty());
        assert_eq!(provider.calls.load(Ordering::Relaxed), 2);
        assert_eq!(before, after, "notes must not mutate the timeline");
        assert_eq!(first.notes, cached.notes);

        let contract = serde_json::to_string(&first.notes)?;
        for forbidden in [
            "rep_percent",
            "customer_percent",
            "customer_questions",
            "objections",
            "competitor_mentions",
        ] {
            assert!(
                !contract.contains(forbidden),
                "meeting-notes contract must not contain sales field {forbidden}"
            );
        }
        Ok(())
    }

    #[tokio::test]
    async fn caller_cancellation_reaches_the_notes_provider()
    -> Result<(), Box<dyn std::error::Error>> {
        let (store, session_id) = store_with_utterances(&[1])?;
        let provider = Arc::new(CancellationProvider {
            observed: AtomicUsize::new(0),
        });
        let cancellation = CancellationToken::new();
        cancellation.cancel();

        let result = MeetingNotesGenerator::new(&store, provider.clone())
            .with_backend_fingerprint(fingerprint("test.notes.cancellation")?)
            .generate_with_cancellation(session_id, cancellation)
            .await;

        assert!(matches!(result, Err(MeetingNotesError::Context(_))));
        assert_eq!(provider.observed.load(Ordering::Acquire), 1);
        Ok(())
    }

    #[tokio::test]
    async fn long_session_map_reduce_preserves_and_validates_citations()
    -> Result<(), Box<dyn std::error::Error>> {
        let (store, session_id) = store_with_utterances(&[1, 1_201])?;
        let first_window = r#"{"overview":[{"text":"Launch planning began","citations":[1]}],"topics":[],"decisions":[],"action_items":[],"open_questions":[],"risks":[],"follow_ups":[]}"#;
        let second_window = r#"{"overview":[{"text":"Rollout risk was discussed","citations":[2]}],"topics":[],"decisions":[],"action_items":[],"open_questions":[],"risks":[{"text":"The rollout risk remains open","citations":[2]}],"follow_ups":[]}"#;
        let reduced = r#"{"overview":[{"text":"The meeting covered launch planning and rollout risk","citations":[1,2]}],"topics":[{"text":"Launch readiness","citations":[1,2]}],"decisions":[],"action_items":[],"open_questions":[],"risks":[{"text":"The rollout risk remains open","citations":[2]}],"follow_ups":[]}"#;
        let provider = Arc::new(QueueProvider::new([first_window, second_window, reduced]));

        let report = MeetingNotesGenerator::new(&store, provider.clone())
            .with_backend_fingerprint(fingerprint("test.notes.long")?)
            .generate(session_id)
            .await?;

        assert_eq!(report.calls, 3, "two map windows require one reduce call");
        assert_eq!(provider.calls.load(Ordering::Relaxed), 3);
        assert_eq!(report.notes.overview[0].citations.len(), 2);
        let inputs = provider.inputs.lock().map_err(|_| "inputs poisoned")?;
        assert!(inputs[0].contains("event:1"));
        assert!(!inputs[0].contains("event:2"));
        assert!(inputs[1].contains("event:2"));
        assert!(!inputs[1].contains("event:1"));
        assert!(inputs[2].contains("Launch planning began"));
        assert!(inputs[2].contains("Rollout risk was discussed"));
        Ok(())
    }

    async fn invalid_first_map_in_two_window_session(
        output: &'static str,
    ) -> Result<(MeetingNotesError, usize), Box<dyn std::error::Error>> {
        let (store, session_id) = store_with_utterances(&[1, 1_201])?;
        let provider = Arc::new(QueueProvider::new([output]));
        let result = MeetingNotesGenerator::new(&store, provider.clone())
            .with_backend_fingerprint(fingerprint("test.notes.window-boundary")?)
            .generate(session_id)
            .await;
        let error = match result {
            Err(error) => error,
            Ok(_) => return Err("future-window citation unexpectedly passed map validation".into()),
        };
        Ok((error, provider.calls.load(Ordering::Relaxed)))
    }

    #[tokio::test]
    async fn map_cannot_cite_factual_event_from_a_future_window()
    -> Result<(), Box<dyn std::error::Error>> {
        let (error, calls) = invalid_first_map_in_two_window_session(
            r#"{"overview":[{"text":"Future claim","citations":[2]}],"topics":[],"decisions":[],"action_items":[],"open_questions":[],"risks":[],"follow_ups":[]}"#,
        )
        .await?;
        assert!(matches!(
            error,
            MeetingNotesError::UnknownCitation {
                field: "overview",
                event
            } if event == sotto_core::EventId::new(2)
        ));
        assert_eq!(calls, 1, "generation must stop at the invalid first map");
        Ok(())
    }

    #[tokio::test]
    async fn map_cannot_use_future_window_as_owner_or_due_date_evidence()
    -> Result<(), Box<dyn std::error::Error>> {
        let owner = r#"{"overview":[],"topics":[],"decisions":[],"action_items":[{"text":"Prepare checklist","citations":[1],"owner":"Morgan","owner_citations":[2],"due_date":null,"due_date_citations":[]}],"open_questions":[],"risks":[],"follow_ups":[]}"#;
        let (owner_error, owner_calls) = invalid_first_map_in_two_window_session(owner).await?;
        assert!(matches!(
            owner_error,
            MeetingNotesError::UnknownCitation {
                field: "action_items",
                event
            } if event == sotto_core::EventId::new(2)
        ));
        assert_eq!(owner_calls, 1);

        let due_date = r#"{"overview":[],"topics":[],"decisions":[],"action_items":[{"text":"Prepare checklist","citations":[1],"owner":null,"owner_citations":[],"due_date":"Friday","due_date_citations":[2]}],"open_questions":[],"risks":[],"follow_ups":[]}"#;
        let (due_date_error, due_date_calls) =
            invalid_first_map_in_two_window_session(due_date).await?;
        assert!(matches!(
            due_date_error,
            MeetingNotesError::UnknownCitation {
                field: "action_items",
                event
            } if event == sotto_core::EventId::new(2)
        ));
        assert_eq!(due_date_calls, 1);
        Ok(())
    }

    async fn invalid_output(
        output: &'static str,
    ) -> Result<MeetingNotesError, Box<dyn std::error::Error>> {
        let (store, session_id) = store_with_utterances(&[1])?;
        let provider = Arc::new(QueueProvider::new([output]));
        let result = MeetingNotesGenerator::new(&store, provider)
            .with_backend_fingerprint(fingerprint("test.notes.invalid")?)
            .generate(session_id)
            .await;
        result.map_or_else(Ok, |_| {
            Err("invalid notes unexpectedly passed validation".into())
        })
    }

    #[tokio::test]
    async fn empty_and_unknown_citations_fail_closed() -> Result<(), Box<dyn std::error::Error>> {
        let missing = invalid_output(
            r#"{"overview":[{"text":"Unsupported","citations":[]}],"topics":[],"decisions":[],"action_items":[],"open_questions":[],"risks":[],"follow_ups":[]}"#,
        )
        .await?;
        assert!(matches!(
            missing,
            MeetingNotesError::MissingCitation { field: "overview" }
        ));

        let unknown = invalid_output(
            r#"{"overview":[{"text":"Unknown","citations":[99]}],"topics":[],"decisions":[],"action_items":[],"open_questions":[],"risks":[],"follow_ups":[]}"#,
        )
        .await?;
        assert!(matches!(
            unknown,
            MeetingNotesError::UnknownCitation {
                field: "overview",
                ..
            }
        ));

        let sales_field = invalid_output(
            r#"{"overview":[],"topics":[],"decisions":[],"action_items":[],"open_questions":[],"risks":[],"follow_ups":[],"objections":[]}"#,
        )
        .await?;
        assert!(matches!(sales_field, MeetingNotesError::Context(_)));
        Ok(())
    }

    #[tokio::test]
    async fn owner_and_due_date_without_separate_evidence_fail_closed()
    -> Result<(), Box<dyn std::error::Error>> {
        let owner = invalid_output(
            r#"{"overview":[],"topics":[],"decisions":[],"action_items":[{"text":"Prepare checklist","citations":[1],"owner":"Morgan","owner_citations":[],"due_date":null,"due_date_citations":[]}],"open_questions":[],"risks":[],"follow_ups":[]}"#,
        )
        .await?;
        assert!(matches!(
            owner,
            MeetingNotesError::MissingOwnerEvidence {
                field: "action_items"
            }
        ));

        let due_date = invalid_output(
            r#"{"overview":[],"topics":[],"decisions":[],"action_items":[],"open_questions":[],"risks":[],"follow_ups":[{"text":"Review the launch","citations":[1],"owner":null,"owner_citations":[],"due_date":"Friday","due_date_citations":[]}]}"#,
        )
        .await?;
        assert!(matches!(
            due_date,
            MeetingNotesError::MissingDueDateEvidence {
                field: "follow_ups"
            }
        ));

        let empty_owner = invalid_output(
            r#"{"overview":[],"topics":[],"decisions":[],"action_items":[{"text":"Prepare checklist","citations":[1],"owner":" ","owner_citations":[],"due_date":null,"due_date_citations":[]}],"open_questions":[],"risks":[],"follow_ups":[]}"#,
        )
        .await?;
        assert!(matches!(
            empty_owner,
            MeetingNotesError::EmptyOwner {
                field: "action_items"
            }
        ));
        Ok(())
    }
}
