//! The single adapter from T012's GPUI timeline entity into board geometry.

use gpui::{Context, Entity, Subscription};

use crate::devwindow::TimelineState;
use sotto_core::SessionId;

use super::BoardProjection;

/// Board-owned projection state observing the one shared timeline entity.
///
/// No second Tokio channel is introduced. Both live capture and persisted replay append through
/// `TimelineState`; this adapter consumes only the unseen suffix of that state.
pub struct BoardState {
    timeline: Option<Entity<TimelineState>>,
    projection: BoardProjection,
    consumed_events: usize,
    session_filter: Option<SessionId>,
    _timeline_subscription: Option<Subscription>,
}

impl BoardState {
    #[must_use]
    pub fn new(timeline: Entity<TimelineState>, cx: &mut Context<Self>) -> Self {
        let timeline_subscription = cx.observe(&timeline, |_, _, cx| cx.notify());
        Self {
            timeline: Some(timeline),
            projection: BoardProjection::default(),
            consumed_events: 0,
            session_filter: None,
            _timeline_subscription: Some(timeline_subscription),
        }
    }

    #[must_use]
    pub fn from_events(events: &[sotto_core::TimelineEvent]) -> Self {
        let mut projection = BoardProjection::default();
        projection.extend(events);
        Self {
            timeline: None,
            projection,
            consumed_events: events.len(),
            session_filter: None,
            _timeline_subscription: None,
        }
    }

    /// Observes the shared append-only seam while admitting only one exact live session.
    #[must_use]
    pub fn for_live_session(
        timeline: Entity<TimelineState>,
        session_id: SessionId,
        cx: &mut Context<Self>,
    ) -> Self {
        let timeline_subscription = cx.observe(&timeline, |_, _, cx| cx.notify());
        Self {
            timeline: Some(timeline),
            projection: BoardProjection::default(),
            consumed_events: 0,
            session_filter: Some(session_id),
            _timeline_subscription: Some(timeline_subscription),
        }
    }

    /// Applies only timeline events not seen by the board before this call.
    ///
    /// Returns the number consumed, which lets a renderer skip work on unrelated redraws.
    pub fn refresh(&mut self, cx: &Context<Self>) -> usize {
        let Some(timeline) = &self.timeline else {
            return 0;
        };
        let new_events = {
            let timeline = timeline.read(cx);
            timeline
                .events()
                .get(self.consumed_events..)
                .unwrap_or_default()
                .to_vec()
        };
        self.consumed_events = self.consumed_events.saturating_add(new_events.len());
        self.extend_events(&new_events)
    }

    fn extend_events(&mut self, new_events: &[sotto_core::TimelineEvent]) -> usize {
        let admitted: Vec<_> = new_events
            .iter()
            .filter(|event| {
                self.session_filter
                    .is_none_or(|session_id| event.session_id() == session_id)
            })
            .cloned()
            .collect();
        self.projection.extend(&admitted);
        admitted.len()
    }

    #[must_use]
    pub const fn projection(&self) -> &BoardProjection {
        &self.projection
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use sotto_core::{
        CaptureTarget, EventPayload, Session, SessionId, Source, TargetKind, TimelineBuilder,
        Utterance,
    };

    use super::BoardState;
    use crate::board::BoardEventKey;

    fn event(session_id: u128, text: &str) -> sotto_core::TimelineEvent {
        let mut timeline = TimelineBuilder::new(Session::new(
            SessionId::new(session_id),
            CaptureTarget {
                bundle_id: None,
                display_name: "Meeting".to_owned(),
                window_title: None,
                kind: TargetKind::Window,
                audio_scoped: true,
            },
            0,
        ));
        timeline.append(
            Duration::ZERO,
            EventPayload::UtteranceFinal(Utterance {
                source: Source::Mic,
                start: Duration::ZERO,
                end: Duration::from_secs(1),
                text: text.to_owned(),
                avg_logprob: 0.0,
                annotations: Vec::new(),
            }),
        )
    }

    fn filtered_state(session_id: SessionId) -> BoardState {
        BoardState {
            timeline: None,
            projection: Default::default(),
            consumed_events: 0,
            session_filter: Some(session_id),
            _timeline_subscription: None,
        }
    }

    #[test]
    fn second_live_session_never_replays_first_session_events() {
        let first = event(1, "first meeting");
        let second = event(2, "second meeting");
        let late_first = event(1, "late first meeting event");

        let mut first_board = filtered_state(SessionId::new(1));
        assert_eq!(first_board.extend_events(std::slice::from_ref(&first)), 1);

        let mut second_board = filtered_state(SessionId::new(2));
        assert_eq!(
            second_board.extend_events(&[first.clone(), second.clone(), late_first]),
            1
        );
        assert!(
            second_board
                .projection()
                .item_for_event(BoardEventKey::new(first.session_id(), first.id()))
                .is_none()
        );
        assert!(
            second_board
                .projection()
                .item_for_event(BoardEventKey::new(second.session_id(), second.id()))
                .is_some()
        );
    }
}
