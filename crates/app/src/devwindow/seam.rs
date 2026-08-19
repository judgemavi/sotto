use std::time::Duration;

use gpui::{App, AppContext, Entity, Timer};
use sotto_core::TimelineEvent;
use tokio::sync::mpsc;

/// GPUI-owned append-only projection of the session timeline.
#[derive(Debug, Default)]
pub struct TimelineState {
    events: Vec<TimelineEvent>,
}

impl TimelineState {
    #[must_use]
    pub fn events(&self) -> &[TimelineEvent] {
        &self.events
    }

    fn append(&mut self, event: TimelineEvent) {
        self.events.push(event);
    }
}

/// The sole sending handle across Sotto's Tokio-to-GPUI boundary.
#[derive(Clone)]
pub struct TimelineIngress(mpsc::Sender<TimelineEvent>);

impl TimelineIngress {
    pub async fn send(&self, event: TimelineEvent) -> Result<(), TimelineEvent> {
        self.0.send(event).await.map_err(|error| error.0)
    }
}

#[cfg(test)]
pub(crate) fn test_ingress(capacity: usize) -> (TimelineIngress, mpsc::Receiver<TimelineEvent>) {
    let (sender, receiver) = mpsc::channel(capacity.max(1));
    (TimelineIngress(sender), receiver)
}

/// Creates and drains the one bounded ingress channel into a shared GPUI entity.
///
/// All UI projections consume `TimelineState`; none subscribe directly to the core bus.
#[must_use]
pub fn attach_ingress(cx: &mut App, capacity: usize) -> (TimelineIngress, Entity<TimelineState>) {
    let (sender, mut receiver) = mpsc::channel(capacity.max(1));
    let state = cx.new(|_| TimelineState::default());
    let drain_state = state.clone();
    cx.spawn(async move |cx| {
        loop {
            Timer::after(Duration::from_millis(16)).await;
            let mut batch = Vec::new();
            while let Ok(event) = receiver.try_recv() {
                batch.push(event);
            }
            if batch.is_empty() {
                continue;
            }
            if cx
                .update(|cx| {
                    drain_state.update(cx, |state, cx| {
                        for event in batch {
                            state.append(event);
                        }
                        cx.notify();
                    });
                })
                .is_err()
            {
                return;
            }
        }
    })
    .detach();
    (TimelineIngress(sender), state)
}
