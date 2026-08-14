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
    /// The latest in-progress ASR hypothesis for each independently captured stream.
    ///
    /// Sliding-window transcription can move an utterance's estimated start timestamp as
    /// more audio arrives. The stream identity, rather than that provisional timestamp, is
    /// therefore the stable identity of the active hypothesis chain.
    active_utterances: HashMap<Source, TimelineEvent>,
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
            StageMessage::UserAnnotation(ts, anchor, text, mark) => self
                .timeline
                .append_user_annotation(ts, anchor, text, mark)
                .into_iter()
                .collect(),
            // Superseding by id, not by envelope: `checkpoint` drains the pending payload buffer
            // every 64 events, so looking the target up in `events()` lost every note older than
            // that and dropped the edit without a word. The builder retains the id.
            StageMessage::SupersedeUserAnnotation(ts, target_id, text, mark) => self
                .timeline
                .supersede_user_annotation(ts, text, mark, target_id)
                .into_iter()
                .collect(),
        };
        for event in events {
            if let EventPayload::Prosody(delta) = event.payload() {
                self.ratios[source_index(delta.source)] = delta.talk_time_ratio;
            }
            self.publish(event, output);
        }
    }

    fn append_payload(&mut self, ts: Duration, payload: EventPayload) -> TimelineEvent {
        match &payload {
            EventPayload::UtterancePartial(value) => {
                let source = value.source;
                let event = self
                    .active_utterances
                    .get(&source)
                    .and_then(|previous| {
                        self.timeline.supersede(ts, payload.clone(), previous).ok()
                    })
                    .unwrap_or_else(|| self.timeline.append(ts, payload));
                self.active_utterances.insert(source, event.clone());
                event
            }
            EventPayload::UtteranceFinal(value) => {
                let previous = self.active_utterances.remove(&value.source);
                previous
                    .as_ref()
                    .and_then(|previous| {
                        self.timeline.supersede(ts, payload.clone(), previous).ok()
                    })
                    .unwrap_or_else(|| self.timeline.append(ts, payload))
            }
            _ => self.timeline.append(ts, payload),
        }
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

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use crate::{
        CaptureTarget, EventPayload, Session, SessionId, Source, TargetKind, Utterance,
        timeline::replay,
    };

    use super::ActorState;

    fn session() -> Session {
        Session::new(
            SessionId::new(7),
            CaptureTarget {
                bundle_id: Some("com.example.meeting".to_owned()),
                display_name: "Meeting".to_owned(),
                window_title: Some("Rolling transcript".to_owned()),
                kind: TargetKind::Window,
                audio_scoped: true,
            },
            1_753_776_000_000,
        )
    }

    fn utterance(source: Source, start_ms: u64, text: &str) -> Utterance {
        Utterance {
            source,
            start: Duration::from_millis(start_ms),
            end: Duration::from_millis(start_ms.saturating_add(750)),
            text: text.to_owned(),
            avg_logprob: -0.1,
            annotations: Vec::new(),
        }
    }

    #[test]
    fn drifting_partial_start_remains_one_chain_and_final_closes_it()
    -> Result<(), Box<dyn std::error::Error>> {
        let mut state = ActorState::new(session(), 16);
        let first = state.append_payload(
            Duration::from_millis(800),
            EventPayload::UtterancePartial(utterance(Source::Mic, 0, "we")),
        );
        let shifted = state.append_payload(
            Duration::from_millis(1_300),
            EventPayload::UtterancePartial(utterance(Source::Mic, 500, "we should")),
        );
        let final_event = state.append_payload(
            Duration::from_millis(1_800),
            EventPayload::UtteranceFinal(utterance(Source::Mic, 500, "we should proceed")),
        );

        assert_eq!(
            shifted.supersedes(),
            Some(first.id()),
            "a shifted rolling-window hypothesis must replace the active card"
        );
        assert_eq!(
            final_event.supersedes(),
            Some(shifted.id()),
            "the final utterance must settle the active partial card"
        );
        assert!(
            state.active_utterances.is_empty(),
            "a final utterance must close the source's active hypothesis chain"
        );
        let replayed = replay(state.timeline.events())?;
        assert_eq!(
            replayed.active().keys().copied().collect::<Vec<_>>(),
            vec![final_event.id()],
            "the board projection must see only the settled utterance"
        );

        let next = state.append_payload(
            Duration::from_millis(2_500),
            EventPayload::UtterancePartial(utterance(Source::Mic, 2_000, "next point")),
        );
        assert_eq!(
            next.supersedes(),
            None,
            "the next utterance must not replace the previous final"
        );
        Ok(())
    }

    #[test]
    fn mic_and_meeting_audio_keep_independent_partial_chains() {
        let mut state = ActorState::new(session(), 16);
        let mic = state.append_payload(
            Duration::from_millis(700),
            EventPayload::UtterancePartial(utterance(Source::Mic, 0, "local")),
        );
        let system = state.append_payload(
            Duration::from_millis(800),
            EventPayload::UtterancePartial(utterance(Source::System, 0, "remote")),
        );
        let mic_shifted = state.append_payload(
            Duration::from_millis(1_200),
            EventPayload::UtterancePartial(utterance(Source::Mic, 500, "local update")),
        );

        assert_eq!(
            mic_shifted.supersedes(),
            Some(mic.id()),
            "mic hypotheses must replace only the mic chain"
        );
        assert_eq!(
            state
                .active_utterances
                .get(&Source::System)
                .map(|event| event.id()),
            Some(system.id()),
            "meeting-audio hypotheses must remain independent"
        );
    }
}
