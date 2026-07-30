//! Flat diagnostic timeline and the single Tokio-to-GPUI ingress seam.

mod seam;

pub use seam::{TimelineIngress, TimelineState, attach_ingress};

use gpui::{Context, Entity, IntoElement, Render, Window, div, prelude::*, rgb, uniform_list};
use sotto_core::{EventKind, EventPayload, TimelineEvent};

pub struct DevTimeline {
    timeline: Entity<TimelineState>,
    filter: Option<EventKind>,
}

impl DevTimeline {
    #[must_use]
    pub const fn new(timeline: Entity<TimelineState>) -> Self {
        Self {
            timeline,
            filter: None,
        }
    }

    pub fn set_filter(&mut self, filter: Option<EventKind>, cx: &mut Context<Self>) {
        self.filter = filter;
        cx.notify();
    }
}

impl Render for DevTimeline {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let state = self.timeline.read(cx);
        let rows: Vec<_> = state
            .events()
            .iter()
            .filter(|event| self.filter.is_none_or(|kind| event.kind() == kind))
            .cloned()
            .collect();
        div()
            .size_full()
            .bg(rgb(0x111318))
            .text_color(rgb(0xe6e8eb))
            .child(
                uniform_list("timeline-events", rows.len(), move |range, _, _| {
                    range
                        .map(|index| {
                            div()
                                .id(("event", index))
                                .px_3()
                                .py_2()
                                .border_b_1()
                                .border_color(rgb(0x2b3038))
                                .child(render_event(&rows[index]))
                        })
                        .collect()
                })
                .h_full(),
            )
    }
}

fn render_event(event: &TimelineEvent) -> String {
    let supersedes = event
        .supersedes()
        .map(|id| format!(" supersedes=#{}", id.get()))
        .unwrap_or_default();
    let detail = match event.payload() {
        EventPayload::UtterancePartial(value) | EventPayload::UtteranceFinal(value) => {
            value.render_inline()
        }
        EventPayload::Vad(value) => format!("{} {:?}", value.source.speaker_name(), value.kind),
        EventPayload::Prosody(value) => format!(
            "{} talk={:.0}% {:?}",
            value.source.speaker_name(),
            value.talk_time_ratio * 100.0,
            value.annotations
        ),
        EventPayload::ScreenSnapshot(value) => format!(
            "screen={} window={} OCR={}",
            value.active_app.as_deref().unwrap_or("unknown"),
            value.window_title.as_deref().unwrap_or("unknown"),
            value.ocr_text
        ),
        other => format!("{other:?}"),
    };
    format!(
        "#{:<6} {:>9.3}s {:<20}{}  {}",
        event.id().get(),
        event.ts().as_secs_f64(),
        event.kind().as_str(),
        supersedes,
        detail
    )
}
