use std::{
    collections::{BTreeMap, HashSet},
    time::Duration,
};

use thiserror::Error;

use super::{EventId, EventPayload, Session, SessionId, TimelineEvent};

/// Rejected append-only timeline operation.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum TimelineError {
    #[error("event belongs to session {actual:?}, expected {expected:?}")]
    ForeignSession {
        expected: SessionId,
        actual: SessionId,
    },
    #[error("superseded event {0:?} is not present in this timeline")]
    UnknownSupersededEvent(EventId),
    #[error("event {superseded:?} must precede replacement {replacement:?}")]
    SupersededEventNotEarlier {
        superseded: EventId,
        replacement: EventId,
    },
    #[error("event ids are not strictly increasing: {previous:?} then {current:?}")]
    NonMonotonicId { previous: EventId, current: EventId },
}

/// Constructs valid events for one session and retains the append-only log.
#[derive(Debug)]
pub struct TimelineBuilder {
    session: Session,
    known_ids: HashSet<EventId>,
    active_ids: HashSet<EventId>,
    /// Events not yet handed to persistence by `checkpoint`.
    events: Vec<TimelineEvent>,
}

impl TimelineBuilder {
    #[must_use]
    pub fn new(session: Session) -> Self {
        Self {
            session,
            known_ids: HashSet::new(),
            active_ids: HashSet::new(),
            events: Vec::new(),
        }
    }

    pub fn append(&mut self, ts: Duration, payload: EventPayload) -> TimelineEvent {
        let event = TimelineEvent::new(
            self.session.next_event_id(),
            self.session.id(),
            ts,
            None,
            payload,
        );
        self.known_ids.insert(event.id());
        self.active_ids.insert(event.id());
        self.events.push(event.clone());
        event
    }

    pub fn supersede(
        &mut self,
        ts: Duration,
        payload: EventPayload,
        target: &TimelineEvent,
    ) -> Result<TimelineEvent, TimelineError> {
        if target.session_id() != self.session.id() {
            return Err(TimelineError::ForeignSession {
                expected: self.session.id(),
                actual: target.session_id(),
            });
        }

        let replacement = self.session.peek_next_event_id();
        if target.id() >= replacement {
            return Err(TimelineError::SupersededEventNotEarlier {
                superseded: target.id(),
                replacement,
            });
        }
        if !self.known_ids.contains(&target.id()) || !self.active_ids.contains(&target.id()) {
            return Err(TimelineError::UnknownSupersededEvent(target.id()));
        }

        let event = TimelineEvent::new(
            self.session.next_event_id(),
            self.session.id(),
            ts,
            Some(target.id()),
            payload,
        );
        self.active_ids.remove(&target.id());
        self.known_ids.insert(event.id());
        self.active_ids.insert(event.id());
        self.events.push(event.clone());
        Ok(event)
    }

    #[must_use]
    pub fn events(&self) -> &[TimelineEvent] {
        &self.events
    }

    /// Drains events ready for durable persistence while retaining compact validation
    /// state for every allocated id.
    ///
    /// T011 must persist the returned batch before dropping it. A previously drained
    /// event may still be superseded if it remains active; callers only need retain the
    /// small target envelope, not its place in this pending payload buffer.
    pub fn checkpoint(&mut self) -> Vec<TimelineEvent> {
        std::mem::take(&mut self.events)
    }

    #[must_use]
    pub fn into_events(self) -> Vec<TimelineEvent> {
        self.events
    }
}

/// Deterministic projection containing only the latest active event versions.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ReplayState {
    active: BTreeMap<EventId, TimelineEvent>,
}

/// Best-effort replay result for persisted timelines.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct LenientReplay {
    state: ReplayState,
    issues: Vec<TimelineError>,
}

impl LenientReplay {
    #[must_use]
    pub const fn state(&self) -> &ReplayState {
        &self.state
    }

    #[must_use]
    pub fn issues(&self) -> &[TimelineError] {
        &self.issues
    }
}

impl ReplayState {
    #[must_use]
    pub fn active(&self) -> &BTreeMap<EventId, TimelineEvent> {
        &self.active
    }
}

pub fn replay(events: &[TimelineEvent]) -> Result<ReplayState, TimelineError> {
    let mut state = ReplayState::default();
    let mut previous = None;
    let session_id = events.first().map(TimelineEvent::session_id);

    for event in events {
        apply_event(&mut state, &mut previous, session_id, event)?;
    }
    Ok(state)
}

/// Replays a persisted timeline without allowing one malformed event to hide the call.
///
/// Invalid events are skipped and returned in `issues`; all valid events still render.
#[must_use]
pub fn replay_lenient(events: &[TimelineEvent]) -> LenientReplay {
    let mut result = LenientReplay::default();
    let mut previous = None;
    let session_id = events.first().map(TimelineEvent::session_id);

    for event in events {
        if let Err(error) = apply_event(&mut result.state, &mut previous, session_id, event) {
            result.issues.push(error);
        }
    }
    result
}

fn apply_event(
    state: &mut ReplayState,
    previous: &mut Option<EventId>,
    session_id: Option<SessionId>,
    event: &TimelineEvent,
) -> Result<(), TimelineError> {
    if let Some(expected) = session_id
        && event.session_id() != expected
    {
        return Err(TimelineError::ForeignSession {
            expected,
            actual: event.session_id(),
        });
    }
    if let Some(previous_id) = *previous
        && event.id() <= previous_id
    {
        return Err(TimelineError::NonMonotonicId {
            previous: previous_id,
            current: event.id(),
        });
    }
    if let Some(target) = event.supersedes() {
        if target >= event.id() {
            return Err(TimelineError::SupersededEventNotEarlier {
                superseded: target,
                replacement: event.id(),
            });
        }
        if !state.active.contains_key(&target) {
            return Err(TimelineError::UnknownSupersededEvent(target));
        }
        state.active.remove(&target);
    }
    state.active.insert(event.id(), event.clone());
    *previous = Some(event.id());
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use crate::{Annotation, Source, Utterance};

    use super::{
        EventPayload, Session, SessionId, TimelineBuilder, TimelineError, replay, replay_lenient,
    };

    fn utterance(text: &str) -> Utterance {
        Utterance {
            source: Source::System,
            start: Duration::ZERO,
            end: Duration::from_secs(1),
            text: text.to_owned(),
            avg_logprob: -0.1,
            annotations: vec![Annotation::Hesitant],
        }
    }

    #[test]
    fn supersede_chain_replays_deterministically() -> Result<(), Box<dyn std::error::Error>> {
        let mut builder = TimelineBuilder::new(Session::new(SessionId::new(7)));
        let partial = builder.append(
            Duration::from_millis(100),
            EventPayload::UtterancePartial(utterance("we")),
        );
        let longer = builder.supersede(
            Duration::from_millis(200),
            EventPayload::UtterancePartial(utterance("we need")),
            &partial,
        )?;
        let final_event = builder.supersede(
            Duration::from_millis(300),
            EventPayload::UtteranceFinal(utterance("we need security review")),
            &longer,
        )?;

        let first = replay(builder.events())?;
        let second = replay(builder.events())?;
        assert_eq!(
            first, second,
            "replaying the same log must be deterministic"
        );
        assert_eq!(
            first.active().keys().copied().collect::<Vec<_>>(),
            vec![final_event.id()],
            "only the latest correction should remain active"
        );
        assert_eq!(
            builder.events().len(),
            3,
            "superseded events must remain in the log"
        );
        Ok(())
    }

    #[test]
    fn rejects_foreign_session_supersede() {
        let mut first = TimelineBuilder::new(Session::new(SessionId::new(1)));
        let foreign = first.append(Duration::ZERO, EventPayload::UtteranceFinal(utterance("x")));
        let mut second = TimelineBuilder::new(Session::new(SessionId::new(2)));

        let result = second.supersede(
            Duration::from_secs(1),
            EventPayload::UtteranceFinal(utterance("y")),
            &foreign,
        );
        assert!(
            matches!(result, Err(TimelineError::ForeignSession { .. })),
            "foreign-session supersession must be rejected"
        );
    }

    #[test]
    fn rejects_later_event_supersede() {
        let session_id = SessionId::new(3);
        let mut builder = TimelineBuilder::new(Session::new(session_id));
        let future = super::TimelineEvent::new(
            super::EventId::new(9),
            session_id,
            Duration::from_secs(9),
            None,
            EventPayload::UtteranceFinal(utterance("future")),
        );

        let result = builder.supersede(
            Duration::from_secs(1),
            EventPayload::UtteranceFinal(utterance("now")),
            &future,
        );
        assert!(
            matches!(result, Err(TimelineError::SupersededEventNotEarlier { .. })),
            "later-event supersession must be rejected"
        );
    }

    #[test]
    fn checkpoint_bounds_payload_buffer_without_losing_supersession() -> Result<(), TimelineError> {
        let mut builder = TimelineBuilder::new(Session::new(SessionId::new(4)));
        let partial = builder.append(
            Duration::ZERO,
            EventPayload::UtterancePartial(utterance("partial")),
        );
        let persisted = builder.checkpoint();
        assert_eq!(
            persisted.len(),
            1,
            "checkpoint must return pending payloads"
        );
        assert!(
            builder.events().is_empty(),
            "checkpoint must bound pending payload memory"
        );

        let replacement = builder.supersede(
            Duration::from_secs(1),
            EventPayload::UtteranceFinal(utterance("final")),
            &partial,
        )?;
        assert_eq!(
            replacement.supersedes(),
            Some(partial.id()),
            "an active checkpointed event must remain supersedable"
        );
        Ok(())
    }

    #[test]
    fn lenient_replay_reports_and_skips_bad_rows() {
        let session_id = SessionId::new(5);
        let valid = super::TimelineEvent::new(
            super::EventId::new(1),
            session_id,
            Duration::ZERO,
            None,
            EventPayload::UtteranceFinal(utterance("valid")),
        );
        let malformed = super::TimelineEvent::new(
            super::EventId::new(2),
            session_id,
            Duration::from_secs(1),
            Some(super::EventId::new(99)),
            EventPayload::UtteranceFinal(utterance("bad")),
        );
        let recovered = replay_lenient(&[valid.clone(), malformed]);

        assert_eq!(recovered.issues().len(), 1, "bad rows must be reported");
        assert_eq!(
            recovered.state().active().get(&valid.id()),
            Some(&valid),
            "valid rows must remain renderable"
        );
    }
}
