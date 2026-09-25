//! Small runtime loops and live-session transition helpers.

use std::time::Duration;

use gpui_kit::Context;
use sotto_core::SessionId;

use super::MeetingWorkspace;

pub(super) fn newly_active_session(
    observed: &mut Option<SessionId>,
    active: Option<SessionId>,
) -> Option<SessionId> {
    let active = active?;
    if *observed == Some(active) {
        return None;
    }
    *observed = Some(active);
    Some(active)
}

impl MeetingWorkspace {
    pub(super) fn poll_notes(&mut self, cx: &mut Context<Self>) {
        let workspace = cx.weak_entity();
        cx.spawn(async move |_, cx| {
            loop {
                cx.background_executor()
                    .timer(Duration::from_millis(50))
                    .await;
                if workspace
                    .update(cx, |workspace, cx| {
                        if workspace.notes.update(cx, |notes, _| notes.poll()) {
                            workspace.rebuild_library_index(cx);
                            cx.notify();
                        }
                    })
                    .is_err()
                {
                    return;
                }
            }
        })
        .detach();
    }

    pub(super) fn poll_ask_updates(&mut self, cx: &mut Context<Self>) {
        let workspace = cx.weak_entity();
        cx.spawn(async move |_, cx| {
            loop {
                cx.background_executor()
                    .timer(Duration::from_millis(50))
                    .await;
                if workspace
                    .update(cx, |workspace, cx| {
                        if workspace.poll_ask(cx) {
                            cx.notify();
                        }
                    })
                    .is_err()
                {
                    return;
                }
            }
        })
        .detach();
    }
}

#[cfg(test)]
mod tests {
    use super::newly_active_session;
    use sotto_core::SessionId;

    #[test]
    fn live_identity_changes_once() {
        let mut seen = None;
        let id = SessionId::new(1);
        assert_eq!(newly_active_session(&mut seen, Some(id)), Some(id));
        assert_eq!(newly_active_session(&mut seen, Some(id)), None);
    }
}
