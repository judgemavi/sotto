#[cfg(test)]
mod tests {
    use std::{
        collections::VecDeque,
        num::NonZeroUsize,
        path::{Path, PathBuf},
        sync::{
            Arc,
            atomic::{AtomicU64, Ordering},
        },
        time::{Duration, Instant},
    };

    use sotto_core::{
        AudioFrame, BoxFuture, CaptureBackend, CaptureError, CaptureTarget, EventKind,
        EventPayload, PermissionStatus, PersistenceSink, Pipeline, PipelineConfig, ProsodyDelta,
        RagError, Session, SessionId, Source, SpeechState, TargetKind, TimelineEvent, Transcriber,
        TranscriptAnnotator, TranscriptUpdate, Utterance, VadSegment, VoiceActivityDetector,
    };
    use tokio::sync::broadcast;

    struct FixtureCapture {
        frames: usize,
        frame_interval: Duration,
    }

    struct WavFixtureCapture {
        mic: PathBuf,
        system: PathBuf,
    }

    impl CaptureBackend for WavFixtureCapture {
        fn start(&mut self, sink: broadcast::Sender<AudioFrame>) -> Result<(), CaptureError> {
            let mut frames = read_wav(&self.mic, Source::Mic)?;
            frames.extend(read_wav(&self.system, Source::System)?);
            frames.sort_by_key(|frame| (frame.stream_offset, source_index(frame.source)));
            for frame in frames {
                let _ = sink.send(frame);
            }
            Ok(())
        }

        fn stop(&mut self) {}

        fn permission_status(&self) -> PermissionStatus {
            PermissionStatus::Authorized
        }
    }

    impl CaptureBackend for FixtureCapture {
        fn start(&mut self, sink: broadcast::Sender<AudioFrame>) -> Result<(), CaptureError> {
            for sequence in 0..self.frames {
                let source = if sequence % 2 == 0 {
                    Source::Mic
                } else {
                    Source::System
                };
                let _ = sink.send(AudioFrame {
                    source,
                    samples: Arc::from([0.1_f32; 160]),
                    sample_rate: 16_000,
                    seq: sequence as u64,
                    capture_ts: Instant::now(),
                    stream_offset: self
                        .frame_interval
                        .saturating_mul(u32::try_from(sequence).unwrap_or(u32::MAX)),
                });
            }
            Ok(())
        }

        fn stop(&mut self) {}

        fn permission_status(&self) -> PermissionStatus {
            PermissionStatus::Authorized
        }
    }

    struct FakeVad(Source);

    impl VoiceActivityDetector for FakeVad {
        fn push(&mut self, frame: &AudioFrame) -> Option<VadSegment> {
            (frame.source == self.0 && frame.seq % 10 == source_offset(self.0)).then_some(
                VadSegment {
                    source: frame.source,
                    start: frame.stream_offset,
                    end: None,
                    kind: SpeechState::SpeechStart,
                },
            )
        }

        fn reset(&mut self) {}
    }

    #[derive(Default)]
    struct FakeTranscriber {
        pending: VecDeque<TranscriptUpdate>,
    }

    impl Transcriber for FakeTranscriber {
        fn push(&mut self, frame: &AudioFrame) {
            self.pending.push_back(TranscriptUpdate::Final(Utterance {
                source: frame.source,
                start: frame.stream_offset,
                end: frame.stream_offset + Duration::from_millis(10),
                text: format!("turn {}", frame.seq),
                avg_logprob: -0.1,
                annotations: Vec::new(),
            }));
        }

        fn poll(&mut self) -> Vec<TranscriptUpdate> {
            self.pending.drain(..).collect()
        }
    }

    #[derive(Default)]
    struct FakeAnnotator;

    impl TranscriptAnnotator for FakeAnnotator {
        fn observe_vad(&mut self, _segment: &VadSegment) {}

        fn annotate(&mut self, utterance: &mut Utterance) -> Option<ProsodyDelta> {
            Some(ProsodyDelta {
                source: utterance.source,
                speech_rate: Some(120.0),
                talk_time_ratio: 0.5,
                annotations: Vec::new(),
            })
        }
    }

    struct SlowPersistence {
        delay: Duration,
        appended: Arc<AtomicU64>,
    }

    struct FailingPersistence;

    #[derive(Default)]
    struct DeferredTranscriber {
        pending: VecDeque<TranscriptUpdate>,
        polls_since_push: usize,
    }

    impl Transcriber for DeferredTranscriber {
        fn push(&mut self, frame: &AudioFrame) {
            self.polls_since_push = 0;
            self.pending.push_back(TranscriptUpdate::Final(Utterance {
                source: frame.source,
                start: frame.stream_offset,
                end: frame.stream_offset + Duration::from_millis(10),
                text: format!("deferred {}", frame.seq),
                avg_logprob: -0.1,
                annotations: Vec::new(),
            }));
        }

        fn poll(&mut self) -> Vec<TranscriptUpdate> {
            self.polls_since_push = self.polls_since_push.saturating_add(1);
            if self.polls_since_push < 2 {
                Vec::new()
            } else {
                self.pending.drain(..).collect()
            }
        }
    }

    struct SlowAnnotator;

    impl TranscriptAnnotator for SlowAnnotator {
        fn observe_vad(&mut self, _segment: &VadSegment) {}

        fn annotate(&mut self, _utterance: &mut Utterance) -> Option<ProsodyDelta> {
            std::thread::sleep(Duration::from_millis(1));
            None
        }
    }

    impl PersistenceSink for FailingPersistence {
        fn append<'a>(
            &'a self,
            _events: Vec<TimelineEvent>,
        ) -> BoxFuture<'a, Result<(), RagError>> {
            Box::pin(async { Err(RagError::Storage("disk full".to_owned())) })
        }
    }

    impl PersistenceSink for SlowPersistence {
        fn append<'a>(&'a self, events: Vec<TimelineEvent>) -> BoxFuture<'a, Result<(), RagError>> {
            Box::pin(async move {
                tokio::time::sleep(self.delay).await;
                self.appended
                    .fetch_add(events.len() as u64, Ordering::Relaxed);
                Ok(())
            })
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn two_sources_merge_without_slow_consumers_or_persistence_stalling_audio()
    -> Result<(), Box<dyn std::error::Error>> {
        let appended = Arc::new(AtomicU64::new(0));
        let config = PipelineConfig {
            audio_capacity: nonzero(512),
            transcript_capacity: nonzero(32),
            timeline_capacity: nonzero(32),
            control_capacity: nonzero(4),
            event_capacity: nonzero(16),
            recent_event_capacity: nonzero(32),
            checkpoint_events: nonzero(8),
            pending_persistence_events: nonzero(1_024),
            persistence_queue_capacity: nonzero(1),
            asr_poll_interval: Duration::from_millis(1),
        };
        let pipeline = Pipeline::builder(session())
            .capture(FixtureCapture {
                frames: 200,
                frame_interval: Duration::from_millis(10),
            })
            .vad(FakeVad(Source::Mic), FakeVad(Source::System))
            .transcriber(FakeTranscriber::default())
            .annotator(FakeAnnotator)
            .persistence(Arc::new(SlowPersistence {
                delay: Duration::from_millis(200),
                appended: Arc::clone(&appended),
            }))
            .config(config)
            .start()?;
        let _intentionally_slow = pipeline.events().subscribe("slow-board");
        let mut live = pipeline.events().subscribe("test-live");

        let started = Instant::now();
        let mut finals = [0_usize; 2];
        while finals.iter().sum::<usize>() < 6 {
            let event = tokio::time::timeout(Duration::from_secs(2), live.recv()).await??;
            if let EventPayload::UtteranceFinal(utterance) = event.payload() {
                finals[source_index(utterance.source)] += 1;
            }
        }
        assert!(finals.iter().all(|count| *count > 0));
        assert!(started.elapsed() < Duration::from_secs(1));

        tokio::time::sleep(Duration::from_millis(25)).await;
        let session = pipeline.session();
        let counters = pipeline.counters().clone();
        pipeline.stop().await;
        let view = session.view();
        assert!(view.recent_ordered.len() <= 32);
        assert!(view.checkpointed_events > 32);
        assert!(counters.persistence_saturated() > 0);
        let persisted = appended.load(Ordering::Relaxed);
        let allocated = view
            .recent_ordered
            .iter()
            .map(|event| event.id().get())
            .max()
            .unwrap_or(0);
        assert_eq!(
            persisted, allocated,
            "saturation must not lose a checkpoint"
        );
        assert_eq!(view.checkpointed_events, persisted);
        assert_eq!(view.pending_persistence_events, 0);
        assert!(
            view.recent_ordered
                .windows(2)
                .all(|pair| pair[0].ts() <= pair[1].ts())
        );
        Ok(())
    }

    #[tokio::test(flavor = "current_thread")]
    async fn capture_failure_pauses_and_surfaces_an_error_without_tearing_down()
    -> Result<(), Box<dyn std::error::Error>> {
        let pipeline = Pipeline::builder(session())
            .capture(FixtureCapture {
                frames: 100,
                frame_interval: Duration::from_millis(10),
            })
            .vad(FakeVad(Source::Mic), FakeVad(Source::System))
            .transcriber(FakeTranscriber::default())
            .annotator(SlowAnnotator)
            .config(PipelineConfig {
                transcript_capacity: nonzero(4),
                timeline_capacity: nonzero(1),
                control_capacity: nonzero(1),
                ..PipelineConfig::default()
            })
            .start()?;
        let mut events = pipeline.events().subscribe("errors");
        tokio::time::sleep(Duration::from_millis(5)).await;
        pipeline
            .report_capture_error(Duration::from_secs(4), CaptureError::PermissionRevoked)
            .await;
        let event = loop {
            let event = tokio::time::timeout(Duration::from_secs(1), events.recv()).await??;
            if event.kind() == EventKind::Error {
                break event;
            }
        };
        assert_eq!(event.kind(), EventKind::Error);
        assert!(pipeline.is_paused());
        pipeline.resume();
        assert!(!pipeline.is_paused());
        pipeline.stop().await;
        Ok(())
    }

    #[tokio::test(flavor = "current_thread")]
    async fn sustained_saturation_exposes_terminal_degraded_recording_state()
    -> Result<(), Box<dyn std::error::Error>> {
        let pipeline = Pipeline::builder(session())
            .capture(FixtureCapture {
                frames: 100,
                frame_interval: Duration::from_millis(10),
            })
            .vad(FakeVad(Source::Mic), FakeVad(Source::System))
            .transcriber(FakeTranscriber::default())
            .persistence(Arc::new(SlowPersistence {
                delay: Duration::from_millis(250),
                appended: Arc::new(AtomicU64::new(0)),
            }))
            .config(PipelineConfig {
                checkpoint_events: nonzero(1),
                pending_persistence_events: nonzero(8),
                persistence_queue_capacity: nonzero(1),
                recent_event_capacity: nonzero(64),
                ..PipelineConfig::default()
            })
            .start()?;
        let session = pipeline.session();
        pipeline.stop().await;
        let view = session.view();
        assert!(matches!(
            &view.recording_state,
            sotto_core::RecordingState::Degraded { reason }
                if reason.contains("live session continues without recording")
        ));
        assert!(view.unrecorded_events > 0);
        assert!(
            view.recent_ordered
                .iter()
                .any(|event| event.kind() == EventKind::UtteranceFinal),
            "live timeline should continue after persistence degradation"
        );
        Ok(())
    }

    #[tokio::test(flavor = "current_thread")]
    async fn persistence_failure_becomes_an_error_event_without_stopping_live_stages()
    -> Result<(), Box<dyn std::error::Error>> {
        let pipeline = Pipeline::builder(session())
            .capture(FixtureCapture {
                frames: 4,
                frame_interval: Duration::from_millis(10),
            })
            .vad(FakeVad(Source::Mic), FakeVad(Source::System))
            .transcriber(FakeTranscriber::default())
            .persistence(Arc::new(FailingPersistence))
            .config(PipelineConfig {
                checkpoint_events: nonzero(4),
                recent_event_capacity: nonzero(64),
                ..PipelineConfig::default()
            })
            .start()?;
        let session = pipeline.session();
        let mut events = pipeline.events().subscribe("persistence-errors");
        let error = loop {
            let event = tokio::time::timeout(Duration::from_secs(1), events.recv()).await??;
            if event.kind() == EventKind::Error {
                break event;
            }
        };
        assert!(matches!(
            error.payload(),
            EventPayload::Error(sotto_core::PipelineError::Rag(RagError::Storage(message)))
                if message == "disk full"
        ));
        assert!(!pipeline.is_paused());
        for second in [2_u64, 3] {
            pipeline
                .report_capture_error(
                    Duration::from_secs(second),
                    CaptureError::StreamFailed(format!("tail {second}")),
                )
                .await;
        }
        pipeline.stop().await;
        let view = session.view();
        let allocated = view
            .recent_ordered
            .iter()
            .map(|event| event.id().get())
            .max()
            .unwrap_or(0);
        assert_eq!(view.pending_persistence_events, 0);
        assert_eq!(
            view.checkpointed_events + view.unrecorded_events,
            allocated,
            "durability accounting omitted the degraded shutdown tail"
        );
        Ok(())
    }

    #[tokio::test(flavor = "current_thread")]
    async fn shutdown_drains_asr_before_closing_annotation_and_timeline()
    -> Result<(), Box<dyn std::error::Error>> {
        let pipeline = Pipeline::builder(session())
            .capture(FixtureCapture {
                frames: 20,
                frame_interval: Duration::from_millis(10),
            })
            .vad(FakeVad(Source::Mic), FakeVad(Source::System))
            .transcriber(DeferredTranscriber::default())
            .config(PipelineConfig {
                recent_event_capacity: nonzero(64),
                ..PipelineConfig::default()
            })
            .start()?;
        let session = pipeline.session();
        pipeline.stop().await;
        let finals = session
            .view()
            .recent_ordered
            .iter()
            .filter(|event| event.kind() == EventKind::UtteranceFinal)
            .count();
        assert_eq!(finals, 20, "stop lost final ASR updates");
        Ok(())
    }

    #[tokio::test(flavor = "current_thread")]
    async fn slow_internal_consumer_cannot_exceed_configured_queue_bounds()
    -> Result<(), Box<dyn std::error::Error>> {
        let pipeline = Pipeline::builder(session())
            .capture(FixtureCapture {
                frames: 100,
                frame_interval: Duration::from_millis(10),
            })
            .vad(FakeVad(Source::Mic), FakeVad(Source::System))
            .transcriber(FakeTranscriber::default())
            .annotator(SlowAnnotator)
            .config(PipelineConfig {
                audio_capacity: nonzero(128),
                transcript_capacity: nonzero(2),
                timeline_capacity: nonzero(2),
                recent_event_capacity: nonzero(128),
                ..PipelineConfig::default()
            })
            .start()?;
        let counters = pipeline.counters().clone();
        pipeline.stop().await;
        assert_eq!(counters.transcript_queue_high_water(), 2);
        assert!(counters.timeline_queue_high_water() <= 2);
        assert!(counters.transcript_updates_dropped() > 0);
        Ok(())
    }

    #[tokio::test(flavor = "current_thread")]
    async fn simulated_two_hour_call_keeps_only_the_configured_live_window()
    -> Result<(), Box<dyn std::error::Error>> {
        let config = PipelineConfig {
            audio_capacity: nonzero(8_192),
            recent_event_capacity: nonzero(24),
            checkpoint_events: nonzero(32),
            ..PipelineConfig::default()
        };
        let pipeline = Pipeline::builder(session())
            .capture(FixtureCapture {
                frames: 7_201,
                frame_interval: Duration::from_secs(1),
            })
            .vad(FakeVad(Source::Mic), FakeVad(Source::System))
            .transcriber(FakeTranscriber::default())
            .annotator(FakeAnnotator)
            .config(config)
            .start()?;
        let session = pipeline.session();
        pipeline.stop().await;
        let view = session.view();
        assert_eq!(view.recent_ordered.len(), 24);
        assert!(view.checkpointed_events > 14_000);
        assert!(
            view.recent_ordered
                .last()
                .is_some_and(|event| event.ts() >= Duration::from_secs(7_200))
        );
        Ok(())
    }

    #[tokio::test(flavor = "current_thread")]
    async fn t009_two_voice_fixture_flows_through_both_sources_headlessly()
    -> Result<(), Box<dyn std::error::Error>> {
        let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures");
        let pipeline = Pipeline::builder(session())
            .capture(WavFixtureCapture {
                mic: fixtures.join("call-01-mic.wav"),
                system: fixtures.join("call-01-sys.wav"),
            })
            .vad(FakeVad(Source::Mic), FakeVad(Source::System))
            .transcriber(FakeTranscriber::default())
            .annotator(FakeAnnotator)
            .config(PipelineConfig {
                audio_capacity: nonzero(2_048),
                recent_event_capacity: nonzero(2_048),
                checkpoint_events: nonzero(64),
                ..PipelineConfig::default()
            })
            .start()?;
        let session = pipeline.session();
        pipeline.stop().await;
        let view = session.view();
        let mut sources = [false; 2];
        for event in &view.recent_ordered {
            if let EventPayload::UtteranceFinal(utterance) = event.payload() {
                sources[source_index(utterance.source)] = true;
            }
        }
        assert_eq!(sources, [true, true]);
        assert!(view.checkpointed_events > 1_000);
        Ok(())
    }

    fn session() -> Session {
        Session::new(
            SessionId::new(11),
            CaptureTarget {
                bundle_id: Some("com.example.call".to_owned()),
                display_name: "Fixture call".to_owned(),
                window_title: Some("T011".to_owned()),
                kind: TargetKind::Window,
                audio_scoped: true,
            },
            1_753_776_000_000,
        )
    }

    const fn source_offset(source: Source) -> u64 {
        match source {
            Source::Mic => 0,
            Source::System => 1,
        }
    }

    const fn source_index(source: Source) -> usize {
        match source {
            Source::Mic => 0,
            Source::System => 1,
        }
    }

    fn nonzero(value: usize) -> NonZeroUsize {
        NonZeroUsize::new(value).unwrap_or(NonZeroUsize::MIN)
    }

    fn read_wav(path: &Path, source: Source) -> Result<Vec<AudioFrame>, CaptureError> {
        let mut reader = hound::WavReader::open(path)
            .map_err(|error| CaptureError::StreamFailed(error.to_string()))?;
        let rate = reader.spec().sample_rate;
        let samples = reader
            .samples::<i16>()
            .map(|sample| {
                sample
                    .map(|value| f32::from(value) / 32_768.0)
                    .map_err(|error| CaptureError::StreamFailed(error.to_string()))
            })
            .collect::<Result<Vec<_>, _>>()?;
        Ok(samples
            .chunks(512)
            .enumerate()
            .map(|(sequence, samples)| AudioFrame {
                source,
                samples: Arc::from(samples),
                sample_rate: rate,
                seq: sequence as u64,
                capture_ts: Instant::now(),
                stream_offset: Duration::from_secs_f64(sequence as f64 * 512.0 / f64::from(rate)),
            })
            .collect())
    }
}
