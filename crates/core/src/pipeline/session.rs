use std::{
    collections::{HashMap, VecDeque},
    num::NonZeroUsize,
    sync::{
        Arc, RwLock,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::Duration,
};

use tokio::{sync::mpsc, task::JoinHandle};

use super::{PersistenceAck, PersistenceBatch, StageMessage};
use crate::{
    EventPayload, PipelineError, RagError, Session, Source, TimelineBuilder, TimelineEvent,
    Utterance,
};

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub enum RecordingState {
    #[default]
    Recording,
    /// Durable timeline writes are terminally disabled; the live map still operates.
    Degraded { reason: String },
}

#[derive(Clone, Debug, Default)]
pub struct SessionView {
    /// Bounded append order used by record/replay adapters.
    pub recent_append_order: Vec<TimelineEvent>,
    pub recent_ordered: Vec<TimelineEvent>,
    pub mic_talk_time_ratio: f32,
    pub system_talk_time_ratio: f32,
    /// Events confirmed committed by the persistence worker.
    pub checkpointed_events: u64,
    pub pending_persistence_events: usize,
    pub recording_state: RecordingState,
    pub unrecorded_events: u64,
}

#[derive(Clone)]
pub struct CallSession {
    view: Arc<RwLock<SessionView>>,
    recent_capacity: NonZeroUsize,
    session: Arc<RwLock<Option<Session>>>,
}

impl CallSession {
    pub(super) fn new(session: Session, recent_capacity: NonZeroUsize) -> Self {
        Self {
            view: Arc::new(RwLock::new(SessionView::default())),
            recent_capacity,
            session: Arc::new(RwLock::new(Some(session))),
        }
    }

    #[must_use]
    pub fn view(&self) -> SessionView {
        self.view
            .read()
            .map_or_else(|value| value.into_inner().clone(), |value| value.clone())
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "actor wiring is explicit at the task boundary"
    )]
    pub(super) fn spawn(
        &self,
        mut input: mpsc::Receiver<StageMessage>,
        mut control: mpsc::Receiver<StageMessage>,
        mut acknowledgements: mpsc::Receiver<PersistenceAck>,
        event_output: tokio::sync::broadcast::Sender<TimelineEvent>,
        persistence: mpsc::Sender<PersistenceBatch>,
        failed: Arc<AtomicBool>,
        saturated: Arc<AtomicU64>,
        checkpoint_events: NonZeroUsize,
        pending_limit: NonZeroUsize,
    ) -> JoinHandle<()> {
        let view = Arc::clone(&self.view);
        let session = Arc::clone(&self.session);
        let recent_capacity = self.recent_capacity.get();
        tokio::spawn(async move {
            let Some(value) = session.write().ok().and_then(|mut value| value.take()) else {
                return;
            };
            let mut state = ActorState::new(value, recent_capacity);
            let mut input_open = true;
            let mut control_open = true;
            while input_open || control_open {
                tokio::select! {
                    message = input.recv(), if input_open => match message {
                        Some(message) => state.append(message, &event_output),
                        None => input_open = false,
                    },
                    message = control.recv(), if control_open => match message {
                        Some(message) => state.append(message, &event_output),
                        None => control_open = false,
                    },
                    ack = acknowledgements.recv(), if state.outstanding > 0 => {
                        state.apply_ack(ack, &event_output);
                    }
                }
                state.attempt_checkpoint(
                    &persistence,
                    &failed,
                    &saturated,
                    checkpoint_events,
                    pending_limit,
                    &event_output,
                );
                state.update_view(&view);
            }
            if !failed.load(Ordering::Acquire)
                && !state.timeline.events().is_empty()
                && let Ok(permit) = persistence.reserve().await
            {
                let events = state.timeline.checkpoint();
                state.outstanding = state.outstanding.saturating_add(1);
                permit.send(PersistenceBatch { events });
            }
            drop(persistence);
            while state.outstanding > 0 {
                state.apply_ack(acknowledgements.recv().await, &event_output);
            }
            if failed.load(Ordering::Acquire) {
                state.discard_pending();
            }
            state.update_view(&view);
        })
    }
}

struct ActorState {
    timeline: TimelineBuilder,
    recent: VecDeque<TimelineEvent>,
    recent_capacity: usize,
    durable: u64,
    outstanding: usize,
    ratios: [f32; 2],
    recording: RecordingState,
    unrecorded: u64,
    active_utterances: HashMap<(Source, Duration), TimelineEvent>,
}

impl ActorState {
    fn new(session: Session, recent_capacity: usize) -> Self {
        Self {
            timeline: TimelineBuilder::new(session),
            recent: VecDeque::with_capacity(recent_capacity),
            recent_capacity,
            durable: 0,
            outstanding: 0,
            ratios: [0.0; 2],
            recording: RecordingState::Recording,
            unrecorded: 0,
            active_utterances: HashMap::new(),
        }
    }

    fn append(
        &mut self,
        message: StageMessage,
        output: &tokio::sync::broadcast::Sender<TimelineEvent>,
    ) {
        let events = match message {
            StageMessage::Payload(ts, payload) => vec![self.append_payload(ts, payload)],
            StageMessage::Annotated(ts, utterance, delta) => {
                let mut events =
                    vec![self.append_payload(ts, EventPayload::UtteranceFinal(utterance))];
                if let Some(delta) = delta {
                    events.push(self.timeline.append(ts, EventPayload::Prosody(delta)));
                }
                events
            }
        };
        for event in events {
            if let EventPayload::Prosody(delta) = event.payload() {
                self.ratios[source_index(delta.source)] = delta.talk_time_ratio;
            }
            self.publish(event, output);
        }
    }

    fn append_payload(&mut self, ts: Duration, payload: EventPayload) -> TimelineEvent {
        let key = match &payload {
            EventPayload::UtterancePartial(value) | EventPayload::UtteranceFinal(value) => {
                Some((value.source, value.start))
            }
            _ => None,
        };
        let event = key
            .and_then(|key| self.active_utterances.get(&key))
            .and_then(|previous| self.timeline.supersede(ts, payload.clone(), previous).ok())
            .unwrap_or_else(|| self.timeline.append(ts, payload));
        if let Some(key) = key {
            self.active_utterances.insert(key, event.clone());
        }
        event
    }

    fn attempt_checkpoint(
        &mut self,
        persistence: &mpsc::Sender<PersistenceBatch>,
        failed: &AtomicBool,
        saturated: &AtomicU64,
        checkpoint_events: NonZeroUsize,
        pending_limit: NonZeroUsize,
        output: &tokio::sync::broadcast::Sender<TimelineEvent>,
    ) {
        if self.timeline.events().len() < checkpoint_events.get() {
            return;
        }
        if failed.load(Ordering::Acquire) {
            self.discard_pending();
            return;
        }
        match persistence.try_reserve() {
            Ok(permit) => {
                let events = self.timeline.checkpoint();
                self.outstanding = self.outstanding.saturating_add(1);
                permit.send(PersistenceBatch { events });
            }
            Err(mpsc::error::TrySendError::Full(_)) => {
                saturated.fetch_add(1, Ordering::Relaxed);
                if self.timeline.events().len() >= pending_limit.get() {
                    failed.store(true, Ordering::Release);
                    let reason = "timeline persistence stopped after sustained saturation; live session continues without recording".to_owned();
                    self.recording = RecordingState::Degraded {
                        reason: reason.clone(),
                    };
                    let ts = self
                        .timeline
                        .events()
                        .last()
                        .map_or(Duration::ZERO, TimelineEvent::ts);
                    let event = self.timeline.append(
                        ts,
                        EventPayload::Error(PipelineError::Rag(RagError::Storage(reason))),
                    );
                    self.publish(event, output);
                    self.discard_pending();
                }
            }
            Err(mpsc::error::TrySendError::Closed(_)) => {
                failed.store(true, Ordering::Release);
                self.recording = RecordingState::Degraded { reason:
                    "timeline persistence channel closed; live session continues without recording".to_owned() };
            }
        }
    }

    fn apply_ack(
        &mut self,
        ack: Option<PersistenceAck>,
        output: &tokio::sync::broadcast::Sender<TimelineEvent>,
    ) {
        let Some(ack) = ack else {
            self.outstanding = 0;
            return;
        };
        self.outstanding = self.outstanding.saturating_sub(1);
        match ack.result {
            Ok(()) => {
                self.durable = self
                    .durable
                    .saturating_add(u64::try_from(ack.count).unwrap_or(u64::MAX))
            }
            Err(error) => {
                self.unrecorded = self
                    .unrecorded
                    .saturating_add(u64::try_from(ack.count).unwrap_or(u64::MAX));
                self.recording = RecordingState::Degraded {
                    reason: format!(
                        "timeline persistence stopped after write failure: {error}; live session continues without recording"
                    ),
                };
                let event = self.timeline.append(
                    ack.timestamp,
                    EventPayload::Error(PipelineError::Rag(error)),
                );
                self.publish(event, output);
            }
        }
    }

    fn discard_pending(&mut self) {
        self.unrecorded = self
            .unrecorded
            .saturating_add(u64::try_from(self.timeline.checkpoint().len()).unwrap_or(u64::MAX));
    }

    fn publish(
        &mut self,
        event: TimelineEvent,
        output: &tokio::sync::broadcast::Sender<TimelineEvent>,
    ) {
        if self.recent.len() == self.recent_capacity {
            self.recent.pop_front();
        }
        self.recent.push_back(event.clone());
        let _ = output.send(event);
    }

    fn update_view(&self, view: &RwLock<SessionView>) {
        let mut ordered = self.recent.iter().cloned().collect::<Vec<_>>();
        ordered.sort_by(event_order);
        let next = SessionView {
            recent_append_order: self.recent.iter().cloned().collect(),
            recent_ordered: ordered,
            mic_talk_time_ratio: self.ratios[0],
            system_talk_time_ratio: self.ratios[1],
            checkpointed_events: self.durable,
            pending_persistence_events: self.timeline.events().len(),
            recording_state: self.recording.clone(),
            unrecorded_events: self.unrecorded,
        };
        match view.write() {
            Ok(mut value) => *value = next,
            Err(value) => *value.into_inner() = next,
        }
    }
}

fn event_order(left: &TimelineEvent, right: &TimelineEvent) -> std::cmp::Ordering {
    event_key(left).cmp(&event_key(right))
}
fn event_key(event: &TimelineEvent) -> (Duration, Duration, u8, u8, u64) {
    let (start, end, finality, source) = match event.payload() {
        EventPayload::UtteranceFinal(value) => utterance_key(value, 0),
        EventPayload::UtterancePartial(value) => utterance_key(value, 1),
        _ => (event.ts(), event.ts(), 2, 2),
    };
    (start, end, finality, source, event.id().get())
}
fn utterance_key(value: &Utterance, finality: u8) -> (Duration, Duration, u8, u8) {
    (
        value.start,
        value.end,
        finality,
        match value.source {
            Source::System => 0,
            Source::Mic => 1,
        },
    )
}
const fn source_index(source: Source) -> usize {
    match source {
        Source::Mic => 0,
        Source::System => 1,
    }
}
