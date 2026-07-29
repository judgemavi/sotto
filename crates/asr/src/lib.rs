//! On-device, sliding-window Whisper transcription.
//!
//! [`WhisperTranscriber`] owns two lock-free audio rings and one background worker.
//! The single Whisper model is deliberately shared: keeping two copies resident is a
//! poor trade on a machine already running a call. The worker gives customer/system
//! audio priority, then alternates naturally as each stream becomes due.
//!
//! # Commit policy
//!
//! Every pass emits at most one partial for the stable `(source, start)` key, and an
//! identical consecutive partial is suppressed. Words outside the configurable
//! unstable tail become final only after the complete commit candidate has agreed in
//! `agreement_passes` consecutive passes. A final advances the stream commit point;
//! committed audio is never placed in another inference window, so finals cannot
//! retract. The default is two agreeing passes and a 2.5 second unstable tail.

#![deny(warnings)]

mod ring;
mod stabilizer;

use std::{
    collections::VecDeque,
    path::PathBuf,
    sync::{Arc, Mutex, mpsc},
    thread,
    time::{Duration, Instant},
};

use ring::{Consumer, Producer};
use sotto_core::{AsrError, AudioFrame, Source, SpeechState, Transcriber, TranscriptUpdate};
use stabilizer::{Hypothesis, Stabilizer};
use whisper_rs::{FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters};

pub use ring::{RingConsumer, RingProducer, spsc_ring};

const SAMPLE_RATE: u32 = 16_000;

/// Bundled-weight-free model size selected by the user/download UI.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum ModelSize {
    BaseEn,
    #[default]
    SmallEn,
    MediumEn,
}

/// Runtime policy for sliding inference and stable commits.
#[derive(Clone, Debug)]
pub struct Config {
    pub model_path: PathBuf,
    pub model_size: ModelSize,
    pub ring_capacity: Duration,
    pub window: Duration,
    pub cadence: Duration,
    pub unstable_tail: Duration,
    pub agreement_passes: usize,
    pub idle_unload: Duration,
    pub vad_gating: bool,
}

impl Config {
    #[must_use]
    pub fn new(model_path: impl Into<PathBuf>) -> Self {
        Self {
            model_path: model_path.into(),
            model_size: ModelSize::default(),
            ring_capacity: Duration::from_secs(30),
            window: Duration::from_secs(10),
            cadence: Duration::from_millis(500),
            unstable_tail: Duration::from_millis(2_500),
            agreement_passes: 2,
            idle_unload: Duration::from_secs(120),
            vad_gating: true,
        }
    }
}

/// Non-blocking front end to the shared Whisper worker.
pub struct WhisperTranscriber {
    mic: Producer,
    system: Producer,
    control: mpsc::Sender<Control>,
    output: mpsc::Receiver<TranscriptUpdate>,
    errors: Arc<Mutex<VecDeque<AsrError>>>,
    worker: Option<thread::JoinHandle<()>>,
}

impl WhisperTranscriber {
    pub fn new(config: Config) -> Result<Self, AsrError> {
        validate(&config)?;
        let capacity = duration_samples(config.ring_capacity);
        let (mic, mic_consumer) = spsc_ring(capacity);
        let (system, system_consumer) = spsc_ring(capacity);
        let (control_tx, control_rx) = mpsc::channel();
        let (output_tx, output) = mpsc::channel();
        let errors = Arc::new(Mutex::new(VecDeque::new()));
        let worker_errors = Arc::clone(&errors);
        let worker = thread::Builder::new()
            .name("sotto-whisper".to_owned())
            .spawn(move || {
                Worker::new(
                    config,
                    mic_consumer,
                    system_consumer,
                    control_rx,
                    output_tx,
                    worker_errors,
                )
                .run();
            })
            .map_err(|error| AsrError::ModelLoad(error.to_string()))?;
        Ok(Self {
            mic,
            system,
            control: control_tx,
            output,
            errors,
            worker: Some(worker),
        })
    }

    /// Applies an optional VAD transition. With gating disabled this is ignored.
    pub fn set_speech_state(&self, source: Source, state: SpeechState) {
        let _ = self.control.send(Control::Speech(source, state));
    }

    /// Drains worker failures without blocking the realtime consumer.
    pub fn poll_errors(&self) -> Vec<AsrError> {
        match self.errors.lock() {
            Ok(mut errors) => errors.drain(..).collect(),
            Err(poisoned) => poisoned.into_inner().drain(..).collect(),
        }
    }
}

impl Transcriber for WhisperTranscriber {
    fn push(&mut self, frame: &AudioFrame) {
        if frame.sample_rate != SAMPLE_RATE {
            return;
        }
        match frame.source {
            Source::Mic => self.mic.push_slice(&frame.samples),
            Source::System => self.system.push_slice(&frame.samples),
        }
    }

    fn poll(&mut self) -> Vec<TranscriptUpdate> {
        self.output.try_iter().collect()
    }
}

impl Drop for WhisperTranscriber {
    fn drop(&mut self) {
        let _ = self.control.send(Control::Shutdown);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

fn validate(config: &Config) -> Result<(), AsrError> {
    if !config.model_path.is_file() {
        return Err(AsrError::ModelNotFound {
            path: config.model_path.display().to_string(),
        });
    }
    if config.agreement_passes == 0 || config.window <= config.unstable_tail {
        return Err(AsrError::ModelLoad(
            "agreement_passes must be positive and window must exceed unstable_tail".to_owned(),
        ));
    }
    Ok(())
}

fn duration_samples(duration: Duration) -> usize {
    usize::try_from(duration.as_millis())
        .unwrap_or(usize::MAX)
        .saturating_mul(16_000)
        / 1_000
}

enum Control {
    Speech(Source, SpeechState),
    Shutdown,
}

struct StreamState {
    consumer: Consumer,
    stabilizer: Stabilizer,
    speech: bool,
    history: VecDeque<f32>,
    consumed_samples: u64,
}

struct Worker {
    config: Config,
    mic: StreamState,
    system: StreamState,
    control: mpsc::Receiver<Control>,
    output: mpsc::Sender<TranscriptUpdate>,
    errors: Arc<Mutex<VecDeque<AsrError>>>,
    context: Option<WhisperContext>,
    last_inference: Instant,
}

impl Worker {
    fn new(
        config: Config,
        mic: Consumer,
        system: Consumer,
        control: mpsc::Receiver<Control>,
        output: mpsc::Sender<TranscriptUpdate>,
        errors: Arc<Mutex<VecDeque<AsrError>>>,
    ) -> Self {
        let agreement_passes = config.agreement_passes;
        let unstable_tail = config.unstable_tail;
        let stabilizer = || Stabilizer::new(agreement_passes, unstable_tail);
        Self {
            config,
            mic: StreamState {
                consumer: mic,
                stabilizer: stabilizer(),
                speech: false,
                history: VecDeque::new(),
                consumed_samples: 0,
            },
            system: StreamState {
                consumer: system,
                stabilizer: stabilizer(),
                speech: false,
                history: VecDeque::new(),
                consumed_samples: 0,
            },
            control,
            output,
            errors,
            context: None,
            last_inference: Instant::now(),
        }
    }

    fn run(mut self) {
        loop {
            match self.control.recv_timeout(self.config.cadence) {
                Ok(Control::Shutdown) => return,
                Ok(Control::Speech(source, state)) => {
                    self.stream_mut(source).speech = state == SpeechState::SpeechStart
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => return,
                Err(mpsc::RecvTimeoutError::Timeout) => {}
            }
            while let Ok(control) = self.control.try_recv() {
                match control {
                    Control::Shutdown => return,
                    Control::Speech(source, state) => {
                        self.stream_mut(source).speech = state == SpeechState::SpeechStart
                    }
                }
            }
            // Customer audio wins contention by being processed first.
            self.transcribe_due(Source::System);
            self.transcribe_due(Source::Mic);
            if self.context.is_some() && self.last_inference.elapsed() >= self.config.idle_unload {
                self.context = None;
            }
        }
    }

    fn stream_mut(&mut self, source: Source) -> &mut StreamState {
        match source {
            Source::Mic => &mut self.mic,
            Source::System => &mut self.system,
        }
    }

    fn transcribe_due(&mut self, source: Source) {
        let gating = self.config.vad_gating;
        let window_samples = duration_samples(self.config.window);
        let cadence_samples = duration_samples(self.config.cadence);
        let stream = self.stream_mut(source);
        if (gating && !stream.speech) || stream.consumer.available() < cadence_samples {
            return;
        }
        let (fresh, _) = stream.consumer.take_latest(usize::MAX);
        stream.consumed_samples = stream
            .consumed_samples
            .saturating_add(u64::try_from(fresh.len()).unwrap_or(u64::MAX));
        stream.history.extend(fresh);
        while stream.history.len() > window_samples {
            stream.history.pop_front();
        }
        let samples = stream.history.iter().copied().collect::<Vec<_>>();
        if samples.is_empty() {
            return;
        }
        if let Err(error) = self.infer(source, samples) {
            self.record_error(error);
        }
    }

    fn infer(&mut self, source: Source, samples: Vec<f32>) -> Result<(), AsrError> {
        if self.context.is_none() {
            tracing::info!(model = %self.config.model_path.display(), "loading whisper model; inspect whisper.cpp backend log for Metal activation");
            let params = WhisperContextParameters::default();
            self.context = Some(
                WhisperContext::new_with_params(
                    self.config.model_path.to_string_lossy().as_ref(),
                    params,
                )
                .map_err(|e| AsrError::ModelLoad(e.to_string()))?,
            );
        }
        let context = self
            .context
            .as_ref()
            .ok_or_else(|| AsrError::ModelLoad("context absent after load".to_owned()))?;
        let mut state = context
            .create_state()
            .map_err(|e| AsrError::Inference(e.to_string()))?;
        let mut params = FullParams::new(SamplingStrategy::Greedy { best_of: 1 });
        params.set_n_threads(4);
        params.set_language(Some("en"));
        params.set_print_progress(false);
        params.set_print_realtime(false);
        params.set_print_timestamps(false);
        state
            .full(params, &samples)
            .map_err(|e| AsrError::Inference(e.to_string()))?;
        let segments = state
            .as_iter()
            .map(|segment| {
                let start = Duration::from_millis(
                    u64::try_from(segment.start_timestamp().max(0)).unwrap_or(0) * 10,
                );
                let end = Duration::from_millis(
                    u64::try_from(segment.end_timestamp().max(0)).unwrap_or(0) * 10,
                );
                Ok::<Hypothesis, AsrError>(Hypothesis {
                    start,
                    end,
                    text: segment
                        .to_str_lossy()
                        .map_err(|error| AsrError::Inference(error.to_string()))?
                        .into_owned(),
                    avg_logprob: 0.0,
                })
            })
            .collect::<Result<Vec<_>, _>>()?;
        let stream = self.stream_mut(source);
        let base_samples = stream
            .consumed_samples
            .saturating_sub(u64::try_from(stream.history.len()).unwrap_or(u64::MAX));
        let base = Duration::from_secs_f64(base_samples as f64 / f64::from(SAMPLE_RATE));
        let updates = self
            .stream_mut(source)
            .stabilizer
            .observe(source, base, &segments);
        for update in updates {
            let _ = self.output.send(update);
        }
        self.last_inference = Instant::now();
        Ok(())
    }

    fn record_error(&self, error: AsrError) {
        match self.errors.lock() {
            Ok(mut errors) => errors.push_back(error),
            Err(poisoned) => poisoned.into_inner().push_back(error),
        }
    }
}
