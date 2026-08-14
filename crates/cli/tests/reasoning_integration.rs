#![expect(
    clippy::tests_outside_test_module,
    reason = "integration test files compile only under cfg(test)"
)]

mod tests {
    use std::{
        collections::{HashSet, VecDeque},
        fs::OpenOptions,
        io::Write,
        sync::{
            Arc, Mutex,
            atomic::{AtomicUsize, Ordering},
        },
        time::Duration,
    };

    use futures_util::{StreamExt, stream};
    use providers::{
        AuthKind, AuthStatus, BackendCapabilities, BackendCapability, BackendDescriptor, BackendId,
        ReasoningProvider, Registry, Role, openai::OpenAiProvider,
    };
    use screen::{ImageInspectionPolicy, OcrEngine, RetainedScreenInspector, ScreenError};
    use serde_json::{Value, json};
    use sotto_core::{
        BoxFuture, BoxStream, CancellationToken, CaptureTarget, CompletionMessage,
        CompletionProvider, CompletionRequest, Delta, EventId, EventPayload, FrameRef,
        JsonSchemaConstraint, MessageRole, ProviderError, ReasoningRequest, ScreenSnapshot,
        Session, SessionId, Source, StopReason, TargetKind, TimelineBuilder, TimelineEvent, Usage,
        Utterance,
    };
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::{TcpListener, TcpStream},
        sync::oneshot,
    };

    const RECAP: &str = include_str!("../../../fixtures/reasoning/recap.json");
    const CLUSTERS: &str = include_str!("../../../fixtures/reasoning/clusters.json");
    const INSPECT_IMAGE: &str = include_str!("../../../fixtures/reasoning/inspect-image.json");

    fn text_delta(sequence: u64, text: &str) -> Value {
        json!({
            "type": "response.output_text.delta", "sequence_number": sequence,
            "item_id": "msg_1", "output_index": 0, "content_index": 0, "delta": text
        })
    }

    fn completed(sequence: u64) -> Value {
        json!({
            "type": "response.completed", "sequence_number": sequence,
            "response": {
                "created_at": 1, "completed_at": 2, "id": "resp_1",
                "model": "mock-model", "object": "response", "output": [],
                "status": "completed",
                "usage": {
                    "input_tokens": 19, "input_tokens_details": {"cached_tokens": 5},
                    "output_tokens": 7, "output_tokens_details": {"reasoning_tokens": 2},
                    "total_tokens": 26
                }
            }
        })
    }

    fn response(text: &str) -> String {
        [text_delta(1, text), completed(2)]
            .into_iter()
            .fold(String::new(), |mut body, event| {
                use std::fmt::Write as _;
                let _ = write!(body, "data: {event}\n\n");
                body
            })
    }

    async fn read_request(socket: &mut TcpStream) -> std::io::Result<String> {
        let mut bytes = Vec::new();
        let header_end = loop {
            let mut chunk = [0_u8; 4096];
            let count = socket.read(&mut chunk).await?;
            if count == 0 {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::UnexpectedEof,
                    "request headers were incomplete",
                ));
            }
            bytes.extend_from_slice(&chunk[..count]);
            if let Some(index) = bytes.windows(4).position(|window| window == b"\r\n\r\n") {
                break index + 4;
            }
        };
        let headers = String::from_utf8_lossy(&bytes[..header_end]);
        let content_length = headers
            .lines()
            .find_map(|line| {
                let (name, value) = line.split_once(':')?;
                name.eq_ignore_ascii_case("content-length")
                    .then(|| value.trim().parse::<usize>().ok())
                    .flatten()
            })
            .unwrap_or_default();
        while bytes.len() < header_end + content_length {
            let mut chunk = [0_u8; 4096];
            let count = socket.read(&mut chunk).await?;
            if count == 0 {
                break;
            }
            bytes.extend_from_slice(&chunk[..count]);
        }
        String::from_utf8(bytes)
            .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))
    }

    async fn spawn_responses(
        bodies: Vec<String>,
    ) -> Result<(String, oneshot::Receiver<Vec<String>>), Box<dyn std::error::Error>> {
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let address = listener.local_addr()?;
        let (sender, receiver) = oneshot::channel();
        tokio::spawn(async move {
            let mut requests = Vec::with_capacity(bodies.len());
            for body in bodies {
                let Ok((mut socket, _)) = listener.accept().await else {
                    return;
                };
                let Ok(request) = read_request(&mut socket).await else {
                    return;
                };
                let reply = format!(
                    "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                    body.len()
                );
                if socket.write_all(reply.as_bytes()).await.is_err() {
                    return;
                }
                requests.push(request);
            }
            let _ = sender.send(requests);
        });
        Ok((format!("http://{address}"), receiver))
    }

    fn resolved_openai(
        api_base: String,
    ) -> Result<providers::ResolvedBackend, Box<dyn std::error::Error>> {
        let provider = Arc::new(
            OpenAiProvider::new("mock-model", Some("ci-only-key".to_owned().into()))
                .with_api_base(api_base),
        );
        cli::reasoning::resolve_registered(provider, Role::Summarizer)?
            .ok_or_else(|| "OpenAI backend was not selected".into())
    }

    fn persisted_store(
        frame_path: Option<&std::path::Path>,
    ) -> Result<(rag::Store, SessionId), Box<dyn std::error::Error>> {
        let store = rag::Store::open_in_memory()?;
        let id = SessionId::new(71);
        let session = Session::new(
            id,
            CaptureTarget {
                bundle_id: Some("us.zoom.xos".to_owned()),
                display_name: "Zoom".to_owned(),
                window_title: Some("Acme pricing".to_owned()),
                kind: TargetKind::Window,
                audio_scoped: true,
            },
            1,
        );
        store.save_session(&session)?;
        let mut timeline = TimelineBuilder::new(session);
        for (index, text) in [
            "The price is too high.",
            "Migration and support are included.",
        ]
        .into_iter()
        .enumerate()
        {
            let start = Duration::from_secs(index as u64 * 3 + 1);
            timeline.append(
                start,
                EventPayload::UtteranceFinal(Utterance {
                    source: if index == 0 {
                        Source::System
                    } else {
                        Source::Mic
                    },
                    start,
                    end: start + Duration::from_secs(2),
                    text: text.to_owned(),
                    avg_logprob: 0.0,
                    annotations: Vec::new(),
                }),
            );
        }
        if let Some(path) = frame_path {
            timeline.append(
                Duration::from_secs(10),
                EventPayload::ScreenSnapshot(ScreenSnapshot {
                    frame_ref: FrameRef::new(path.to_string_lossy()),
                    ocr_text: "OCR_SENTINEL_MUST_NOT_BE_EAGER".to_owned(),
                    active_app: Some("Keynote".to_owned()),
                    window_title: Some("Pricing".to_owned()),
                    visible_from: Duration::from_secs(10),
                    visible_to: Some(Duration::from_secs(20)),
                }),
            );
        }
        store.append_events(timeline.events())?;
        Ok((store, id))
    }

    fn request_body(raw: &str) -> Result<Value, Box<dyn std::error::Error>> {
        let (_, body) = raw.split_once("\r\n\r\n").ok_or("missing request body")?;
        Ok(serde_json::from_str(body)?)
    }

    struct NoOcr;

    impl OcrEngine for NoOcr {
        fn recognize(&self, _frame: &screen::Frame) -> Result<String, ScreenError> {
            Err(ScreenError::Ocr("OCR must remain lazy".to_owned()))
        }
    }

    #[tokio::test]
    async fn openai_summary_and_clustering_are_cited_private_cached_and_immutable()
    -> Result<(), Box<dyn std::error::Error>> {
        let directory = tempfile::tempdir()?;
        let frame = directory.path().join("pricing.png");
        std::fs::write(&frame, [0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a, 1])?;
        let (store, id) = persisted_store(Some(&frame))?;
        let before = serde_json::to_vec(&store.load_session(id)?)?;
        let (api_base, requests) =
            spawn_responses(vec![response(RECAP), response(CLUSTERS)]).await?;
        let backend = resolved_openai(api_base)?;

        let summary = cli::reasoning::summarize(&store, id, &backend).await?;
        assert_eq!(
            summary.recap.topics[0].citations[0].get(),
            1,
            "summary citation"
        );
        assert_eq!(
            summary.usage.input_tokens, 19,
            "SDK usage must survive normalization"
        );
        let first = cli::reasoning::cluster(&store, id, &backend).await?;
        let second = cli::reasoning::cluster(&store, id, &backend).await?;
        assert!(!first.cached, "first clustering run must call the backend");
        assert!(
            second.cached,
            "unchanged input and fingerprint must use the cache"
        );
        assert_eq!(
            before,
            serde_json::to_vec(&store.load_session(id)?)?,
            "derived reasoning must not mutate the timeline"
        );

        let requests = requests.await?;
        assert_eq!(requests.len(), 2, "cache hit must not make a third request");
        for raw in requests {
            assert!(
                raw.starts_with("POST /responses HTTP/1.1"),
                "only Responses is reachable"
            );
            assert!(
                raw.contains("from:1.000s to:3.000s"),
                "timestamped transcript is required"
            );
            assert!(
                !raw.contains("OCR_SENTINEL_MUST_NOT_BE_EAGER"),
                "OCR cannot be eager"
            );
            assert!(
                !raw.contains("pricing.png"),
                "local frame paths cannot enter first pass"
            );
            assert!(
                !raw.contains("input_image"),
                "first pass must remain transcript-only"
            );
        }
        Ok(())
    }

    #[tokio::test]
    async fn screen_image_requires_opt_in_and_authorized_second_pass_has_no_path()
    -> Result<(), Box<dyn std::error::Error>> {
        for policy in [ImageInspectionPolicy::Deny, ImageInspectionPolicy::Allow] {
            let directory = tempfile::tempdir()?;
            let frame = directory.path().join("pricing.png");
            std::fs::write(
                &frame,
                [0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a, 1, 2, 3],
            )?;
            let (store, id) = persisted_store(Some(&frame))?;
            let (api_base, requests) =
                spawn_responses(vec![response(INSPECT_IMAGE), response(RECAP)]).await?;
            let backend = resolved_openai(api_base)?;
            let inspector = Arc::new(RetainedScreenInspector::new(
                directory.path(),
                None::<NoOcr>,
                policy,
            ));
            let report =
                cli::reasoning::summarize_with_inspector(&store, id, &backend, Some(inspector))
                    .await?;
            assert_eq!(
                report.calls, 2,
                "inspection must be one bounded second pass"
            );
            let requests = requests.await?;
            assert!(
                !requests[0].contains("input_image"),
                "first pass is always transcript-only"
            );
            let second = &requests[1];
            match policy {
                ImageInspectionPolicy::Deny => {
                    assert!(
                        second.contains("image_opt_in_required"),
                        "denial must be explicit"
                    );
                    assert!(
                        !second.contains("input_image"),
                        "denied image must not reach transport"
                    );
                }
                ImageInspectionPolicy::Allow => {
                    assert!(
                        second.contains("input_image"),
                        "authorized image must use typed SDK input"
                    );
                    assert!(
                        second.contains("snapshot_event=3"),
                        "sampled-frame provenance is required"
                    );
                    assert!(
                        !second.contains("pricing.png"),
                        "authorized transport must not disclose paths"
                    );
                }
            }
        }
        Ok(())
    }

    #[tokio::test]
    async fn schema_errors_and_cancellation_are_normalized_through_resolved_openai()
    -> Result<(), Box<dyn std::error::Error>> {
        let rate_limit = json!({
            "type": "error", "sequence_number": 1, "code": "rate_limit_exceeded",
            "message": "slow down", "param": null
        });
        let rate_body = format!("data: {rate_limit}\n\n");
        let (api_base, requests) = spawn_responses(vec![response("{}"), rate_body]).await?;
        let backend = resolved_openai(api_base)?;
        let schema = JsonSchemaConstraint::new(
            "recap_eval",
            None,
            r#"{"type":"object","additionalProperties":false}"#,
        )?;
        let request = CompletionRequest {
            model: "mock-model".to_owned(),
            system: Some("Return JSON".to_owned()),
            messages: Vec::new(),
            max_tokens: Some(64),
            temperature: Some(0.0),
            stop: Vec::new(),
        };
        let measured = cli::reasoning::measure_reasoning_call(
            &backend,
            ReasoningRequest::json_schema(request.clone(), schema),
            CancellationToken::new(),
        )
        .await?;
        assert_eq!(
            measured.text, "{}",
            "measured call must preserve streamed text"
        );
        assert_eq!(
            measured.usage.input_tokens, 19,
            "measured call must preserve usage"
        );
        assert!(
            measured.time_to_first_delta.is_some(),
            "mock call must observe TTFT"
        );
        assert!(
            measured.total >= measured.time_to_first_delta.unwrap_or_default(),
            "total latency cannot precede first text"
        );
        let mut error_stream = backend
            .provider()
            .stream_advanced_reasoning(
                ReasoningRequest::json_object(request),
                None,
                CancellationToken::new(),
            )
            .await?;
        assert_eq!(
            error_stream
                .next()
                .await
                .ok_or("missing rate-limit event")?,
            Err(ProviderError::RateLimit { retry_after: None }),
            "SDK errors must normalize at the registry-pinned seam"
        );
        let requests = requests.await?;
        let schema_json = request_body(&requests[0])?;
        assert_eq!(
            schema_json["text"]["format"]["type"], "json_schema",
            "strict schema type"
        );
        assert_eq!(
            schema_json["text"]["format"]["strict"], true,
            "schema must be strict"
        );

        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let address = listener.local_addr()?;
        let (accepted_tx, accepted_rx) = oneshot::channel();
        tokio::spawn(async move {
            if let Ok((mut socket, _)) = listener.accept().await {
                let _ = read_request(&mut socket).await;
                let _ = accepted_tx.send(());
                let mut byte = [0_u8; 1];
                let _ = socket.read(&mut byte).await;
            }
        });
        let cancelled = resolved_openai(format!("http://{address}"))?;
        let token = CancellationToken::new();
        let mut stream = cancelled
            .provider()
            .stream_advanced_reasoning(
                ReasoningRequest::json_object(CompletionRequest {
                    model: "mock-model".to_owned(),
                    system: None,
                    messages: Vec::new(),
                    max_tokens: None,
                    temperature: None,
                    stop: Vec::new(),
                }),
                None,
                token.clone(),
            )
            .await?;
        let next = stream.next();
        tokio::pin!(next);
        tokio::select! {
            accepted = accepted_rx => accepted?,
            item = &mut next => return Err(format!("request ended before cancellation: {item:?}").into()),
        }
        token.cancel();
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(2), &mut next)
                .await?
                .ok_or("missing cancellation")?,
            Err(ProviderError::Cancelled),
            "pre-text cancellation must be normalized"
        );
        Ok(())
    }

    struct QueueProvider {
        outputs: Mutex<VecDeque<String>>,
        calls: Arc<AtomicUsize>,
    }

    impl CompletionProvider for QueueProvider {
        fn stream(
            &self,
            _request: CompletionRequest,
            _cancellation: CancellationToken,
        ) -> BoxFuture<'_, Result<BoxStream<'static, Result<Delta, ProviderError>>, ProviderError>>
        {
            self.calls.fetch_add(1, Ordering::Relaxed);
            let output = self
                .outputs
                .lock()
                .map_err(|_| ProviderError::Network("queue poisoned".to_owned()))
                .and_then(|mut outputs| {
                    outputs
                        .pop_front()
                        .ok_or_else(|| ProviderError::Network("fixture exhausted".to_owned()))
                });
            Box::pin(async move {
                Ok(Box::pin(stream::iter([Ok(Delta {
                    text: output?,
                    is_final: true,
                    usage: Some(Usage::default()),
                    stop_reason: Some(StopReason::EndTurn),
                })]))
                    as BoxStream<'static, Result<Delta, ProviderError>>)
            })
        }

        fn model_id(&self) -> &str {
            "mock-model"
        }
    }

    impl ReasoningProvider for QueueProvider {}

    fn resolved_fake(
        id: &str,
        output: &str,
    ) -> Result<providers::ResolvedBackend, Box<dyn std::error::Error>> {
        let provider = Arc::new(QueueProvider {
            outputs: Mutex::new(VecDeque::from([output.to_owned()])),
            calls: Arc::new(AtomicUsize::new(0)),
        });
        let descriptor = BackendDescriptor::new(
            BackendId::new(id)?,
            "Fixture backend",
            "mock-model",
            1,
            json_object_capabilities(),
            AuthKind::None,
            AuthStatus::Ready,
        )?;
        let backend_id = descriptor.id().clone();
        let mut registry = Registry::default();
        registry.register_reasoning(descriptor, provider)?;
        registry.select(Role::Summarizer, Some(&backend_id))?;
        registry
            .resolve(Role::Summarizer)?
            .ok_or_else(|| "fixture backend unresolved".into())
    }

    fn json_object_capabilities() -> BackendCapabilities {
        BackendCapabilities::new([
            BackendCapability::Streaming,
            BackendCapability::Cancellation,
            BackendCapability::JsonObjectOutput,
        ])
    }

    fn resolve_queue_backend(
        id: &str,
        capabilities: BackendCapabilities,
        outputs: Vec<String>,
        calls: Arc<AtomicUsize>,
    ) -> Result<providers::ResolvedBackend, Box<dyn std::error::Error>> {
        let provider = Arc::new(QueueProvider {
            outputs: Mutex::new(outputs.into()),
            calls,
        });
        let descriptor = BackendDescriptor::new(
            BackendId::new(id)?,
            "Fixture backend",
            "mock-model",
            1,
            capabilities,
            AuthKind::None,
            AuthStatus::Ready,
        )?;
        let backend_id = descriptor.id().clone();
        let mut registry = Registry::default();
        registry.register_reasoning(descriptor, provider)?;
        registry.select(Role::Summarizer, Some(&backend_id))?;
        registry
            .resolve(Role::Summarizer)?
            .ok_or_else(|| "fixture backend unresolved".into())
    }

    #[tokio::test]
    async fn structured_consumers_reject_missing_json_capability_before_transport()
    -> Result<(), Box<dyn std::error::Error>> {
        let (store, id) = persisted_store(None)?;
        let calls = Arc::new(AtomicUsize::new(0));
        let backend = resolve_queue_backend(
            "test.no-json",
            BackendCapabilities::reasoning_baseline(),
            vec![CLUSTERS.to_owned()],
            Arc::clone(&calls),
        )?;
        let result = cli::reasoning::cluster(&store, id, &backend).await;
        assert!(
            result.as_ref().is_err_and(|error| error
                .to_string()
                .contains("does not advertise JsonObjectOutput")),
            "structured consumers must report an actionable capability error"
        );
        assert_eq!(
            calls.load(Ordering::Relaxed),
            0,
            "missing JSON capability must fail before provider I/O"
        );
        Ok(())
    }

    #[tokio::test]
    async fn image_request_on_text_backend_fails_before_image_transport()
    -> Result<(), Box<dyn std::error::Error>> {
        let directory = tempfile::tempdir()?;
        let frame = directory.path().join("pricing.png");
        std::fs::write(
            &frame,
            [0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a, 1, 2, 3],
        )?;
        let (store, id) = persisted_store(Some(&frame))?;
        let calls = Arc::new(AtomicUsize::new(0));
        let backend = resolve_queue_backend(
            "test.text-only",
            json_object_capabilities(),
            vec![INSPECT_IMAGE.to_owned()],
            Arc::clone(&calls),
        )?;
        assert!(
            !backend
                .descriptor()
                .capabilities()
                .contains(BackendCapability::ImageInput),
            "fixture descriptor must truthfully omit image transport"
        );
        let inspector = Arc::new(RetainedScreenInspector::new(
            directory.path(),
            None::<NoOcr>,
            ImageInspectionPolicy::Allow,
        ));
        let result =
            cli::reasoning::summarize_with_inspector(&store, id, &backend, Some(inspector)).await;
        assert!(
            result.as_ref().is_err_and(|error| format!("{error:#}")
                .contains("does not support authorized screen image input")),
            "resolved text backend must return an honest unsupported-image result: {result:?}"
        );
        assert_eq!(
            calls.load(Ordering::Relaxed),
            1,
            "only the transcript-first inspection request may reach text transport"
        );
        Ok(())
    }

    #[tokio::test]
    async fn fake_third_backend_runs_unchanged_consumer_and_separates_cache()
    -> Result<(), Box<dyn std::error::Error>> {
        let (store, id) = persisted_store(None)?;
        let first_backend = resolved_fake("test.first", CLUSTERS)?;
        let first = cli::reasoning::cluster(&store, id, &first_backend).await?;
        let third_backend = resolved_fake("test.third", CLUSTERS)?;
        let report = cli::reasoning::cluster(&store, id, &third_backend).await?;
        assert!(
            !first.cached,
            "first connector must create its own artifact"
        );
        assert!(
            !report.cached,
            "new connector fingerprint must not reuse another artifact"
        );
        assert_ne!(
            first_backend.cache_fingerprint(),
            third_backend.cache_fingerprint(),
            "connector identity must participate in cache separation"
        );
        assert_eq!(
            report.view.regions[0].event_ids.len(),
            2,
            "unchanged cluster consumer must validate citations"
        );
        Ok(())
    }

    fn ensure_known(
        known: &HashSet<EventId>,
        citations: impl IntoIterator<Item = EventId>,
        label: &str,
    ) -> Result<(), Box<dyn std::error::Error>> {
        for citation in citations {
            if !known.contains(&citation) {
                return Err(format!("{label} cites unknown timeline event {citation:?}").into());
            }
        }
        Ok(())
    }

    fn validate_live_summary(
        report: &insight::SummaryReport,
        known: &HashSet<EventId>,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let mut evidenced_items = 0_usize;
        for item in &report.recap.attendees {
            ensure_nonempty(&item.name, "summary attendee name")?;
            ensure_nonempty(&item.role, "summary attendee role")?;
            ensure_cited_known(known, &item.citations, "summary attendee")?;
            evidenced_items += 1;
        }
        for (label, items) in [
            ("summary topic", report.recap.topics.as_slice()),
            (
                "summary customer question",
                report.recap.customer_questions.as_slice(),
            ),
            (
                "summary competitor mention",
                report.recap.competitor_mentions.as_slice(),
            ),
        ] {
            for item in items {
                ensure_nonempty(&item.text, label)?;
                ensure_cited_known(known, &item.citations, label)?;
                evidenced_items += 1;
            }
        }
        for item in &report.recap.objections {
            ensure_nonempty(&item.text, "summary objection")?;
            if item.resolved {
                ensure_nonempty(
                    item.resolution.as_deref().unwrap_or_default(),
                    "summary resolved objection resolution",
                )?;
            } else if let Some(resolution) = &item.resolution {
                ensure_nonempty(resolution, "summary objection resolution")?;
            }
            ensure_cited_known(known, &item.citations, "summary objection")?;
            evidenced_items += 1;
        }
        for (label, items) in [
            ("summary commitment", report.recap.commitments.as_slice()),
            ("summary next step", report.recap.next_steps.as_slice()),
        ] {
            for item in items {
                ensure_nonempty(&item.text, label)?;
                ensure_nonempty(&item.owner, label)?;
                ensure_cited_known(known, &item.citations, label)?;
                evidenced_items += 1;
            }
        }
        if evidenced_items == 0 {
            return Err("live summary contains no cited evidence".into());
        }
        Ok(())
    }

    fn validate_live_clusters(
        report: &insight::ClusterReport,
        known: &HashSet<EventId>,
    ) -> Result<(), Box<dyn std::error::Error>> {
        if report.cached {
            return Err(
                "live clustering reused a cached artifact; use a fresh T035 evidence database"
                    .into(),
            );
        }
        let mut evidenced_items = 0_usize;
        for region in &report.view.regions {
            ensure_nonempty(&region.label, "clustering region label")?;
            ensure_cited_known(known, &region.event_ids, "clustering region")?;
            evidenced_items += 1;
        }
        for link in &report.view.links {
            ensure_nonempty(&link.reason, "clustering link reason")?;
            ensure_known(known, [link.from, link.to], "clustering link")?;
            evidenced_items += 1;
        }
        for thread in &report.view.open_threads {
            ensure_nonempty(&thread.summary, "clustering open-thread summary")?;
            ensure_known(known, [thread.event_id], "clustering open thread")?;
            evidenced_items += 1;
        }
        if evidenced_items == 0 {
            return Err("live clustering contains no cited region, link, or open thread".into());
        }
        Ok(())
    }

    fn ensure_nonempty(value: &str, label: &str) -> Result<(), Box<dyn std::error::Error>> {
        if value.trim().is_empty() {
            return Err(format!("{label} contains no semantic content").into());
        }
        Ok(())
    }

    fn ensure_cited_known(
        known: &HashSet<EventId>,
        citations: &[EventId],
        label: &str,
    ) -> Result<(), Box<dyn std::error::Error>> {
        if citations.is_empty() {
            return Err(format!("{label} contains no cited evidence").into());
        }
        ensure_known(known, citations.iter().copied(), label)
    }

    #[test]
    fn live_gate_rejects_blank_summary_semantics() -> Result<(), Box<dyn std::error::Error>> {
        let mut recap: insight::Recap = serde_json::from_str(RECAP)?;
        recap.topics[0].text = "  ".to_owned();
        let report = insight::SummaryReport {
            recap,
            usage: Usage::default(),
            cost: None,
            model: "test".to_owned(),
            calls: 1,
        };
        let known = HashSet::from([EventId::new(1), EventId::new(2)]);

        let Err(error) = validate_live_summary(&report, &known) else {
            return Err("blank topic passed the live summary gate".into());
        };
        assert!(error.to_string().contains("semantic content"));
        Ok(())
    }

    #[test]
    fn live_gate_rejects_blank_cluster_semantics() -> Result<(), Box<dyn std::error::Error>> {
        let mut view: insight::DerivedView = serde_json::from_str(CLUSTERS)?;
        view.links[0].reason = "\t".to_owned();
        let report = insight::ClusterReport {
            view,
            usage: Usage::default(),
            model: "test".to_owned(),
            cached: false,
        };
        let known = HashSet::from([EventId::new(1), EventId::new(2)]);

        let Err(error) = validate_live_clusters(&report, &known) else {
            return Err("blank reason passed the live clustering gate".into());
        };
        assert!(error.to_string().contains("semantic content"));
        Ok(())
    }

    fn live_transcript(events: &[TimelineEvent]) -> String {
        events
            .iter()
            .filter_map(|event| {
                let EventPayload::UtteranceFinal(utterance) = event.payload() else {
                    return None;
                };
                Some(format!(
                    "[event:{} from:{:.3}s to:{:.3}s speaker:{:?}] {}",
                    event.id().get(),
                    utterance.start.as_secs_f64(),
                    utterance.end.as_secs_f64(),
                    utterance.source,
                    utterance.text
                ))
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[tokio::test]
    #[ignore = "manual only: requires explicit T035 acknowledgement, evidence path, Keychain key, real session, and live network"]
    async fn live_keychain_t035_reasoning_evidence() -> Result<(), Box<dyn std::error::Error>> {
        let acknowledgement = std::env::var("SOTTO_T035_ACCEPTANCE_ACK")?;
        if acknowledgement != "owner-accepted" {
            return Err("SOTTO_T035_ACCEPTANCE_ACK must equal owner-accepted".into());
        }
        let evidence_path = std::env::var("SOTTO_T035_EVIDENCE_PATH")?;
        if evidence_path.trim().is_empty() {
            return Err("SOTTO_T035_EVIDENCE_PATH must be an explicit new file".into());
        }
        let sample_count = std::env::var("SOTTO_T035_LATENCY_SAMPLES")?.parse::<usize>()?;
        if sample_count < 5 {
            return Err("SOTTO_T035_LATENCY_SAMPLES must be at least 5".into());
        }
        let database = std::env::var("SOTTO_T035_DATABASE")?;
        let session_id = std::env::var("SOTTO_T035_SESSION_ID")?.parse::<u128>()?;
        let model = std::env::var("SOTTO_T035_OPENAI_MODEL")?;
        let store = rag::Store::open(database)?;
        let session_id = SessionId::new(session_id);
        let before_events = store.load_session(session_id)?;
        let before = serde_json::to_vec(&before_events)?;
        let known: HashSet<_> = before_events.iter().map(TimelineEvent::id).collect();
        if known.is_empty() {
            return Err("T035 accepted session has no persisted timeline events".into());
        }
        let backend = cli::reasoning::resolve_keychain_backend(
            cli::reasoning::BackendChoice::OpenAi,
            Some(&model),
            Role::Summarizer,
        )?
        .ok_or("OpenAI not selected")?;
        let summary = cli::reasoning::summarize(&store, session_id, &backend).await?;
        let clusters = cli::reasoning::cluster(&store, session_id, &backend).await?;
        validate_live_summary(&summary, &known)?;
        validate_live_clusters(&clusters, &known)?;

        let transcript = live_transcript(&before_events);
        if transcript.is_empty() {
            return Err("T035 accepted session has no final transcript events".into());
        }
        let mut startup = Vec::with_capacity(sample_count);
        let mut first_delta = Vec::with_capacity(sample_count);
        let mut total = Vec::with_capacity(sample_count);
        for _ in 0..sample_count {
            let startup_begin = std::time::Instant::now();
            let measured_backend = cli::reasoning::resolve_keychain_backend(
                cli::reasoning::BackendChoice::OpenAi,
                Some(&model),
                Role::Summarizer,
            )?
            .ok_or("OpenAI not selected during latency sample")?;
            startup.push(startup_begin.elapsed());
            let request = CompletionRequest {
                model: model.clone(),
                system: Some("Return a JSON object acknowledging the transcript.".to_owned()),
                messages: vec![CompletionMessage {
                    role: MessageRole::User,
                    content: transcript.clone(),
                    cache_boundary: false,
                }],
                max_tokens: Some(32),
                temperature: Some(0.0),
                stop: Vec::new(),
            };
            let measured = cli::reasoning::measure_reasoning_call(
                &measured_backend,
                ReasoningRequest::json_object(request),
                CancellationToken::new(),
            )
            .await?;
            first_delta.push(
                measured
                    .time_to_first_delta
                    .ok_or("live latency sample completed without a text delta")?,
            );
            total.push(measured.total);
        }
        if startup.len() != sample_count
            || first_delta.len() != sample_count
            || total.len() != sample_count
        {
            return Err("live latency evidence populations are incomplete".into());
        }
        let startup_latency = cli::reasoning::duration_report(&mut startup);
        let reasoning_latency = cli::reasoning::latency_report(&mut first_delta, &mut total);
        let after = serde_json::to_vec(&store.load_session(session_id)?)?;
        if before != after {
            return Err("live reasoning mutated the persisted timeline".into());
        }

        let evidence = json!({
            "contract": "sotto.t029.live-evidence.v1",
            "t035_acknowledgement": acknowledgement,
            "session_id": session_id.get(),
            "model": model,
            "sample_count_required": sample_count,
            "timeline_event_count": known.len(),
            "timeline_byte_identical": true,
            "summary": summary,
            "clustering": clusters,
            "startup_latency": startup_latency,
            "reasoning_latency": reasoning_latency,
        });
        let mut output = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(evidence_path)?;
        serde_json::to_writer_pretty(&mut output, &evidence)?;
        writeln!(output)?;
        Ok(())
    }
}
